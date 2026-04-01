use crate::coord::{Bounds, Coord, Coord3D, Transformable, Transformable3D};
use crate::crs::{CrsDef, ProjectionMethod};
use crate::datum::Datum;
use crate::error::{Error, Result};
use crate::geocentric;
use crate::helmert;
use crate::projection::{make_projection, ProjectionImpl};
use crate::registry;

/// A reusable coordinate transformation between two CRS.
///
/// Create once with [`Transform::new`], then call [`convert`](Transform::convert)
/// or [`convert_3d`](Transform::convert_3d) for each coordinate. All input/output
/// coordinates use the CRS's native units: degrees (lon/lat) for geographic CRS,
/// and the CRS's native projected linear unit for projected CRS. For 3D
/// coordinates, the third ordinate is preserved
/// unchanged by the current horizontal-only pipeline.
///
/// # Example
///
/// ```
/// use proj_core::Transform;
///
/// let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
/// let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
/// assert!((x - (-8238310.0)).abs() < 1.0);
/// ```
pub struct Transform {
    source: CrsDef,
    target: CrsDef,
    pipeline: TransformPipeline,
}

enum TransformPipeline {
    /// Source and target are the same CRS — no-op.
    Identity,
    /// Same datum, geographic source → projected target.
    SameDatumForward { forward: Box<dyn ProjectionImpl> },
    /// Same datum, projected source → geographic target.
    SameDatumInverse { inverse: Box<dyn ProjectionImpl> },
    /// Same datum, projected source → projected target.
    SameDatumBoth {
        inverse: Box<dyn ProjectionImpl>,
        forward: Box<dyn ProjectionImpl>,
    },
    /// Different datums — full pipeline with geocentric conversion and Helmert shift.
    DatumShift {
        inverse: Option<Box<dyn ProjectionImpl>>,
        source_datum: Datum,
        target_datum: Datum,
        forward: Option<Box<dyn ProjectionImpl>>,
    },
}

impl Transform {
    /// Create a transform from authority code strings (e.g., `"EPSG:4326"`).
    ///
    /// This is the primary constructor, matching the API pattern from C PROJ's
    /// `Proj::new_known_crs()`.
    pub fn new(from_crs: &str, to_crs: &str) -> Result<Self> {
        let source = registry::lookup_authority_code(from_crs)?;
        let target = registry::lookup_authority_code(to_crs)?;
        Self::from_crs_defs(&source, &target)
    }

    /// Create a transform from EPSG codes directly.
    pub fn from_epsg(from: u32, to: u32) -> Result<Self> {
        let source = registry::lookup_epsg(from)
            .ok_or_else(|| Error::UnknownCrs(format!("unknown EPSG code: {from}")))?;
        let target = registry::lookup_epsg(to)
            .ok_or_else(|| Error::UnknownCrs(format!("unknown EPSG code: {to}")))?;
        Self::from_crs_defs(&source, &target)
    }

    /// Create a transform from explicit CRS definitions.
    ///
    /// Use this for custom CRS not in the built-in registry.
    pub fn from_crs_defs(from: &CrsDef, to: &CrsDef) -> Result<Self> {
        // Identity check for known EPSG codes and semantically identical custom CRS definitions.
        if (from.epsg() != 0 && from.epsg() == to.epsg()) || same_crs_definition(from, to) {
            return Ok(Self {
                source: *from,
                target: *to,
                pipeline: TransformPipeline::Identity,
            });
        }

        let source_datum = from.datum();
        let target_datum = to.datum();

        let same_datum = source_datum.same_datum(target_datum)
            || (source_datum.is_wgs84_compatible() && target_datum.is_wgs84_compatible());

        let pipeline = if same_datum {
            // Optimized path: no datum shift needed
            match (from, to) {
                (CrsDef::Geographic(_), CrsDef::Geographic(_)) => TransformPipeline::Identity,
                (CrsDef::Geographic(_), CrsDef::Projected(p)) => {
                    let forward = make_projection(&p.method(), p.datum())?;
                    TransformPipeline::SameDatumForward { forward }
                }
                (CrsDef::Projected(p), CrsDef::Geographic(_)) => {
                    let inverse = make_projection(&p.method(), p.datum())?;
                    TransformPipeline::SameDatumInverse { inverse }
                }
                (CrsDef::Projected(p_from), CrsDef::Projected(p_to)) => {
                    let inverse = make_projection(&p_from.method(), p_from.datum())?;
                    let forward = make_projection(&p_to.method(), p_to.datum())?;
                    TransformPipeline::SameDatumBoth { inverse, forward }
                }
            }
        } else {
            // Both datums must have a path to WGS84
            if !source_datum.is_wgs84_compatible() && source_datum.to_wgs84.is_none() {
                return Err(Error::UnsupportedProjection(format!(
                    "source CRS EPSG:{} has no known datum shift to WGS84",
                    from.epsg()
                )));
            }
            if !target_datum.is_wgs84_compatible() && target_datum.to_wgs84.is_none() {
                return Err(Error::UnsupportedProjection(format!(
                    "target CRS EPSG:{} has no known datum shift to WGS84",
                    to.epsg()
                )));
            }

            let inverse = match from {
                CrsDef::Projected(p) => Some(make_projection(&p.method(), p.datum())?),
                CrsDef::Geographic(_) => None,
            };
            let forward = match to {
                CrsDef::Projected(p) => Some(make_projection(&p.method(), p.datum())?),
                CrsDef::Geographic(_) => None,
            };

            TransformPipeline::DatumShift {
                inverse,
                source_datum: *source_datum,
                target_datum: *target_datum,
                forward,
            }
        };

        Ok(Self {
            source: *from,
            target: *to,
            pipeline,
        })
    }

    /// Transform a single coordinate.
    ///
    /// Input and output units are the native units of the respective CRS:
    /// degrees for geographic CRS, and the CRS's native projected linear unit
    /// for projected CRS.
    ///
    /// The return type matches the input type:
    /// - `(f64, f64)` in → `(f64, f64)` out
    /// - `Coord` in → `Coord` out
    /// - `geo_types::Coord<f64>` in → `geo_types::Coord<f64>` out (with `geo-types` feature)
    pub fn convert<T: Transformable>(&self, coord: T) -> Result<T> {
        let c = coord.into_coord();
        let result = self.convert_coord(c)?;
        Ok(T::from_coord(result))
    }

    /// Transform a single 3D coordinate.
    ///
    /// The horizontal components use the CRS's native units:
    /// degrees for geographic CRS and the CRS's native projected linear unit
    /// for projected CRS.
    /// The vertical component is carried through unchanged because the current
    /// CRS model is horizontal-only.
    ///
    /// The return type matches the input type:
    /// - `(f64, f64, f64)` in → `(f64, f64, f64)` out
    /// - `Coord3D` in → `Coord3D` out
    pub fn convert_3d<T: Transformable3D>(&self, coord: T) -> Result<T> {
        let c = coord.into_coord3d();
        let result = self.convert_coord3d(c)?;
        Ok(T::from_coord3d(result))
    }

    /// Return the source CRS definition for this transform.
    pub fn source_crs(&self) -> &CrsDef {
        &self.source
    }

    /// Return the target CRS definition for this transform.
    pub fn target_crs(&self) -> &CrsDef {
        &self.target
    }

    /// Build the inverse transform by swapping the source and target CRS.
    pub fn inverse(&self) -> Result<Self> {
        Self::from_crs_defs(&self.target, &self.source)
    }

    /// Reproject a 2D bounding box by sampling its perimeter.
    ///
    /// `densify_points` controls how many evenly spaced interior points are sampled
    /// on each edge between the corners. `0` samples only the four corners.
    ///
    /// The returned bounds are axis-aligned in the target CRS. Geographic outputs
    /// that cross the antimeridian are not normalized into a wrapped representation.
    pub fn transform_bounds(&self, bounds: Bounds, densify_points: usize) -> Result<Bounds> {
        if !bounds.is_valid() {
            return Err(Error::OutOfRange(
                "bounds must be finite and satisfy min <= max".into(),
            ));
        }

        let segments = densify_points
            .checked_add(1)
            .ok_or_else(|| Error::OutOfRange("densify point count is too large".into()))?;

        let mut transformed: Option<Bounds> = None;
        for i in 0..=segments {
            let t = i as f64 / segments as f64;
            let x = bounds.min_x + bounds.width() * t;
            let y = bounds.min_y + bounds.height() * t;

            for sample in [
                Coord::new(x, bounds.min_y),
                Coord::new(x, bounds.max_y),
                Coord::new(bounds.min_x, y),
                Coord::new(bounds.max_x, y),
            ] {
                let coord = self.convert_coord(sample)?;
                if let Some(accum) = &mut transformed {
                    accum.expand_to_include(coord);
                } else {
                    transformed = Some(Bounds::new(coord.x, coord.y, coord.x, coord.y));
                }
            }
        }

        transformed.ok_or_else(|| Error::OutOfRange("failed to sample bounds".into()))
    }

    /// Transform a single `Coord` value.
    fn convert_coord(&self, c: Coord) -> Result<Coord> {
        let result = self.convert_coord3d(Coord3D::new(c.x, c.y, 0.0))?;
        Ok(Coord {
            x: result.x,
            y: result.y,
        })
    }

    /// Transform a single `Coord3D` value.
    fn convert_coord3d(&self, c: Coord3D) -> Result<Coord3D> {
        match &self.pipeline {
            TransformPipeline::Identity => Ok(c),

            TransformPipeline::SameDatumForward { forward } => {
                // Source is geographic (degrees) → radians → forward project → meters
                // → target native projected units.
                let lon_rad = c.x.to_radians();
                let lat_rad = c.y.to_radians();
                let (x_m, y_m) = forward.forward(lon_rad, lat_rad)?;
                let (x, y) = self.projected_meters_to_target_native(x_m, y_m);
                Ok(Coord3D { x, y, z: c.z })
            }

            TransformPipeline::SameDatumInverse { inverse } => {
                // Source is projected (native units) → meters → inverse project
                // → radians → degrees.
                let (x_m, y_m) = self.source_projected_native_to_meters(c.x, c.y);
                let (lon_rad, lat_rad) = inverse.inverse(x_m, y_m)?;
                Ok(Coord3D {
                    x: lon_rad.to_degrees(),
                    y: lat_rad.to_degrees(),
                    z: c.z,
                })
            }

            TransformPipeline::SameDatumBoth { inverse, forward } => {
                // Projected native units → meters → inverse → radians → forward
                // → meters → target native units.
                let (x_m, y_m) = self.source_projected_native_to_meters(c.x, c.y);
                let (lon_rad, lat_rad) = inverse.inverse(x_m, y_m)?;
                let (x_m, y_m) = forward.forward(lon_rad, lat_rad)?;
                let (x, y) = self.projected_meters_to_target_native(x_m, y_m);
                Ok(Coord3D { x, y, z: c.z })
            }

            TransformPipeline::DatumShift {
                inverse,
                source_datum,
                target_datum,
                forward,
            } => {
                // Step 1: Get geographic coords in radians on source datum
                let (lon_rad, lat_rad) = if let Some(inv) = inverse {
                    let (x_m, y_m) = self.source_projected_native_to_meters(c.x, c.y);
                    inv.inverse(x_m, y_m)?
                } else {
                    (c.x.to_radians(), c.y.to_radians())
                };

                // Step 2: Source geodetic → geocentric (source ellipsoid)
                let (x, y, z) = geocentric::geodetic_to_geocentric(
                    &source_datum.ellipsoid,
                    lon_rad,
                    lat_rad,
                    0.0,
                );

                // Step 3: Helmert: source datum → WGS84
                let (x2, y2, z2) = if let Some(params) = &source_datum.to_wgs84 {
                    helmert::helmert_forward(params, x, y, z)
                } else {
                    (x, y, z) // already WGS84
                };

                // Step 4: Helmert: WGS84 → target datum (inverse)
                let (x3, y3, z3) = if let Some(params) = &target_datum.to_wgs84 {
                    helmert::helmert_inverse(params, x2, y2, z2)
                } else {
                    (x2, y2, z2) // target is WGS84
                };

                // Step 5: Geocentric → geodetic (target ellipsoid)
                let (lon_out, lat_out, _h_out) =
                    geocentric::geocentric_to_geodetic(&target_datum.ellipsoid, x3, y3, z3);

                // Step 6: Forward project if target is projected, else convert to degrees
                if let Some(fwd) = forward {
                    let (x_m, y_m) = fwd.forward(lon_out, lat_out)?;
                    let (x, y) = self.projected_meters_to_target_native(x_m, y_m);
                    Ok(Coord3D { x, y, z: c.z })
                } else {
                    Ok(Coord3D {
                        x: lon_out.to_degrees(),
                        y: lat_out.to_degrees(),
                        z: c.z,
                    })
                }
            }
        }
    }

    fn source_projected_native_to_meters(&self, x: f64, y: f64) -> (f64, f64) {
        match self.source {
            CrsDef::Projected(p) => (p.linear_unit().to_meters(x), p.linear_unit().to_meters(y)),
            CrsDef::Geographic(_) => (x, y),
        }
    }

    fn projected_meters_to_target_native(&self, x: f64, y: f64) -> (f64, f64) {
        match self.target {
            CrsDef::Projected(p) => (
                p.linear_unit().from_meters(x),
                p.linear_unit().from_meters(y),
            ),
            CrsDef::Geographic(_) => (x, y),
        }
    }

    /// Batch transform (sequential).
    pub fn convert_batch<T: Transformable + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert(c.clone())).collect()
    }

    /// Batch transform of 3D coordinates (sequential).
    pub fn convert_batch_3d<T: Transformable3D + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert_3d(c.clone())).collect()
    }

    /// Batch transform with Rayon parallelism.
    #[cfg(feature = "rayon")]
    pub fn convert_batch_parallel<T: Transformable + Send + Sync + Clone>(
        &self,
        coords: &[T],
    ) -> Result<Vec<T>> {
        use rayon::prelude::*;
        coords.par_iter().map(|c| self.convert(c.clone())).collect()
    }

    /// Batch transform of 3D coordinates with Rayon parallelism.
    #[cfg(feature = "rayon")]
    pub fn convert_batch_parallel_3d<T: Transformable3D + Send + Sync + Clone>(
        &self,
        coords: &[T],
    ) -> Result<Vec<T>> {
        use rayon::prelude::*;
        coords
            .par_iter()
            .map(|c| self.convert_3d(c.clone()))
            .collect()
    }
}

fn same_crs_definition(from: &CrsDef, to: &CrsDef) -> bool {
    match (from, to) {
        (CrsDef::Geographic(a), CrsDef::Geographic(b)) => a.datum().same_datum(b.datum()),
        (CrsDef::Projected(a), CrsDef::Projected(b)) => {
            a.datum().same_datum(b.datum())
                && approx_eq(a.linear_unit_to_meter(), b.linear_unit_to_meter())
                && projection_methods_equivalent(&a.method(), &b.method())
        }
        _ => false,
    }
}

fn projection_methods_equivalent(a: &ProjectionMethod, b: &ProjectionMethod) -> bool {
    match (a, b) {
        (ProjectionMethod::WebMercator, ProjectionMethod::WebMercator) => true,
        (
            ProjectionMethod::TransverseMercator {
                lon0: a_lon0,
                lat0: a_lat0,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::TransverseMercator {
                lon0: b_lon0,
                lat0: b_lat0,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::PolarStereographic {
                lon0: a_lon0,
                lat_ts: a_lat_ts,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::PolarStereographic {
                lon0: b_lon0,
                lat_ts: b_lat_ts,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat_ts, *b_lat_ts)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::LambertConformalConic {
                lon0: a_lon0,
                lat0: a_lat0,
                lat1: a_lat1,
                lat2: a_lat2,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::LambertConformalConic {
                lon0: b_lon0,
                lat0: b_lat0,
                lat1: b_lat1,
                lat2: b_lat2,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_lat1, *b_lat1)
                && approx_eq(*a_lat2, *b_lat2)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::AlbersEqualArea {
                lon0: a_lon0,
                lat0: a_lat0,
                lat1: a_lat1,
                lat2: a_lat2,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::AlbersEqualArea {
                lon0: b_lon0,
                lat0: b_lat0,
                lat1: b_lat1,
                lat2: b_lat2,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_lat1, *b_lat1)
                && approx_eq(*a_lat2, *b_lat2)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::Mercator {
                lon0: a_lon0,
                lat_ts: a_lat_ts,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::Mercator {
                lon0: b_lon0,
                lat_ts: b_lat_ts,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat_ts, *b_lat_ts)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::EquidistantCylindrical {
                lon0: a_lon0,
                lat_ts: a_lat_ts,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::EquidistantCylindrical {
                lon0: b_lon0,
                lat_ts: b_lat_ts,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat_ts, *b_lat_ts)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        _ => false,
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crs::{CrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod};
    use crate::datum;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

    #[test]
    fn identity_same_crs() {
        let t = Transform::new("EPSG:4326", "EPSG:4326").unwrap();
        let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
        assert_eq!(x, -74.006);
        assert_eq!(y, 40.7128);
    }

    #[test]
    fn wgs84_to_web_mercator() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
        // NYC in Web Mercator
        assert!((x - (-8238310.0)).abs() < 100.0, "x = {x}");
        assert!((y - 4970072.0).abs() < 100.0, "y = {y}");
    }

    #[test]
    fn web_mercator_to_wgs84() {
        let t = Transform::new("EPSG:3857", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-8238310.0, 4970072.0)).unwrap();
        assert!((lon - (-74.006)).abs() < 0.001, "lon = {lon}");
        assert!((lat - 40.7128).abs() < 0.001, "lat = {lat}");
    }

    #[test]
    fn roundtrip_4326_3857() {
        let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let inv = fwd.inverse().unwrap();

        let original = (-74.0445, 40.6892);
        let projected = fwd.convert(original).unwrap();
        let back = inv.convert(projected).unwrap();

        assert!(
            (back.0 - original.0).abs() < 1e-8,
            "lon: {} vs {}",
            back.0,
            original.0
        );
        assert!(
            (back.1 - original.1).abs() < 1e-8,
            "lat: {} vs {}",
            back.1,
            original.1
        );
    }

    #[test]
    fn wgs84_to_utm_18n() {
        let t = Transform::new("EPSG:4326", "EPSG:32618").unwrap();
        let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - 583960.0).abs() < 1.0, "easting = {x}");
        assert!(y > 4_500_000.0 && y < 4_510_000.0, "northing = {y}");
    }

    #[test]
    fn equivalent_meter_and_foot_state_plane_crs_match_after_unit_conversion() {
        let coord = (-80.8431, 35.2271); // Charlotte, NC
        let meter_tx = Transform::new("EPSG:4326", "EPSG:32119").unwrap();
        let foot_tx = Transform::new("EPSG:4326", "EPSG:2264").unwrap();

        let (mx, my) = meter_tx.convert(coord).unwrap();
        let (fx, fy) = foot_tx.convert(coord).unwrap();

        assert!(
            (fx * US_FOOT_TO_METER - mx).abs() < 0.02,
            "x mismatch: {fx} ft vs {mx} m"
        );
        assert!(
            (fy * US_FOOT_TO_METER - my).abs() < 0.02,
            "y mismatch: {fy} ft vs {my} m"
        );
    }

    #[test]
    fn inverse_transform_accepts_native_projected_units_for_foot_crs() {
        let coord = (-80.8431, 35.2271); // Charlotte, NC
        let forward = Transform::new("EPSG:4326", "EPSG:2264").unwrap();
        let inverse = Transform::new("EPSG:2264", "EPSG:4326").unwrap();

        let projected = forward.convert(coord).unwrap();
        let roundtrip = inverse.convert(projected).unwrap();

        assert!((roundtrip.0 - coord.0).abs() < 1e-8, "lon: {}", roundtrip.0);
        assert!((roundtrip.1 - coord.1).abs() < 1e-8, "lat: {}", roundtrip.1);
    }

    #[test]
    fn utm_to_web_mercator() {
        // Projected → Projected (SameDatumBoth)
        let t = Transform::new("EPSG:32618", "EPSG:3857").unwrap();
        // NYC in UTM 18N
        let (x, _y) = t.convert((583960.0, 4507523.0)).unwrap();
        // Should be near NYC in Web Mercator
        assert!((x - (-8238310.0)).abs() < 200.0, "x = {x}");
    }

    #[test]
    fn wgs84_to_polar_stereo_3413() {
        let t = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
        let (x, y) = t.convert((-45.0, 90.0)).unwrap();
        // North pole should be at origin for EPSG:3413
        assert!(x.abs() < 1.0, "x = {x}");
        assert!(y.abs() < 1.0, "y = {y}");
    }

    #[test]
    fn roundtrip_4326_3413() {
        let fwd = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
        let inv = fwd.inverse().unwrap();

        let original = (-45.0, 75.0);
        let projected = fwd.convert(original).unwrap();
        let back = inv.convert(projected).unwrap();

        assert!(
            (back.0 - original.0).abs() < 1e-6,
            "lon: {} vs {}",
            back.0,
            original.0
        );
        assert!(
            (back.1 - original.1).abs() < 1e-6,
            "lat: {} vs {}",
            back.1,
            original.1
        );
    }

    #[test]
    fn geographic_to_geographic_same_datum_is_identity() {
        // NAD83 and WGS84 are both WGS84-compatible
        let t = Transform::new("EPSG:4269", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-74.006, 40.7128)).unwrap();
        assert_eq!(lon, -74.006);
        assert_eq!(lat, 40.7128);
    }

    #[test]
    fn unknown_crs_error() {
        let result = Transform::new("EPSG:99999", "EPSG:4326");
        assert!(result.is_err());
    }

    #[test]
    fn cross_datum_nad27_to_wgs84() {
        let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-90.0, 45.0)).unwrap();
        // NAD27→WGS84 shift is small (tens of meters → fraction of degree)
        assert!((lon - (-90.0)).abs() < 0.01, "lon = {lon}");
        assert!((lat - 45.0).abs() < 0.01, "lat = {lat}");
    }

    #[test]
    fn cross_datum_roundtrip_nad27() {
        let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let inv = fwd.inverse().unwrap();
        let original = (-90.0, 45.0);
        let shifted = fwd.convert(original).unwrap();
        let back = inv.convert(shifted).unwrap();
        assert!(
            (back.0 - original.0).abs() < 1e-6,
            "lon: {} vs {}",
            back.0,
            original.0
        );
        assert!(
            (back.1 - original.1).abs() < 1e-6,
            "lat: {} vs {}",
            back.1,
            original.1
        );
    }

    #[test]
    fn cross_datum_osgb36_to_wgs84() {
        let t = Transform::new("EPSG:4277", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-0.1278, 51.5074)).unwrap(); // London
                                                                 // OSGB36→WGS84 shift is larger due to 7-parameter Helmert
        assert!((lon - (-0.1278)).abs() < 0.01, "lon = {lon}");
        assert!((lat - 51.5074).abs() < 0.01, "lat = {lat}");
    }

    #[test]
    fn wgs84_to_web_mercator_3d_preserves_height() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0, "x = {x}");
        assert!((y - 4970072.0).abs() < 100.0, "y = {y}");
        assert!((z - 123.45).abs() < 1e-12, "z = {z}");
    }

    #[test]
    fn cross_datum_roundtrip_nad27_3d() {
        let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let inv = fwd.inverse().unwrap();
        let original = (-90.0, 45.0, 250.0);
        let shifted = fwd.convert_3d(original).unwrap();
        let back = inv.convert_3d(shifted).unwrap();
        assert!(
            (back.0 - original.0).abs() < 1e-6,
            "lon: {} vs {}",
            back.0,
            original.0
        );
        assert!(
            (back.1 - original.1).abs() < 1e-6,
            "lat: {} vs {}",
            back.1,
            original.1
        );
        assert!(
            (back.2 - original.2).abs() < 1e-12,
            "h: {} vs {}",
            back.2,
            original.2
        );
    }

    #[test]
    fn identical_custom_projected_crs_is_identity() {
        let from = CrsDef::Projected(ProjectedCrsDef::new(
            0,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "Custom Web Mercator A",
        ));
        let to = CrsDef::Projected(ProjectedCrsDef::new(
            0,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "Custom Web Mercator B",
        ));

        let t = Transform::from_crs_defs(&from, &to).unwrap();
        assert!(matches!(t.pipeline, TransformPipeline::Identity));
    }

    #[test]
    fn inverse_exposes_swapped_crs() {
        let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let inv = fwd.inverse().unwrap();

        assert_eq!(fwd.source_crs().epsg(), 4326);
        assert_eq!(fwd.target_crs().epsg(), 3857);
        assert_eq!(inv.source_crs().epsg(), 3857);
        assert_eq!(inv.target_crs().epsg(), 4326);
    }

    #[test]
    fn transform_bounds_web_mercator() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let bounds = Bounds::new(-74.3, 40.45, -73.65, 40.95);

        let result = t.transform_bounds(bounds, 8).unwrap();

        assert!(result.min_x < -8_200_000.0);
        assert!(result.max_x < -8_100_000.0);
        assert!(result.min_y > 4_900_000.0);
        assert!(result.max_y > result.min_y);
    }

    #[test]
    fn transform_bounds_rejects_invalid_input() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let err = t
            .transform_bounds(Bounds::new(10.0, 5.0, -10.0, 20.0), 0)
            .unwrap_err();

        assert!(matches!(err, Error::OutOfRange(_)));
    }

    #[test]
    fn batch_transform() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64)> = (0..10)
            .map(|i| (-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
            .collect();

        let results = t.convert_batch(&coords).unwrap();
        assert_eq!(results.len(), 10);
        for (x, _y) in &results {
            assert!(*x < 0.0); // all points are west of prime meridian
        }
    }

    #[test]
    fn batch_transform_3d() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64, f64)> = (0..10)
            .map(|i| (-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1, i as f64))
            .collect();

        let results = t.convert_batch_3d(&coords).unwrap();
        assert_eq!(results.len(), 10);
        for (index, (x, _y, z)) in results.iter().enumerate() {
            assert!(*x < 0.0);
            assert!((*z - index as f64).abs() < 1e-12);
        }
    }

    #[test]
    fn coord_type() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let c = Coord::new(-74.006, 40.7128);
        let result = t.convert(c).unwrap();
        assert!((result.x - (-8238310.0)).abs() < 100.0);
    }

    #[cfg(feature = "geo-types")]
    #[test]
    fn geo_types_coord() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let c = geo_types::Coord {
            x: -74.006,
            y: 40.7128,
        };
        let result: geo_types::Coord<f64> = t.convert(c).unwrap();
        assert!((result.x - (-8238310.0)).abs() < 100.0);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batch_transform() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64)> = (0..100)
            .map(|i| (-74.0 + i as f64 * 0.01, 40.0 + i as f64 * 0.01))
            .collect();

        let results = t.convert_batch_parallel(&coords).unwrap();
        assert_eq!(results.len(), 100);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batch_transform_3d() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64, f64)> = (0..100)
            .map(|i| (-74.0 + i as f64 * 0.01, 40.0 + i as f64 * 0.01, i as f64))
            .collect();

        let results = t.convert_batch_parallel_3d(&coords).unwrap();
        assert_eq!(results.len(), 100);
    }
}

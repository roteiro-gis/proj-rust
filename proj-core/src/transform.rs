use crate::coord::{Coord, Transformable};
use crate::crs::CrsDef;
use crate::datum::Datum;
use crate::error::{Error, Result};
use crate::geocentric;
use crate::helmert;
use crate::projection::{make_projection, ProjectionImpl};
use crate::registry;

/// A reusable coordinate transformation between two CRS.
///
/// Create once with [`Transform::new`], then call [`convert`](Transform::convert)
/// for each coordinate. All input/output coordinates use the CRS's native units:
/// degrees (lon/lat) for geographic CRS, meters for projected CRS.
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
        // Identity check — only for known EPSG codes (not custom CRS with epsg=0)
        if from.epsg() != 0 && from.epsg() == to.epsg() {
            return Ok(Self {
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
                    let forward = make_projection(&p.method, &p.datum)?;
                    TransformPipeline::SameDatumForward { forward }
                }
                (CrsDef::Projected(p), CrsDef::Geographic(_)) => {
                    let inverse = make_projection(&p.method, &p.datum)?;
                    TransformPipeline::SameDatumInverse { inverse }
                }
                (CrsDef::Projected(p_from), CrsDef::Projected(p_to)) => {
                    let inverse = make_projection(&p_from.method, &p_from.datum)?;
                    let forward = make_projection(&p_to.method, &p_to.datum)?;
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
                CrsDef::Projected(p) => Some(make_projection(&p.method, &p.datum)?),
                CrsDef::Geographic(_) => None,
            };
            let forward = match to {
                CrsDef::Projected(p) => Some(make_projection(&p.method, &p.datum)?),
                CrsDef::Geographic(_) => None,
            };

            TransformPipeline::DatumShift {
                inverse,
                source_datum: *source_datum,
                target_datum: *target_datum,
                forward,
            }
        };

        Ok(Self { pipeline })
    }

    /// Transform a single coordinate.
    ///
    /// Input and output units are the native units of the respective CRS:
    /// degrees for geographic CRS, meters for projected CRS.
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

    /// Transform a single `Coord` value.
    fn convert_coord(&self, c: Coord) -> Result<Coord> {
        match &self.pipeline {
            TransformPipeline::Identity => Ok(c),

            TransformPipeline::SameDatumForward { forward } => {
                // Source is geographic (degrees) → radians → forward project → meters
                let lon_rad = c.x.to_radians();
                let lat_rad = c.y.to_radians();
                let (x, y) = forward.forward(lon_rad, lat_rad)?;
                Ok(Coord { x, y })
            }

            TransformPipeline::SameDatumInverse { inverse } => {
                // Source is projected (meters) → inverse project → radians → degrees
                let (lon_rad, lat_rad) = inverse.inverse(c.x, c.y)?;
                Ok(Coord {
                    x: lon_rad.to_degrees(),
                    y: lat_rad.to_degrees(),
                })
            }

            TransformPipeline::SameDatumBoth { inverse, forward } => {
                // Projected → inverse → radians → forward → projected
                let (lon_rad, lat_rad) = inverse.inverse(c.x, c.y)?;
                let (x, y) = forward.forward(lon_rad, lat_rad)?;
                Ok(Coord { x, y })
            }

            TransformPipeline::DatumShift {
                inverse,
                source_datum,
                target_datum,
                forward,
            } => {
                // Step 1: Get geographic coords in radians on source datum
                let (lon_rad, lat_rad) = if let Some(inv) = inverse {
                    inv.inverse(c.x, c.y)?
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
                let (lon_out, lat_out, _h) =
                    geocentric::geocentric_to_geodetic(&target_datum.ellipsoid, x3, y3, z3);

                // Step 6: Forward project if target is projected, else convert to degrees
                if let Some(fwd) = forward {
                    let (x, y) = fwd.forward(lon_out, lat_out)?;
                    Ok(Coord { x, y })
                } else {
                    Ok(Coord {
                        x: lon_out.to_degrees(),
                        y: lat_out.to_degrees(),
                    })
                }
            }
        }
    }

    /// Batch transform (sequential).
    pub fn convert_batch<T: Transformable + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert(c.clone())).collect()
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let inv = Transform::new("EPSG:3857", "EPSG:4326").unwrap();

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
        let inv = Transform::new("EPSG:3413", "EPSG:4326").unwrap();

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
        let inv = Transform::new("EPSG:4326", "EPSG:4267").unwrap();
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
}

use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, meridian_arc, meridian_arc_coefficients,
    normalize_longitude, validate_angle, validate_latitude_param, validate_lon_lat,
    validate_offset, validate_projected,
};

const EQUATOR_TOL: f64 = 1e-10;
const NEWTON_TOL: f64 = 1e-12;
const NEWTON_ITERATIONS: usize = 20;

/// American Polyconic (EPSG method 9818), matching C PROJ's `poly`.
///
/// Each parallel is projected as the arc of a tangent cone, so the projection
/// is neither conformal nor equal-area; it survives in the Brazilian national
/// grids.
#[derive(Clone)]
pub(crate) struct AmericanPolyconic {
    a: f64,
    e2: f64,
    lon0: f64,
    lat0: f64,
    false_easting: f64,
    false_northing: f64,
    en: [f64; 13],
    /// Meridional arc at the latitude of origin (adimensional; the negated
    /// origin latitude in the spherical case).
    ml0: f64,
}

impl AmericanPolyconic {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_latitude_param("latitude of natural origin", lat0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let e2 = ellipsoid.e2();
        let en = meridian_arc_coefficients(e2);
        let ml0 = if e2 == 0.0 {
            -lat0
        } else {
            meridian_arc(lat0, lat0.sin(), lat0.cos(), &en)
        };

        Ok(Self {
            a: ellipsoid.semi_major_axis(),
            e2,
            lon0,
            lat0,
            false_easting,
            false_northing,
            en,
            ml0,
        })
    }
}

impl super::ProjectionImpl for AmericanPolyconic {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);

        let (x, y) = if self.e2 == 0.0 {
            if lat.abs() <= EQUATOR_TOL {
                (lam, self.ml0)
            } else {
                let cot = 1.0 / lat.tan();
                let e = lam * lat.sin();
                (e.sin() * cot, lat - self.lat0 + cot * (1.0 - e.cos()))
            }
        } else if lat.abs() <= EQUATOR_TOL {
            (lam, -self.ml0)
        } else {
            let sp = lat.sin();
            let cp = lat.cos();
            let ms = if cp.abs() > EQUATOR_TOL {
                cp / (1.0 - self.e2 * sp * sp).sqrt() / sp
            } else {
                0.0
            };
            let e = lam * sp;
            (
                ms * e.sin(),
                (meridian_arc(lat, sp, cp, &self.en) - self.ml0) + ms * (1.0 - e.cos()),
            )
        };

        ensure_finite_xy(
            "American Polyconic",
            self.false_easting + self.a * x,
            self.false_northing + self.a * y,
        )
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let xs = (x - self.false_easting) / self.a;

        if self.e2 == 0.0 {
            let ys = self.lat0 + (y - self.false_northing) / self.a;
            if ys.abs() <= EQUATOR_TOL {
                return ensure_finite_lon_lat("American Polyconic", self.lon0 + xs, 0.0);
            }
            let b = xs * xs + ys * ys;
            let mut phi = ys;
            for iteration in 0.. {
                if iteration == NEWTON_ITERATIONS {
                    return Err(Error::NonConvergence {
                        context: "American Polyconic inverse latitude",
                        iterations: NEWTON_ITERATIONS,
                    });
                }
                let tp = phi.tan();
                let dphi = (ys * (phi * tp + 1.0) - phi - 0.5 * (phi * phi + b) * tp)
                    / ((phi - ys) / tp - 1.0);
                phi -= dphi;
                if dphi.abs() <= NEWTON_TOL {
                    break;
                }
            }
            let lam = (xs * phi.tan()).asin() / phi.sin();
            return ensure_finite_lon_lat("American Polyconic", self.lon0 + lam, phi);
        }

        let ys = (y - self.false_northing) / self.a + self.ml0;
        if ys.abs() <= EQUATOR_TOL {
            return ensure_finite_lon_lat("American Polyconic", self.lon0 + xs, 0.0);
        }

        let r = ys * ys + xs * xs;
        let mut phi = ys;
        for iteration in 0.. {
            if iteration == NEWTON_ITERATIONS {
                return Err(Error::NonConvergence {
                    context: "American Polyconic inverse latitude",
                    iterations: NEWTON_ITERATIONS,
                });
            }
            let sp = phi.sin();
            let cp = phi.cos();
            if cp.abs() < NEWTON_TOL {
                return Err(Error::OutOfRange(
                    "American Polyconic inverse is undefined at the pole".into(),
                ));
            }
            let s2ph = sp * cp;
            let nu = (1.0 - self.e2 * sp * sp).sqrt();
            let c = sp * nu / cp;
            let ml = meridian_arc(phi, sp, cp, &self.en);
            let mlb = ml * ml + r;
            let mlp = (1.0 - self.e2) / (nu * nu * nu);
            let dphi = (ml + ml + c * mlb - 2.0 * ys * (c * ml + 1.0))
                / (self.e2 * s2ph * (mlb - 2.0 * ys * ml) / c
                    + 2.0 * (ys - ml) * (c * mlp - 1.0 / s2ph)
                    - mlp
                    - mlp);
            phi += dphi;
            if dphi.abs() <= NEWTON_TOL {
                break;
            }
        }

        let sp = phi.sin();
        let lam = (xs * phi.tan() * (1.0 - self.e2 * sp * sp).sqrt()).asin() / sp;
        ensure_finite_lon_lat("American Polyconic", self.lon0 + lam, phi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    fn poly_grs80() -> AmericanPolyconic {
        AmericanPolyconic::new(ellipsoid::GRS80, 0.0, 0.0, 0.0, 0.0).unwrap()
    }

    /// Expectations from C PROJ's gie suite (`+proj=poly +ellps=GRS80`,
    /// 0.1 mm tolerance).
    #[test]
    fn matches_c_proj_gie_vectors() {
        let proj = poly_grs80();
        let cases: [((f64, f64), (f64, f64)); 4] = [
            ((2.0, 1.0), (222_605.285770237, 110_642.194561440)),
            ((2.0, -1.0), (222_605.285770237, -110_642.194561440)),
            ((-2.0, 1.0), (-222_605.285770237, 110_642.194561440)),
            ((-2.0, -1.0), (-222_605.285770237, -110_642.194561440)),
        ];
        for ((lon, lat), (ex, ey)) in cases {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            assert!((x - ex).abs() < 1e-4, "({lon},{lat}): x = {x} vs {ex}");
            assert!((y - ey).abs() < 1e-4, "({lon},{lat}): y = {y} vs {ey}");
        }
    }

    #[test]
    fn inverse_matches_c_proj_gie_vectors() {
        let proj = poly_grs80();
        let cases: [((f64, f64), (f64, f64)); 2] = [
            ((200.0, 100.0), (0.001796631, 0.000904369)),
            ((-200.0, -100.0), (-0.001796631, -0.000904369)),
        ];
        for ((x, y), (elon, elat)) in cases {
            let (lon, lat) = proj.inverse(x, y).unwrap();
            assert!(
                (lon.to_degrees() - elon).abs() < 1e-9,
                "lon = {}",
                lon.to_degrees()
            );
            assert!(
                (lat.to_degrees() - elat).abs() < 1e-9,
                "lat = {}",
                lat.to_degrees()
            );
        }
    }

    #[test]
    fn roundtrip_brazil() {
        // SIRGAS 2000 / Brazil Polyconic parameters (EPSG:5880).
        let proj = AmericanPolyconic::new(
            ellipsoid::GRS80,
            (-54.0_f64).to_radians(),
            0.0,
            5_000_000.0,
            10_000_000.0,
        )
        .unwrap();
        for (lon, lat) in [
            (-43.2_f64, -22.9_f64),
            (-46.6, -23.5),
            (-60.0, 2.8),
            (-67.8, -9.97),
        ] {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            assert!(
                (lon2.to_degrees() - lon).abs() < 1e-9,
                "lon {lon}: {}",
                lon2.to_degrees()
            );
            assert!(
                (lat2.to_degrees() - lat).abs() < 1e-9,
                "lat {lat}: {}",
                lat2.to_degrees()
            );
        }
    }

    #[test]
    fn equator_line_is_linear() {
        let proj = poly_grs80();
        let (x, y) = proj.forward(2.0_f64.to_radians(), 0.0).unwrap();
        assert!((y - 0.0).abs() < 1e-9, "y = {y}");
        assert!(x > 0.0);
        let (lon, lat) = proj.inverse(x, y).unwrap();
        assert!((lon.to_degrees() - 2.0).abs() < 1e-9);
        assert!(lat.abs() < 1e-12);
    }

    #[test]
    fn spherical_matches_c_proj_gie_vectors() {
        let proj =
            AmericanPolyconic::new(Ellipsoid::sphere(6_400_000.0).unwrap(), 0.0, 0.0, 0.0, 0.0)
                .unwrap();
        let (x, y) = proj
            .forward(2.0_f64.to_radians(), 1.0_f64.to_radians())
            .unwrap();
        assert!((x - 223_368.105210219).abs() < 1e-4, "x = {x}");
        assert!((y - 111_769.110491225).abs() < 1e-4, "y = {y}");
        let (lon, lat) = proj.inverse(200.0, 100.0).unwrap();
        assert!((lon.to_degrees() - 0.001790493).abs() < 1e-9);
        assert!((lat.to_degrees() - 0.000895247).abs() < 1e-9);
    }
}

use crate::ellipsoid::Ellipsoid;
use crate::error::Result;
use crate::projection::{
    authalic_q, converge, ensure_finite_lon_lat, ensure_finite_xy, geodetic_from_authalic,
    normalize_longitude, validate_angle, validate_lon_lat, validate_offset, validate_projected,
};

/// Equal Earth (EPSG method 1078).
///
/// Pseudocylindrical equal-area world projection (Šavrič, Patterson & Jenny,
/// 2018). The ellipsoidal form maps through the authalic latitude; formulas
/// match C PROJ's `eqearth`.
#[derive(Clone)]
pub(crate) struct EqualEarth {
    a: f64,
    e2: f64,
    lon0: f64,
    false_easting: f64,
    false_northing: f64,
    /// Polar authalic `q`, for converting to the authalic latitude.
    qp: f64,
    /// Authalic radius divided by the semi-major axis.
    rqda: f64,
}

/// Equal Earth polynomial coefficients.
const A1: f64 = 1.340264;
const A2: f64 = -0.081106;
const A3: f64 = 0.000893;
const A4: f64 = 0.003796;

/// The parametric latitude ψ of the geodetic pole: asin(M·1) scaled through
/// the polynomial gives y = 1.3173627591574 on the unit sphere.
const MAX_Y: f64 = 1.3173627591574;

fn m_const() -> f64 {
    3.0_f64.sqrt() / 2.0
}

/// dy/dψ of the Equal Earth polynomial.
fn y_derivative(psi2: f64, psi6: f64) -> f64 {
    A1 + 3.0 * A2 * psi2 + psi6 * (7.0 * A3 + 9.0 * A4 * psi2)
}

impl EqualEarth {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let e2 = ellipsoid.e2();
        let (qp, rqda) = if e2 == 0.0 {
            (2.0, 1.0)
        } else {
            let qp = authalic_q(std::f64::consts::FRAC_PI_2, e2);
            (qp, (0.5 * qp).sqrt())
        };

        Ok(Self {
            a: ellipsoid.semi_major_axis(),
            e2,
            lon0,
            false_easting,
            false_northing,
            qp,
            rqda,
        })
    }
}

impl super::ProjectionImpl for EqualEarth {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);

        let sbeta = if self.e2 == 0.0 {
            lat.sin()
        } else {
            (authalic_q(lat, self.e2) / self.qp).clamp(-1.0, 1.0)
        };

        let psi = (m_const() * sbeta).asin();
        let psi2 = psi * psi;
        let psi6 = psi2 * psi2 * psi2;

        let scale = self.a * self.rqda;
        let x =
            self.false_easting + scale * lam * psi.cos() / (m_const() * y_derivative(psi2, psi6));
        let y = self.false_northing + scale * psi * (A1 + A2 * psi2 + psi6 * (A3 + A4 * psi2));
        ensure_finite_xy("Equal Earth", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let scale = self.a * self.rqda;
        let xs = (x - self.false_easting) / scale;
        // C PROJ clamps out-of-range northings to the pole instead of
        // erroring; keep that behavior for parity.
        let ys = ((y - self.false_northing) / scale).clamp(-MAX_Y, MAX_Y);

        // Newton-Raphson for the parametric latitude ψ.
        let psi = converge("Equal Earth inverse latitude", ys, 15, 1e-11, |yc| {
            let y2 = yc * yc;
            let y6 = y2 * y2 * y2;
            let f = yc * (A1 + A2 * y2 + y6 * (A3 + A4 * y2)) - ys;
            yc - f / y_derivative(y2, y6)
        })?;

        let psi2 = psi * psi;
        let psi6 = psi2 * psi2 * psi2;
        let lon = self.lon0 + m_const() * xs * y_derivative(psi2, psi6) / psi.cos();

        let beta = (psi.sin() / m_const()).clamp(-1.0, 1.0).asin();
        let lat = if self.e2 == 0.0 {
            beta
        } else {
            geodetic_from_authalic(beta, self.e2)
        };

        ensure_finite_lon_lat("Equal Earth", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    fn equal_earth_greenwich() -> EqualEarth {
        EqualEarth::new(ellipsoid::WGS84, 0.0, 0.0, 0.0).unwrap()
    }

    /// Expectations from C PROJ's gie suite (`+proj=eqearth +ellps=WGS84`,
    /// 1 cm tolerance).
    #[test]
    fn matches_c_proj_gie_vectors() {
        let proj = equal_earth_greenwich();
        let cases: [((f64, f64), (f64, f64)); 7] = [
            ((0.0, 0.0), (0.0, 0.0)),
            ((-180.0, 90.0), (-10_216_474.79, 8_392_927.6)),
            ((0.0, 90.0), (0.0, 8_392_927.6)),
            ((180.0, 90.0), (10_216_474.79, 8_392_927.6)),
            ((180.0, 45.0), (14_792_474.75, 5_466_867.76)),
            ((180.0, 0.0), (17_243_959.06, 0.0)),
            ((-70.0, -31.2), (-6_241_081.64, -3_907_019.16)),
        ];
        for ((lon, lat), (ex, ey)) in cases {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            assert!((x - ex).abs() < 1e-2, "({lon},{lat}): x = {x} vs {ex}");
            assert!((y - ey).abs() < 1e-2, "({lon},{lat}): y = {y} vs {ey}");
        }
    }

    #[test]
    fn inverse_matches_c_proj_gie_vectors() {
        let proj = equal_earth_greenwich();
        let cases = [
            ((-6_241_081.64, -3_907_019.16), (-70.0, -31.2)),
            ((17_243_959.06, 0.0), (180.0, 0.0)),
            ((14_792_474.75, 5_466_867.76), (180.0, 45.0)),
            ((0.0, 0.0), (0.0, 0.0)),
        ];
        for ((x, y), (elon, elat)) in cases {
            let (lon, lat) = proj.inverse(x, y).unwrap();
            let (lon, lat) = (lon.to_degrees(), lat.to_degrees());
            // The gie inputs are rounded to the centimetre, so allow the
            // corresponding angular slack and compare longitudes wrapped.
            let dlon = (lon - elon)
                .rem_euclid(360.0)
                .min((elon - lon).rem_euclid(360.0));
            assert!(dlon < 5e-7, "({x},{y}): lon = {lon}");
            assert!((lat - elat).abs() < 5e-7, "({x},{y}): lat = {lat}");
        }
    }

    #[test]
    fn roundtrip_world_grid() {
        let proj =
            EqualEarth::new(ellipsoid::WGS84, (-90.0_f64).to_radians(), 500.0, -200.0).unwrap();
        for lon in [-170.0_f64, -60.0, 0.0, 45.0, 179.0] {
            for lat in [-80.0_f64, -30.0, 0.0, 30.0, 80.0] {
                let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
                let (lon2, lat2) = proj.inverse(x, y).unwrap();
                // The inverse converts authalic→geodetic latitude with the
                // same truncated series C PROJ uses, which bounds roundtrip
                // accuracy near 1e-8°.
                assert!(
                    (lon2.to_degrees() - lon).abs() < 1e-7,
                    "lon {lon}: {}",
                    lon2.to_degrees()
                );
                assert!(
                    (lat2.to_degrees() - lat).abs() < 1e-7,
                    "lat {lat}: {}",
                    lat2.to_degrees()
                );
            }
        }
    }

    #[test]
    fn out_of_range_northing_clamps_to_pole() {
        let proj = equal_earth_greenwich();
        let (_, lat) = proj.inverse(0.0, 9_000_000.0).unwrap();
        // MAX_Y is C PROJ's rounded constant, so the clamped pole is exact
        // only to ~1e-5°.
        assert!((lat.to_degrees() - 90.0).abs() < 1e-4, "lat = {lat}");
    }
}

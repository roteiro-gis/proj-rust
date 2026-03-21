use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

use crate::ellipsoid::{self, Ellipsoid};
use crate::error::{Error, Result};

/// Web Mercator (EPSG:3857) projection.
///
/// Uses the spherical Mercator formulas with WGS84's semi-major axis,
/// matching the EPSG:3857 / "Pseudo-Mercator" definition.
pub(crate) struct WebMercator {
    ellipsoid: Ellipsoid,
}

impl WebMercator {
    pub(crate) fn new() -> Self {
        // EPSG:3857 uses sphere with radius = WGS84 semi-major axis.
        Self {
            ellipsoid: Ellipsoid::sphere(ellipsoid::WGS84.a),
        }
    }
}

/// Web Mercator latitude limit in radians (~85.06°).
const LAT_LIMIT: f64 = 1.4844222297453324; // 85.06_f64.to_radians() precomputed

impl super::ProjectionImpl for WebMercator {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        if lat.abs() > LAT_LIMIT {
            return Err(Error::OutOfRange(format!(
                "latitude {:.4}° exceeds Web Mercator limit of ±85.06°",
                lat.to_degrees()
            )));
        }

        let a = self.ellipsoid.a;
        let x = a * lon;
        let y = a * (FRAC_PI_4 + lat / 2.0).tan().ln();

        Ok((x, y))
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let lon = x / a;
        let lat = FRAC_PI_2 - 2.0 * (-y / a).exp().atan();

        Ok((lon, lat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::ProjectionImpl;

    const TOLERANCE: f64 = 0.01; // 1 cm

    #[test]
    fn origin() {
        let proj = WebMercator::new();
        let (x, y) = proj.forward(0.0, 0.0).unwrap();
        assert!(x.abs() < TOLERANCE);
        assert!(y.abs() < TOLERANCE);
    }

    #[test]
    fn new_york_roundtrip() {
        let proj = WebMercator::new();
        let lon = (-74.006_f64).to_radians();
        let lat = 40.7128_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();

        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((y - 4970072.0).abs() < 100.0);

        let (lon_back, lat_back) = proj.inverse(x, y).unwrap();
        assert!((lon_back - lon).abs() < 1e-10);
        assert!((lat_back - lat).abs() < 1e-10);
    }

    #[test]
    fn rejects_polar() {
        let proj = WebMercator::new();
        let result = proj.forward(0.0, 86.0_f64.to_radians());
        assert!(result.is_err());
    }
}

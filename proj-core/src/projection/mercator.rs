use crate::ellipsoid::Ellipsoid;
use crate::error::Result;

/// Standard Mercator projection (ellipsoidal, 1SP/2SP).
///
/// Distinct from Web Mercator (EPSG:3857), which uses spherical formulas.
/// This uses the full ellipsoidal equations. EPSG example: 3395 (WGS 84 / World Mercator).
pub(crate) struct Mercator {
    ellipsoid: Ellipsoid,
    lon0: f64,
    k0: f64,
    false_easting: f64,
    false_northing: f64,
}

impl Mercator {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat_ts: f64,
        k0_input: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Self {
        let e2 = ellipsoid.e2();
        // If lat_ts != 0, compute k0 from latitude of true scale (2SP variant)
        let k0 = if lat_ts.abs() > 1e-10 {
            let sin_ts = lat_ts.sin();
            lat_ts.cos() / (1.0 - e2 * sin_ts * sin_ts).sqrt()
        } else {
            k0_input
        };

        Self {
            ellipsoid,
            lon0,
            k0,
            false_easting,
            false_northing,
        }
    }
}

impl super::ProjectionImpl for Mercator {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let e = self.ellipsoid.e();

        let sin_lat = lat.sin();
        let e_sin = e * sin_lat;

        let x = self.false_easting + a * self.k0 * (lon - self.lon0);
        let y = self.false_northing
            + a * self.k0
                * ((std::f64::consts::FRAC_PI_4 + lat / 2.0).tan()
                    * ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0))
                .ln();

        Ok((x, y))
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let e = self.ellipsoid.e();

        let lon = self.lon0 + (x - self.false_easting) / (a * self.k0);
        let t = (-(y - self.false_northing) / (a * self.k0)).exp();

        // Iterative latitude from isometric latitude
        let mut lat = std::f64::consts::FRAC_PI_2 - 2.0 * t.atan();
        for _ in 0..15 {
            let e_sin = e * lat.sin();
            let new_lat = std::f64::consts::FRAC_PI_2
                - 2.0 * (t * ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)).atan();
            if (new_lat - lat).abs() < 1e-14 {
                lat = new_lat;
                break;
            }
            lat = new_lat;
        }

        Ok((lon, lat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn roundtrip_world_mercator() {
        // EPSG:3395 World Mercator (1SP, k0=1)
        let proj = Mercator::new(ellipsoid::WGS84, 0.0, 0.0, 1.0, 0.0, 0.0);

        let lon = (-74.006_f64).to_radians();
        let lat = 40.7128_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!(
            (lon2 - lon).abs() < 1e-8,
            "lon: {} vs {}",
            lon2.to_degrees(),
            lon.to_degrees()
        );
        assert!(
            (lat2 - lat).abs() < 1e-8,
            "lat: {} vs {}",
            lat2.to_degrees(),
            lat.to_degrees()
        );
    }

    #[test]
    fn origin_at_zero() {
        let proj = Mercator::new(ellipsoid::WGS84, 0.0, 0.0, 1.0, 0.0, 0.0);
        let (x, y) = proj.forward(0.0, 0.0).unwrap();
        assert!(x.abs() < 0.01, "x = {x}");
        assert!(y.abs() < 0.01, "y = {y}");
    }
}

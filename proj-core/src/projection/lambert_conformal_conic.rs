use crate::ellipsoid::Ellipsoid;
use crate::error::Result;

/// Lambert Conformal Conic projection (1SP and 2SP).
///
/// Used by many national and regional grids including US State Plane zones,
/// European grids (France Lambert, etc.).
pub(crate) struct LambertConformalConic {
    ellipsoid: Ellipsoid,
    lon0: f64,
    n: f64,
    f_const: f64,
    rho0: f64,
    false_easting: f64,
    false_northing: f64,
}

impl LambertConformalConic {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        lat1: f64,
        lat2: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Self {
        let e = ellipsoid.e();
        let m1 = m_func(lat1, e);
        let m2 = m_func(lat2, e);
        let t0 = t_func(lat0, e);
        let t1 = t_func(lat1, e);
        let t2 = t_func(lat2, e);

        let n = if (lat1 - lat2).abs() < 1e-10 {
            lat1.sin()
        } else {
            (m1.ln() - m2.ln()) / (t1.ln() - t2.ln())
        };

        let f_const = m1 / (n * t1.powf(n));
        let rho0 = ellipsoid.a * f_const * t0.powf(n);

        Self {
            ellipsoid,
            lon0,
            n,
            f_const,
            rho0,
            false_easting,
            false_northing,
        }
    }
}

fn m_func(lat: f64, e: f64) -> f64 {
    let sin_lat = lat.sin();
    lat.cos() / (1.0 - e * e * sin_lat * sin_lat).sqrt()
}

fn t_func(lat: f64, e: f64) -> f64 {
    let sin_lat = lat.sin();
    let e_sin = e * sin_lat;
    (std::f64::consts::FRAC_PI_4 - lat / 2.0).tan() / ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)
}

fn lat_from_t_lcc(t: f64, e: f64) -> f64 {
    let mut lat = std::f64::consts::FRAC_PI_2 - 2.0 * t.atan();
    for _ in 0..15 {
        let e_sin = e * lat.sin();
        let new_lat = std::f64::consts::FRAC_PI_2
            - 2.0 * (t * ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)).atan();
        if (new_lat - lat).abs() < 1e-14 {
            return new_lat;
        }
        lat = new_lat;
    }
    lat
}

impl super::ProjectionImpl for LambertConformalConic {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let e = self.ellipsoid.e();
        let t = t_func(lat, e);
        let rho = a * self.f_const * t.powf(self.n);
        let theta = self.n * (lon - self.lon0);

        let x = self.false_easting + rho * theta.sin();
        let y = self.false_northing + self.rho0 - rho * theta.cos();

        Ok((x, y))
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let e = self.ellipsoid.e();

        let dx = x - self.false_easting;
        let dy = self.rho0 - (y - self.false_northing);

        let rho = (dx * dx + dy * dy).sqrt() * self.n.signum();
        let theta = dx.atan2(dy);

        let t = (rho / (a * self.f_const)).powf(1.0 / self.n);
        let lat = lat_from_t_lcc(t, e);
        let lon = self.lon0 + theta / self.n;

        Ok((lon, lat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn roundtrip_2sp() {
        // Approximate France Lambert (EPSG:2154 parameters)
        let proj = LambertConformalConic::new(
            ellipsoid::GRS80,
            3.0_f64.to_radians(),
            46.5_f64.to_radians(),
            44.0_f64.to_radians(),
            49.0_f64.to_radians(),
            700_000.0,
            6_600_000.0,
        );

        let lon = 2.3522_f64.to_radians(); // Paris
        let lat = 48.8566_f64.to_radians();
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
    fn roundtrip_1sp() {
        // 1SP variant (lat1 == lat2)
        let proj = LambertConformalConic::new(
            ellipsoid::WGS84,
            (-96.0_f64).to_radians(),
            33.0_f64.to_radians(),
            33.0_f64.to_radians(),
            33.0_f64.to_radians(),
            500_000.0,
            0.0,
        );

        let lon = (-96.0_f64).to_radians();
        let lat = 33.0_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }
}

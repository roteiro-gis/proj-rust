use crate::ellipsoid::Ellipsoid;
use crate::error::Result;

/// Albers Equal Area Conic projection.
///
/// Used for statistical mapping, USGS, and large-area equal-area maps.
/// EPSG examples: 5070 (CONUS Albers), 3005 (BC Albers).
pub(crate) struct AlbersEqualArea {
    ellipsoid: Ellipsoid,
    lon0: f64,
    n: f64,
    c: f64,
    rho0: f64,
    false_easting: f64,
    false_northing: f64,
}

impl AlbersEqualArea {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        lat1: f64,
        lat2: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Self {
        let e2 = ellipsoid.e2();
        let m1 = m_func(lat1, e2);
        let m2 = m_func(lat2, e2);
        let q0 = q_func(lat0, e2);
        let q1 = q_func(lat1, e2);
        let q2 = q_func(lat2, e2);

        let n = if (lat1 - lat2).abs() < 1e-10 {
            lat1.sin()
        } else {
            (m1 * m1 - m2 * m2) / (q2 - q1)
        };

        let c = m1 * m1 + n * q1;
        let rho0 = ellipsoid.a * (c - n * q0).abs().sqrt() / n;

        Self {
            ellipsoid,
            lon0,
            n,
            c,
            rho0,
            false_easting,
            false_northing,
        }
    }
}

fn m_func(lat: f64, e2: f64) -> f64 {
    let sin_lat = lat.sin();
    lat.cos() / (1.0 - e2 * sin_lat * sin_lat).sqrt()
}

fn q_func(lat: f64, e2: f64) -> f64 {
    let e = e2.sqrt();
    let sin_lat = lat.sin();
    let e_sin = e * sin_lat;
    (1.0 - e2)
        * (sin_lat / (1.0 - e2 * sin_lat * sin_lat)
            - (1.0 / (2.0 * e)) * ((1.0 - e_sin) / (1.0 + e_sin)).ln())
}

fn lat_from_q(q: f64, e2: f64) -> f64 {
    let e = e2.sqrt();
    let mut lat = (q / 2.0).asin();
    for _ in 0..15 {
        let sin_lat = lat.sin();
        let e_sin = e * sin_lat;
        let one_minus_e2sin2 = 1.0 - e2 * sin_lat * sin_lat;
        let new_lat = lat
            + one_minus_e2sin2 * one_minus_e2sin2 / (2.0 * lat.cos())
                * (q / (1.0 - e2) - sin_lat / one_minus_e2sin2
                    + (1.0 / (2.0 * e)) * ((1.0 - e_sin) / (1.0 + e_sin)).ln());
        if (new_lat - lat).abs() < 1e-14 {
            return new_lat;
        }
        lat = new_lat;
    }
    lat
}

impl super::ProjectionImpl for AlbersEqualArea {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let e2 = self.ellipsoid.e2();
        let q = q_func(lat, e2);
        let rho = a * (self.c - self.n * q).abs().sqrt() / self.n;
        let theta = self.n * (lon - self.lon0);

        let x = self.false_easting + rho * theta.sin();
        let y = self.false_northing + self.rho0 - rho * theta.cos();

        Ok((x, y))
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let a = self.ellipsoid.a;
        let e2 = self.ellipsoid.e2();

        let dx = x - self.false_easting;
        let dy = self.rho0 - (y - self.false_northing);
        let rho = (dx * dx + dy * dy).sqrt();
        let theta = dx.atan2(dy);

        let q = (self.c - rho * rho * self.n * self.n / (a * a)) / self.n;
        let lat = lat_from_q(q, e2);
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
    fn roundtrip_conus() {
        // EPSG:5070 Conus Albers parameters
        let proj = AlbersEqualArea::new(
            ellipsoid::GRS80,
            (-96.0_f64).to_radians(),
            23.0_f64.to_radians(),
            29.5_f64.to_radians(),
            45.5_f64.to_radians(),
            0.0,
            0.0,
        );

        let lon = (-96.0_f64).to_radians();
        let lat = 37.0_f64.to_radians();
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
}

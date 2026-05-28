use crate::ellipsoid::Ellipsoid;
use crate::error::Result;
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected,
};

/// Cassini-Soldner projection.
///
/// Implements EPSG method 9806 for older cadastral and national grid systems.
pub(crate) struct CassiniSoldner {
    a: f64,
    e2: f64,
    ep2: f64,
    a_one_minus_e2: f64,
    lon0: f64,
    false_easting: f64,
    false_northing: f64,
    m0: f64,
    meridional_coeff0: f64,
    meridional_coeff2: f64,
    meridional_coeff4: f64,
    meridional_coeff6: f64,
    mu_denom: f64,
    e1_coeff2: f64,
    e1_coeff4: f64,
    e1_coeff6: f64,
    e1_coeff8: f64,
}

impl CassiniSoldner {
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

        let a = ellipsoid.semi_major_axis();
        let e2 = ellipsoid.e2();
        let ep2 = ellipsoid.ep2();
        let e2_2 = e2 * e2;
        let e2_3 = e2_2 * e2;

        let meridional_coeff0 = a * (1.0 - e2 / 4.0 - 3.0 * e2_2 / 64.0 - 5.0 * e2_3 / 256.0);
        let meridional_coeff2 = a * (3.0 * e2 / 8.0 + 3.0 * e2_2 / 32.0 + 45.0 * e2_3 / 1024.0);
        let meridional_coeff4 = a * (15.0 * e2_2 / 256.0 + 45.0 * e2_3 / 1024.0);
        let meridional_coeff6 = a * (35.0 * e2_3 / 3072.0);
        let m0 = meridional_arc(
            lat0,
            meridional_coeff0,
            meridional_coeff2,
            meridional_coeff4,
            meridional_coeff6,
        );

        let sqrt_one_minus_e2 = (1.0 - e2).sqrt();
        let e1 = (1.0 - sqrt_one_minus_e2) / (1.0 + sqrt_one_minus_e2);
        let e1_2 = e1 * e1;
        let e1_3 = e1_2 * e1;
        let e1_4 = e1_2 * e1_2;

        Ok(Self {
            a,
            e2,
            ep2,
            a_one_minus_e2: a * (1.0 - e2),
            lon0,
            false_easting,
            false_northing,
            m0,
            meridional_coeff0,
            meridional_coeff2,
            meridional_coeff4,
            meridional_coeff6,
            mu_denom: meridional_coeff0,
            e1_coeff2: 3.0 * e1 / 2.0 - 27.0 * e1_3 / 32.0,
            e1_coeff4: 21.0 * e1_2 / 16.0 - 55.0 * e1_4 / 32.0,
            e1_coeff6: 151.0 * e1_3 / 96.0,
            e1_coeff8: 1097.0 * e1_4 / 512.0,
        })
    }

    fn meridional_arc(&self, phi: f64) -> f64 {
        meridional_arc(
            phi,
            self.meridional_coeff0,
            self.meridional_coeff2,
            self.meridional_coeff4,
            self.meridional_coeff6,
        )
    }
}

fn meridional_arc(phi: f64, coeff0: f64, coeff2: f64, coeff4: f64, coeff6: f64) -> f64 {
    coeff0 * phi - coeff2 * (2.0 * phi).sin() + coeff4 * (4.0 * phi).sin()
        - coeff6 * (6.0 * phi).sin()
}

impl super::ProjectionImpl for CassiniSoldner {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let d_lon = normalize_longitude(lon - self.lon0);

        let sin_phi = lat.sin();
        let cos_phi = lat.cos();
        let tan_phi = lat.tan();
        let n_val = self.a / (1.0 - self.e2 * sin_phi * sin_phi).sqrt();
        let t = tan_phi * tan_phi;
        let c = self.ep2 * cos_phi * cos_phi;
        let a_coeff = d_lon * cos_phi;
        let a2 = a_coeff * a_coeff;
        let a4 = a2 * a2;

        let x = self.false_easting
            + n_val
                * (a_coeff
                    - t * a2 * a_coeff / 6.0
                    - (8.0 - t + 8.0 * c) * t * a4 * a_coeff / 120.0);
        let y = self.false_northing + self.meridional_arc(lat) - self.m0
            + n_val * tan_phi * (a2 / 2.0 + (5.0 - t + 6.0 * c) * a4 / 24.0);

        ensure_finite_xy("Cassini-Soldner", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let m1 = self.m0 + y - self.false_northing;
        let mu1 = m1 / self.mu_denom;
        let lat1 = mu1
            + self.e1_coeff2 * (2.0 * mu1).sin()
            + self.e1_coeff4 * (4.0 * mu1).sin()
            + self.e1_coeff6 * (6.0 * mu1).sin()
            + self.e1_coeff8 * (8.0 * mu1).sin();

        let sin_lat1 = lat1.sin();
        let cos_lat1 = lat1.cos();
        let tan_lat1 = lat1.tan();
        let n1 = self.a / (1.0 - self.e2 * sin_lat1 * sin_lat1).sqrt();
        let r1 = self.a_one_minus_e2 / (1.0 - self.e2 * sin_lat1 * sin_lat1).powf(1.5);
        let t1 = tan_lat1 * tan_lat1;
        let d = (x - self.false_easting) / n1;
        let d2 = d * d;
        let d4 = d2 * d2;

        let lat = lat1 - (n1 * tan_lat1 / r1) * (d2 / 2.0 - (1.0 + 3.0 * t1) * d4 / 24.0);
        let lon =
            self.lon0 + (d - t1 * d2 * d / 3.0 + (1.0 + 3.0 * t1) * t1 * d4 * d / 15.0) / cos_lat1;

        ensure_finite_lon_lat("Cassini-Soldner", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn roundtrip_near_origin() {
        let proj = CassiniSoldner::new(
            ellipsoid::WGS84,
            (-61.333_333_333_f64).to_radians(),
            10.441_666_667_f64.to_radians(),
            430_000.0,
            325_000.0,
        )
        .unwrap();

        let lon = (-62.0_f64).to_radians();
        let lat = 10.0_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }
}

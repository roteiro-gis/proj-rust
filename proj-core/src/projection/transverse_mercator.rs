use crate::ellipsoid::Ellipsoid;
use crate::error::Result;
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, validate_angle, validate_latitude_param,
    validate_lon_lat, validate_offset, validate_projected, validate_scale,
};

/// Transverse Mercator projection.
///
/// The foundation for UTM zones and many national grid systems.
/// Uses series expansion for high accuracy.
pub(crate) struct TransverseMercator {
    ellipsoid: Ellipsoid,
    /// Central meridian in radians.
    lon0: f64,
    /// Latitude of origin in radians.
    lat0: f64,
    /// Scale factor on central meridian.
    k0: f64,
    /// False easting in meters.
    false_easting: f64,
    /// False northing in meters.
    false_northing: f64,
}

impl TransverseMercator {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("central meridian", lon0)?;
        validate_latitude_param("latitude of origin", lat0)?;
        validate_scale("scale factor", k0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        Ok(Self {
            ellipsoid,
            lon0,
            lat0,
            k0,
            false_easting,
            false_northing,
        })
    }

    /// Compute meridional arc length from equator to latitude phi.
    fn meridional_arc(&self, phi: f64) -> f64 {
        let e2 = self.ellipsoid.e2();
        let a = self.ellipsoid.a;
        let e2_2 = e2 * e2;
        let e2_3 = e2_2 * e2;

        a * ((1.0 - e2 / 4.0 - 3.0 * e2_2 / 64.0 - 5.0 * e2_3 / 256.0) * phi
            - (3.0 * e2 / 8.0 + 3.0 * e2_2 / 32.0 + 45.0 * e2_3 / 1024.0) * (2.0 * phi).sin()
            + (15.0 * e2_2 / 256.0 + 45.0 * e2_3 / 1024.0) * (4.0 * phi).sin()
            - (35.0 * e2_3 / 3072.0) * (6.0 * phi).sin())
    }
}

impl super::ProjectionImpl for TransverseMercator {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let e2 = self.ellipsoid.e2();
        let a = self.ellipsoid.a;
        let k0 = self.k0;

        let phi = lat;
        let d_lon = lon - self.lon0;

        let sin_phi = phi.sin();
        let cos_phi = phi.cos();
        let tan_phi = phi.tan();

        let n_val = a / (1.0 - e2 * sin_phi * sin_phi).sqrt();
        let t = tan_phi * tan_phi;
        let c = self.ellipsoid.ep2() * cos_phi * cos_phi;
        let a_coeff = d_lon * cos_phi;

        let m = self.meridional_arc(phi);
        let m0 = if self.lat0.abs() < 1e-12 {
            0.0
        } else {
            self.meridional_arc(self.lat0)
        };

        let a2 = a_coeff * a_coeff;

        let x = self.false_easting
            + k0 * n_val
                * (a_coeff
                    + (1.0 - t + c) * a2 * a_coeff / 6.0
                    + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * self.ellipsoid.ep2())
                        * a2
                        * a2
                        * a_coeff
                        / 120.0);

        let y = self.false_northing
            + k0 * (m - m0
                + n_val
                    * tan_phi
                    * (a2 / 2.0
                        + (5.0 - t + 9.0 * c + 4.0 * c * c) * a2 * a2 / 24.0
                        + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * self.ellipsoid.ep2())
                            * a2
                            * a2
                            * a2
                            / 720.0));

        ensure_finite_xy("Transverse Mercator", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let e2 = self.ellipsoid.e2();
        let a = self.ellipsoid.a;
        let k0 = self.k0;
        let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());

        let e2_2 = e2 * e2;
        let e2_3 = e2_2 * e2;

        let m0 = if self.lat0.abs() < 1e-12 {
            0.0
        } else {
            self.meridional_arc(self.lat0)
        };

        let m = m0 + (y - self.false_northing) / k0;
        let mu = m / (a * (1.0 - e2 / 4.0 - 3.0 * e2_2 / 64.0 - 5.0 * e2_3 / 256.0));

        let e1_2 = e1 * e1;
        let e1_3 = e1_2 * e1;

        let phi1 = mu
            + (3.0 * e1 / 2.0 - 27.0 * e1_3 / 32.0) * (2.0 * mu).sin()
            + (21.0 * e1_2 / 16.0 - 55.0 * e1_2 * e1_2 / 32.0) * (4.0 * mu).sin()
            + (151.0 * e1_3 / 96.0) * (6.0 * mu).sin();

        let sin_phi1 = phi1.sin();
        let cos_phi1 = phi1.cos();
        let tan_phi1 = phi1.tan();
        let n1 = a / (1.0 - e2 * sin_phi1 * sin_phi1).sqrt();
        let t1 = tan_phi1 * tan_phi1;
        let c1 = self.ellipsoid.ep2() * cos_phi1 * cos_phi1;
        let r1 = a * (1.0 - e2) / (1.0 - e2 * sin_phi1 * sin_phi1).powf(1.5);
        let d = (x - self.false_easting) / (n1 * k0);
        let d2 = d * d;

        let lat = phi1
            - (n1 * tan_phi1 / r1)
                * (d2 / 2.0
                    - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * self.ellipsoid.ep2())
                        * d2
                        * d2
                        / 24.0
                    + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1
                        - 252.0 * self.ellipsoid.ep2()
                        - 3.0 * c1 * c1)
                        * d2
                        * d2
                        * d2
                        / 720.0);

        let lon = self.lon0
            + (d - (1.0 + 2.0 * t1 + c1) * d2 * d / 6.0
                + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1
                    + 8.0 * self.ellipsoid.ep2()
                    + 24.0 * t1 * t1)
                    * d2
                    * d2
                    * d
                    / 120.0)
                / cos_phi1;

        ensure_finite_lon_lat("Transverse Mercator", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    const TOLERANCE: f64 = 0.01; // 1 cm

    fn utm(zone: u8, north: bool) -> TransverseMercator {
        let lon0 = ((zone as f64 - 1.0) * 6.0 - 180.0 + 3.0).to_radians();
        TransverseMercator::new(
            ellipsoid::WGS84,
            lon0,
            0.0,
            0.9996,
            500_000.0,
            if north { 0.0 } else { 10_000_000.0 },
        )
        .unwrap()
    }

    #[test]
    fn utm_zone_18n_new_york() {
        let proj = utm(18, true);
        let lon = (-74.006_f64).to_radians();
        let lat = 40.7128_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();

        assert!((x - 583960.0).abs() < 1.0);
        assert!(y > 4_500_000.0 && y < 4_510_000.0);

        let (lon_back, lat_back) = proj.inverse(x, y).unwrap();
        assert!((lon_back - lon).abs() < 1e-8);
        assert!((lat_back - lat).abs() < 1e-8);
    }

    #[test]
    fn utm_equator_central_meridian() {
        let proj = utm(31, true);
        let lon = 3.0_f64.to_radians();
        let lat = 0.0;
        let (x, y) = proj.forward(lon, lat).unwrap();
        assert!((x - 500000.0).abs() < TOLERANCE);
        assert!(y.abs() < TOLERANCE);
    }
}

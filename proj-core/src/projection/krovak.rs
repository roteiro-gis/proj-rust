use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    converge, ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected, validate_scale,
};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

/// Krovak oblique conformal conic, north-orientated (EPSG methods 1041 and
/// 1043).
///
/// The projection maps through a conformal sphere onto a cone whose axis
/// passes obliquely through the projection centre. Native Krovak coordinates
/// are southing/westing; the north-orientated variants negate both axes, so
/// Czech and Slovak coordinates are negative. Formulas from IOGP Publication
/// 373-7-2 (EPSG Guidance Note 7 part 2), matching C PROJ's `krovak` /
/// `mod_krovak` in east-north mode.
///
/// The modified variant (EPSG method 1043) applies the S-JTSK/05 polynomial
/// distortion correction whose coefficients are defining constants of the
/// EPSG method.
#[derive(Clone)]
pub(crate) struct Krovak {
    a: f64,
    e: f64,
    lon0: f64,
    false_easting: f64,
    false_northing: f64,
    alpha: f64,
    k_const: f64,
    /// Cone constant: sine of the pseudo standard parallel.
    n: f64,
    /// Adimensional radius of the pseudo standard parallel circle.
    rho0: f64,
    /// Co-latitude of the cone axis (radians).
    ad: f64,
    /// tan(s0/2 + π/4) for the pseudo standard parallel s0.
    tan_half_s0: f64,
    modified: bool,
}

/// S-JTSK/05 distortion-correction constants (EPSG methods 1042/1043).
mod modified_correction {
    pub(super) const X0: f64 = 1_089_000.0;
    pub(super) const Y0: f64 = 654_000.0;
    pub(super) const C1: f64 = 2.946529277e-02;
    pub(super) const C2: f64 = 2.515965696e-02;
    pub(super) const C3: f64 = 1.193845912e-07;
    pub(super) const C4: f64 = -4.668270147e-07;
    pub(super) const C5: f64 = 9.233980362e-12;
    pub(super) const C6: f64 = 1.523735715e-12;
    pub(super) const C7: f64 = 1.696780024e-18;
    pub(super) const C8: f64 = 4.408314235e-18;
    pub(super) const C9: f64 = -8.331083518e-24;
    pub(super) const C10: f64 = -3.689471323e-24;

    /// Correction in meters for reduced southing/westing `(xr, yr)`.
    pub(super) fn dx_dy(xr: f64, yr: f64) -> (f64, f64) {
        let xr2 = xr * xr;
        let yr2 = yr * yr;
        let xr4 = xr2 * xr2;
        let yr4 = yr2 * yr2;

        let dx = C1 + C3 * xr - C4 * yr - 2.0 * C6 * xr * yr
            + C5 * (xr2 - yr2)
            + C7 * xr * (xr2 - 3.0 * yr2)
            - C8 * yr * (3.0 * xr2 - yr2)
            + 4.0 * C9 * xr * yr * (xr2 - yr2)
            + C10 * (xr4 + yr4 - 6.0 * xr2 * yr2);
        let dy = C2
            + C3 * yr
            + C4 * xr
            + 2.0 * C5 * xr * yr
            + C6 * (xr2 - yr2)
            + C8 * xr * (xr2 - 3.0 * yr2)
            + C7 * yr * (3.0 * xr2 - yr2)
            - 4.0 * C10 * xr * yr * (xr2 - yr2)
            + C9 * (xr4 + yr4 - 6.0 * xr2 * yr2);
        (dx, dy)
    }
}

impl Krovak {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        co_latitude_cone_axis: f64,
        lat_pseudo_standard_parallel: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
        modified: bool,
    ) -> Result<Self> {
        validate_angle("longitude of origin", lon0)?;
        validate_latitude_param("latitude of projection centre", lat0)?;
        validate_latitude_param(
            "latitude of pseudo standard parallel",
            lat_pseudo_standard_parallel,
        )?;
        validate_scale("scale factor on pseudo standard parallel", k0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;
        if !co_latitude_cone_axis.is_finite()
            || co_latitude_cone_axis <= 0.0
            || co_latitude_cone_axis >= FRAC_PI_2
        {
            return Err(Error::InvalidDefinition(
                "co-latitude of cone axis must be in (0°, 90°)".into(),
            ));
        }
        if lat0 <= -FRAC_PI_2 + 1e-10 || lat0 >= FRAC_PI_2 - 1e-10 {
            return Err(Error::InvalidDefinition(
                "Krovak latitude of projection centre must be strictly between the poles".into(),
            ));
        }
        let s0 = lat_pseudo_standard_parallel;
        if s0.abs() >= FRAC_PI_2 - 1e-10 || s0.sin().abs() < 1e-10 {
            return Err(Error::InvalidDefinition(
                "Krovak pseudo standard parallel must be strictly between a pole and the equator"
                    .into(),
            ));
        }

        let a = ellipsoid.semi_major_axis();
        let e2 = ellipsoid.e2();
        let e = e2.sqrt();
        let sin_lat0 = lat0.sin();
        let cos_lat0 = lat0.cos();

        let alpha = (1.0 + (e2 * cos_lat0.powi(4)) / (1.0 - e2)).sqrt();
        let u0 = (sin_lat0 / alpha).asin();
        let g = ((1.0 + e * sin_lat0) / (1.0 - e * sin_lat0)).powf(alpha * e / 2.0);
        let tan_half_lat0 = (lat0 / 2.0 + FRAC_PI_4).tan();
        let k_const = (u0 / 2.0 + FRAC_PI_4).tan() / tan_half_lat0.powf(alpha) * g;
        let n0 = (1.0 - e2).sqrt() / (1.0 - e2 * sin_lat0 * sin_lat0);
        let n = s0.sin();
        let rho0 = k0 * n0 / s0.tan();

        Ok(Self {
            a,
            e,
            lon0,
            false_easting,
            false_northing,
            alpha,
            k_const,
            n,
            rho0,
            ad: co_latitude_cone_axis,
            tan_half_s0: (s0 / 2.0 + FRAC_PI_4).tan(),
            modified,
        })
    }
}

impl super::ProjectionImpl for Krovak {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);

        let sin_lat = lat.sin();
        let gfi =
            ((1.0 + self.e * sin_lat) / (1.0 - self.e * sin_lat)).powf(self.alpha * self.e / 2.0);
        let u = 2.0
            * ((self.k_const * (lat / 2.0 + FRAC_PI_4).tan().powf(self.alpha) / gfi).atan()
                - FRAC_PI_4);
        let deltav = -lam * self.alpha;

        let s = (self.ad.cos() * u.sin() + self.ad.sin() * u.cos() * deltav.cos()).asin();
        let cos_s = s.cos();
        if cos_s < 1e-12 {
            return Err(Error::OutOfRange(
                "Krovak forward: point maps to the cone axis pole".into(),
            ));
        }
        let d = (u.cos() * deltav.sin() / cos_s).asin();

        let eps = self.n * d;
        let rho =
            self.rho0 * self.tan_half_s0.powf(self.n) / (s / 2.0 + FRAC_PI_4).tan().powf(self.n);

        // Native Krovak axes: southing along the central meridian, westing
        // perpendicular to it (both adimensional here).
        let mut southing = rho * eps.cos();
        let mut westing = rho * eps.sin();

        if self.modified {
            use modified_correction::{dx_dy, X0, Y0};
            let (dx, dy) = dx_dy(southing * self.a - X0, westing * self.a - Y0);
            southing -= dx / self.a;
            westing -= dy / self.a;
        }

        // North orientation negates both axes after the false offsets apply
        // to the southing/westing values.
        let x = -(self.a * westing + self.false_easting);
        let y = -(self.a * southing + self.false_northing);
        ensure_finite_xy("Krovak", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let mut westing = (-x - self.false_easting) / self.a;
        let mut southing = (-y - self.false_northing) / self.a;

        if self.modified {
            use modified_correction::{dx_dy, X0, Y0};
            let (dx, dy) = dx_dy(southing * self.a - X0, westing * self.a - Y0);
            southing += dx / self.a;
            westing += dy / self.a;
        }

        let rho = southing.hypot(westing);
        let eps = westing.atan2(southing);
        let d = eps / self.n;
        let s = if rho == 0.0 {
            FRAC_PI_2
        } else {
            2.0 * (((self.rho0 / rho).powf(1.0 / self.n) * self.tan_half_s0).atan() - FRAC_PI_4)
        };

        let u = (self.ad.cos() * s.sin() - self.ad.sin() * s.cos() * d.cos()).asin();
        let deltav = (s.cos() * d.sin() / u.cos()).asin();
        let lon = self.lon0 - deltav / self.alpha;

        let k_pow = self.k_const.powf(-1.0 / self.alpha);
        let tan_u_pow = (u / 2.0 + FRAC_PI_4).tan().powf(1.0 / self.alpha);
        let lat = converge("Krovak inverse latitude", u, 20, 1e-14, |lat| {
            let es = self.e * lat.sin();
            2.0 * ((k_pow * tan_u_pow * ((1.0 + es) / (1.0 - es)).powf(self.e / 2.0)).atan()
                - FRAC_PI_4)
        })?;

        ensure_finite_lon_lat("Krovak", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    fn dms(d: f64, m: f64, s: f64) -> f64 {
        d.signum() * (d.abs() + m / 60.0 + s / 3600.0)
    }

    /// S-JTSK / Krovak East North (EPSG:5514) parameters, with the longitude
    /// of origin referenced to Greenwich.
    fn krovak_east_north() -> Krovak {
        Krovak::new(
            ellipsoid::BESSEL1841,
            dms(24.0, 50.0, 0.0).to_radians(),
            dms(49.0, 30.0, 0.0).to_radians(),
            dms(30.0, 17.0, 17.30311).to_radians(),
            dms(78.0, 30.0, 0.0).to_radians(),
            0.9999,
            0.0,
            0.0,
            false,
        )
        .unwrap()
    }

    /// S-JTSK/05 / Modified Krovak East North (EPSG:5516) parameters.
    fn modified_krovak_east_north() -> Krovak {
        Krovak::new(
            ellipsoid::BESSEL1841,
            dms(24.0, 50.0, 0.0).to_radians(),
            dms(49.0, 30.0, 0.0).to_radians(),
            dms(30.0, 17.0, 17.30311).to_radians(),
            dms(78.0, 30.0, 0.0).to_radians(),
            0.9999,
            5_000_000.0,
            5_000_000.0,
            true,
        )
        .unwrap()
    }

    /// EPSG Guidance Note 7-2 Krovak worked example (as pinned by C PROJ's
    /// own gie suite at 1.1 cm): 50°12'32.442"N, 16°50'59.179"E of Greenwich
    /// maps to southing 1050538.64, westing 568991.00 — east-north negates
    /// both.
    #[test]
    fn gn7_2_worked_example() {
        let proj = krovak_east_north();
        let lon = dms(16.0, 50.0, 59.179).to_radians();
        let lat = dms(50.0, 12.0, 32.442).to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        assert!((x - (-568_991.00)).abs() < 1.1e-2, "x = {x}");
        assert!((y - (-1_050_538.64)).abs() < 1.1e-2, "y = {y}");

        let (lon2, lat2) = proj.inverse(x, y).unwrap();
        assert!((lon2 - lon).abs() < 1e-10, "lon: {lon2} vs {lon}");
        assert!((lat2 - lat).abs() < 1e-10, "lat: {lat2} vs {lat}");
    }

    /// EPSG Guidance Note 7-2 Modified Krovak worked example (same point):
    /// southing 6050538.71, westing 5568990.91 including the 5,000,000
    /// false offsets.
    #[test]
    fn gn7_2_modified_worked_example() {
        let proj = modified_krovak_east_north();
        let lon = dms(16.0, 50.0, 59.179).to_radians();
        let lat = dms(50.0, 12.0, 32.442).to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        assert!((x - (-5_568_990.91)).abs() < 2e-2, "x = {x}");
        assert!((y - (-6_050_538.71)).abs() < 2e-2, "y = {y}");

        let (lon2, lat2) = proj.inverse(x, y).unwrap();
        assert!((lon2 - lon).abs() < 1e-10, "lon: {lon2} vs {lon}");
        assert!((lat2 - lat).abs() < 1e-10, "lat: {lat2} vs {lat}");
    }

    #[test]
    fn roundtrip_across_czech_and_slovak_territory() {
        let proj = krovak_east_north();
        for &(lon_deg, lat_deg) in &[
            (14.42_f64, 50.088_f64),
            (17.107, 48.149),
            (21.25, 48.72),
            (12.55, 50.4),
        ] {
            let lon = lon_deg.to_radians();
            let lat = lat_deg.to_radians();
            let (x, y) = proj.forward(lon, lat).unwrap();
            assert!(x < 0.0 && y < 0.0, "north-orientated axes are negative");
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            assert!((lon2 - lon).abs() < 1e-10);
            assert!((lat2 - lat).abs() < 1e-10);
        }
    }

    #[test]
    fn forward_wraps_longitude_delta() {
        let proj = krovak_east_north();
        let lat = 49.5_f64.to_radians();
        let wrapped = proj.forward((24.83 - 360.0_f64).to_radians(), lat).unwrap();
        let canonical = proj.forward(24.83_f64.to_radians(), lat).unwrap();
        assert!((wrapped.0 - canonical.0).abs() < 1e-6);
        assert!((wrapped.1 - canonical.1).abs() < 1e-6);
    }

    #[test]
    fn rejects_invalid_cone_axis_co_latitude() {
        for colat in [0.0_f64, -10.0, 90.0, f64::NAN] {
            let result = Krovak::new(
                ellipsoid::BESSEL1841,
                24.83_f64.to_radians(),
                49.5_f64.to_radians(),
                colat.to_radians(),
                78.5_f64.to_radians(),
                0.9999,
                0.0,
                0.0,
                false,
            );
            assert!(result.is_err(), "co-latitude {colat}° must be rejected");
        }
    }

    #[test]
    fn rejects_equatorial_pseudo_standard_parallel() {
        let result = Krovak::new(
            ellipsoid::BESSEL1841,
            24.83_f64.to_radians(),
            49.5_f64.to_radians(),
            30.288_f64.to_radians(),
            0.0,
            0.9999,
            0.0,
            0.0,
            false,
        );
        assert!(result.is_err(), "zero cone constant must be rejected");
    }
}

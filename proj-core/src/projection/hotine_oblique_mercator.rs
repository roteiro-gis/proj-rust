use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected, validate_scale,
};

const POLE_EPSILON: f64 = 1e-12;
const ECCENTRICITY_EPSILON: f64 = 1e-15;
const RIGHT_ANGLE_EPSILON: f64 = 1e-12;
const SWISS_INVERSE_TOLERANCE: f64 = 1e-13;
const SWISS_INVERSE_ITERATIONS: usize = 10;

/// Hotine Oblique Mercator / Rectified Skew Orthomorphic projection.
///
/// Implements EPSG methods 9812 (variant A) and 9815 (variant B). Variant B
/// uses easting/northing at the projection centre. The Swiss right-angle form
/// is evaluated with its dedicated conformal-sphere equations to avoid the
/// numerically singular Hotine centre-line offset at 90 degrees.
#[derive(Clone)]
pub(crate) struct HotineObliqueMercator {
    kernel: HotineKernel,
}

#[derive(Clone)]
enum HotineKernel {
    General(GeneralHotineObliqueMercator),
    Swiss(SwissObliqueMercator),
}

#[derive(Clone)]
struct GeneralHotineObliqueMercator {
    e: f64,
    a_const: f64,
    b: f64,
    h: f64,
    gamma0: f64,
    gamma_c: f64,
    lon0: f64,
    u_c: f64,
    false_easting: f64,
    false_northing: f64,
}

impl HotineObliqueMercator {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        latc: f64,
        lonc: f64,
        azimuth: f64,
        rectified_grid_angle: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
        variant_b: bool,
    ) -> Result<Self> {
        validate_latitude_param("latitude of projection centre", latc)?;
        validate_angle("longitude of projection centre", lonc)?;
        validate_angle("azimuth of central line", azimuth)?;
        validate_angle("rectified grid angle", rectified_grid_angle)?;
        validate_scale("scale factor", k0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        if variant_b && is_swiss_right_angle_form(azimuth, rectified_grid_angle) {
            return Ok(Self {
                kernel: HotineKernel::Swiss(SwissObliqueMercator::new(
                    ellipsoid,
                    latc,
                    lonc,
                    k0,
                    false_easting,
                    false_northing,
                )?),
            });
        }

        Ok(Self {
            kernel: HotineKernel::General(GeneralHotineObliqueMercator::new(
                ellipsoid,
                latc,
                lonc,
                azimuth,
                rectified_grid_angle,
                k0,
                false_easting,
                false_northing,
                variant_b,
            )?),
        })
    }
}

impl GeneralHotineObliqueMercator {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ellipsoid: Ellipsoid,
        latc: f64,
        lonc: f64,
        azimuth: f64,
        rectified_grid_angle: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
        variant_b: bool,
    ) -> Result<Self> {
        if (latc.abs() - std::f64::consts::FRAC_PI_2).abs() < POLE_EPSILON {
            return Err(Error::InvalidDefinition(
                "Hotine Oblique Mercator projection centre cannot be at a pole".into(),
            ));
        }

        let e2 = ellipsoid.e2();
        let e = ellipsoid.e();
        let sin_latc = latc.sin();
        let cos_latc = latc.cos();
        let b = (1.0 + e2 * cos_latc.powi(4) / (1.0 - e2)).sqrt();
        let a_const = ellipsoid.semi_major_axis() * b * k0 * (1.0 - e2).sqrt()
            / (1.0 - e2 * sin_latc * sin_latc);
        let t0 = t_func(latc, e);
        let d = b * (1.0 - e2).sqrt() / (cos_latc * (1.0 - e2 * sin_latc * sin_latc).sqrt());
        let d_sq = (d * d).max(1.0);
        let f = d + (d_sq - 1.0).sqrt() * latc.signum();
        if !f.is_finite() || f <= 0.0 {
            return Err(Error::InvalidDefinition(
                "Hotine Oblique Mercator origin constants are invalid".into(),
            ));
        }
        let h = f * t0.powf(b);
        let g = (f - 1.0 / f) / 2.0;
        let gamma0 = (azimuth.sin() / d).clamp(-1.0, 1.0).asin();
        let lon0 = lonc - (g * gamma0.tan()).clamp(-1.0, 1.0).asin() / b;

        let u_c = if variant_b {
            (a_const / b) * (d_sq - 1.0).sqrt().atan2(azimuth.cos()) * latc.signum()
        } else {
            0.0
        };

        Ok(GeneralHotineObliqueMercator {
            e,
            a_const,
            b,
            h,
            gamma0,
            gamma_c: rectified_grid_angle,
            lon0,
            u_c,
            false_easting,
            false_northing,
        })
    }
}

#[derive(Clone)]
struct SwissObliqueMercator {
    e: f64,
    half_e: f64,
    c: f64,
    k: f64,
    k_r: f64,
    cos_chi0: f64,
    sin_chi0: f64,
    lon0: f64,
    false_easting: f64,
    false_northing: f64,
    one_minus_e2: f64,
}

impl SwissObliqueMercator {
    fn new(
        ellipsoid: Ellipsoid,
        lat0: f64,
        lon0: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        if (lat0.abs() - std::f64::consts::FRAC_PI_2).abs() < POLE_EPSILON {
            return Err(Error::InvalidDefinition(
                "Swiss Oblique Mercator projection centre cannot be at a pole".into(),
            ));
        }

        let e2 = ellipsoid.e2();
        let e = ellipsoid.e();
        let one_minus_e2 = 1.0 - e2;
        let half_e = 0.5 * e;
        let sin_lat0 = lat0.sin();
        let cos_lat0 = lat0.cos();
        let c = (1.0 + e2 * cos_lat0.powi(4) / one_minus_e2).sqrt();
        let sin_chi0 = sin_lat0 / c;
        let chi0 = unit_asin(sin_chi0, "Swiss Oblique Mercator origin constants")?;
        let cos_chi0 = chi0.cos();
        let e_sin_lat0 = e * sin_lat0;
        let k = isometric_latitude_sphere(chi0)
            - c * isometric_latitude_ellipsoid(lat0, half_e, e_sin_lat0);
        let k_r = ellipsoid.semi_major_axis() * k0 * one_minus_e2.sqrt()
            / (1.0 - e_sin_lat0 * e_sin_lat0);

        Ok(Self {
            e,
            half_e,
            c,
            k,
            k_r,
            cos_chi0,
            sin_chi0,
            lon0,
            false_easting,
            false_northing,
            one_minus_e2,
        })
    }

    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let e_sin_lat = self.e * lat.sin();
        let chi = 2.0
            * (self.c * isometric_latitude_ellipsoid(lat, self.half_e, e_sin_lat) + self.k)
                .exp()
                .atan()
            - std::f64::consts::FRAC_PI_2;
        let lambda = self.c * normalize_longitude(lon - self.lon0);
        let cos_chi = chi.cos();
        let chi_rot = unit_asin(
            self.cos_chi0 * chi.sin() - self.sin_chi0 * cos_chi * lambda.cos(),
            "Swiss Oblique Mercator",
        )?;
        let cos_chi_rot = chi_rot.cos();
        if cos_chi_rot.abs() < POLE_EPSILON {
            return Err(Error::OutOfRange(
                "Swiss Oblique Mercator is undefined at this coordinate".into(),
            ));
        }
        let lambda_rot = unit_asin(
            cos_chi * lambda.sin() / cos_chi_rot,
            "Swiss Oblique Mercator",
        )?;

        let x = self.false_easting + self.k_r * lambda_rot;
        let y = self.false_northing + self.k_r * isometric_latitude_sphere(chi_rot);
        ensure_finite_xy("Swiss Oblique Mercator", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let dx = x - self.false_easting;
        let dy = y - self.false_northing;
        let chi_rot = 2.0 * ((dy / self.k_r).exp().atan() - std::f64::consts::FRAC_PI_4);
        let lambda_rot = dx / self.k_r;
        let cos_chi_rot = chi_rot.cos();
        let chi = unit_asin(
            self.cos_chi0 * chi_rot.sin() + self.sin_chi0 * cos_chi_rot * lambda_rot.cos(),
            "Swiss Oblique Mercator inverse",
        )?;
        let cos_chi = chi.cos();
        if cos_chi.abs() < POLE_EPSILON {
            return Err(Error::OutOfRange(
                "Swiss Oblique Mercator inverse is undefined at this coordinate".into(),
            ));
        }
        let lambda = unit_asin(
            cos_chi_rot * lambda_rot.sin() / cos_chi,
            "Swiss Oblique Mercator inverse",
        )?;

        let target = (self.k - isometric_latitude_sphere(chi)) / self.c;
        let lat = crate::projection::converge(
            "Swiss Oblique Mercator inverse latitude",
            chi,
            SWISS_INVERSE_ITERATIONS,
            SWISS_INVERSE_TOLERANCE,
            |lat| {
                let e_sin = self.e * lat.sin();
                lat - (target + isometric_latitude_ellipsoid(lat, self.half_e, e_sin))
                    * (1.0 - e_sin * e_sin)
                    * lat.cos()
                    / self.one_minus_e2
            },
        )?;
        let lon = self.lon0 + lambda / self.c;
        ensure_finite_lon_lat("Swiss Oblique Mercator", lon, lat)
    }
}

fn is_swiss_right_angle_form(azimuth: f64, rectified_grid_angle: f64) -> bool {
    (azimuth - std::f64::consts::FRAC_PI_2).abs() < RIGHT_ANGLE_EPSILON
        && (rectified_grid_angle - std::f64::consts::FRAC_PI_2).abs() < RIGHT_ANGLE_EPSILON
}

fn isometric_latitude_sphere(lat: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + 0.5 * lat).tan().ln()
}

fn isometric_latitude_ellipsoid(lat: f64, half_e: f64, e_sin_lat: f64) -> f64 {
    isometric_latitude_sphere(lat) - half_e * ((1.0 + e_sin_lat) / (1.0 - e_sin_lat)).ln()
}

fn unit_asin(value: f64, projection: &str) -> Result<f64> {
    if !value.is_finite() || value.abs() > 1.0 + 1e-12 {
        return Err(Error::OutOfRange(format!(
            "{projection} is undefined for this coordinate"
        )));
    }
    Ok(value.clamp(-1.0, 1.0).asin())
}

fn t_func(lat: f64, e: f64) -> f64 {
    if e.abs() < ECCENTRICITY_EPSILON {
        return (std::f64::consts::FRAC_PI_4 - lat / 2.0).tan();
    }

    let sin_lat = lat.sin();
    let e_sin = e * sin_lat;
    (std::f64::consts::FRAC_PI_4 - lat / 2.0).tan() / ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)
}

impl super::ProjectionImpl for HotineObliqueMercator {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        match &self.kernel {
            HotineKernel::General(proj) => proj.forward(lon, lat),
            HotineKernel::Swiss(proj) => proj.forward(lon, lat),
        }
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        match &self.kernel {
            HotineKernel::General(proj) => proj.inverse(x, y),
            HotineKernel::Swiss(proj) => proj.inverse(x, y),
        }
    }
}

impl GeneralHotineObliqueMercator {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let d_lon = normalize_longitude(lon - self.lon0);
        let b_d_lon = self.b * d_lon;
        let t = t_func(lat, self.e);
        let q = self.h / t.powf(self.b);
        let s = (q - 1.0 / q) / 2.0;
        let t_h = (q + 1.0 / q) / 2.0;
        let v = b_d_lon.sin();
        let u_factor = (-v * self.gamma0.cos() + s * self.gamma0.sin()) / t_h;
        if u_factor.abs() >= 1.0 {
            return Err(Error::OutOfRange(
                "Hotine Oblique Mercator is undefined for this coordinate".into(),
            ));
        }

        let skew_v = self.a_const * ((1.0 - u_factor) / (1.0 + u_factor)).ln() / (2.0 * self.b);
        let skew_u = self.a_const
            * (s * self.gamma0.cos() + v * self.gamma0.sin()).atan2(b_d_lon.cos())
            / self.b
            - self.u_c;

        let x = self.false_easting + skew_v * self.gamma_c.cos() + skew_u * self.gamma_c.sin();
        let y = self.false_northing + skew_u * self.gamma_c.cos() - skew_v * self.gamma_c.sin();

        ensure_finite_xy("Hotine Oblique Mercator", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let dx = x - self.false_easting;
        let dy = y - self.false_northing;
        let skew_v = dx * self.gamma_c.cos() - dy * self.gamma_c.sin();
        let skew_u = dy * self.gamma_c.cos() + dx * self.gamma_c.sin() + self.u_c;

        let q = (-self.b * skew_v / self.a_const).exp();
        let s = (q - 1.0 / q) / 2.0;
        let t_h = (q + 1.0 / q) / 2.0;
        let v = (self.b * skew_u / self.a_const).sin();
        let u = (v * self.gamma0.cos() + s * self.gamma0.sin()) / t_h;
        if u.abs() >= 1.0 {
            return Err(Error::OutOfRange(
                "Hotine Oblique Mercator inverse is undefined for this coordinate".into(),
            ));
        }

        let t = (self.h / ((1.0 + u) / (1.0 - u)).sqrt()).powf(1.0 / self.b);
        let lat = crate::projection::latitude_from_conformal_t(
            "Hotine Oblique Mercator inverse latitude",
            t,
            self.e,
        )?;
        let lon = self.lon0
            - (s * self.gamma0.cos() - v * self.gamma0.sin())
                .atan2((self.b * skew_u / self.a_const).cos())
                / self.b;

        ensure_finite_lon_lat("Hotine Oblique Mercator", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid::Ellipsoid;
    use crate::projection::ProjectionImpl;

    fn dms(deg: f64, min: f64, sec: f64) -> f64 {
        deg + min / 60.0 + sec / 3600.0
    }

    fn everest_1830_1967() -> Ellipsoid {
        Ellipsoid::from_a_rf(6_377_298.556, 300.8017).unwrap()
    }

    fn bessel_1841() -> Ellipsoid {
        Ellipsoid::from_a_rf(6_377_397.155, 299.1528128).unwrap()
    }

    #[test]
    fn epsg_variant_a_example() {
        let proj = HotineObliqueMercator::new(
            everest_1830_1967(),
            dms(4.0, 0.0, 0.0).to_radians(),
            dms(115.0, 0.0, 0.0).to_radians(),
            dms(53.0, 18.0, 56.9537).to_radians(),
            dms(53.0, 7.0, 48.3685).to_radians(),
            0.99984,
            0.0,
            0.0,
            false,
        )
        .unwrap();

        let lon = dms(115.0, 48.0, 19.8196).to_radians();
        let lat = dms(5.0, 23.0, 14.1129).to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();

        assert!((x - 679_245.73).abs() < 0.02, "x = {x}");
        assert!((y - 596_562.78).abs() < 0.02, "y = {y}");

        let (lon2, lat2) = proj.inverse(x, y).unwrap();
        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }

    #[test]
    fn epsg_variant_b_example() {
        let proj = HotineObliqueMercator::new(
            everest_1830_1967(),
            dms(4.0, 0.0, 0.0).to_radians(),
            dms(115.0, 0.0, 0.0).to_radians(),
            dms(53.0, 18.0, 56.9537).to_radians(),
            dms(53.0, 7.0, 48.3685).to_radians(),
            0.99984,
            590_476.87,
            442_857.65,
            true,
        )
        .unwrap();

        let lon = dms(115.0, 48.0, 19.8196).to_radians();
        let lat = dms(5.0, 23.0, 14.1129).to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();

        assert!((x - 679_245.73).abs() < 0.02, "x = {x}");
        assert!((y - 596_562.78).abs() < 0.02, "y = {y}");

        let (lon2, lat2) = proj.inverse(x, y).unwrap();
        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }

    #[test]
    fn swiss_right_angle_variant_b_matches_lv95() {
        let proj = HotineObliqueMercator::new(
            bessel_1841(),
            dms(46.0, 57.0, 8.66).to_radians(),
            dms(7.0, 26.0, 22.5).to_radians(),
            90.0_f64.to_radians(),
            90.0_f64.to_radians(),
            1.0,
            2_600_000.0,
            1_200_000.0,
            true,
        )
        .unwrap();

        let bern_lon = 7.4386_f64.to_radians();
        let bern_lat = 46.9511_f64.to_radians();
        let (x, y) = proj.forward(bern_lon, bern_lat).unwrap();

        assert!((x - 2_599_925.1524788374).abs() < 1e-6, "x = {x}");
        assert!((y - 1_199_854.87823513).abs() < 1e-6, "y = {y}");

        let (lon, lat) = proj.inverse(x, y).unwrap();
        assert!((lon - bern_lon).abs() < 1e-12, "lon = {lon}");
        assert!((lat - bern_lat).abs() < 1e-12, "lat = {lat}");

        let (zurich_x, zurich_y) = proj
            .forward(8.5417_f64.to_radians(), 47.3769_f64.to_radians())
            .unwrap();

        assert!(
            (zurich_x - 2_683_220.7548285224).abs() < 1e-6,
            "x = {zurich_x}"
        );
        assert!(
            (zurich_y - 1_247_772.848570864).abs() < 1e-6,
            "y = {zurich_y}"
        );
    }
}

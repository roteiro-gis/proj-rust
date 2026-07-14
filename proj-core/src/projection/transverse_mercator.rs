use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected, validate_scale,
};

/// Transverse Mercator projection.
///
/// The foundation for UTM zones and many national grid systems.
///
/// Implements the exact ("extended") transverse Mercator of Poder & Engsager
/// (Engsager, K. & Poder, K., 2007, "A highly accurate world wide algorithm
/// for the transverse Mercator mapping (almost)", ICC2007), the same
/// formulation C PROJ uses by default. Sixth-order series in the third
/// flattening keep sub-nanometer accuracy over the full ±150° domain around
/// the central meridian, including the poles, where the classic Snyder
/// series loses longitude to catastrophic cancellation.
pub(crate) struct TransverseMercator {
    /// Semi-major axis.
    a: f64,
    /// Central meridian in radians.
    lon0: f64,
    /// False easting in meters.
    false_easting: f64,
    /// False northing in meters.
    false_northing: f64,
    /// Normalized meridian quadrant; folds in the central-meridian scale k0.
    qn: f64,
    /// Origin northing offset for the latitude of origin, in qn-units.
    zb: f64,
    /// Gaussian latitude → geodetic latitude series.
    cgb: [f64; 6],
    /// Geodetic latitude → Gaussian latitude series.
    cbg: [f64; 6],
    /// Ellipsoidal normalized N,E → complex spherical N,E series.
    utg: [f64; 6],
    /// Complex spherical N,E → ellipsoidal normalized N,E series.
    gtu: [f64; 6],
}

/// Domain bound on the normalized easting: 150° from the central meridian.
const MAX_NORMALIZED_EASTING: f64 = 2.623_395_162_778;

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

        let a = ellipsoid.semi_major_axis();
        let e2 = ellipsoid.e2();

        // Third flattening n = f / (2 - f).
        let sqrt_one_minus_e2 = (1.0 - e2).sqrt();
        let n = (1.0 - sqrt_one_minus_e2) / (1.0 + sqrt_one_minus_e2);

        // Series coefficients, Engsager & Poder ICC2007 (6th degree).
        // cgb: Gaussian -> geodetic, cbg: geodetic -> Gaussian.
        let mut cgb = [0.0; 6];
        let mut cbg = [0.0; 6];
        let mut np = n;
        cgb[0] = n
            * (2.0
                + n * (-2.0 / 3.0
                    + n * (-2.0 + n * (116.0 / 45.0 + n * (26.0 / 45.0 + n * (-2854.0 / 675.0))))));
        cbg[0] = n
            * (-2.0
                + n * (2.0 / 3.0
                    + n * (4.0 / 3.0
                        + n * (-82.0 / 45.0 + n * (32.0 / 45.0 + n * (4642.0 / 4725.0))))));
        np *= n;
        cgb[1] = np
            * (7.0 / 3.0
                + n * (-8.0 / 5.0
                    + n * (-227.0 / 45.0 + n * (2704.0 / 315.0 + n * (2323.0 / 945.0)))));
        cbg[1] = np
            * (5.0 / 3.0
                + n * (-16.0 / 15.0
                    + n * (-13.0 / 9.0 + n * (904.0 / 315.0 + n * (-1522.0 / 945.0)))));
        np *= n;
        cgb[2] = np
            * (56.0 / 15.0 + n * (-136.0 / 35.0 + n * (-1262.0 / 105.0 + n * (73814.0 / 2835.0))));
        cbg[2] =
            np * (-26.0 / 15.0 + n * (34.0 / 21.0 + n * (8.0 / 5.0 + n * (-12686.0 / 2835.0))));
        np *= n;
        cgb[3] = np * (4279.0 / 630.0 + n * (-332.0 / 35.0 + n * (-399572.0 / 14175.0)));
        cbg[3] = np * (1237.0 / 630.0 + n * (-12.0 / 5.0 + n * (-24832.0 / 14175.0)));
        np *= n;
        cgb[4] = np * (4174.0 / 315.0 + n * (-144838.0 / 6237.0));
        cbg[4] = np * (-734.0 / 315.0 + n * (109598.0 / 31185.0));
        np *= n;
        cgb[5] = np * (601676.0 / 22275.0);
        cbg[5] = np * (444337.0 / 155925.0);

        // Normalized meridian quadrant, folding in k0.
        let n2 = n * n;
        let qn = k0 / (1.0 + n) * (1.0 + n2 * (1.0 / 4.0 + n2 * (1.0 / 64.0 + n2 / 256.0)));

        // utg: ellipsoidal N,E -> spherical; gtu: spherical -> ellipsoidal.
        let mut utg = [0.0; 6];
        let mut gtu = [0.0; 6];
        let mut np = n;
        utg[0] = n
            * (-0.5
                + n * (2.0 / 3.0
                    + n * (-37.0 / 96.0
                        + n * (1.0 / 360.0 + n * (81.0 / 512.0 + n * (-96199.0 / 604800.0))))));
        gtu[0] = n
            * (0.5
                + n * (-2.0 / 3.0
                    + n * (5.0 / 16.0
                        + n * (41.0 / 180.0 + n * (-127.0 / 288.0 + n * (7891.0 / 37800.0))))));
        np *= n;
        utg[1] = np
            * (-1.0 / 48.0
                + n * (-1.0 / 15.0
                    + n * (437.0 / 1440.0 + n * (-46.0 / 105.0 + n * (1118711.0 / 3870720.0)))));
        gtu[1] = np
            * (13.0 / 48.0
                + n * (-3.0 / 5.0
                    + n * (557.0 / 1440.0 + n * (281.0 / 630.0 + n * (-1983433.0 / 1935360.0)))));
        np *= n;
        utg[2] = np
            * (-17.0 / 480.0 + n * (37.0 / 840.0 + n * (209.0 / 4480.0 + n * (-5569.0 / 90720.0))));
        gtu[2] = np
            * (61.0 / 240.0
                + n * (-103.0 / 140.0 + n * (15061.0 / 26880.0 + n * (167603.0 / 181440.0))));
        np *= n;
        utg[3] = np * (-4397.0 / 161280.0 + n * (11.0 / 504.0 + n * (830251.0 / 7257600.0)));
        gtu[3] = np * (49561.0 / 161280.0 + n * (-179.0 / 168.0 + n * (6601661.0 / 7257600.0)));
        np *= n;
        utg[4] = np * (-4583.0 / 161280.0 + n * (108847.0 / 3991680.0));
        gtu[4] = np * (34729.0 / 80640.0 + n * (-3418889.0 / 1995840.0));
        np *= n;
        utg[5] = np * (-20648693.0 / 638668800.0);
        gtu[5] = np * (212378941.0 / 319334400.0);

        // Gaussian latitude of the origin latitude, and the origin northing
        // offset relative to the equator.
        let z = gatg(&cbg, lat0, (2.0 * lat0).cos(), (2.0 * lat0).sin());
        let zb = -qn * (z + clens(&gtu, 2.0 * z));

        Ok(Self {
            a,
            lon0,
            false_easting,
            false_northing,
            qn,
            zb,
            cgb,
            cbg,
            utg,
            gtu,
        })
    }
}

/// Gaussian ↔ geodetic latitude via Clenshaw summation of a sin(2kB) series.
fn gatg(coefficients: &[f64; 6], b: f64, cos_2b: f64, sin_2b: f64) -> f64 {
    let two_cos_2b = 2.0 * cos_2b;
    let mut h = 0.0;
    let mut h2 = 0.0;
    let mut iter = coefficients.iter().rev();
    let mut h1 = *iter.next().expect("coefficient array is non-empty");
    for &coefficient in iter {
        h = -h2 + two_cos_2b * h1 + coefficient;
        h2 = h1;
        h1 = h;
    }
    b + h * sin_2b
}

/// Complex Clenshaw summation; returns the (real, imaginary) series sum.
fn clen_s(
    coefficients: &[f64; 6],
    sin_arg_r: f64,
    cos_arg_r: f64,
    sinh_arg_i: f64,
    cosh_arg_i: f64,
) -> (f64, f64) {
    let r = 2.0 * cos_arg_r * cosh_arg_i;
    let i = -2.0 * sin_arg_r * sinh_arg_i;

    let mut hr1 = 0.0;
    let mut hi1 = 0.0;
    let mut hi = 0.0;
    let mut iter = coefficients.iter().rev();
    let mut hr = *iter.next().expect("coefficient array is non-empty");
    for &coefficient in iter {
        let hr2 = hr1;
        let hi2 = hi1;
        hr1 = hr;
        hi1 = hi;
        hr = -hr2 + r * hr1 - i * hi1 + coefficient;
        hi = -hi2 + i * hr1 + r * hi1;
    }

    let sr = sin_arg_r * cosh_arg_i;
    let si = cos_arg_r * sinh_arg_i;
    (sr * hr - si * hi, sr * hi + si * hr)
}

/// Real Clenshaw summation of a sin(k·arg) series.
fn clens(coefficients: &[f64; 6], arg_r: f64) -> f64 {
    let r = 2.0 * arg_r.cos();
    let mut hr1 = 0.0;
    let mut iter = coefficients.iter().rev();
    let mut hr = *iter.next().expect("coefficient array is non-empty");
    for &coefficient in iter {
        let hr2 = hr1;
        hr1 = hr;
        hr = -hr2 + r * hr1 + coefficient;
    }
    arg_r.sin() * hr
}

impl super::ProjectionImpl for TransverseMercator {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);

        // Geodetic -> Gaussian latitude.
        let cn_gauss = gatg(&self.cbg, lat, (2.0 * lat).cos(), (2.0 * lat).sin());

        // Gaussian -> complex spherical N,E.
        let sin_cn = cn_gauss.sin();
        let cos_cn = cn_gauss.cos();
        let sin_ce = lam.sin();
        let cos_ce = lam.cos();
        let cos_cn_cos_ce = cos_cn * cos_ce;

        let mut cn = sin_cn.atan2(cos_cn_cos_ce);
        let inv_denom_tan_ce = 1.0 / sin_cn.hypot(cos_cn_cos_ce);
        let tan_ce = sin_ce * cos_cn * inv_denom_tan_ce;
        let mut ce = tan_ce.asinh();

        // sin/cos(2·Cn) and sinh/cosh(2·Ce) without extra trig calls.
        let two_inv_denom_tan_ce = 2.0 * inv_denom_tan_ce;
        let two_inv_denom_tan_ce_square = two_inv_denom_tan_ce * inv_denom_tan_ce;
        let tmp_r = cos_cn_cos_ce * two_inv_denom_tan_ce_square;
        let sin_arg_r = sin_cn * tmp_r;
        let cos_arg_r = cos_cn_cos_ce * tmp_r - 1.0;
        let sinh_arg_i = tan_ce * two_inv_denom_tan_ce;
        let cosh_arg_i = two_inv_denom_tan_ce_square - 1.0;

        // Spherical -> ellipsoidal normalized N,E.
        let (d_cn, d_ce) = clen_s(&self.gtu, sin_arg_r, cos_arg_r, sinh_arg_i, cosh_arg_i);
        cn += d_cn;
        ce += d_ce;

        if ce.abs() > MAX_NORMALIZED_EASTING {
            return Err(Error::OutOfRange(
                "coordinate is outside the transverse Mercator projection domain".into(),
            ));
        }

        let x = self.false_easting + self.a * self.qn * ce;
        let y = self.false_northing + self.a * (self.qn * cn + self.zb);
        ensure_finite_xy("Transverse Mercator", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;

        // Normalize N, E.
        let mut cn = ((y - self.false_northing) / self.a - self.zb) / self.qn;
        let mut ce = (x - self.false_easting) / (self.a * self.qn);

        if ce.abs() > MAX_NORMALIZED_EASTING {
            return Err(Error::OutOfRange(
                "coordinate is outside the transverse Mercator projection domain".into(),
            ));
        }

        // Ellipsoidal -> spherical normalized N,E.
        let sin_arg_r = (2.0 * cn).sin();
        let cos_arg_r = (2.0 * cn).cos();
        let exp_2_ce = (2.0 * ce).exp();
        let half_inv_exp_2_ce = 0.5 / exp_2_ce;
        let sinh_arg_i = 0.5 * exp_2_ce - half_inv_exp_2_ce;
        let cosh_arg_i = 0.5 * exp_2_ce + half_inv_exp_2_ce;

        let (d_cn, d_ce) = clen_s(&self.utg, sin_arg_r, cos_arg_r, sinh_arg_i, cosh_arg_i);
        cn += d_cn;
        ce += d_ce;

        // Complex spherical -> Gaussian latitude and longitude.
        let sin_cn = cn.sin();
        let cos_cn = cn.cos();
        let sinh_ce = ce.sinh();

        let ce_out = sinh_ce.atan2(cos_cn);
        let modulus_ce = sinh_ce.hypot(cos_cn);
        let cn_out = sin_cn.atan2(modulus_ce);

        // Gaussian -> geodetic latitude; sin/cos(2·Cn) computed directly.
        let tmp = 2.0 * modulus_ce / (sinh_ce * sinh_ce + 1.0);
        let sin_2_cn = sin_cn * tmp;
        let cos_2_cn = tmp * modulus_ce - 1.0;

        let lat = gatg(&self.cgb, cn_out, cos_2_cn, sin_2_cn);
        let lon = self.lon0 + ce_out;

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

    #[test]
    fn forward_wraps_longitude_delta() {
        let proj = TransverseMercator::new(
            ellipsoid::WGS84,
            179.0_f64.to_radians(),
            0.0,
            0.9996,
            500_000.0,
            0.0,
        )
        .unwrap();
        let lat = 10.0_f64.to_radians();

        let wrapped = proj.forward((-181.0_f64).to_radians(), lat).unwrap();
        let canonical = proj.forward(179.0_f64.to_radians(), lat).unwrap();

        assert!((wrapped.0 - canonical.0).abs() < 1e-8);
        assert!((wrapped.1 - canonical.1).abs() < 1e-8);
    }

    #[test]
    fn near_pole_roundtrip_preserves_longitude() {
        // The Snyder series this implementation replaced lost several
        // millidegrees of longitude here to cancellation in the /cos(phi1)
        // term. Longitude is ill-conditioned this close to the pole, so the
        // meaningful criterion is displacement: re-projecting the recovered
        // coordinate must land within a micrometer of the original point.
        let proj = utm(18, true);
        for &(lon_deg, lat_deg) in &[
            (-70.5, 89.999999),
            (-75.0, 89.9999999),
            (-71.0, 89.99999999),
        ] {
            let lon = f64::to_radians(lon_deg);
            let lat = f64::to_radians(lat_deg);
            let (x, y) = proj.forward(lon, lat).unwrap();
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            let (x2, y2) = proj.forward(lon2, lat2).unwrap();
            assert!(
                (x2 - x).abs() < 1e-6 && (y2 - y).abs() < 1e-6,
                "({lon_deg}, {lat_deg}): displaced by ({:e}, {:e}) m",
                x2 - x,
                y2 - y
            );
            assert!(
                (lat2 - lat).abs().to_degrees() < 1e-9,
                "({lon_deg}, {lat_deg}): lat came back {}",
                lat2.to_degrees()
            );
        }

        // Away from the last few centimeters around the pole the longitude
        // itself must be recovered tightly (C PROJ recovers ~1e-7 deg here).
        let lon = f64::to_radians(-70.5);
        let lat = f64::to_radians(89.999999);
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, _) = proj.inverse(x, y).unwrap();
        assert!(
            (lon2 - lon).abs().to_degrees() < 1e-6,
            "lon came back {}",
            lon2.to_degrees()
        );
    }

    #[test]
    fn pole_roundtrips_exactly() {
        let proj = utm(18, true);
        let (x, y) = proj
            .forward((-75.0_f64).to_radians(), std::f64::consts::FRAC_PI_2)
            .unwrap();
        let (_, lat) = proj.inverse(x, y).unwrap();
        assert!((lat - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    }

    #[test]
    fn near_equatorial_singularity_errors() {
        // The projection is singular at the equatorial points 90° from the
        // central meridian; the conformal-easting domain bound rejects the
        // surrounding band with a typed error rather than huge coordinates.
        let proj = utm(31, true);
        let error = proj
            .forward(93.0_f64.to_radians(), 1.0_f64.to_radians())
            .unwrap_err();
        assert!(
            error.to_string().contains("projection domain"),
            "got: {error}"
        );
    }

    #[test]
    fn latitude_of_origin_offsets_northing() {
        // Gauss-Krüger style definition with a non-zero latitude of origin
        // must place the origin latitude at the false northing.
        let lat0 = 49.0_f64.to_radians();
        let proj = TransverseMercator::new(
            ellipsoid::WGS84,
            (-2.0_f64).to_radians(),
            lat0,
            0.9996012717,
            400_000.0,
            -100_000.0,
        )
        .unwrap();
        let (x, y) = proj.forward((-2.0_f64).to_radians(), lat0).unwrap();
        assert!((x - 400_000.0).abs() < 1e-6);
        assert!((y - (-100_000.0)).abs() < 1e-6);
    }
}

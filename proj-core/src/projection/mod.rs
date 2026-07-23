pub(crate) mod albers_equal_area;
pub(crate) mod american_polyconic;
pub(crate) mod azimuthal_equidistant;
pub(crate) mod cassini_soldner;
pub(crate) mod colombia_urban;
pub(crate) mod equal_earth;
pub(crate) mod equidistant_cylindrical;
pub(crate) mod hotine_oblique_mercator;
pub(crate) mod krovak;
pub(crate) mod laborde;
pub(crate) mod lambert_azimuthal_equal_area;
pub(crate) mod lambert_conformal_conic;
pub(crate) mod mercator;
pub(crate) mod oblique_stereographic;
pub(crate) mod polar_stereographic;
pub(crate) mod transverse_mercator;
pub(crate) mod web_mercator;

use crate::crs::ProjectionMethod;
use crate::datum::Datum;
use crate::error::{Error, Result};

const LAT_EPSILON: f64 = 1e-12;
const ITERATION_ECCENTRICITY_EPSILON: f64 = 1e-15;

/// Run the fixed-point iteration `step` from `initial` until successive
/// iterates differ by less than `tolerance`.
///
/// Unlike the fixed-count loops this replaces, exhausting `max_iterations`
/// is an error instead of silently returning a non-converged iterate.
pub(crate) fn converge(
    context: &'static str,
    initial: f64,
    max_iterations: usize,
    tolerance: f64,
    mut step: impl FnMut(f64) -> f64,
) -> Result<f64> {
    let mut value = initial;
    for _ in 0..max_iterations {
        let next = step(value);
        if (next - value).abs() < tolerance {
            return Ok(next);
        }
        value = next;
    }
    Err(Error::NonConvergence {
        context,
        iterations: max_iterations,
    })
}

/// Order of the meridional-arc expansion in the third flattening `n`;
/// full double precision for |f| ≤ 1/150.
const MERIDIAN_ARC_ORDER: usize = 6;

/// Coefficients for the meridional-arc series and its inverse, matching
/// C PROJ's `pj_enfn`: a 6th-order expansion in the third flattening
/// (Karney, arXiv:2212.05818). Layout: `[0]` is the rectifying-radius
/// multiplier, `[1..=6]` convert φ→μ, `[7..=12]` convert μ→φ.
pub(crate) fn meridian_arc_coefficients(es: f64) -> [f64; 13] {
    // Third flattening n = (a - b) / (a + b) from the eccentricity squared.
    let one_minus_f = (1.0 - es).sqrt();
    let n = (1.0 - one_minus_f) / (1.0 + one_minus_f);

    // Expansion of (quarter meridian) / ((a+b)/2 * π/2) as a series in n²;
    // the coefficients are ((2k-3)!! / (2k)!!)² for k = 0..3.
    const COEFF_RAD: [f64; 4] = [1.0, 1.0 / 4.0, 1.0 / 64.0, 1.0 / 256.0];
    // φ→μ, Eq. A5 with zero terms dropped.
    const COEFF_MU_PHI: [f64; 12] = [
        -3.0 / 2.0,
        9.0 / 16.0,
        -3.0 / 32.0,
        15.0 / 16.0,
        -15.0 / 32.0,
        135.0 / 2048.0,
        -35.0 / 48.0,
        105.0 / 256.0,
        315.0 / 512.0,
        -189.0 / 512.0,
        -693.0 / 1280.0,
        1001.0 / 2048.0,
    ];
    // μ→φ, Eq. A6 with zero terms dropped.
    const COEFF_PHI_MU: [f64; 12] = [
        3.0 / 2.0,
        -27.0 / 32.0,
        269.0 / 512.0,
        21.0 / 16.0,
        -55.0 / 32.0,
        6759.0 / 4096.0,
        151.0 / 96.0,
        -417.0 / 128.0,
        1097.0 / 512.0,
        -15543.0 / 2560.0,
        8011.0 / 2560.0,
        293393.0 / 61440.0,
    ];

    fn polyval(x: f64, p: &[f64]) -> f64 {
        p.iter().rev().fold(0.0, |y, &c| y * x + c)
    }

    let n2 = n * n;
    let mut en = [0.0; 2 * MERIDIAN_ARC_ORDER + 1];
    en[0] = polyval(n2, &COEFF_RAD[..=MERIDIAN_ARC_ORDER / 2]) / (1.0 + n);
    let mut d = n;
    let mut o = 0;
    for l in 0..MERIDIAN_ARC_ORDER {
        let m = (MERIDIAN_ARC_ORDER - l - 1) / 2;
        en[l + 1] = d * polyval(n2, &COEFF_MU_PHI[o..=o + m]);
        en[l + 1 + MERIDIAN_ARC_ORDER] = d * polyval(n2, &COEFF_PHI_MU[o..=o + m]);
        d *= n;
        o += m + 1;
    }
    en
}

/// Evaluate `sum(c[k] * sin((2k+2)·ζ))` by Clenshaw summation.
fn clenshaw(sin_zeta: f64, cos_zeta: f64, c: &[f64]) -> f64 {
    let x = 2.0 * (cos_zeta - sin_zeta) * (cos_zeta + sin_zeta);
    let (mut u0, mut u1) = (0.0, 0.0);
    for &coeff in c.iter().rev() {
        let t = x * u0 - u1 + coeff;
        u1 = u0;
        u0 = t;
    }
    2.0 * sin_zeta * cos_zeta * u0
}

/// Meridional arc length from the equator to latitude φ, in semi-major-axis
/// units (C PROJ's `pj_mlfn`).
pub(crate) fn meridian_arc(phi: f64, sin_phi: f64, cos_phi: f64, en: &[f64; 13]) -> f64 {
    en[0] * (phi + clenshaw(sin_phi, cos_phi, &en[1..=MERIDIAN_ARC_ORDER]))
}

/// Latitude φ from the meridional arc length, in semi-major-axis units
/// (C PROJ's `pj_inv_mlfn`).
pub(crate) fn inverse_meridian_arc(ml: f64, en: &[f64; 13]) -> f64 {
    let mu = ml / en[0];
    mu + clenshaw(mu.sin(), mu.cos(), &en[MERIDIAN_ARC_ORDER + 1..])
}

/// Authalic `q` for geodetic latitude `lat` (Snyder 3-12), shared by the
/// equal-area projections (LAEA, Equal Earth).
pub(crate) fn authalic_q(lat: f64, e2: f64) -> f64 {
    if e2.abs() < ITERATION_ECCENTRICITY_EPSILON {
        return 2.0 * lat.sin();
    }

    let e = e2.sqrt();
    let sin_lat = lat.sin();
    let e_sin = e * sin_lat;
    (1.0 - e2)
        * (sin_lat / (1.0 - e2 * sin_lat * sin_lat)
            - (1.0 / (2.0 * e)) * ((1.0 - e_sin) / (1.0 + e_sin)).ln())
}

/// Authalic latitude β from `q` and the polar `q_p`.
pub(crate) fn authalic_latitude(q: f64, q_p: f64) -> f64 {
    (q / q_p).clamp(-1.0, 1.0).asin()
}

/// Geodetic latitude from the authalic latitude β (Snyder 3-18 series,
/// matching C PROJ's `pj_authlat`).
pub(crate) fn geodetic_from_authalic(beta: f64, e2: f64) -> f64 {
    if e2.abs() < ITERATION_ECCENTRICITY_EPSILON {
        return beta;
    }

    let e4 = e2 * e2;
    let e6 = e4 * e2;
    beta + (e2 / 3.0 + 31.0 * e4 / 180.0 + 517.0 * e6 / 5040.0) * (2.0 * beta).sin()
        + (23.0 * e4 / 360.0 + 251.0 * e6 / 3780.0) * (4.0 * beta).sin()
        + (761.0 * e6 / 45360.0) * (6.0 * beta).sin()
}

/// Geodetic latitude from the conformal parameter
/// `t = tan(π/4 − φ/2) / ((1 − e·sinφ)/(1 + e·sinφ))^(e/2)`
/// (EPSG Guidance Note 7-2), shared by the Mercator, Lambert Conformal
/// Conic, Polar Stereographic, and Hotine Oblique Mercator inverses.
pub(crate) fn latitude_from_conformal_t(context: &'static str, t: f64, e: f64) -> Result<f64> {
    let initial = std::f64::consts::FRAC_PI_2 - 2.0 * t.atan();
    if e.abs() < ITERATION_ECCENTRICITY_EPSILON {
        return Ok(initial);
    }
    converge(context, initial, 15, 1e-14, |lat| {
        let e_sin = e * lat.sin();
        std::f64::consts::FRAC_PI_2
            - 2.0 * (t * ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)).atan()
    })
}

/// Internal trait for projection math.
///
/// All inputs and outputs are in **radians** (geographic) and **meters** (projected).
/// The `Transform` pipeline handles degree↔radian conversion at the API boundary.
pub(crate) trait ProjectionImpl: Send + Sync {
    /// Forward projection: (lon_rad, lat_rad) → (easting_m, northing_m).
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)>;

    /// Inverse projection: (easting_m, northing_m) → (lon_rad, lat_rad).
    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)>;
}

#[derive(Clone)]
pub(crate) enum Projection {
    WebMercator(web_mercator::WebMercator),
    TransverseMercator(transverse_mercator::TransverseMercator),
    PolarStereographic(polar_stereographic::PolarStereographic),
    LambertConformalConic(lambert_conformal_conic::LambertConformalConic),
    AlbersEqualArea(albers_equal_area::AlbersEqualArea),
    LambertAzimuthalEqualArea(lambert_azimuthal_equal_area::LambertAzimuthalEqualArea),
    ObliqueStereographic(oblique_stereographic::ObliqueStereographic),
    HotineObliqueMercator(hotine_oblique_mercator::HotineObliqueMercator),
    CassiniSoldner(cassini_soldner::CassiniSoldner),
    ColombiaUrban(colombia_urban::ColombiaUrban),
    Krovak(krovak::Krovak),
    Laborde(laborde::Laborde),
    EqualEarth(equal_earth::EqualEarth),
    AmericanPolyconic(american_polyconic::AmericanPolyconic),
    AzimuthalEquidistant(azimuthal_equidistant::AzimuthalEquidistant),
    GuamProjection(azimuthal_equidistant::GuamProjection),
    Mercator(mercator::Mercator),
    EquidistantCylindrical(equidistant_cylindrical::EquidistantCylindrical),
}

impl Projection {
    pub(crate) fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        match self {
            Projection::WebMercator(proj) => proj.forward(lon, lat),
            Projection::TransverseMercator(proj) => proj.forward(lon, lat),
            Projection::PolarStereographic(proj) => proj.forward(lon, lat),
            Projection::LambertConformalConic(proj) => proj.forward(lon, lat),
            Projection::AlbersEqualArea(proj) => proj.forward(lon, lat),
            Projection::LambertAzimuthalEqualArea(proj) => proj.forward(lon, lat),
            Projection::ObliqueStereographic(proj) => proj.forward(lon, lat),
            Projection::HotineObliqueMercator(proj) => proj.forward(lon, lat),
            Projection::CassiniSoldner(proj) => proj.forward(lon, lat),
            Projection::ColombiaUrban(proj) => proj.forward(lon, lat),
            Projection::Krovak(proj) => proj.forward(lon, lat),
            Projection::Laborde(proj) => proj.forward(lon, lat),
            Projection::EqualEarth(proj) => proj.forward(lon, lat),
            Projection::AmericanPolyconic(proj) => proj.forward(lon, lat),
            Projection::AzimuthalEquidistant(proj) => proj.forward(lon, lat),
            Projection::GuamProjection(proj) => proj.forward(lon, lat),
            Projection::Mercator(proj) => proj.forward(lon, lat),
            Projection::EquidistantCylindrical(proj) => proj.forward(lon, lat),
        }
    }

    pub(crate) fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        match self {
            Projection::WebMercator(proj) => proj.inverse(x, y),
            Projection::TransverseMercator(proj) => proj.inverse(x, y),
            Projection::PolarStereographic(proj) => proj.inverse(x, y),
            Projection::LambertConformalConic(proj) => proj.inverse(x, y),
            Projection::AlbersEqualArea(proj) => proj.inverse(x, y),
            Projection::LambertAzimuthalEqualArea(proj) => proj.inverse(x, y),
            Projection::ObliqueStereographic(proj) => proj.inverse(x, y),
            Projection::HotineObliqueMercator(proj) => proj.inverse(x, y),
            Projection::CassiniSoldner(proj) => proj.inverse(x, y),
            Projection::ColombiaUrban(proj) => proj.inverse(x, y),
            Projection::Krovak(proj) => proj.inverse(x, y),
            Projection::Laborde(proj) => proj.inverse(x, y),
            Projection::EqualEarth(proj) => proj.inverse(x, y),
            Projection::AmericanPolyconic(proj) => proj.inverse(x, y),
            Projection::AzimuthalEquidistant(proj) => proj.inverse(x, y),
            Projection::GuamProjection(proj) => proj.inverse(x, y),
            Projection::Mercator(proj) => proj.inverse(x, y),
            Projection::EquidistantCylindrical(proj) => proj.inverse(x, y),
        }
    }
}

/// Construct the appropriate projection implementation from a method definition and datum.
pub(crate) fn make_projection(method: &ProjectionMethod, datum: &Datum) -> Result<Projection> {
    match method {
        ProjectionMethod::WebMercator => {
            Ok(Projection::WebMercator(web_mercator::WebMercator::new()?))
        }
        ProjectionMethod::TransverseMercator {
            lon0,
            lat0,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::TransverseMercator(
            transverse_mercator::TransverseMercator::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *k0,
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::PolarStereographic {
            lon0,
            lat_ts,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::PolarStereographic(
            polar_stereographic::PolarStereographic::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat_ts.to_radians(),
                *k0,
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::LambertConformalConic {
            lon0,
            lat0,
            lat1,
            lat2,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::LambertConformalConic(
            lambert_conformal_conic::LambertConformalConic::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                lat1.to_radians(),
                lat2.to_radians(),
                *k0,
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::LambertConformalConicMichigan {
            lon0,
            lat0,
            lat1,
            lat2,
            ellipsoid_scaling_factor,
            false_easting,
            false_northing,
        } => {
            // The ellipsoid scaling factor scales the semi-major axis while
            // preserving the ellipsoid shape (EPSG method 1051).
            let base = datum.ellipsoid();
            let scaled_a = base.semi_major_axis() * ellipsoid_scaling_factor;
            let scaled = if base.flattening() == 0.0 {
                crate::ellipsoid::Ellipsoid::sphere(scaled_a)
            } else {
                crate::ellipsoid::Ellipsoid::from_a_rf(scaled_a, base.inverse_flattening())
            }?;
            Ok(Projection::LambertConformalConic(
                lambert_conformal_conic::LambertConformalConic::new(
                    scaled,
                    lon0.to_radians(),
                    lat0.to_radians(),
                    lat1.to_radians(),
                    lat2.to_radians(),
                    1.0,
                    *false_easting,
                    *false_northing,
                )?,
            ))
        }
        ProjectionMethod::LambertConformalConic1SPVariantB {
            lon0,
            lat0,
            k0,
            lat_false_origin,
            false_easting,
            false_northing,
        } => Ok(Projection::LambertConformalConic(
            lambert_conformal_conic::LambertConformalConic::new_1sp_variant_b(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *k0,
                lat_false_origin.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::AlbersEqualArea {
            lon0,
            lat0,
            lat1,
            lat2,
            false_easting,
            false_northing,
        } => Ok(Projection::AlbersEqualArea(
            albers_equal_area::AlbersEqualArea::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                lat1.to_radians(),
                lat2.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::LambertAzimuthalEqualArea {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => Ok(Projection::LambertAzimuthalEqualArea(
            lambert_azimuthal_equal_area::LambertAzimuthalEqualArea::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::LambertAzimuthalEqualAreaSpherical {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => Ok(Projection::LambertAzimuthalEqualArea(
            lambert_azimuthal_equal_area::LambertAzimuthalEqualArea::new_spherical(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::ObliqueStereographic {
            lon0,
            lat0,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::ObliqueStereographic(
            oblique_stereographic::ObliqueStereographic::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *k0,
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::HotineObliqueMercator {
            latc,
            lonc,
            azimuth,
            rectified_grid_angle,
            k0,
            false_easting,
            false_northing,
            variant_b,
        } => Ok(Projection::HotineObliqueMercator(
            hotine_oblique_mercator::HotineObliqueMercator::new(
                datum.ellipsoid(),
                latc.to_radians(),
                lonc.to_radians(),
                azimuth.to_radians(),
                rectified_grid_angle.to_radians(),
                *k0,
                *false_easting,
                *false_northing,
                *variant_b,
            )?,
        )),
        ProjectionMethod::CassiniSoldner {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => Ok(Projection::CassiniSoldner(
            cassini_soldner::CassiniSoldner::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::Mercator {
            lon0,
            lat_ts,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::Mercator(mercator::Mercator::new(
            datum.ellipsoid(),
            lon0.to_radians(),
            lat_ts.to_radians(),
            *k0,
            *false_easting,
            *false_northing,
        )?)),
        ProjectionMethod::ColombiaUrban {
            lon0,
            lat0,
            h0,
            false_easting,
            false_northing,
        } => Ok(Projection::ColombiaUrban(
            colombia_urban::ColombiaUrban::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *h0,
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::KrovakNorthOrientated {
            lon0,
            lat0,
            co_latitude_cone_axis,
            lat_pseudo_standard_parallel,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::Krovak(krovak::Krovak::new(
            datum.ellipsoid(),
            lon0.to_radians(),
            lat0.to_radians(),
            co_latitude_cone_axis.to_radians(),
            lat_pseudo_standard_parallel.to_radians(),
            *k0,
            *false_easting,
            *false_northing,
            false,
        )?)),
        ProjectionMethod::KrovakModifiedNorthOrientated {
            lon0,
            lat0,
            co_latitude_cone_axis,
            lat_pseudo_standard_parallel,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::Krovak(krovak::Krovak::new(
            datum.ellipsoid(),
            lon0.to_radians(),
            lat0.to_radians(),
            co_latitude_cone_axis.to_radians(),
            lat_pseudo_standard_parallel.to_radians(),
            *k0,
            *false_easting,
            *false_northing,
            true,
        )?)),
        ProjectionMethod::EqualEarth {
            lon0,
            false_easting,
            false_northing,
        } => Ok(Projection::EqualEarth(equal_earth::EqualEarth::new(
            datum.ellipsoid(),
            lon0.to_radians(),
            *false_easting,
            *false_northing,
        )?)),
        ProjectionMethod::AmericanPolyconic {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => Ok(Projection::AmericanPolyconic(
            american_polyconic::AmericanPolyconic::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::AzimuthalEquidistant {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => Ok(Projection::AzimuthalEquidistant(
            azimuthal_equidistant::AzimuthalEquidistant::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::GuamProjection {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => Ok(Projection::GuamProjection(
            azimuthal_equidistant::GuamProjection::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
        ProjectionMethod::PolarStereographicVariantC {
            lon0,
            lat_ts,
            easting_false_origin,
            northing_false_origin,
        } => Ok(Projection::PolarStereographic(
            polar_stereographic::PolarStereographic::new_variant_c(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat_ts.to_radians(),
                *easting_false_origin,
                *northing_false_origin,
            )?,
        )),
        ProjectionMethod::LabordeObliqueMercator {
            lon0,
            lat0,
            azimuth,
            k0,
            false_easting,
            false_northing,
        } => Ok(Projection::Laborde(laborde::Laborde::new(
            datum.ellipsoid(),
            lon0.to_radians(),
            lat0.to_radians(),
            azimuth.to_radians(),
            *k0,
            *false_easting,
            *false_northing,
        )?)),
        ProjectionMethod::EquidistantCylindrical {
            lon0,
            lat_ts,
            false_easting,
            false_northing,
        } => Ok(Projection::EquidistantCylindrical(
            equidistant_cylindrical::EquidistantCylindrical::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat_ts.to_radians(),
                *false_easting,
                *false_northing,
            )?,
        )),
    }
}

pub(crate) fn validate_lon_lat(lon: f64, lat: f64) -> Result<()> {
    if !lon.is_finite() || !lat.is_finite() {
        return Err(Error::OutOfRange(
            "geographic input coordinate must be finite".into(),
        ));
    }
    if lat.abs() > std::f64::consts::FRAC_PI_2 + LAT_EPSILON {
        return Err(Error::OutOfRange(format!(
            "latitude {:.8}° is outside the valid range [-90°, 90°]",
            lat.to_degrees()
        )));
    }
    Ok(())
}

pub(crate) fn validate_projected(x: f64, y: f64) -> Result<()> {
    if !x.is_finite() || !y.is_finite() {
        return Err(Error::OutOfRange(
            "projected input coordinate must be finite".into(),
        ));
    }
    Ok(())
}

pub(crate) fn ensure_finite_xy(kind: &str, x: f64, y: f64) -> Result<(f64, f64)> {
    if !x.is_finite() || !y.is_finite() {
        return Err(Error::OutOfRange(format!(
            "{kind} projection produced a non-finite result"
        )));
    }
    Ok((x, y))
}

pub(crate) fn ensure_finite_lon_lat(kind: &str, lon: f64, lat: f64) -> Result<(f64, f64)> {
    if !lon.is_finite() || !lat.is_finite() {
        return Err(Error::OutOfRange(format!(
            "{kind} inverse projection produced a non-finite result"
        )));
    }
    if lat.abs() > std::f64::consts::FRAC_PI_2 + LAT_EPSILON {
        return Err(Error::OutOfRange(format!(
            "{kind} inverse projection produced latitude {:.8}° outside [-90°, 90°]",
            lat.to_degrees()
        )));
    }
    Ok((normalize_longitude(lon), lat))
}

pub(crate) fn validate_angle(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() {
        return Err(Error::InvalidDefinition(format!("{name} must be finite")));
    }
    Ok(())
}

pub(crate) fn validate_latitude_param(name: &str, value: f64) -> Result<()> {
    validate_angle(name, value)?;
    if value.abs() > std::f64::consts::FRAC_PI_2 + LAT_EPSILON {
        return Err(Error::InvalidDefinition(format!(
            "{name} {:.8}° is outside [-90°, 90°]",
            value.to_degrees()
        )));
    }
    Ok(())
}

pub(crate) fn validate_scale(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        return Err(Error::InvalidDefinition(format!(
            "{name} must be a finite positive number"
        )));
    }
    Ok(())
}

pub(crate) fn validate_offset(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() {
        return Err(Error::InvalidDefinition(format!("{name} must be finite")));
    }
    Ok(())
}

pub(crate) fn normalize_longitude(lon: f64) -> f64 {
    let normalized =
        (lon + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI) - std::f64::consts::PI;

    if normalized == -std::f64::consts::PI && lon > 0.0 {
        std::f64::consts::PI
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_longitude_handles_huge_finite_values() {
        let normalized = normalize_longitude(1.0e20);

        assert!(normalized.is_finite());
        assert!(
            (-std::f64::consts::PI..=std::f64::consts::PI).contains(&normalized),
            "normalized longitude {normalized} outside [-pi, pi]"
        );
    }

    #[test]
    fn normalize_longitude_preserves_positive_pi_boundary() {
        assert_eq!(
            normalize_longitude(std::f64::consts::PI),
            std::f64::consts::PI
        );
        assert_eq!(
            normalize_longitude(3.0 * std::f64::consts::PI),
            std::f64::consts::PI
        );
        assert_eq!(
            normalize_longitude(-3.0 * std::f64::consts::PI),
            -std::f64::consts::PI
        );
    }

    #[test]
    fn converge_returns_fixed_point() {
        let result = converge("test", 0.5, 15, 1e-14, |x| (x + 2.0 / x) / 2.0).unwrap();
        assert!((result - std::f64::consts::SQRT_2).abs() < 1e-14);
    }

    #[test]
    fn converge_errors_on_exhaustion() {
        let error = converge("test iteration", 0.0, 15, 1e-14, |x| x + 1.0).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("test iteration did not converge after 15 iterations"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn converge_errors_when_iteration_produces_nan() {
        let error = converge("test iteration", 1.0, 15, 1e-14, |_| f64::NAN).unwrap_err();
        assert!(error.to_string().contains("did not converge"));
    }

    #[test]
    fn latitude_from_conformal_t_recovers_latitude() {
        // WGS84 first eccentricity
        let e = 0.081_819_190_842_622_f64;
        for lat_deg in [-80.0, -45.0, 0.0, 30.0, 60.0, 89.0] {
            let lat = f64::to_radians(lat_deg);
            let e_sin = e * lat.sin();
            let t = (std::f64::consts::FRAC_PI_4 - lat / 2.0).tan()
                / ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0);
            let recovered = latitude_from_conformal_t("test", t, e).unwrap();
            assert!(
                (recovered - lat).abs() < 1e-12,
                "lat {lat_deg}: recovered {}",
                recovered.to_degrees()
            );
        }
    }

    #[test]
    fn latitude_from_conformal_t_spherical_shortcut() {
        let lat = f64::to_radians(37.0);
        let t = (std::f64::consts::FRAC_PI_4 - lat / 2.0).tan();
        let recovered = latitude_from_conformal_t("test", t, 0.0).unwrap();
        assert!((recovered - lat).abs() < 1e-14);
    }
}

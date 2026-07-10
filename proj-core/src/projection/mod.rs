pub(crate) mod albers_equal_area;
pub(crate) mod cassini_soldner;
pub(crate) mod equidistant_cylindrical;
pub(crate) mod hotine_oblique_mercator;
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
            false_easting,
            false_northing,
        } => Ok(Projection::LambertConformalConic(
            lambert_conformal_conic::LambertConformalConic::new(
                datum.ellipsoid(),
                lon0.to_radians(),
                lat0.to_radians(),
                lat1.to_radians(),
                lat2.to_radians(),
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

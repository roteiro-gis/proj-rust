pub(crate) mod albers_equal_area;
pub(crate) mod equidistant_cylindrical;
pub(crate) mod lambert_conformal_conic;
pub(crate) mod mercator;
pub(crate) mod polar_stereographic;
pub(crate) mod transverse_mercator;
pub(crate) mod web_mercator;

use crate::crs::ProjectionMethod;
use crate::datum::Datum;
use crate::error::{Error, Result};

const LAT_EPSILON: f64 = 1e-12;

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
                datum.ellipsoid,
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
                datum.ellipsoid,
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
                datum.ellipsoid,
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
                datum.ellipsoid,
                lon0.to_radians(),
                lat0.to_radians(),
                lat1.to_radians(),
                lat2.to_radians(),
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
            datum.ellipsoid,
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
                datum.ellipsoid,
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

pub(crate) fn normalize_longitude(mut lon: f64) -> f64 {
    while lon > std::f64::consts::PI {
        lon -= 2.0 * std::f64::consts::PI;
    }
    while lon < -std::f64::consts::PI {
        lon += 2.0 * std::f64::consts::PI;
    }
    lon
}

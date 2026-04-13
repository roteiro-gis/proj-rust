use crate::datum::Datum;
use crate::error::{Error, Result};

/// A coordinate system's projected linear unit.
///
/// The stored value is the conversion factor from one native unit to meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearUnit {
    meters_per_unit: f64,
}

impl LinearUnit {
    /// Metre-based projected coordinates.
    pub const fn metre() -> Self {
        Self {
            meters_per_unit: 1.0,
        }
    }

    /// Alias for [`LinearUnit::metre`].
    pub const fn meter() -> Self {
        Self::metre()
    }

    /// Kilometer-based projected coordinates.
    pub const fn kilometre() -> Self {
        Self {
            meters_per_unit: 1000.0,
        }
    }

    /// Alias for [`LinearUnit::kilometre`].
    pub const fn kilometer() -> Self {
        Self::kilometre()
    }

    /// International foot-based projected coordinates.
    pub const fn foot() -> Self {
        Self {
            meters_per_unit: 0.3048,
        }
    }

    /// US survey foot-based projected coordinates.
    pub const fn us_survey_foot() -> Self {
        Self {
            meters_per_unit: 0.3048006096012192,
        }
    }

    /// Construct a custom projected linear unit from its meter conversion factor.
    pub fn from_meters_per_unit(meters_per_unit: f64) -> Result<Self> {
        if !meters_per_unit.is_finite() || meters_per_unit <= 0.0 {
            return Err(Error::InvalidDefinition(
                "linear unit conversion factor must be a finite positive number".into(),
            ));
        }

        Ok(Self { meters_per_unit })
    }

    /// Return the number of meters represented by one native projected unit.
    pub const fn meters_per_unit(self) -> f64 {
        self.meters_per_unit
    }

    /// Convert a native projected coordinate value into meters.
    pub const fn to_meters(self, value: f64) -> f64 {
        value * self.meters_per_unit
    }

    /// Convert a meter value into the native projected unit.
    pub const fn from_meters(self, value: f64) -> f64 {
        value / self.meters_per_unit
    }
}

/// A Coordinate Reference System definition.
#[derive(Debug, Clone, Copy)]
pub enum CrsDef {
    /// Geographic CRS (lon/lat in degrees).
    Geographic(GeographicCrsDef),
    /// Projected CRS (easting/northing in the CRS's native linear unit).
    Projected(ProjectedCrsDef),
}

impl CrsDef {
    /// Get the datum for this CRS.
    pub fn datum(&self) -> &Datum {
        match self {
            CrsDef::Geographic(g) => g.datum(),
            CrsDef::Projected(p) => p.datum(),
        }
    }

    /// Get the EPSG code for this CRS.
    pub fn epsg(&self) -> u32 {
        match self {
            CrsDef::Geographic(g) => g.epsg(),
            CrsDef::Projected(p) => p.epsg(),
        }
    }

    /// Get the CRS name.
    pub fn name(&self) -> &str {
        match self {
            CrsDef::Geographic(g) => g.name(),
            CrsDef::Projected(p) => p.name(),
        }
    }

    /// Returns true if this is a geographic CRS.
    pub fn is_geographic(&self) -> bool {
        matches!(self, CrsDef::Geographic(_))
    }

    /// Returns true if this is a projected CRS.
    pub fn is_projected(&self) -> bool {
        matches!(self, CrsDef::Projected(_))
    }

    /// Returns true when two CRS definitions map to the same internal semantics.
    pub fn semantically_equivalent(&self, other: &Self) -> bool {
        match (self, other) {
            (CrsDef::Geographic(a), CrsDef::Geographic(b)) => a.datum().same_datum(b.datum()),
            (CrsDef::Projected(a), CrsDef::Projected(b)) => {
                a.datum().same_datum(b.datum())
                    && approx_eq(a.linear_unit_to_meter(), b.linear_unit_to_meter())
                    && projection_methods_equivalent(&a.method(), &b.method())
            }
            _ => false,
        }
    }
}

/// Definition of a geographic CRS (longitude, latitude in degrees).
#[derive(Debug, Clone, Copy)]
pub struct GeographicCrsDef {
    epsg: u32,
    datum: Datum,
    name: &'static str,
}

impl GeographicCrsDef {
    pub const fn new(epsg: u32, datum: Datum, name: &'static str) -> Self {
        Self { epsg, datum, name }
    }

    pub const fn epsg(&self) -> u32 {
        self.epsg
    }

    pub const fn datum(&self) -> &Datum {
        &self.datum
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Definition of a projected CRS (easting, northing in the CRS's native linear unit).
#[derive(Debug, Clone, Copy)]
pub struct ProjectedCrsDef {
    epsg: u32,
    datum: Datum,
    method: ProjectionMethod,
    linear_unit: LinearUnit,
    name: &'static str,
}

impl ProjectedCrsDef {
    pub const fn new(
        epsg: u32,
        datum: Datum,
        method: ProjectionMethod,
        linear_unit: LinearUnit,
        name: &'static str,
    ) -> Self {
        Self {
            epsg,
            datum,
            method,
            linear_unit,
            name,
        }
    }

    pub const fn epsg(&self) -> u32 {
        self.epsg
    }

    pub const fn datum(&self) -> &Datum {
        &self.datum
    }

    pub const fn method(&self) -> ProjectionMethod {
        self.method
    }

    pub const fn linear_unit(&self) -> LinearUnit {
        self.linear_unit
    }

    pub const fn linear_unit_to_meter(&self) -> f64 {
        self.linear_unit.meters_per_unit()
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// All supported projection methods with their parameters.
///
/// Angle parameters are stored in **degrees**. Conversion to radians happens
/// at projection construction time (once), not per-transform.
#[derive(Debug, Clone, Copy)]
pub enum ProjectionMethod {
    /// Web Mercator (EPSG:3857) — spherical Mercator on WGS84 semi-major axis.
    WebMercator,

    /// Transverse Mercator (includes UTM zones).
    TransverseMercator {
        /// Central meridian (degrees).
        lon0: f64,
        /// Latitude of origin (degrees).
        lat0: f64,
        /// Scale factor on central meridian.
        k0: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Polar Stereographic.
    PolarStereographic {
        /// Central meridian / straight vertical longitude (degrees).
        lon0: f64,
        /// Latitude of true scale (degrees). Determines the hemisphere.
        lat_ts: f64,
        /// Scale factor (used when lat_ts = ±90°, otherwise derived from lat_ts).
        k0: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Lambert Conformal Conic (1SP or 2SP).
    LambertConformalConic {
        /// Central meridian (degrees).
        lon0: f64,
        /// Latitude of origin (degrees).
        lat0: f64,
        /// First standard parallel (degrees).
        lat1: f64,
        /// Second standard parallel (degrees). Set equal to lat1 for 1SP variant.
        lat2: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Albers Equal Area Conic.
    AlbersEqualArea {
        /// Central meridian (degrees).
        lon0: f64,
        /// Latitude of origin (degrees).
        lat0: f64,
        /// First standard parallel (degrees).
        lat1: f64,
        /// Second standard parallel (degrees).
        lat2: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Standard Mercator (ellipsoidal, distinct from Web Mercator).
    Mercator {
        /// Central meridian (degrees).
        lon0: f64,
        /// Latitude of true scale (degrees). 0 for 1SP variant.
        lat_ts: f64,
        /// Scale factor (for 1SP when lat_ts = 0).
        k0: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Equidistant Cylindrical / Plate Carrée.
    EquidistantCylindrical {
        /// Central meridian (degrees).
        lon0: f64,
        /// Latitude of true scale (degrees).
        lat_ts: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },
}

fn projection_methods_equivalent(a: &ProjectionMethod, b: &ProjectionMethod) -> bool {
    match (a, b) {
        (ProjectionMethod::WebMercator, ProjectionMethod::WebMercator) => true,
        (
            ProjectionMethod::TransverseMercator {
                lon0: a_lon0,
                lat0: a_lat0,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::TransverseMercator {
                lon0: b_lon0,
                lat0: b_lat0,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::PolarStereographic {
                lon0: a_lon0,
                lat_ts: a_lat_ts,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::PolarStereographic {
                lon0: b_lon0,
                lat_ts: b_lat_ts,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat_ts, *b_lat_ts)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::LambertConformalConic {
                lon0: a_lon0,
                lat0: a_lat0,
                lat1: a_lat1,
                lat2: a_lat2,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::LambertConformalConic {
                lon0: b_lon0,
                lat0: b_lat0,
                lat1: b_lat1,
                lat2: b_lat2,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_lat1, *b_lat1)
                && approx_eq(*a_lat2, *b_lat2)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::AlbersEqualArea {
                lon0: a_lon0,
                lat0: a_lat0,
                lat1: a_lat1,
                lat2: a_lat2,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::AlbersEqualArea {
                lon0: b_lon0,
                lat0: b_lat0,
                lat1: b_lat1,
                lat2: b_lat2,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_lat1, *b_lat1)
                && approx_eq(*a_lat2, *b_lat2)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::Mercator {
                lon0: a_lon0,
                lat_ts: a_lat_ts,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::Mercator {
                lon0: b_lon0,
                lat_ts: b_lat_ts,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat_ts, *b_lat_ts)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::EquidistantCylindrical {
                lon0: a_lon0,
                lat_ts: a_lat_ts,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::EquidistantCylindrical {
                lon0: b_lon0,
                lat_ts: b_lat_ts,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat_ts, *b_lat_ts)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        _ => false,
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum;

    #[test]
    fn geographic_crs_is_geographic() {
        let crs = CrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
        assert!(crs.is_geographic());
        assert!(!crs.is_projected());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn projected_crs_is_projected() {
        let crs = CrsDef::Projected(ProjectedCrsDef::new(
            3857,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "WGS 84 / Pseudo-Mercator",
        ));
        assert!(crs.is_projected());
        assert!(!crs.is_geographic());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn linear_unit_validates_positive_finite_conversion() {
        assert!(LinearUnit::from_meters_per_unit(0.3048).is_ok());
        assert!(LinearUnit::from_meters_per_unit(0.0).is_err());
        assert!(LinearUnit::from_meters_per_unit(f64::NAN).is_err());
    }
}

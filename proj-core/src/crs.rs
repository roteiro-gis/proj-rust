use crate::datum::Datum;

/// A Coordinate Reference System definition.
#[derive(Debug, Clone, Copy)]
pub enum CrsDef {
    /// Geographic CRS (lon/lat in degrees).
    Geographic(GeographicCrsDef),
    /// Projected CRS (easting/northing in meters).
    Projected(ProjectedCrsDef),
}

impl CrsDef {
    /// Get the datum for this CRS.
    pub fn datum(&self) -> &Datum {
        match self {
            CrsDef::Geographic(g) => &g.datum,
            CrsDef::Projected(p) => &p.datum,
        }
    }

    /// Get the EPSG code for this CRS.
    pub fn epsg(&self) -> u32 {
        match self {
            CrsDef::Geographic(g) => g.epsg,
            CrsDef::Projected(p) => p.epsg,
        }
    }

    /// Get the CRS name.
    pub fn name(&self) -> &str {
        match self {
            CrsDef::Geographic(g) => g.name,
            CrsDef::Projected(p) => p.name,
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
}

/// Definition of a geographic CRS (longitude, latitude in degrees).
#[derive(Debug, Clone, Copy)]
pub struct GeographicCrsDef {
    /// EPSG code.
    pub epsg: u32,
    /// Geodetic datum.
    pub datum: Datum,
    /// Human-readable name.
    pub name: &'static str,
}

/// Definition of a projected CRS (easting, northing in meters).
#[derive(Debug, Clone, Copy)]
pub struct ProjectedCrsDef {
    /// EPSG code.
    pub epsg: u32,
    /// Geodetic datum.
    pub datum: Datum,
    /// Projection method and parameters.
    pub method: ProjectionMethod,
    /// Human-readable name.
    pub name: &'static str,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum;

    #[test]
    fn geographic_crs_is_geographic() {
        let crs = CrsDef::Geographic(GeographicCrsDef {
            epsg: 4326,
            datum: datum::WGS84,
            name: "WGS 84",
        });
        assert!(crs.is_geographic());
        assert!(!crs.is_projected());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn projected_crs_is_projected() {
        let crs = CrsDef::Projected(ProjectedCrsDef {
            epsg: 3857,
            datum: datum::WGS84,
            method: ProjectionMethod::WebMercator,
            name: "WGS 84 / Pseudo-Mercator",
        });
        assert!(crs.is_projected());
        assert!(!crs.is_geographic());
        assert_eq!(crs.epsg(), 3857);
    }
}

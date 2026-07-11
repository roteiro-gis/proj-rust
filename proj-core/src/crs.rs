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
#[derive(Debug, Clone)]
pub enum CrsDef {
    /// Geographic CRS (lon/lat in degrees).
    Geographic(GeographicCrsDef),
    /// Projected CRS (easting/northing in the CRS's native linear unit).
    Projected(ProjectedCrsDef),
    /// Compound horizontal + vertical CRS.
    Compound(Box<CompoundCrsDef>),
}

impl CrsDef {
    /// Get the horizontal datum for this CRS.
    pub fn datum(&self) -> &Datum {
        match self {
            CrsDef::Geographic(g) => g.datum(),
            CrsDef::Projected(p) => p.datum(),
            CrsDef::Compound(c) => c.horizontal_datum(),
        }
    }

    /// Get the EPSG code for this CRS.
    pub fn epsg(&self) -> u32 {
        match self {
            CrsDef::Geographic(g) => g.epsg(),
            CrsDef::Projected(p) => p.epsg(),
            CrsDef::Compound(c) => c.epsg(),
        }
    }

    /// Get the CRS name.
    pub fn name(&self) -> &str {
        match self {
            CrsDef::Geographic(g) => g.name(),
            CrsDef::Projected(p) => p.name(),
            CrsDef::Compound(c) => c.name(),
        }
    }

    /// Returns true if this CRS's horizontal component is geographic.
    pub fn is_geographic(&self) -> bool {
        self.as_geographic().is_some()
    }

    /// Returns true if this CRS's horizontal component is projected.
    pub fn is_projected(&self) -> bool {
        self.as_projected().is_some()
    }

    /// Returns true if this is a compound horizontal + vertical CRS.
    pub fn is_compound(&self) -> bool {
        matches!(self, CrsDef::Compound(_))
    }

    /// Return the geographic horizontal component, when present.
    pub fn as_geographic(&self) -> Option<&GeographicCrsDef> {
        match self {
            CrsDef::Geographic(g) => Some(g),
            CrsDef::Projected(_) => None,
            CrsDef::Compound(c) => c.as_geographic(),
        }
    }

    /// Return the projected horizontal component, when present.
    pub fn as_projected(&self) -> Option<&ProjectedCrsDef> {
        match self {
            CrsDef::Geographic(_) => None,
            CrsDef::Projected(p) => Some(p),
            CrsDef::Compound(c) => c.as_projected(),
        }
    }

    /// Return the explicit vertical CRS component, when this is compound.
    pub fn vertical_crs(&self) -> Option<&VerticalCrsDef> {
        match self {
            CrsDef::Compound(c) => Some(c.vertical_crs()),
            CrsDef::Geographic(_) | CrsDef::Projected(_) => None,
        }
    }

    /// Return this CRS's horizontal component as a standalone CRS definition.
    ///
    /// This intentionally drops an explicit vertical component. Use it only for
    /// horizontal-only workflows such as AOI filtering, footprint reprojection,
    /// and 2D previews where `z` is outside the operation contract.
    pub fn horizontal_crs(&self) -> Option<CrsDef> {
        match self {
            CrsDef::Geographic(_) | CrsDef::Projected(_) => Some(self.clone()),
            CrsDef::Compound(c) => Some(c.horizontal().to_crs_def()),
        }
    }

    /// Returns the geographic CRS EPSG code used for operation selection, when known.
    pub fn base_geographic_crs_epsg(&self) -> Option<u32> {
        match self {
            CrsDef::Geographic(g) if g.epsg() != 0 => Some(g.epsg()),
            CrsDef::Projected(p) if p.base_geographic_crs_epsg() != 0 => {
                Some(p.base_geographic_crs_epsg())
            }
            CrsDef::Compound(c) => c.base_geographic_crs_epsg(),
            _ => None,
        }
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
            (CrsDef::Compound(a), CrsDef::Compound(b)) => a.semantically_equivalent(b),
            _ => false,
        }
    }
}

/// Definition of a geographic CRS (longitude, latitude in degrees).
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct ProjectedCrsDef {
    epsg: u32,
    base_geographic_crs_epsg: u32,
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
        Self::new_with_base_geographic_crs(epsg, 0, datum, method, linear_unit, name)
    }

    pub const fn new_with_base_geographic_crs(
        epsg: u32,
        base_geographic_crs_epsg: u32,
        datum: Datum,
        method: ProjectionMethod,
        linear_unit: LinearUnit,
        name: &'static str,
    ) -> Self {
        Self {
            epsg,
            base_geographic_crs_epsg,
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

    pub const fn base_geographic_crs_epsg(&self) -> u32 {
        self.base_geographic_crs_epsg
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

/// A compound CRS made from one horizontal CRS and one vertical CRS.
#[derive(Debug, Clone)]
pub struct CompoundCrsDef {
    epsg: u32,
    horizontal: HorizontalCrsDef,
    vertical: VerticalCrsDef,
    name: &'static str,
}

impl CompoundCrsDef {
    pub fn new(
        epsg: u32,
        horizontal: HorizontalCrsDef,
        vertical: VerticalCrsDef,
        name: &'static str,
    ) -> Self {
        Self {
            epsg,
            horizontal,
            vertical,
            name,
        }
    }

    pub fn from_crs_def(
        epsg: u32,
        horizontal: CrsDef,
        vertical: VerticalCrsDef,
        name: &'static str,
    ) -> Result<Self> {
        let horizontal = HorizontalCrsDef::try_from(horizontal)?;
        Ok(Self::new(epsg, horizontal, vertical, name))
    }

    pub const fn epsg(&self) -> u32 {
        self.epsg
    }

    pub const fn horizontal(&self) -> &HorizontalCrsDef {
        &self.horizontal
    }

    pub const fn vertical_crs(&self) -> &VerticalCrsDef {
        &self.vertical
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub fn as_geographic(&self) -> Option<&GeographicCrsDef> {
        self.horizontal.as_geographic()
    }

    pub fn as_projected(&self) -> Option<&ProjectedCrsDef> {
        self.horizontal.as_projected()
    }

    pub fn horizontal_datum(&self) -> &Datum {
        self.horizontal.datum()
    }

    pub fn base_geographic_crs_epsg(&self) -> Option<u32> {
        self.horizontal.base_geographic_crs_epsg()
    }

    pub fn semantically_equivalent(&self, other: &Self) -> bool {
        self.horizontal.semantically_equivalent(&other.horizontal)
            && self.vertical.semantically_equivalent(&other.vertical)
    }
}

/// Horizontal component of a compound CRS.
#[derive(Debug, Clone)]
pub enum HorizontalCrsDef {
    Geographic(GeographicCrsDef),
    Projected(ProjectedCrsDef),
}

impl HorizontalCrsDef {
    pub fn datum(&self) -> &Datum {
        match self {
            Self::Geographic(g) => g.datum(),
            Self::Projected(p) => p.datum(),
        }
    }

    pub fn epsg(&self) -> u32 {
        match self {
            Self::Geographic(g) => g.epsg(),
            Self::Projected(p) => p.epsg(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Geographic(g) => g.name(),
            Self::Projected(p) => p.name(),
        }
    }

    pub fn as_geographic(&self) -> Option<&GeographicCrsDef> {
        match self {
            Self::Geographic(g) => Some(g),
            Self::Projected(_) => None,
        }
    }

    pub fn as_projected(&self) -> Option<&ProjectedCrsDef> {
        match self {
            Self::Geographic(_) => None,
            Self::Projected(p) => Some(p),
        }
    }

    pub fn base_geographic_crs_epsg(&self) -> Option<u32> {
        match self {
            Self::Geographic(g) if g.epsg() != 0 => Some(g.epsg()),
            Self::Projected(p) if p.base_geographic_crs_epsg() != 0 => {
                Some(p.base_geographic_crs_epsg())
            }
            _ => None,
        }
    }

    pub fn semantically_equivalent(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Geographic(a), Self::Geographic(b)) => a.datum().same_datum(b.datum()),
            (Self::Projected(a), Self::Projected(b)) => {
                a.datum().same_datum(b.datum())
                    && approx_eq(a.linear_unit_to_meter(), b.linear_unit_to_meter())
                    && projection_methods_equivalent(&a.method(), &b.method())
            }
            _ => false,
        }
    }

    pub fn to_crs_def(&self) -> CrsDef {
        match self {
            Self::Geographic(g) => CrsDef::Geographic(g.clone()),
            Self::Projected(p) => CrsDef::Projected(p.clone()),
        }
    }
}

impl TryFrom<CrsDef> for HorizontalCrsDef {
    type Error = Error;

    fn try_from(value: CrsDef) -> Result<Self> {
        match value {
            CrsDef::Geographic(g) => Ok(Self::Geographic(g)),
            CrsDef::Projected(p) => Ok(Self::Projected(p)),
            CrsDef::Compound(_) => Err(Error::InvalidDefinition(
                "compound CRS horizontal component cannot itself be compound".into(),
            )),
        }
    }
}

impl From<GeographicCrsDef> for HorizontalCrsDef {
    fn from(value: GeographicCrsDef) -> Self {
        Self::Geographic(value)
    }
}

impl From<ProjectedCrsDef> for HorizontalCrsDef {
    fn from(value: ProjectedCrsDef) -> Self {
        Self::Projected(value)
    }
}

/// Definition of an explicit vertical CRS component.
#[derive(Debug, Clone)]
pub struct VerticalCrsDef {
    epsg: u32,
    kind: VerticalCrsKind,
    linear_unit: LinearUnit,
    name: &'static str,
}

impl VerticalCrsDef {
    /// Construct an ellipsoidal-height vertical CRS tied to a geodetic datum.
    pub fn ellipsoidal_height(
        epsg: u32,
        datum: Datum,
        linear_unit: LinearUnit,
        name: &'static str,
    ) -> Self {
        Self {
            epsg,
            kind: VerticalCrsKind::EllipsoidalHeight {
                datum: Box::new(datum),
            },
            linear_unit,
            name,
        }
    }

    /// Construct a gravity-related vertical CRS by vertical datum EPSG code.
    pub fn gravity_related_height(
        epsg: u32,
        vertical_datum_epsg: u32,
        linear_unit: LinearUnit,
        name: &'static str,
    ) -> Result<Self> {
        if vertical_datum_epsg == 0 {
            return Err(Error::InvalidDefinition(
                "gravity-related vertical CRS requires a vertical datum EPSG code".into(),
            ));
        }

        Ok(Self {
            epsg,
            kind: VerticalCrsKind::GravityRelatedHeight {
                vertical_datum_epsg,
            },
            linear_unit,
            name,
        })
    }

    pub const fn epsg(&self) -> u32 {
        self.epsg
    }

    pub const fn kind(&self) -> &VerticalCrsKind {
        &self.kind
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

    pub fn semantically_equivalent(&self, other: &Self) -> bool {
        approx_eq(self.linear_unit_to_meter(), other.linear_unit_to_meter())
            && self.kind.semantically_equivalent(&other.kind)
    }

    /// Returns true when two vertical CRS definitions use the same vertical
    /// reference frame, ignoring the coordinate unit.
    pub fn same_vertical_reference(&self, other: &Self) -> bool {
        self.kind.semantically_equivalent(&other.kind)
    }

    pub fn vertical_datum_epsg(&self) -> Option<u32> {
        self.kind.vertical_datum_epsg()
    }
}

/// Supported vertical CRS kinds.
#[derive(Debug, Clone)]
pub enum VerticalCrsKind {
    /// Height above the ellipsoid of the referenced geodetic datum.
    EllipsoidalHeight { datum: Box<Datum> },
    /// Height relative to a gravity-related vertical datum.
    GravityRelatedHeight { vertical_datum_epsg: u32 },
}

impl VerticalCrsKind {
    pub fn semantically_equivalent(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::EllipsoidalHeight { datum: a }, Self::EllipsoidalHeight { datum: b }) => {
                a.same_datum(b)
            }
            (
                Self::GravityRelatedHeight {
                    vertical_datum_epsg: a,
                },
                Self::GravityRelatedHeight {
                    vertical_datum_epsg: b,
                },
            ) => a == b,
            _ => false,
        }
    }

    pub const fn vertical_datum_epsg(&self) -> Option<u32> {
        match self {
            Self::EllipsoidalHeight { .. } => None,
            Self::GravityRelatedHeight {
                vertical_datum_epsg,
            } => Some(*vertical_datum_epsg),
        }
    }

    pub const fn is_ellipsoidal_height(&self) -> bool {
        matches!(self, Self::EllipsoidalHeight { .. })
    }

    pub const fn is_gravity_related_height(&self) -> bool {
        matches!(self, Self::GravityRelatedHeight { .. })
    }
}

/// All supported projection methods with their parameters.
///
/// Angle parameters are stored in **degrees**. Conversion to radians happens
/// at projection construction time (once), not per-transform.
#[derive(Debug, Clone, Copy, PartialEq)]
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

    /// Lambert Azimuthal Equal Area.
    LambertAzimuthalEqualArea {
        /// Longitude of natural origin (degrees).
        lon0: f64,
        /// Latitude of natural origin (degrees).
        lat0: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Lambert Azimuthal Equal Area (spherical).
    LambertAzimuthalEqualAreaSpherical {
        /// Longitude of natural origin (degrees).
        lon0: f64,
        /// Latitude of natural origin (degrees).
        lat0: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// EPSG Oblique Stereographic (Roussilhe / double stereographic).
    ObliqueStereographic {
        /// Longitude of natural origin (degrees).
        lon0: f64,
        /// Latitude of natural origin (degrees).
        lat0: f64,
        /// Scale factor at natural origin.
        k0: f64,
        /// False easting (meters).
        false_easting: f64,
        /// False northing (meters).
        false_northing: f64,
    },

    /// Hotine Oblique Mercator / Rectified Skew Orthomorphic.
    HotineObliqueMercator {
        /// Latitude of projection centre (degrees).
        latc: f64,
        /// Longitude of projection centre (degrees).
        lonc: f64,
        /// Azimuth of central line at projection centre (degrees clockwise from north).
        azimuth: f64,
        /// Angle from rectified to skew grid (degrees).
        rectified_grid_angle: f64,
        /// Scale factor at projection centre.
        k0: f64,
        /// False easting or easting at projection centre (meters).
        false_easting: f64,
        /// False northing or northing at projection centre (meters).
        false_northing: f64,
        /// EPSG variant B offsets the natural origin to the projection centre.
        variant_b: bool,
    },

    /// Cassini-Soldner.
    CassiniSoldner {
        /// Longitude of natural origin (degrees).
        lon0: f64,
        /// Latitude of natural origin (degrees).
        lat0: f64,
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
    /// Colombia Urban (EPSG method 1052): plane projection at the elevation
    /// of the mapped city. `h0` is the projection plane origin height in
    /// meters.
    ColombiaUrban {
        lon0: f64,
        lat0: f64,
        h0: f64,
        false_easting: f64,
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
            ProjectionMethod::LambertAzimuthalEqualArea {
                lon0: a_lon0,
                lat0: a_lat0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::LambertAzimuthalEqualArea {
                lon0: b_lon0,
                lat0: b_lat0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::LambertAzimuthalEqualAreaSpherical {
                lon0: a_lon0,
                lat0: a_lat0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::LambertAzimuthalEqualAreaSpherical {
                lon0: b_lon0,
                lat0: b_lat0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::ObliqueStereographic {
                lon0: a_lon0,
                lat0: a_lat0,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::ObliqueStereographic {
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
            ProjectionMethod::HotineObliqueMercator {
                latc: a_latc,
                lonc: a_lonc,
                azimuth: a_azimuth,
                rectified_grid_angle: a_rectified_grid_angle,
                k0: a_k0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
                variant_b: a_variant_b,
            },
            ProjectionMethod::HotineObliqueMercator {
                latc: b_latc,
                lonc: b_lonc,
                azimuth: b_azimuth,
                rectified_grid_angle: b_rectified_grid_angle,
                k0: b_k0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
                variant_b: b_variant_b,
            },
        ) => {
            a_variant_b == b_variant_b
                && approx_eq(*a_latc, *b_latc)
                && approx_eq(*a_lonc, *b_lonc)
                && approx_eq(*a_azimuth, *b_azimuth)
                && approx_eq(*a_rectified_grid_angle, *b_rectified_grid_angle)
                && approx_eq(*a_k0, *b_k0)
                && approx_eq(*a_false_easting, *b_false_easting)
                && approx_eq(*a_false_northing, *b_false_northing)
        }
        (
            ProjectionMethod::CassiniSoldner {
                lon0: a_lon0,
                lat0: a_lat0,
                false_easting: a_false_easting,
                false_northing: a_false_northing,
            },
            ProjectionMethod::CassiniSoldner {
                lon0: b_lon0,
                lat0: b_lat0,
                false_easting: b_false_easting,
                false_northing: b_false_northing,
            },
        ) => {
            approx_eq(*a_lon0, *b_lon0)
                && approx_eq(*a_lat0, *b_lat0)
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
    fn compound_crs_exposes_horizontal_and_vertical_components() {
        let horizontal = GeographicCrsDef::new(4326, datum::WGS84, "WGS 84");
        let vertical = VerticalCrsDef::ellipsoidal_height(
            0,
            datum::WGS84,
            LinearUnit::metre(),
            "WGS 84 ellipsoidal height",
        );
        let crs = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            4979,
            HorizontalCrsDef::Geographic(horizontal),
            vertical,
            "WGS 84",
        )));

        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert!(!crs.is_projected());
        assert_eq!(crs.epsg(), 4979);
        assert_eq!(crs.base_geographic_crs_epsg(), Some(4326));
        assert!(crs.vertical_crs().is_some());
    }

    #[test]
    fn linear_unit_validates_positive_finite_conversion() {
        assert!(LinearUnit::from_meters_per_unit(0.3048).is_ok());
        assert!(LinearUnit::from_meters_per_unit(0.0).is_err());
        assert!(LinearUnit::from_meters_per_unit(f64::NAN).is_err());
    }
}

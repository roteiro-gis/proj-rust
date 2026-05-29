use crate::{ParseError, Result};
use proj_core::{Datum, LinearUnit, VerticalCrsDef};

const SEMANTICS_EPSILON: f64 = 1e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AxisDirection {
    East,
    North,
    West,
    South,
    Up,
    Down,
    Other,
}

impl AxisDirection {
    pub(crate) fn from_str(value: &str) -> Self {
        match normalize_key(value).as_str() {
            "east" => Self::East,
            "north" => Self::North,
            "west" => Self::West,
            "south" => Self::South,
            "up" => Self::Up,
            "down" => Self::Down,
            _ => Self::Other,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::East => "east",
            Self::North => "north",
            Self::West => "west",
            Self::South => "south",
            Self::Up => "up",
            Self::Down => "down",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CoordinateSystemSpec {
    pub subtype: Option<String>,
    pub dimension: Option<usize>,
    pub axes: Vec<AxisDirection>,
    pub axis_linear_units: Vec<Option<LinearUnit>>,
    pub axis_angle_unit_to_degree: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeographicCoordinateSystemKind {
    TwoDimensional,
    ThreeDimensionalEllipsoidalHeight,
}

pub(crate) fn validate_supported_geographic_semantics(
    context: &str,
    angle_unit_to_degree: Option<f64>,
    prime_meridian_degrees: Option<f64>,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<()> {
    if let Some(angle_unit_to_degree) = angle_unit_to_degree {
        if !approx_eq(angle_unit_to_degree, 1.0) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses angular units other than degrees"
            )));
        }
    }

    if let Some(prime_meridian_degrees) = prime_meridian_degrees {
        if !approx_eq(prime_meridian_degrees, 0.0) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses a non-Greenwich prime meridian"
            )));
        }
    }

    validate_coordinate_system(
        context,
        coordinate_system,
        Some("ellipsoidal"),
        &[AxisDirection::East, AxisDirection::North],
        "longitude/east, latitude/north",
    )
}

pub(crate) fn validate_supported_geographic_or_ellipsoidal_height_semantics(
    context: &str,
    angle_unit_to_degree: Option<f64>,
    prime_meridian_degrees: Option<f64>,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<GeographicCoordinateSystemKind> {
    if let Some(angle_unit_to_degree) = angle_unit_to_degree {
        if !approx_eq(angle_unit_to_degree, 1.0) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses angular units other than degrees"
            )));
        }
    }

    if let Some(prime_meridian_degrees) = prime_meridian_degrees {
        if !approx_eq(prime_meridian_degrees, 0.0) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses a non-Greenwich prime meridian"
            )));
        }
    }

    let inferred_dimension = coordinate_system
        .dimension
        .or_else(|| (!coordinate_system.axes.is_empty()).then_some(coordinate_system.axes.len()));

    if inferred_dimension == Some(3) {
        if coordinate_system.axes.is_empty() {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} declares a 3D coordinate system without explicit axis directions"
            )));
        }
        validate_coordinate_system(
            context,
            coordinate_system,
            Some("ellipsoidal"),
            &[AxisDirection::East, AxisDirection::North, AxisDirection::Up],
            "longitude/east, latitude/north, ellipsoidal height/up",
        )?;
        return Ok(GeographicCoordinateSystemKind::ThreeDimensionalEllipsoidalHeight);
    }

    validate_coordinate_system(
        context,
        coordinate_system,
        Some("ellipsoidal"),
        &[AxisDirection::East, AxisDirection::North],
        "longitude/east, latitude/north",
    )?;
    Ok(GeographicCoordinateSystemKind::TwoDimensional)
}

pub(crate) fn validate_supported_projected_semantics(
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<()> {
    validate_coordinate_system(
        context,
        coordinate_system,
        Some("cartesian"),
        &[AxisDirection::East, AxisDirection::North],
        "easting/east, northing/north",
    )
}

pub(crate) fn validate_supported_vertical_coordinate_system(
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<()> {
    if let Some(subtype) = &coordinate_system.subtype {
        if normalize_key(subtype) != "vertical" {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses unsupported coordinate system subtype `{subtype}`"
            )));
        }
    }

    if let Some(dimension) = coordinate_system.dimension {
        if dimension != 1 {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses {dimension} axes, but only 1D vertical coordinate systems are supported"
            )));
        }
    }

    if !coordinate_system.axes.is_empty() && coordinate_system.axes != [AxisDirection::Up] {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} uses unsupported vertical axis direction; expected up"
        )));
    }

    Ok(())
}

pub(crate) fn validate_vertical_unit_matches_authority(
    context: &str,
    declared_unit: LinearUnit,
    canonical: &VerticalCrsDef,
) -> Result<()> {
    let declared = declared_unit.meters_per_unit();
    let expected = canonical.linear_unit_to_meter();
    if (declared - expected).abs() <= 1e-12 * declared.abs().max(expected.abs()).max(1.0) {
        return Ok(());
    }

    Err(ParseError::UnsupportedSemantics(format!(
        "{context} declares a vertical unit that conflicts with EPSG:{}",
        canonical.epsg()
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DatumAliasScope {
    Wkt,
    ProjJson,
}

#[derive(Debug)]
pub(crate) struct StructuredEllipsoid {
    pub(crate) epsg: Option<u32>,
    pub(crate) name: String,
    pub(crate) semi_major_axis: f64,
    pub(crate) inverse_flattening: f64,
}

struct DatumCandidate {
    common_datum_aliases: &'static [&'static str],
    projjson_datum_aliases: &'static [&'static str],
    ellipsoid_aliases: &'static [&'static str],
    datum: Datum,
    ellipsoid_epsg: Option<u32>,
}

pub(crate) fn resolve_structured_datum(
    scope: DatumAliasScope,
    datum_name: &str,
    ellipsoid: &StructuredEllipsoid,
) -> Option<Datum> {
    datum_candidates().iter().find_map(|candidate| {
        (datum_alias_matches(candidate, scope, datum_name)
            && ellipsoid_matches(
                ellipsoid,
                &candidate.datum,
                candidate.ellipsoid_aliases,
                candidate.ellipsoid_epsg,
            ))
        .then_some(candidate.datum.clone())
    })
}

pub(crate) fn resolve_named_datum(scope: DatumAliasScope, datum_name: &str) -> Option<Datum> {
    datum_candidates().iter().find_map(|candidate| {
        datum_alias_matches(candidate, scope, datum_name).then_some(candidate.datum.clone())
    })
}

fn datum_alias_matches(
    candidate: &DatumCandidate,
    scope: DatumAliasScope,
    datum_name: &str,
) -> bool {
    candidate.common_datum_aliases.contains(&datum_name)
        || (scope == DatumAliasScope::ProjJson
            && candidate.projjson_datum_aliases.contains(&datum_name))
}

fn datum_candidates() -> [DatumCandidate; 8] {
    [
        DatumCandidate {
            common_datum_aliases: &["wgs84", "wgs1984", "worldgeodeticsystem1984"],
            projjson_datum_aliases: &["worldgeodeticsystem1984ensemble"],
            ellipsoid_aliases: &["wgs84"],
            datum: proj_core::datum::WGS84,
            ellipsoid_epsg: Some(7030),
        },
        DatumCandidate {
            common_datum_aliases: &["northamericandatum1983", "nad83"],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["grs1980", "grs80"],
            datum: proj_core::datum::NAD83,
            ellipsoid_epsg: Some(7019),
        },
        DatumCandidate {
            common_datum_aliases: &["northamericandatum1927", "nad27"],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["clarke1866", "clrk66"],
            datum: proj_core::datum::NAD27,
            ellipsoid_epsg: Some(7008),
        },
        DatumCandidate {
            common_datum_aliases: &[
                "europeanterrestrialreferencesystem1989ensemble",
                "europeanterrestrialreferencesystem1989",
                "etrs89",
            ],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["grs1980", "grs80"],
            datum: proj_core::datum::ETRS89,
            ellipsoid_epsg: Some(7019),
        },
        DatumCandidate {
            common_datum_aliases: &["ordnancesurveyofgreatbritain1936", "osgb36"],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["airy1830", "airy"],
            datum: proj_core::datum::OSGB36,
            ellipsoid_epsg: Some(7001),
        },
        DatumCandidate {
            common_datum_aliases: &["europeandatum1950", "ed50"],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["international1924", "intl1924", "intl"],
            datum: proj_core::datum::ED50,
            ellipsoid_epsg: Some(7022),
        },
        DatumCandidate {
            common_datum_aliases: &["pulkovo1942", "pulkovo1942(58)"],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["krassowsky1940", "krassowsky", "krass"],
            datum: proj_core::datum::PULKOVO1942,
            ellipsoid_epsg: Some(7024),
        },
        DatumCandidate {
            common_datum_aliases: &["tokyo", "tokyodatum"],
            projjson_datum_aliases: &[],
            ellipsoid_aliases: &["bessel1841", "bessel"],
            datum: proj_core::datum::TOKYO,
            ellipsoid_epsg: Some(7004),
        },
    ]
}

fn ellipsoid_matches(
    actual: &StructuredEllipsoid,
    datum: &Datum,
    aliases: &[&str],
    epsg: Option<u32>,
) -> bool {
    let expected_rf = if datum.ellipsoid().flattening() == 0.0 {
        0.0
    } else {
        1.0 / datum.ellipsoid().flattening()
    };

    epsg.is_some_and(|expected| actual.epsg == Some(expected))
        || (aliases.iter().any(|alias| *alias == actual.name)
            && (actual.semi_major_axis - datum.ellipsoid().semi_major_axis()).abs() < 1e-9
            && (actual.inverse_flattening - expected_rf).abs() < 1e-9)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionParameterUnitKind {
    Angle,
    Length,
    Scale,
    Other,
}

pub(crate) fn projection_parameter_unit_kind(normalized_name: &str) -> ProjectionParameterUnitKind {
    match normalized_name {
        "centralmeridian"
        | "longitudeofcenter"
        | "longitudeofcentre"
        | "longitudeofprojectioncenter"
        | "longitudeofprojectioncentre"
        | "longitudeofnaturalorigin"
        | "longitudeoffalseorigin"
        | "longitudeoforigin"
        | "latitudeoforigin"
        | "latitudeofcenter"
        | "latitudeofcentre"
        | "latitudeofprojectioncenter"
        | "latitudeofprojectioncentre"
        | "latitudeofnaturalorigin"
        | "latitudeoffalseorigin"
        | "azimuth"
        | "azimuthinitialline"
        | "azimuthofinitialline"
        | "azimuthatprojectioncenter"
        | "azimuthatprojectioncentre"
        | "rectifiedgridangle"
        | "anglefromrectifiedtoskewgrid"
        | "standardparallel"
        | "standardparallel1"
        | "standardparallel2"
        | "latitudeofstandardparallel"
        | "latitudeof1ststandardparallel"
        | "latitudeof2ndstandardparallel" => ProjectionParameterUnitKind::Angle,
        "falseeasting"
        | "falsenorthing"
        | "eastingatfalseorigin"
        | "northingatfalseorigin"
        | "eastingatprojectioncenter"
        | "eastingatprojectioncentre"
        | "northingatprojectioncenter"
        | "northingatprojectioncentre" => ProjectionParameterUnitKind::Length,
        "scalefactor"
        | "scalefactoratnaturalorigin"
        | "scalefactoratprojectionorigin"
        | "scalefactoratprojectioncenter"
        | "scalefactoratprojectioncentre" => ProjectionParameterUnitKind::Scale,
        _ => ProjectionParameterUnitKind::Other,
    }
}

pub(crate) fn linear_unit_from_meters_per_unit(factor: f64) -> Option<LinearUnit> {
    LinearUnit::from_meters_per_unit(factor).ok()
}

pub(crate) fn linear_unit_name(name: &str) -> Option<LinearUnit> {
    match normalize_key(name).as_str() {
        "metre" | "meter" => Some(LinearUnit::metre()),
        "kilometre" | "kilometer" => Some(LinearUnit::kilometre()),
        "foot" | "internationalfoot" | "ft" => Some(LinearUnit::foot()),
        "ussurveyfoot" | "usfoot" | "usft" => Some(LinearUnit::us_survey_foot()),
        "yard" => linear_unit_from_meters_per_unit(0.9144),
        "nauticalmile" => linear_unit_from_meters_per_unit(1852.0),
        _ => None,
    }
}

pub(crate) fn angle_unit_name_to_degree(name: &str) -> Option<f64> {
    match normalize_key(name).as_str() {
        "degree" => Some(1.0),
        "radian" => Some(radians_to_degrees_factor(1.0)),
        "grad" | "gon" => Some(0.9),
        _ => None,
    }
}

pub(crate) fn radians_to_degrees_factor(radians_per_unit: f64) -> f64 {
    radians_per_unit.to_degrees()
}

pub(crate) fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < SEMANTICS_EPSILON
}

pub(crate) fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn validate_coordinate_system(
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
    expected_subtype: Option<&str>,
    expected_axes: &[AxisDirection],
    expected_axes_description: &str,
) -> Result<()> {
    if let Some(expected_subtype) = expected_subtype {
        if let Some(subtype) = &coordinate_system.subtype {
            if normalize_key(subtype) != normalize_key(expected_subtype) {
                return Err(ParseError::UnsupportedSemantics(format!(
                    "{context} uses unsupported coordinate system subtype `{subtype}`"
                )));
            }
        }
    }

    if let Some(dimension) = coordinate_system.dimension {
        if dimension != expected_axes.len() {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses {dimension} axes, but only 2D coordinate systems are supported"
            )));
        }
    }

    if coordinate_system.axes.is_empty() {
        return Ok(());
    }

    if coordinate_system.axes.len() != expected_axes.len() {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} defines {} explicit axes, but only {expected_axes_description} is supported",
            coordinate_system.axes.len()
        )));
    }

    if coordinate_system.axes != expected_axes {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} uses unsupported axis order/directions `{}`; expected {expected_axes_description}",
            format_axes(&coordinate_system.axes)
        )));
    }

    Ok(())
}

fn format_axes(axes: &[AxisDirection]) -> String {
    axes.iter()
        .map(|axis| axis.description())
        .collect::<Vec<_>>()
        .join(", ")
}

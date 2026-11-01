use crate::{ParseError, Result};
use proj_core::{Datum, HelmertParams, LinearUnit, VerticalCrsDef};

const SEMANTICS_EPSILON: f64 = 1e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AxisDirection {
    East,
    North,
    West,
    South,
    Up,
    Down,
    GeocentricX,
    GeocentricY,
    GeocentricZ,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AxisOrderPolicy {
    Strict,
    NormalizePermutation,
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
            "geocentricx" => Self::GeocentricX,
            "geocentricy" => Self::GeocentricY,
            "geocentricz" => Self::GeocentricZ,
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
            Self::GeocentricX => "geocentricX",
            Self::GeocentricY => "geocentricY",
            Self::GeocentricZ => "geocentricZ",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrsTextFormat {
    Wkt1,
    Wkt2,
    ProjJson,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CoordinateSystemSpec {
    pub subtype: Option<String>,
    pub dimension: Option<usize>,
    pub axes: Vec<AxisDirection>,
    pub axis_names: Vec<String>,
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
    axis_order_policy: AxisOrderPolicy,
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
        axis_order_policy,
    )
}

pub(crate) fn validate_supported_geographic_or_ellipsoidal_height_semantics(
    context: &str,
    angle_unit_to_degree: Option<f64>,
    prime_meridian_degrees: Option<f64>,
    coordinate_system: &CoordinateSystemSpec,
    axis_order_policy: AxisOrderPolicy,
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
            axis_order_policy,
        )?;
        return Ok(GeographicCoordinateSystemKind::ThreeDimensionalEllipsoidalHeight);
    }

    validate_coordinate_system(
        context,
        coordinate_system,
        Some("ellipsoidal"),
        &[AxisDirection::East, AxisDirection::North],
        "longitude/east, latitude/north",
        axis_order_policy,
    )?;
    Ok(GeographicCoordinateSystemKind::TwoDimensional)
}

pub(crate) fn validate_supported_projected_semantics(
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
    axis_order_policy: AxisOrderPolicy,
) -> Result<()> {
    validate_coordinate_system(
        context,
        coordinate_system,
        Some("cartesian"),
        &[AxisDirection::East, AxisDirection::North],
        "easting/east, northing/north",
        axis_order_policy,
    )
}

/// Returns true for an explicit `CS[Cartesian,3]` declaration.
///
/// Used to dispatch `GEODCRS` / `GeodeticCRS` between the geographic (ellipsoidal)
/// and geocentric parse paths. Axis-direction checks belong in
/// [`validate_supported_geocentric_semantics`], not here.
pub(crate) fn is_cartesian_3d_coordinate_system(coordinate_system: &CoordinateSystemSpec) -> bool {
    coordinate_system
        .subtype
        .as_deref()
        .is_some_and(|subtype| normalize_key(subtype) == "cartesian")
        && coordinate_system.dimension == Some(3)
}

pub(crate) fn validate_supported_geocentric_semantics(
    context: &str,
    format: CrsTextFormat,
    prime_meridian_degrees: Option<f64>,
    linear_unit: Option<LinearUnit>,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<()> {
    if let Some(prime_meridian_degrees) = prime_meridian_degrees {
        if !approx_eq(prime_meridian_degrees, 0.0) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses a non-Greenwich prime meridian"
            )));
        }
    }

    if let Some(linear_unit) = linear_unit {
        if !approx_eq(linear_unit.meters_per_unit(), 1.0) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses linear units other than metres"
            )));
        }
    }

    // WKT2/PROJJSON use geocentricX/Y/Z. WKT1 GEOCCS from GDAL/PROJ uses
    // OTHER/OTHER/NORTH for the same ECEF frame, but only with Geocentric X/Y/Z
    // names; that legacy direction pattern is not accepted in WKT2 or PROJJSON.
    const WKT2_AXES: &[AxisDirection] = &[
        AxisDirection::GeocentricX,
        AxisDirection::GeocentricY,
        AxisDirection::GeocentricZ,
    ];
    const WKT1_AXES: &[AxisDirection] = &[
        AxisDirection::Other,
        AxisDirection::Other,
        AxisDirection::North,
    ];
    let expected_axes = if coordinate_system.axes == WKT1_AXES {
        if format != CrsTextFormat::Wkt1 {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses unsupported axis order/directions `{}`; expected geocentricX, geocentricY, geocentricZ",
                format_axes(&coordinate_system.axes)
            )));
        }
        if !wkt1_geocentric_axis_names_match(coordinate_system) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses the WKT1 geocentric direction pattern without Geocentric X, Geocentric Y, Geocentric Z axis names"
            )));
        }
        WKT1_AXES
    } else {
        WKT2_AXES
    };

    validate_coordinate_system(
        context,
        coordinate_system,
        Some("cartesian"),
        expected_axes,
        "geocentricX, geocentricY, geocentricZ",
        AxisOrderPolicy::Strict,
    )
}

fn wkt1_geocentric_axis_names_match(coordinate_system: &CoordinateSystemSpec) -> bool {
    coordinate_system.axis_names.len() == 3
        && normalize_key(&coordinate_system.axis_names[0]) == "geocentricx"
        && normalize_key(&coordinate_system.axis_names[1]) == "geocentricy"
        && normalize_key(&coordinate_system.axis_names[2]) == "geocentricz"
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
    let curated = datum_candidates().iter().find_map(|candidate| {
        (datum_alias_matches(candidate, scope, datum_name)
            && ellipsoid_matches(
                ellipsoid,
                &candidate.datum,
                candidate.ellipsoid_aliases,
                candidate.ellipsoid_epsg,
            ))
        .then_some(candidate.datum.clone())
    });
    curated.or_else(|| resolve_registry_datum(datum_name, ellipsoid))
}

/// Registry-backed datum resolution: the EPSG alias table maps the parsed
/// datum name to a datum code, gated on the parsed ellipsoid agreeing with
/// the registry datum's ellipsoid (by ellipsoid code when the definition
/// carries one, else numerically).
fn resolve_registry_datum(datum_name: &str, ellipsoid: &StructuredEllipsoid) -> Option<Datum> {
    let code = proj_core::lookup_datum_code_for_name(datum_name)?;
    let datum = proj_core::lookup_datum_epsg(code)?;

    if let (Some(actual), Some(expected)) = (
        ellipsoid.epsg,
        proj_core::lookup_ellipsoid_code_for_datum(code),
    ) {
        if actual == expected {
            return Some(datum);
        }
    }

    let expected_rf = if datum.ellipsoid().flattening() == 0.0 {
        0.0
    } else {
        1.0 / datum.ellipsoid().flattening()
    };
    ((ellipsoid.semi_major_axis - datum.ellipsoid().semi_major_axis()).abs() < 1e-6
        && (ellipsoid.inverse_flattening - expected_rf).abs() < 1e-6)
        .then_some(datum)
}

/// Resolve a structured datum, falling back to a custom datum built from the
/// ellipsoid numbers when the datum name is not recognized at all. A known
/// datum name with contradictory ellipsoid numbers stays an error instead of
/// silently becoming a different datum. The custom fallback keeps the
/// parse→emit→parse cycle closed: the serializers emit
/// `Unknown datum`/`Unknown ellipsoid` with numeric parameters for
/// definitions without a registry identity.
pub(crate) fn resolve_structured_datum_or_custom(
    scope: DatumAliasScope,
    datum_name: &str,
    ellipsoid: &StructuredEllipsoid,
) -> Option<Datum> {
    if let Some(datum) = resolve_structured_datum(scope, datum_name, ellipsoid) {
        return Some(datum);
    }
    if resolve_named_datum(scope, datum_name).is_some()
        || proj_core::lookup_datum_code_for_name(datum_name).is_some()
    {
        return None;
    }
    custom_datum_from_structured_ellipsoid(ellipsoid)
}

fn custom_datum_from_structured_ellipsoid(ellipsoid: &StructuredEllipsoid) -> Option<Datum> {
    let ellipsoid = if ellipsoid.inverse_flattening == 0.0 {
        proj_core::Ellipsoid::sphere(ellipsoid.semi_major_axis).ok()?
    } else {
        proj_core::Ellipsoid::from_a_rf(ellipsoid.semi_major_axis, ellipsoid.inverse_flattening)
            .ok()?
    };
    Datum::new(ellipsoid, proj_core::DatumToWgs84::Unknown).ok()
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

/// Build a Helmert shift to WGS 84 from a bound CRS transformation.
///
/// `parameter` returns a normalized parameter name's value in metres,
/// arc-seconds, or parts per million.
pub(crate) fn helmert_from_bound_transformation(
    context: &str,
    method: &str,
    parameter: impl Fn(&str) -> Result<Option<f64>>,
) -> Result<HelmertParams> {
    let normalized = normalize_key(method);
    let (rotates, rotation_sign) = if normalized.starts_with("geocentrictranslations") {
        (false, 1.0)
    } else if normalized.starts_with("positionvector") {
        (true, 1.0)
    } else if normalized.starts_with("coordinateframe") {
        (true, -1.0)
    } else {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} uses unsupported transformation method `{method}`"
        )));
    };

    let required = |name: &str| {
        parameter(name)?.ok_or_else(|| {
            ParseError::Parse(format!(
                "{context} is missing transformation parameter `{name}`"
            ))
        })
    };
    let rotation = |name: &str| -> Result<f64> {
        Ok(if rotates {
            rotation_sign * required(name)?
        } else {
            0.0
        })
    };
    Ok(HelmertParams::new(
        required("xaxistranslation")?,
        required("yaxistranslation")?,
        required("zaxistranslation")?,
        rotation("xaxisrotation")?,
        rotation("yaxisrotation")?,
        rotation("zaxisrotation")?,
        if rotates {
            required("scaledifference")?
        } else {
            0.0
        },
    )?)
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
    axis_order_policy: AxisOrderPolicy,
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

    if coordinate_system.axes != expected_axes
        && !(axis_order_policy == AxisOrderPolicy::NormalizePermutation
            && axes_are_permutations(&coordinate_system.axes, expected_axes))
    {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} uses unsupported axis order/directions `{}`; expected {expected_axes_description}",
            format_axes(&coordinate_system.axes)
        )));
    }

    Ok(())
}

fn axes_are_permutations(actual: &[AxisDirection], expected: &[AxisDirection]) -> bool {
    actual.len() == expected.len()
        && expected.iter().all(|expected_axis| {
            actual.iter().filter(|axis| *axis == expected_axis).count()
                == expected
                    .iter()
                    .filter(|axis| *axis == expected_axis)
                    .count()
        })
}

fn format_axes(axes: &[AxisDirection]) -> String {
    axes.iter()
        .map(|axis| axis.description())
        .collect::<Vec<_>>()
        .join(", ")
}

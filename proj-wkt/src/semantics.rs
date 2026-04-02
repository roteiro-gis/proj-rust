use crate::{ParseError, Result};

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

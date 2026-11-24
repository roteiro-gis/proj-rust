//! Operations that position an engineering CRS: WKT2 `COORDINATEOPERATION`,
//! the WKT2 `BOUNDCRS` that carries a frame with its abridged
//! transformation, and their PROJJSON `Transformation` and `BoundCRS` forms.
//!
//! The method is a seven-parameter similarity on the frame's (x, y, h) and
//! the target CRS's (easting, northing, height): C PROJ's
//! `+proj=helmert +exact`. EPSG has no method for it, so its WKT names it
//! `Similarity transformation 3D, position vector` (or `coordinate frame`),
//! with the method identifier the caller gives.

use proj_core::{
    CoordinateOperation, CrsDef, Identifier, OperationAccuracy, OperationDomain, OperationMethod,
    RotationConvention, Similarity3DParams,
};
use serde_json::{json, Map, Value};

use crate::engineering::{
    identifier_from_projjson, identifier_from_wkt, identifier_projjson, identifier_wkt2,
    top_level_identifier, unquote, wkt_number,
};
use crate::semantics::normalize_key;
use crate::wkt::{
    find_top_level_element_inner, for_each_top_level_element, parse_wkt_element,
    split_top_level_fields, trim_wkt_token,
};
use crate::wkt_writer::quote;
use crate::{ParseError, Result};

const POSITION_VECTOR_METHOD: &str = "Similarity transformation 3D, position vector";
const COORDINATE_FRAME_METHOD: &str = "Similarity transformation 3D, coordinate frame";
const ARC_SECOND_IN_RADIANS: f64 = std::f64::consts::PI / (180.0 * 3600.0);
const ARC_SECOND_WKT: &str = "ANGLEUNIT[\"arc-second\",4.84813681109536E-06]";
const PPM_WKT: &str = "SCALEUNIT[\"parts per million\",1E-06]";
const METRE_WKT: &str = "LENGTHUNIT[\"metre\",1]";

/// A coordinate operation between an engineering CRS and another CRS, as
/// WKT2 and PROJJSON state it: its CRSs in full, and its version and
/// identifiers, which [`CoordinateOperation`] has no place for.
///
/// Use [`Self::coordinate_operation`] to pass it to a transform.
#[derive(Debug, Clone)]
pub struct OperationDefinition {
    pub name: String,
    pub version: Option<String>,
    pub source_crs: CrsDef,
    pub target_crs: CrsDef,
    /// Only [`OperationMethod::Similarity3D`] is read and written.
    pub method: OperationMethod,
    /// The method's identifier, such as `GEORIZON:1`, written as given.
    pub method_id: Option<Identifier>,
    /// Accuracy in metres.
    pub accuracy: Option<f64>,
    /// The operation's identifier, such as `GEORIZON:1047`.
    pub id: Option<Identifier>,
}

impl OperationDefinition {
    /// The operation for [`proj_core::SelectionOptions::with_coordinate_operation`].
    ///
    /// The engineering CRS is named by its identifier and the other CRS by
    /// its EPSG code, which it must have.
    pub fn coordinate_operation(&self) -> Result<CoordinateOperation> {
        similarity(self)?;
        if self.source_crs.is_engineering() == self.target_crs.is_engineering() {
            return Err(ParseError::UnsupportedSemantics(
                "a similarity operation connects one engineering CRS and one other CRS".into(),
            ));
        }
        let endpoint = |crs: &CrsDef| -> Result<(Option<u32>, Option<Identifier>)> {
            if let Some(frame) = crs.as_engineering() {
                return Ok((None, frame.identifier().cloned()));
            }
            match crs.epsg() {
                0 => Err(ParseError::UnsupportedSemantics(format!(
                    "operation {}: the CRS an engineering CRS is positioned on needs an EPSG code",
                    self.name
                ))),
                code => Ok((Some(code), None)),
            }
        };
        let (source_crs_epsg, source_crs_id) = endpoint(&self.source_crs)?;
        let (target_crs_epsg, target_crs_id) = endpoint(&self.target_crs)?;
        Ok(CoordinateOperation {
            id: None,
            name: self.name.clone(),
            source_crs_epsg,
            target_crs_epsg,
            source_crs_id,
            target_crs_id,
            source_datum_epsg: None,
            target_datum_epsg: None,
            accuracy: self.accuracy.map(|meters| OperationAccuracy { meters }),
            areas_of_use: Default::default(),
            deprecated: false,
            preferred: true,
            approximate: false,
            superseded: false,
            domain: OperationDomain::IncludesHeight,
            method: self.method.clone(),
        })
    }
}

fn similarity(operation: &OperationDefinition) -> Result<&Similarity3DParams> {
    match &operation.method {
        OperationMethod::Similarity3D { params } => Ok(params),
        _ => Err(ParseError::UnsupportedSemantics(format!(
            "operation {}: only the 3D similarity is supported in operation WKT and PROJJSON",
            operation.name
        ))),
    }
}

fn method_name(params: &Similarity3DParams) -> &'static str {
    match params.convention() {
        RotationConvention::PositionVector => POSITION_VECTOR_METHOD,
        RotationConvention::CoordinateFrame => COORDINATE_FRAME_METHOD,
    }
}

fn convention_from_method(context: &str, name: &str) -> Result<RotationConvention> {
    match normalize_key(name).as_str() {
        "similaritytransformation3dpositionvector" => Ok(RotationConvention::PositionVector),
        "similaritytransformation3dcoordinateframe" => Ok(RotationConvention::CoordinateFrame),
        _ => Err(ParseError::UnsupportedSemantics(format!(
            "{context} method `{name}` is not supported; expected `{POSITION_VECTOR_METHOD}` or `{COORDINATE_FRAME_METHOD}`"
        ))),
    }
}

/// The seven parameters by EPSG name and parameter code.
const PARAMETERS: [(&str, &str, u32); 7] = [
    ("X-axis translation", "xaxistranslation", 8605),
    ("Y-axis translation", "yaxistranslation", 8606),
    ("Z-axis translation", "zaxistranslation", 8607),
    ("X-axis rotation", "xaxisrotation", 8608),
    ("Y-axis rotation", "yaxisrotation", 8609),
    ("Z-axis rotation", "zaxisrotation", 8610),
    ("Scale difference", "scaledifference", 8611),
];

/// Which parameter slot a name or EPSG parameter code is.
fn parameter_slot(name: &str, code: Option<u32>) -> Option<usize> {
    let key = normalize_key(name);
    PARAMETERS
        .iter()
        .position(|(_, normalized, _)| *normalized == key)
        .or_else(|| code.and_then(|code| PARAMETERS.iter().position(|(.., c)| *c == code)))
}

/// A parameter value already in metres, arc-seconds, or parts per million.
fn similarity_from_slots(
    context: &str,
    values: [Option<f64>; 7],
    convention: RotationConvention,
) -> Result<Similarity3DParams> {
    let mut resolved = [0.0; 7];
    for (slot, value) in values.iter().enumerate() {
        resolved[slot] = value.ok_or_else(|| {
            ParseError::Parse(format!("{context} is missing `{}`", PARAMETERS[slot].0))
        })?;
    }
    let [x, y, z, rx, ry, rz, scale] = resolved;
    Ok(Similarity3DParams::new(
        [x, y, z],
        [rx, ry, rz],
        scale,
        convention,
    )?)
}

/// Convert `value` in a unit of `factor` (metres, radians, or unity per
/// unit) to metres, arc-seconds, or parts per million.
fn to_parameter_unit(slot: usize, value: f64, factor: f64) -> f64 {
    let canonical = match slot {
        0..=2 => 1.0,
        3..=5 => ARC_SECOND_IN_RADIANS,
        _ => 1e-6,
    };
    if ((factor - canonical) / canonical).abs() < 1e-12 {
        value
    } else {
        value * factor / canonical
    }
}

fn unit_kind_matches(slot: usize, keyword: &str) -> bool {
    match keyword.to_ascii_uppercase().as_str() {
        "UNIT" => true,
        "LENGTHUNIT" => slot <= 2,
        "ANGLEUNIT" => (3..=5).contains(&slot),
        "SCALEUNIT" => slot == 6,
        _ => false,
    }
}

/// Read `PARAMETER[name,value,unit?,ID?]` elements. Without a unit a value is
/// in metres, arc-seconds, or parts per million; abridged parameters must
/// have none, and state the scale as a multiplier.
fn wkt_parameters(context: &str, inner: &str, abridged: bool) -> Result<[Option<f64>; 7]> {
    let mut values = [None; 7];
    let mut error = None;
    for_each_top_level_element(inner, |name, element| {
        if error.is_some() || !name.eq_ignore_ascii_case("PARAMETER") {
            return;
        }
        if let Err(e) = read_wkt_parameter(context, element, abridged, &mut values) {
            error = Some(e);
        }
    });
    match error {
        Some(error) => Err(error),
        None => Ok(values),
    }
}

fn read_wkt_parameter(
    context: &str,
    element: &str,
    abridged: bool,
    values: &mut [Option<f64>; 7],
) -> Result<()> {
    let fields = split_top_level_fields(element);
    let name = fields
        .first()
        .map(|field| unquote(field))
        .unwrap_or_default();
    let mut code = None;
    let mut unit = None;
    for field in fields.iter().skip(2) {
        let Some((keyword, inner, _)) = parse_wkt_element(field.trim(), 0) else {
            continue;
        };
        if keyword.eq_ignore_ascii_case("ID") || keyword.eq_ignore_ascii_case("AUTHORITY") {
            let id = identifier_from_wkt(inner)?;
            if id.authority().eq_ignore_ascii_case("EPSG") {
                code = id.code().parse().ok();
            }
        } else if keyword.to_ascii_uppercase().ends_with("UNIT") {
            unit = Some((keyword, inner));
        }
    }
    let Some(slot) = parameter_slot(&name, code) else {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} parameter `{name}` is not one of the similarity's"
        )));
    };
    let raw = fields.get(1).map(|field| field.trim()).unwrap_or_default();
    let value = raw
        .parse::<f64>()
        .map_err(|_| ParseError::Parse(format!("{context} parameter `{name}` value `{raw}`")))?;
    let value = match (abridged, unit) {
        (true, Some(_)) => {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} parameter `{name}` names a unit, but abridged parameters use implied units"
            )));
        }
        (true, None) if slot == 6 => (value - 1.0) * 1e6,
        (_, None) => value,
        (false, Some((keyword, inner))) => {
            if !unit_kind_matches(slot, keyword) {
                return Err(ParseError::UnsupportedSemantics(format!(
                    "{context} parameter `{name}` cannot be in a {keyword}"
                )));
            }
            let factor = split_top_level_fields(inner)
                .get(1)
                .and_then(|factor| factor.trim().parse::<f64>().ok())
                .filter(|factor| factor.is_finite() && *factor > 0.0)
                .ok_or_else(|| {
                    ParseError::Parse(format!("{context} parameter `{name}` has an invalid unit"))
                })?;
            to_parameter_unit(slot, value, factor)
        }
    };
    if values[slot].replace(value).is_some() {
        return Err(ParseError::Parse(format!(
            "{context} repeats parameter `{name}`"
        )));
    }
    Ok(())
}

fn method_from_wkt(context: &str, inner: &str) -> Result<(RotationConvention, Option<Identifier>)> {
    let method = find_top_level_element_inner(inner, &["METHOD"])
        .ok_or_else(|| ParseError::Parse(format!("{context} is missing METHOD")))?;
    let name = split_top_level_fields(method)
        .first()
        .map(|field| unquote(field))
        .unwrap_or_default();
    Ok((
        convention_from_method(context, &name)?,
        top_level_identifier(method)?,
    ))
}

fn crs_from_wkt(context: &str, inner: &str, keyword: &str) -> Result<CrsDef> {
    let crs = find_top_level_element_inner(inner, &[keyword])
        .ok_or_else(|| ParseError::Parse(format!("{context} is missing {keyword}")))?;
    crate::parse_crs(crs)
}

fn version_from_wkt(inner: &str) -> Option<String> {
    find_top_level_element_inner(inner, &["VERSION"]).and_then(|version| {
        split_top_level_fields(version)
            .first()
            .map(|field| unquote(field))
    })
}

fn accuracy_from_wkt(context: &str, inner: &str) -> Result<Option<f64>> {
    find_top_level_element_inner(inner, &["OPERATIONACCURACY"])
        .map(|accuracy| {
            let raw = trim_wkt_token(accuracy);
            raw.parse::<f64>()
                .map_err(|_| ParseError::Parse(format!("{context} accuracy `{raw}`")))
        })
        .transpose()
}

/// Parse WKT2 `COORDINATEOPERATION[...]`.
pub(crate) fn parse_wkt_operation(s: &str) -> Result<OperationDefinition> {
    let context = "WKT COORDINATEOPERATION";
    let (root, inner, _) = parse_wkt_element(s.trim(), 0)
        .filter(|(root, ..)| root.eq_ignore_ascii_case("COORDINATEOPERATION"))
        .ok_or_else(|| ParseError::Parse(format!("expected {context}: {s:.40}")))?;
    debug_assert!(root.eq_ignore_ascii_case("COORDINATEOPERATION"));
    let (convention, method_id) = method_from_wkt(context, inner)?;
    let params =
        similarity_from_slots(context, wkt_parameters(context, inner, false)?, convention)?;
    Ok(OperationDefinition {
        name: split_top_level_fields(inner)
            .first()
            .map(|field| unquote(field))
            .unwrap_or_default(),
        version: version_from_wkt(inner),
        source_crs: crs_from_wkt(context, inner, "SOURCECRS")?,
        target_crs: crs_from_wkt(context, inner, "TARGETCRS")?,
        method: OperationMethod::Similarity3D { params },
        method_id,
        accuracy: accuracy_from_wkt(context, inner)?,
        id: top_level_identifier(inner)?,
    })
}

/// Parse a WKT2 `BOUNDCRS[...]` whose source is an engineering CRS.
pub(crate) fn parse_wkt_bound_crs(s: &str) -> Result<OperationDefinition> {
    let context = "WKT BOUNDCRS";
    let (_, inner, _) = parse_wkt_element(s.trim(), 0)
        .filter(|(root, ..)| root.eq_ignore_ascii_case("BOUNDCRS"))
        .ok_or_else(|| ParseError::Parse(format!("expected {context}: {s:.40}")))?;
    let source_crs = crs_from_wkt(context, inner, "SOURCECRS")?;
    if !source_crs.is_engineering() {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context}: only a bound engineering CRS carries an operation; parse_crs reads one bound to WGS 84"
        )));
    }
    let transformation = find_top_level_element_inner(inner, &["ABRIDGEDTRANSFORMATION"])
        .ok_or_else(|| ParseError::Parse(format!("{context} is missing ABRIDGEDTRANSFORMATION")))?;
    let (convention, method_id) = method_from_wkt(context, transformation)?;
    let params = similarity_from_slots(
        context,
        wkt_parameters(context, transformation, true)?,
        convention,
    )?;
    Ok(OperationDefinition {
        name: split_top_level_fields(transformation)
            .first()
            .map(|field| unquote(field))
            .unwrap_or_default(),
        version: version_from_wkt(transformation),
        source_crs,
        target_crs: crs_from_wkt(context, inner, "TARGETCRS")?,
        method: OperationMethod::Similarity3D { params },
        method_id,
        accuracy: accuracy_from_wkt(context, transformation)?,
        id: top_level_identifier(transformation)?,
    })
}

fn parameter_values(params: &Similarity3DParams) -> [f64; 7] {
    let [x, y, z] = params.translation();
    let [rx, ry, rz] = params.rotation_arcsec();
    [x, y, z, rx, ry, rz, params.scale_ppm()]
}

fn method_wkt2(operation: &OperationDefinition, params: &Similarity3DParams) -> String {
    let mut fields = vec![quote(method_name(params))];
    if let Some(id) = &operation.method_id {
        fields.push(identifier_wkt2(id));
    }
    format!("METHOD[{}]", fields.join(","))
}

/// WKT2 `COORDINATEOPERATION`, in the plan's form: units on every parameter.
pub(crate) fn operation_wkt2(operation: &OperationDefinition) -> Result<String> {
    let params = similarity(operation)?;
    let mut fields = vec![quote(&operation.name)];
    if let Some(version) = &operation.version {
        fields.push(format!("VERSION[{}]", quote(version)));
    }
    fields.push(format!(
        "SOURCECRS[{}]",
        crate::to_wkt2(&operation.source_crs)?
    ));
    fields.push(format!(
        "TARGETCRS[{}]",
        crate::to_wkt2(&operation.target_crs)?
    ));
    fields.push(method_wkt2(operation, params));
    for (slot, value) in parameter_values(params).into_iter().enumerate() {
        let unit = match slot {
            0..=2 => METRE_WKT,
            3..=5 => ARC_SECOND_WKT,
            _ => PPM_WKT,
        };
        fields.push(format!(
            "PARAMETER[{},{},{unit}]",
            quote(PARAMETERS[slot].0),
            wkt_number(value)?
        ));
    }
    if let Some(accuracy) = operation.accuracy {
        fields.push(format!("OPERATIONACCURACY[{}]", wkt_number(accuracy)?));
    }
    if let Some(id) = &operation.id {
        fields.push(identifier_wkt2(id));
    }
    Ok(format!("COORDINATEOPERATION[{}]", fields.join(",")))
}

/// WKT2 `BOUNDCRS`: the frame, its target CRS, and the operation abridged,
/// whose parameters have implied units and a scale multiplier.
pub(crate) fn bound_crs_wkt2(operation: &OperationDefinition) -> Result<String> {
    let params = similarity(operation)?;
    if !operation.source_crs.is_engineering() {
        return Err(ParseError::UnsupportedSemantics(
            "a bound CRS carries the operation from its engineering source CRS".into(),
        ));
    }
    let mut fields = vec![quote(&operation.name)];
    if let Some(version) = &operation.version {
        fields.push(format!("VERSION[{}]", quote(version)));
    }
    fields.push(method_wkt2(operation, params));
    for (slot, value) in parameter_values(params).into_iter().enumerate() {
        let value = if slot == 6 { 1.0 + value * 1e-6 } else { value };
        fields.push(format!(
            "PARAMETER[{},{}]",
            quote(PARAMETERS[slot].0),
            wkt_number(value)?
        ));
    }
    if let Some(id) = &operation.id {
        fields.push(identifier_wkt2(id));
    }
    Ok(format!(
        "BOUNDCRS[SOURCECRS[{}],TARGETCRS[{}],ABRIDGEDTRANSFORMATION[{}]]",
        crate::to_wkt2(&operation.source_crs)?,
        crate::to_wkt2(&operation.target_crs)?,
        fields.join(",")
    ))
}

fn projjson_parameters(params: &Similarity3DParams) -> Result<Vec<Value>> {
    parameter_values(params)
        .into_iter()
        .enumerate()
        .map(|(slot, value)| {
            let (name, _, code) = PARAMETERS[slot];
            let unit = match slot {
                0..=2 => json!("metre"),
                3..=5 => json!({
                    "type": "AngularUnit",
                    "name": "arc-second",
                    "conversion_factor": ARC_SECOND_IN_RADIANS,
                }),
                _ => json!({
                    "type": "ScaleUnit",
                    "name": "parts per million",
                    "conversion_factor": 1e-6,
                }),
            };
            Ok(json!({
                "name": name,
                "value": crate::projjson_writer::number(value)?,
                "unit": unit,
                "id": { "authority": "EPSG", "code": code },
            }))
        })
        .collect()
}

/// The transformation's own PROJJSON members. PROJJSON has no operation
/// version, and an abridged transformation has no accuracy either.
fn transformation_projjson(
    operation: &OperationDefinition,
    params: &Similarity3DParams,
    abridged: bool,
) -> Result<Map<String, Value>> {
    let mut method = Map::new();
    method.insert("name".into(), json!(method_name(params)));
    if let Some(id) = &operation.method_id {
        method.insert("id".into(), identifier_projjson(id));
    }
    let mut object = Map::new();
    object.insert("name".into(), json!(operation.name));
    object.insert("method".into(), Value::Object(method));
    object.insert(
        "parameters".into(),
        Value::Array(projjson_parameters(params)?),
    );
    if let Some(accuracy) = operation.accuracy.filter(|_| !abridged) {
        object.insert("accuracy".into(), json!(wkt_number(accuracy)?));
    }
    if let Some(id) = &operation.id {
        object.insert("id".into(), identifier_projjson(id));
    }
    Ok(object)
}

/// PROJJSON `Transformation`, without the version, which PROJJSON cannot
/// state.
pub(crate) fn operation_projjson(operation: &OperationDefinition) -> Result<Value> {
    let params = similarity(operation)?;
    let transformation = transformation_projjson(operation, params, false)?;
    let mut object = Map::new();
    object.insert(
        "$schema".into(),
        json!("https://proj.org/schemas/v0.7/projjson.schema.json"),
    );
    object.insert("type".into(), json!("Transformation"));
    object.insert("name".into(), transformation["name"].clone());
    object.insert(
        "source_crs".into(),
        crate::projjson_writer::crs_value(&operation.source_crs)?,
    );
    object.insert(
        "target_crs".into(),
        crate::projjson_writer::crs_value(&operation.target_crs)?,
    );
    for key in ["method", "parameters", "accuracy", "id"] {
        if let Some(value) = transformation.get(key) {
            object.insert(key.into(), value.clone());
        }
    }
    Ok(Value::Object(object))
}

/// PROJJSON `BoundCRS` of an engineering source CRS.
pub(crate) fn bound_crs_projjson(operation: &OperationDefinition) -> Result<Value> {
    let params = similarity(operation)?;
    if !operation.source_crs.is_engineering() {
        return Err(ParseError::UnsupportedSemantics(
            "a bound CRS carries the operation from its engineering source CRS".into(),
        ));
    }
    Ok(json!({
        "$schema": "https://proj.org/schemas/v0.7/projjson.schema.json",
        "type": "BoundCRS",
        "source_crs": crate::projjson_writer::crs_value(&operation.source_crs)?,
        "target_crs": crate::projjson_writer::crs_value(&operation.target_crs)?,
        "transformation": Value::Object(transformation_projjson(operation, params, true)?),
    }))
}

/// Conversion factor of a PROJJSON unit, a name or a unit object, when it
/// is a unit of the kind parameter `slot` takes.
fn projjson_unit_factor(slot: usize, unit: &Value) -> Option<f64> {
    // A named unit's kind and factor, or an object's declared type.
    let name = unit.as_str().or_else(|| unit.get("name")?.as_str());
    let named = name.and_then(|name| match normalize_key(name).as_str() {
        "metre" | "meter" => Some(("LinearUnit", 1.0)),
        "kilometre" | "kilometer" => Some(("LinearUnit", 1000.0)),
        "radian" => Some(("AngularUnit", 1.0)),
        "arcsecond" => Some(("AngularUnit", ARC_SECOND_IN_RADIANS)),
        "degree" => Some(("AngularUnit", std::f64::consts::PI / 180.0)),
        "unity" => Some(("ScaleUnit", 1.0)),
        "partspermillion" => Some(("ScaleUnit", 1e-6)),
        _ => None,
    });
    let kind = unit
        .get("type")
        .and_then(Value::as_str)
        .or(named.map(|(kind, _)| kind))?;
    let factor = unit
        .get("conversion_factor")
        .and_then(Value::as_f64)
        .or(named.map(|(_, factor)| factor))
        .filter(|factor| factor.is_finite() && *factor > 0.0)?;
    let expected = match slot {
        0..=2 => "LinearUnit",
        3..=5 => "AngularUnit",
        _ => "ScaleUnit",
    };
    (kind == expected || kind == "Unit").then_some(factor)
}

/// A PROJJSON transformation's own fields, without its CRSs.
struct ProjjsonTransformation {
    name: String,
    version: Option<String>,
    method: OperationMethod,
    method_id: Option<Identifier>,
    accuracy: Option<f64>,
    id: Option<Identifier>,
}

fn projjson_transformation(context: &str, value: &Value) -> Result<ProjjsonTransformation> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let method = value
        .get("method")
        .ok_or_else(|| ParseError::Parse(format!("{context} is missing a method")))?;
    let method_name = method
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ParseError::Parse(format!("{context} method has no name")))?;
    let convention = convention_from_method(context, method_name)?;
    let mut values = [None; 7];
    for parameter in value
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let parameter_name = parameter
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let code = parameter
            .get("id")
            .filter(|id| id.get("authority").and_then(Value::as_str) == Some("EPSG"))
            .and_then(|id| id.get("code")?.as_u64())
            .and_then(|code| u32::try_from(code).ok());
        let slot = parameter_slot(parameter_name, code).ok_or_else(|| {
            ParseError::UnsupportedSemantics(format!(
                "{context} parameter `{parameter_name}` is not one of the similarity's"
            ))
        })?;
        let raw = parameter
            .get("value")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                ParseError::Parse(format!(
                    "{context} parameter `{parameter_name}` has no value"
                ))
            })?;
        let value = match parameter.get("unit") {
            None => raw,
            Some(unit) => {
                let factor = projjson_unit_factor(slot, unit).ok_or_else(|| {
                    ParseError::UnsupportedSemantics(format!(
                        "{context} parameter `{parameter_name}` uses an unsupported unit"
                    ))
                })?;
                to_parameter_unit(slot, raw, factor)
            }
        };
        if values[slot].replace(value).is_some() {
            return Err(ParseError::Parse(format!(
                "{context} repeats parameter `{parameter_name}`"
            )));
        }
    }
    let params = similarity_from_slots(context, values, convention)?;
    let accuracy = match value.get("accuracy") {
        None => None,
        Some(Value::String(accuracy)) => Some(
            accuracy
                .parse::<f64>()
                .map_err(|_| ParseError::Parse(format!("{context} accuracy `{accuracy}`")))?,
        ),
        Some(Value::Number(accuracy)) => accuracy.as_f64(),
        Some(other) => {
            return Err(ParseError::Parse(format!("{context} accuracy {other}")));
        }
    };
    Ok(ProjjsonTransformation {
        name,
        version: value
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string),
        method: OperationMethod::Similarity3D { params },
        method_id: identifier_from_projjson(method)?,
        accuracy,
        id: identifier_from_projjson(value)?,
    })
}

fn projjson_crs(context: &str, value: &Value, key: &str) -> Result<CrsDef> {
    let crs = value
        .get(key)
        .ok_or_else(|| ParseError::Parse(format!("{context} is missing {key}")))?;
    crate::projjson::parse_projjson_value(crs)
}

/// Parse a PROJJSON `Transformation`.
pub(crate) fn parse_projjson_operation(value: &Value) -> Result<OperationDefinition> {
    let context = "PROJJSON Transformation";
    if value.get("type").and_then(Value::as_str) != Some("Transformation") {
        return Err(ParseError::Parse(format!("expected a {context}")));
    }
    let transformation = projjson_transformation(context, value)?;
    Ok(OperationDefinition {
        name: transformation.name,
        version: transformation.version,
        source_crs: projjson_crs(context, value, "source_crs")?,
        target_crs: projjson_crs(context, value, "target_crs")?,
        method: transformation.method,
        method_id: transformation.method_id,
        accuracy: transformation.accuracy,
        id: transformation.id,
    })
}

/// Parse a PROJJSON `BoundCRS` whose source is an engineering CRS.
pub(crate) fn parse_projjson_bound_crs(value: &Value) -> Result<OperationDefinition> {
    let context = "PROJJSON BoundCRS";
    if value.get("type").and_then(Value::as_str) != Some("BoundCRS") {
        return Err(ParseError::Parse(format!("expected a {context}")));
    }
    let source_crs = projjson_crs(context, value, "source_crs")?;
    if !source_crs.is_engineering() {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context}: only a bound engineering CRS carries an operation; parse_crs reads one bound to WGS 84"
        )));
    }
    let transformation = value
        .get("transformation")
        .ok_or_else(|| ParseError::Parse(format!("{context} is missing transformation")))?;
    let transformation = projjson_transformation(context, transformation)?;
    Ok(OperationDefinition {
        name: transformation.name,
        version: transformation.version,
        source_crs,
        target_crs: projjson_crs(context, value, "target_crs")?,
        method: transformation.method,
        method_id: transformation.method_id,
        accuracy: transformation.accuracy,
        id: transformation.id,
    })
}

/// The operation a bound engineering CRS carries, or `None` for any other
/// definition.
pub(crate) fn bound_engineering_operation(s: &str) -> Result<Option<OperationDefinition>> {
    let s = s.trim();
    if s.starts_with('{') {
        let Ok(value) = serde_json::from_str::<Value>(s) else {
            return Ok(None);
        };
        let engineering_source = value.get("type").and_then(Value::as_str) == Some("BoundCRS")
            && value
                .get("source_crs")
                .and_then(|source| source.get("type"))
                .and_then(Value::as_str)
                == Some("EngineeringCRS");
        return engineering_source
            .then(|| parse_projjson_bound_crs(&value))
            .transpose();
    }
    let Some((root, inner, _)) = parse_wkt_element(s, 0) else {
        return Ok(None);
    };
    let engineering_source = root.eq_ignore_ascii_case("BOUNDCRS")
        && find_top_level_element_inner(inner, &["SOURCECRS"])
            .is_some_and(crate::engineering::is_engineering_wkt);
    engineering_source
        .then(|| parse_wkt_bound_crs(s))
        .transpose()
}

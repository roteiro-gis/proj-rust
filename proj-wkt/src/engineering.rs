//! Engineering CRSs: WKT2 `ENGCRS`, WKT1 `LOCAL_CS`, and PROJJSON
//! `EngineeringCRS`.
//!
//! [`EngineeringCrsDef`] models a Cartesian frame whose axes point east,
//! north and, in 3D, up, in one linear unit. Definitions with other axis
//! directions or a non-Cartesian coordinate system are rejected.

use proj_core::{AxisOrder, CrsDef, EngineeringCrsDef, Identifier, LinearUnit};
use serde_json::{json, Map, Value};

use crate::projjson::coordinate_system_from_json;
use crate::projjson_writer::linear_unit_value;
use crate::semantics::{approx_eq, AxisDirection, CoordinateSystemSpec};
use crate::wkt::{
    coordinate_system_linear_unit, extract_coordinate_system, extract_top_level_length_unit,
    find_top_level_element_inner, for_each_top_level_element, parse_wkt_element,
    split_top_level_fields, trim_wkt_token,
};
use crate::wkt2_writer::length_unit_wkt2;
use crate::wkt_writer::{linear_unit_wkt, quote};
use crate::{ParseError, Result};

/// WKT keywords of an engineering CRS, WKT2 and WKT1.
pub(crate) const ENGINEERING_CRS_KEYWORDS: &[&str] = &["ENGCRS", "ENGINEERINGCRS", "LOCAL_CS"];

pub(crate) fn is_engineering_wkt(s: &str) -> bool {
    parse_wkt_element(s.trim(), 0).is_some_and(|(name, _, _)| {
        ENGINEERING_CRS_KEYWORDS
            .iter()
            .any(|keyword| name.eq_ignore_ascii_case(keyword))
    })
}

/// Parse `ENGCRS[...]`, `ENGINEERINGCRS[...]`, or `LOCAL_CS[...]`.
pub(crate) fn parse_wkt_engineering(s: &str) -> Result<CrsDef> {
    let (root, inner, _) = parse_wkt_element(s.trim(), 0)
        .ok_or_else(|| ParseError::Parse(format!("unrecognized WKT root element: {s:.40}")))?;
    let wkt1 = root.eq_ignore_ascii_case("LOCAL_CS");
    let context = if wkt1 {
        "WKT LOCAL_CS"
    } else {
        "WKT engineering CRS"
    };
    let fields = split_top_level_fields(inner);
    let name = fields
        .first()
        .filter(|field| field.trim_start().starts_with('"'))
        .map(|field| unquote(field))
        .unwrap_or_default();
    let datum_keywords: &[&str] = if wkt1 {
        &["LOCAL_DATUM"]
    } else {
        &["EDATUM", "ENGINEERINGDATUM"]
    };
    let datum_name = find_top_level_element_inner(inner, datum_keywords)
        .and_then(|datum| {
            split_top_level_fields(datum)
                .first()
                .map(|field| unquote(field))
        })
        .unwrap_or_else(|| name.clone());

    let coordinate_system = extract_coordinate_system(inner);
    let axis_unit = coordinate_system_linear_unit(context, &coordinate_system)?;
    let top_level_unit = extract_top_level_length_unit(inner, true);
    if let (Some(axis), Some(top_level)) = (axis_unit, top_level_unit) {
        if !approx_eq(axis.meters_per_unit(), top_level.meters_per_unit()) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} declares conflicting linear units"
            )));
        }
    }
    let linear_unit = axis_unit
        .or(top_level_unit)
        .unwrap_or_else(LinearUnit::metre);

    let frame = engineering_def(
        context,
        name,
        datum_name,
        &coordinate_system,
        linear_unit,
        top_level_identifier(inner)?,
    )?;
    Ok(CrsDef::Engineering(Box::new(frame)))
}

/// The engineering CRS a coordinate system spec describes. Without axes it is
/// 2D, east then north.
fn engineering_def(
    context: &str,
    name: String,
    datum_name: String,
    coordinate_system: &CoordinateSystemSpec,
    linear_unit: LinearUnit,
    identifier: Option<Identifier>,
) -> Result<EngineeringCrsDef> {
    if let Some(subtype) = &coordinate_system.subtype {
        if !subtype.eq_ignore_ascii_case("Cartesian") {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} must use a Cartesian coordinate system, not {subtype}"
            )));
        }
    }
    let axes = &coordinate_system.axes;
    let dimension = if axes.is_empty() {
        coordinate_system.dimension.unwrap_or(2)
    } else {
        axes.len()
    };
    if coordinate_system
        .dimension
        .is_some_and(|declared| !axes.is_empty() && declared != axes.len())
    {
        return Err(ParseError::Parse(format!(
            "{context} declares {} axes but lists {}",
            coordinate_system.dimension.unwrap_or_default(),
            axes.len()
        )));
    }
    let axis_order = match axes.as_slice() {
        [] => AxisOrder::EastNorth,
        [AxisDirection::East, AxisDirection::North]
        | [AxisDirection::East, AxisDirection::North, AxisDirection::Up] => AxisOrder::EastNorth,
        [AxisDirection::North, AxisDirection::East]
        | [AxisDirection::North, AxisDirection::East, AxisDirection::Up] => AxisOrder::NorthEast,
        _ => {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} axes must point east and north, then up in 3D"
            )));
        }
    };

    let mut frame = EngineeringCrsDef::new(name, datum_name, dimension)?
        .with_linear_unit(linear_unit)
        .with_axis_order(axis_order);
    if !axes.is_empty() && coordinate_system.axis_names.iter().all(|n| !n.is_empty()) {
        frame = frame.with_axis_names(coordinate_system.axis_names.iter().cloned())?;
    }
    if let Some(identifier) = identifier {
        frame = frame.with_identifier(identifier);
    }
    Ok(frame)
}

/// The definition's own `ID[...]` or `AUTHORITY[...]`, whatever the
/// authority.
pub(crate) fn top_level_identifier(inner: &str) -> Result<Option<Identifier>> {
    let mut identifier = None;
    for_each_top_level_element(inner, |name, element| {
        if identifier.is_none()
            && (name.eq_ignore_ascii_case("ID") || name.eq_ignore_ascii_case("AUTHORITY"))
        {
            identifier = Some(element);
        }
    });
    identifier.map(identifier_from_wkt).transpose()
}

pub(crate) fn identifier_from_wkt(inner: &str) -> Result<Identifier> {
    let fields = split_top_level_fields(inner);
    match fields.as_slice() {
        [authority, code, ..] => Ok(Identifier::new(
            trim_wkt_token(authority),
            trim_wkt_token(code),
        )?),
        _ => Err(ParseError::Parse(format!(
            "WKT identifier needs an authority and a code: {inner:.40}"
        ))),
    }
}

pub(crate) fn unquote(field: &str) -> String {
    trim_wkt_token(field).replace("\"\"", "\"")
}

/// `ID["GEORIZON",12]`, with a numeric code unquoted.
pub(crate) fn identifier_wkt2(identifier: &Identifier) -> String {
    format!(
        "ID[{},{}]",
        quote(identifier.authority()),
        wkt_code(identifier.code())
    )
}

fn wkt_code(code: &str) -> String {
    if !code.is_empty() && code.bytes().all(|byte| byte.is_ascii_digit()) {
        code.to_string()
    } else {
        quote(code)
    }
}

pub(crate) fn engineering_wkt2(frame: &EngineeringCrsDef) -> Result<String> {
    let unit = length_unit_wkt2(frame.linear_unit())?;
    let mut fields = vec![
        quote(frame.name()),
        format!("EDATUM[{}]", quote(frame.datum_name())),
        format!("CS[Cartesian,{}]", frame.dimension()),
    ];
    for (index, (name, direction)) in axis_names_and_directions(frame).enumerate() {
        fields.push(format!(
            "AXIS[{},{direction},ORDER[{}],{unit}]",
            quote(name),
            index + 1
        ));
    }
    if let Some(identifier) = frame.identifier() {
        fields.push(identifier_wkt2(identifier));
    }
    Ok(format!("ENGCRS[{}]", fields.join(",")))
}

pub(crate) fn engineering_wkt1(frame: &EngineeringCrsDef) -> Result<String> {
    let unit = linear_unit_wkt(frame.linear_unit())?;
    let mut unit_fields = vec![quote(unit.name), unit.factor];
    if let Some(code) = unit.epsg {
        unit_fields.push(format!("AUTHORITY[\"EPSG\",\"{code}\"]"));
    }
    let mut fields = vec![
        quote(frame.name()),
        // 32767: "other", WKT1's only local datum type.
        format!("LOCAL_DATUM[{},32767]", quote(frame.datum_name())),
        format!("UNIT[{}]", unit_fields.join(",")),
    ];
    for (name, direction) in axis_names_and_directions(frame) {
        fields.push(format!(
            "AXIS[{},{}]",
            quote(name),
            direction.to_ascii_uppercase()
        ));
    }
    if let Some(identifier) = frame.identifier() {
        fields.push(format!(
            "AUTHORITY[{},{}]",
            quote(identifier.authority()),
            quote(identifier.code())
        ));
    }
    Ok(format!("LOCAL_CS[{}]", fields.join(",")))
}

fn axis_names_and_directions(
    frame: &EngineeringCrsDef,
) -> impl Iterator<Item = (&str, &'static str)> {
    let directions: [&str; 3] = match frame.axis_order() {
        AxisOrder::EastNorth => ["east", "north", "up"],
        AxisOrder::NorthEast => ["north", "east", "up"],
    };
    frame
        .axis_names()
        .iter()
        .map(String::as_str)
        .zip(directions)
}

/// PROJJSON `EngineeringCRS`.
pub(crate) fn engineering_projjson(frame: &EngineeringCrsDef) -> Value {
    let unit = linear_unit_value(frame.linear_unit());
    let axes = axis_names_and_directions(frame)
        .map(|(name, direction)| {
            let (name, abbreviation) = split_axis_name(name);
            json!({
                "name": name,
                "abbreviation": abbreviation,
                "direction": direction,
                "unit": unit,
            })
        })
        .collect::<Vec<_>>();
    let mut object = Map::new();
    object.insert("type".into(), json!("EngineeringCRS"));
    object.insert("name".into(), json!(frame.name()));
    object.insert(
        "datum".into(),
        json!({ "type": "EngineeringDatum", "name": frame.datum_name() }),
    );
    object.insert(
        "coordinate_system".into(),
        json!({ "subtype": "Cartesian", "axis": axes }),
    );
    if let Some(identifier) = frame.identifier() {
        object.insert("id".into(), identifier_projjson(identifier));
    }
    Value::Object(object)
}

pub(crate) fn identifier_projjson(identifier: &Identifier) -> Value {
    let code = identifier
        .code()
        .parse::<u64>()
        .ok()
        .filter(|code| code.to_string() == identifier.code())
        .map_or_else(|| json!(identifier.code()), |code| json!(code));
    json!({ "authority": identifier.authority(), "code": code })
}

pub(crate) fn identifier_from_projjson(value: &Value) -> Result<Option<Identifier>> {
    let Some(id) = value.get("id") else {
        return Ok(None);
    };
    let authority = id.get("authority").and_then(Value::as_str);
    let code = match id.get("code") {
        Some(Value::String(code)) => Some(code.clone()),
        Some(Value::Number(code)) => Some(code.to_string()),
        _ => None,
    };
    match (authority, code) {
        (Some(authority), Some(code)) => Ok(Some(Identifier::new(authority, code)?)),
        _ => Err(ParseError::Parse(
            "PROJJSON id needs an authority and a code".into(),
        )),
    }
}

/// `site east (x)` is the name `site east` with abbreviation `x`, WKT2's way
/// of writing both.
fn split_axis_name(name: &str) -> (&str, &str) {
    if let Some(open) = name.rfind(" (") {
        if let Some(abbreviation) = name[open + 2..].strip_suffix(')') {
            return (&name[..open], abbreviation);
        }
    }
    (name, name)
}

/// Parse a PROJJSON `EngineeringCRS`.
pub(crate) fn parse_projjson_engineering(value: &Value) -> Result<CrsDef> {
    let context = "PROJJSON EngineeringCRS";
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let datum_name = value
        .get("datum")
        .and_then(|datum| datum.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| name.clone(), str::to_string);

    let mut coordinate_system = coordinate_system_from_json(value);
    if let Some(axes) = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
    {
        coordinate_system.axis_names = axes
            .iter()
            .map(|axis| {
                let name = axis.get("name").and_then(Value::as_str).unwrap_or_default();
                match axis.get("abbreviation").and_then(Value::as_str) {
                    Some(abbreviation) if abbreviation != name && !name.is_empty() => {
                        format!("{name} ({abbreviation})")
                    }
                    Some(abbreviation) if name.is_empty() => abbreviation.to_string(),
                    _ => name.to_string(),
                }
            })
            .collect();
    }
    let linear_unit = coordinate_system_linear_unit(context, &coordinate_system)?
        .unwrap_or_else(LinearUnit::metre);
    let frame = engineering_def(
        context,
        name,
        datum_name,
        &coordinate_system,
        linear_unit,
        identifier_from_projjson(value)?,
    )?;
    Ok(CrsDef::Engineering(Box::new(frame)))
}

/// A number as WKT writes it: the shortest form that reads back exactly,
/// so a scale multiplier within 1e-9 of 1 is not rounded to 1.
pub(crate) fn wkt_number(value: f64) -> Result<String> {
    if !value.is_finite() {
        return Err(ParseError::UnsupportedSemantics(format!(
            "cannot emit WKT for a non-finite value {value}"
        )));
    }
    Ok(if value == 0.0 {
        "0".to_string()
    } else {
        value.to_string()
    })
}

#[cfg(test)]
mod tests {
    use crate::{parse_crs, to_projjson, to_wkt, to_wkt2};
    use proj_core::{AxisOrder, CrsDef, LinearUnit};

    /// The plan's appendix frame.
    const SITE_GRID_WKT2: &str = r#"ENGCRS["Site grid Example",
    EDATUM["Site grid Example"],
    CS[Cartesian,3],
        AXIS["site east (x)",east,ORDER[1],LENGTHUNIT["metre",1]],
        AXIS["site north (y)",north,ORDER[2],LENGTHUNIT["metre",1]],
        AXIS["height (h)",up,ORDER[3],LENGTHUNIT["metre",1]],
    ID["GEORIZON",12]]"#;

    const SITE_GRID_WKT1: &str = r#"LOCAL_CS["Site grid Example",
    LOCAL_DATUM["Site grid Example",32767],
    UNIT["metre",1],
    AXIS["x",EAST],AXIS["y",NORTH],AXIS["h",UP],
    AUTHORITY["GEORIZON","12"]]"#;

    fn frame(crs: &CrsDef) -> &proj_core::EngineeringCrsDef {
        crs.as_engineering().expect("an engineering CRS")
    }

    #[test]
    fn parses_the_appendix_wkt2_frame() {
        let crs = parse_crs(SITE_GRID_WKT2).unwrap();
        let frame = frame(&crs);
        assert_eq!(frame.name(), "Site grid Example");
        assert_eq!(frame.datum_name(), "Site grid Example");
        assert_eq!(frame.dimension(), 3);
        assert_eq!(frame.axis_order(), AxisOrder::EastNorth);
        assert_eq!(frame.linear_unit(), LinearUnit::metre());
        assert_eq!(
            frame.axis_names(),
            ["site east (x)", "site north (y)", "height (h)"]
        );
        assert_eq!(frame.identifier().unwrap().to_string(), "GEORIZON:12");
    }

    #[test]
    fn parses_the_appendix_wkt1_frame_as_the_same_frame() {
        let wkt1 = parse_crs(SITE_GRID_WKT1).unwrap();
        let wkt2 = parse_crs(SITE_GRID_WKT2).unwrap();
        assert!(frame(&wkt1).same_frame(frame(&wkt2)));
        assert!(wkt1.semantically_equivalent(&wkt2));
        assert_eq!(frame(&wkt1).axis_names(), ["x", "y", "h"]);
    }

    #[test]
    fn engineering_crs_roundtrips_through_every_format() {
        let original = parse_crs(SITE_GRID_WKT2).unwrap();
        let feet = CrsDef::Engineering(Box::new(
            frame(&original)
                .clone()
                .with_linear_unit(LinearUnit::us_survey_foot())
                .with_axis_order(AxisOrder::NorthEast),
        ));
        let flat = original.horizontal_crs().unwrap();
        for crs in [original, feet, flat] {
            for text in [
                to_wkt2(&crs).unwrap(),
                to_wkt(&crs).unwrap(),
                to_projjson(&crs).unwrap(),
            ] {
                let reparsed = parse_crs(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
                assert!(
                    crs.semantically_equivalent(&reparsed),
                    "{text} reparsed as {reparsed:?}"
                );
                if !text.starts_with("LOCAL_CS") {
                    assert_eq!(frame(&crs), frame(&reparsed), "{text}");
                }
            }
        }
    }

    #[test]
    fn writes_the_appendix_shape() {
        let crs = parse_crs(SITE_GRID_WKT2).unwrap();
        assert_eq!(
            to_wkt2(&crs).unwrap(),
            "ENGCRS[\"Site grid Example\",EDATUM[\"Site grid Example\"],CS[Cartesian,3],\
             AXIS[\"site east (x)\",east,ORDER[1],LENGTHUNIT[\"metre\",1]],\
             AXIS[\"site north (y)\",north,ORDER[2],LENGTHUNIT[\"metre\",1]],\
             AXIS[\"height (h)\",up,ORDER[3],LENGTHUNIT[\"metre\",1]],ID[\"GEORIZON\",12]]"
        );
        assert_eq!(
            to_wkt(&crs).unwrap(),
            "LOCAL_CS[\"Site grid Example\",LOCAL_DATUM[\"Site grid Example\",32767],\
             UNIT[\"metre\",1,AUTHORITY[\"EPSG\",\"9001\"]],AXIS[\"site east (x)\",EAST],\
             AXIS[\"site north (y)\",NORTH],AXIS[\"height (h)\",UP],AUTHORITY[\"GEORIZON\",\"12\"]]"
        );
        let projjson: serde_json::Value =
            serde_json::from_str(&to_projjson(&crs).unwrap()).unwrap();
        assert_eq!(projjson["type"], "EngineeringCRS");
        assert_eq!(projjson["id"]["code"], 12);
        assert_eq!(
            projjson["coordinate_system"]["axis"][0]["name"],
            "site east"
        );
        assert_eq!(
            projjson["coordinate_system"]["axis"][0]["abbreviation"],
            "x"
        );
    }

    #[test]
    fn local_cs_without_axes_is_a_2d_east_north_frame() {
        let crs = parse_crs(r#"LOCAL_CS["Plant",UNIT["foot",0.3048]]"#).unwrap();
        let frame = frame(&crs);
        assert_eq!(frame.dimension(), 2);
        assert_eq!(frame.datum_name(), "Plant");
        assert_eq!(frame.linear_unit(), LinearUnit::foot());
    }

    #[test]
    fn rejects_frames_this_model_cannot_state() {
        for wkt in [
            r#"ENGCRS["a",EDATUM["a"],CS[Cartesian,2],AXIS["x",west],AXIS["y",north],LENGTHUNIT["metre",1]]"#,
            r#"ENGCRS["a",EDATUM["a"],CS[Cartesian,3],AXIS["x",east],AXIS["y",north],AXIS["z",down],LENGTHUNIT["metre",1]]"#,
            r#"ENGCRS["a",EDATUM["a"],CS[polar,2],AXIS["r",awayFrom],AXIS["t",clockwise]]"#,
            r#"ENGCRS["a",EDATUM["a"],CS[Cartesian,2],AXIS["x",east,LENGTHUNIT["metre",1]],AXIS["y",north,LENGTHUNIT["foot",0.3048]]]"#,
        ] {
            assert!(parse_crs(wkt).is_err(), "{wkt}");
        }
    }
}

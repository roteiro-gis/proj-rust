use serde_json::Value;
use std::collections::HashMap;

use crate::{ParseError, Result};
use proj_core::{CrsDef, Datum, GeographicCrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod};

pub(crate) fn parse_projjson(s: &str) -> Result<CrsDef> {
    let value: Value =
        serde_json::from_str(s).map_err(|e| ParseError::Parse(format!("invalid PROJJSON: {e}")))?;

    if let Some(epsg) = top_level_epsg_id(&value) {
        return proj_core::lookup_epsg(epsg).ok_or_else(|| {
            ParseError::Parse(format!("unsupported EPSG code in PROJJSON: {epsg}"))
        });
    }

    let crs_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ParseError::Parse("PROJJSON object is missing a CRS type".into()))?;

    match crs_type {
        "GeographicCRS" | "GeodeticCRS" => {
            let datum = infer_datum(&value)?;
            Ok(CrsDef::Geographic(GeographicCrsDef::new(0, datum, "")))
        }
        "ProjectedCRS" => parse_projected_projjson(&value),
        other => Err(ParseError::Parse(format!(
            "unsupported PROJJSON CRS without an EPSG id: {other}"
        ))),
    }
}

fn parse_projected_projjson(value: &Value) -> Result<CrsDef> {
    let conversion = value
        .get("conversion")
        .ok_or_else(|| ParseError::Parse("PROJJSON projected CRS is missing conversion".into()))?;
    let datum = infer_datum(value)?;
    let linear_unit = projected_linear_unit(value).unwrap_or_else(LinearUnit::metre);
    let base_angle_unit_to_degree = base_geographic_angle_unit_to_degree(value).unwrap_or(1.0);
    let method_name = conversion
        .get("method")
        .and_then(|method| method.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ParseError::Parse("PROJJSON projected CRS is missing conversion.method.name".into())
        })?;
    let params = parse_parameters(conversion, linear_unit, base_angle_unit_to_degree);

    let lon0 = first_param(
        &params,
        &[
            "centralmeridian",
            "longitudeofcenter",
            "longitudeofnaturalorigin",
            "longitudeoffalseorigin",
        ],
    )
    .unwrap_or(0.0);
    let lat0 = first_param(
        &params,
        &[
            "latitudeoforigin",
            "latitudeofcenter",
            "latitudeofnaturalorigin",
            "latitudeoffalseorigin",
        ],
    )
    .unwrap_or(0.0);
    let k0 = first_param(
        &params,
        &[
            "scalefactor",
            "scalefactoratnaturalorigin",
            "scalefactoratprojectionorigin",
        ],
    )
    .unwrap_or(1.0);
    let fe = first_param(&params, &["falseeasting"]).unwrap_or(0.0);
    let fn_ = first_param(&params, &["falsenorthing"]).unwrap_or(0.0);
    let normalized_method = normalize_key(method_name);

    let method = match normalized_method.as_str() {
        "transversemercator" => ProjectionMethod::TransverseMercator {
            lon0,
            lat0,
            k0,
            false_easting: fe,
            false_northing: fn_,
        },
        name if name.starts_with("mercator") => ProjectionMethod::Mercator {
            lon0,
            lat_ts: first_param(
                &params,
                &[
                    "standardparallel1",
                    "latitudeof1ststandardparallel",
                    "latitudeofstandardparallel",
                ],
            )
            .unwrap_or(0.0),
            k0,
            false_easting: fe,
            false_northing: fn_,
        },
        "lambertconformalconic1sp" | "lambertconformalconic2sp" | "lambertconformalconic" => {
            ProjectionMethod::LambertConformalConic {
                lon0,
                lat0,
                lat1: first_param(
                    &params,
                    &["standardparallel1", "latitudeof1ststandardparallel"],
                )
                .unwrap_or(lat0),
                lat2: first_param(
                    &params,
                    &["standardparallel2", "latitudeof2ndstandardparallel"],
                )
                .unwrap_or(lat0),
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "albersequalarea" | "albersequalareaconic" => ProjectionMethod::AlbersEqualArea {
            lon0,
            lat0,
            lat1: first_param(
                &params,
                &["standardparallel1", "latitudeof1ststandardparallel"],
            )
            .unwrap_or(lat0),
            lat2: first_param(
                &params,
                &["standardparallel2", "latitudeof2ndstandardparallel"],
            )
            .unwrap_or(lat0),
            false_easting: fe,
            false_northing: fn_,
        },
        "polarstereographicvarianta" | "polarstereographicvariantb" | "polarstereographic" => {
            ProjectionMethod::PolarStereographic {
                lon0,
                lat_ts: first_param(
                    &params,
                    &[
                        "standardparallel",
                        "latitudeofstandardparallel",
                        "latitudeof1ststandardparallel",
                    ],
                )
                .unwrap_or(lat0),
                k0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "equidistantcylindrical" | "platecarree" => ProjectionMethod::EquidistantCylindrical {
            lon0,
            lat_ts: first_param(
                &params,
                &[
                    "standardparallel1",
                    "latitudeof1ststandardparallel",
                    "latitudeofstandardparallel",
                ],
            )
            .unwrap_or(0.0),
            false_easting: fe,
            false_northing: fn_,
        },
        _ => {
            return Err(ParseError::Parse(format!(
                "unsupported PROJJSON conversion method: {method_name}"
            )));
        }
    };

    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        datum,
        method,
        linear_unit,
        "",
    )))
}

fn top_level_epsg_id(value: &Value) -> Option<u32> {
    let id = value.get("id")?;
    let authority = id.get("authority")?.as_str()?;
    if !authority.eq_ignore_ascii_case("EPSG") {
        return None;
    }

    match id.get("code")? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn infer_datum(value: &Value) -> Result<Datum> {
    if any_name_contains(value, &["WORLD GEODETIC SYSTEM 1984", "WGS 84", "WGS84"]) {
        return Ok(proj_core::datum::WGS84);
    }
    if any_name_contains(value, &["NORTH AMERICAN DATUM 1983", "NAD83"]) {
        return Ok(proj_core::datum::NAD83);
    }
    if any_name_contains(value, &["NORTH AMERICAN DATUM 1927", "NAD27"]) {
        return Ok(proj_core::datum::NAD27);
    }
    if any_name_contains(value, &["ETRS89", "ETRS 89"]) {
        return Ok(proj_core::datum::ETRS89);
    }
    if any_name_contains(value, &["OSGB", "ORDNANCE SURVEY GREAT BRITAIN 1936"]) {
        return Ok(proj_core::datum::OSGB36);
    }
    if any_name_contains(value, &["ED50", "EUROPEAN DATUM 1950"]) {
        return Ok(proj_core::datum::ED50);
    }
    if any_name_contains(value, &["PULKOVO"]) {
        return Ok(proj_core::datum::PULKOVO1942);
    }
    if any_name_contains(value, &["TOKYO"]) {
        return Ok(proj_core::datum::TOKYO);
    }

    Err(ParseError::Parse(
        "unsupported PROJJSON datum or CRS definition".into(),
    ))
}

fn any_name_contains(value: &Value, needles: &[&str]) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, val)| {
            (key == "name"
                && val.as_str().is_some_and(|s| {
                    needles
                        .iter()
                        .any(|needle| contains_ascii_case_insensitive(s, needle))
                }))
                || any_name_contains(val, needles)
        }),
        Value::Array(values) => values.iter().any(|val| any_name_contains(val, needles)),
        _ => false,
    }
}

fn parse_parameters(
    conversion: &Value,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> HashMap<String, f64> {
    let mut params = HashMap::new();
    let values = match conversion.get("parameters").and_then(Value::as_array) {
        Some(values) => values,
        None => return params,
    };

    for param in values {
        let Some(name) = param.get("name").and_then(Value::as_str) else {
            continue;
        };
        let normalized_name = normalize_key(name);
        let value = match param.get("value") {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.parse::<f64>().ok(),
            _ => None,
        };
        if let Some(value) = value {
            let factor = parameter_factor_from_json(
                param,
                &normalized_name,
                projected_linear_unit,
                base_angle_unit_to_degree,
            );
            params.insert(normalized_name, value * factor);
        }
    }

    params
}

#[derive(Clone, Copy)]
enum ParameterUnitKind {
    Angle,
    Length,
    Scale,
    Other,
}

fn parameter_factor_from_json(
    param: &Value,
    normalized_name: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> f64 {
    let unit_kind = parameter_unit_kind(normalized_name);
    match unit_kind {
        ParameterUnitKind::Angle => param
            .get("unit")
            .and_then(angle_unit_to_degree_from_json)
            .or_else(|| {
                param
                    .get("unit_conversion_factor")
                    .and_then(Value::as_f64)
                    .map(radians_to_degrees_factor)
            })
            .or_else(|| {
                param
                    .get("conversion_factor")
                    .and_then(Value::as_f64)
                    .map(radians_to_degrees_factor)
            })
            .unwrap_or(base_angle_unit_to_degree),
        ParameterUnitKind::Length => param
            .get("unit")
            .and_then(linear_unit_from_json)
            .map(LinearUnit::meters_per_unit)
            .or_else(|| param.get("unit_conversion_factor").and_then(Value::as_f64))
            .or_else(|| param.get("conversion_factor").and_then(Value::as_f64))
            .unwrap_or(projected_linear_unit.meters_per_unit()),
        ParameterUnitKind::Scale | ParameterUnitKind::Other => 1.0,
    }
}

fn parameter_unit_kind(normalized_name: &str) -> ParameterUnitKind {
    match normalized_name {
        "centralmeridian"
        | "longitudeofcenter"
        | "longitudeofnaturalorigin"
        | "longitudeoffalseorigin"
        | "longitudeoforigin"
        | "latitudeoforigin"
        | "latitudeofcenter"
        | "latitudeofnaturalorigin"
        | "latitudeoffalseorigin"
        | "standardparallel"
        | "standardparallel1"
        | "standardparallel2"
        | "latitudeofstandardparallel"
        | "latitudeof1ststandardparallel"
        | "latitudeof2ndstandardparallel" => ParameterUnitKind::Angle,
        "falseeasting" | "falsenorthing" | "eastingatfalseorigin" | "northingatfalseorigin" => {
            ParameterUnitKind::Length
        }
        "scalefactor" | "scalefactoratnaturalorigin" | "scalefactoratprojectionorigin" => {
            ParameterUnitKind::Scale
        }
        _ => ParameterUnitKind::Other,
    }
}

fn projected_linear_unit(value: &Value) -> Option<LinearUnit> {
    value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
        .and_then(|axis| axis.first())
        .and_then(axis_linear_unit)
}

fn base_geographic_angle_unit_to_degree(value: &Value) -> Option<f64> {
    value
        .get("base_crs")
        .and_then(|crs| crs.get("coordinate_system"))
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
        .and_then(|axis| axis.first())
        .and_then(axis_angle_unit_to_degree)
}

fn axis_linear_unit(axis: &Value) -> Option<LinearUnit> {
    axis.get("unit")
        .and_then(linear_unit_from_json)
        .or_else(|| {
            axis.get("unit_conversion_factor")
                .and_then(Value::as_f64)
                .and_then(|factor| LinearUnit::from_meters_per_unit(factor).ok())
        })
        .or_else(|| {
            axis.get("conversion_factor")
                .and_then(Value::as_f64)
                .and_then(|factor| LinearUnit::from_meters_per_unit(factor).ok())
        })
}

fn axis_angle_unit_to_degree(axis: &Value) -> Option<f64> {
    axis.get("unit")
        .and_then(angle_unit_to_degree_from_json)
        .or_else(|| {
            axis.get("unit_conversion_factor")
                .and_then(Value::as_f64)
                .map(radians_to_degrees_factor)
        })
        .or_else(|| {
            axis.get("conversion_factor")
                .and_then(Value::as_f64)
                .map(radians_to_degrees_factor)
        })
}

fn linear_unit_from_json(value: &Value) -> Option<LinearUnit> {
    if let Some(unit) = value.as_str() {
        return linear_unit_name(unit);
    }

    if let Some(factor) = value.get("conversion_factor").and_then(Value::as_f64) {
        return LinearUnit::from_meters_per_unit(factor).ok();
    }
    if let Some(factor) = value.get("unit_conversion_factor").and_then(Value::as_f64) {
        return LinearUnit::from_meters_per_unit(factor).ok();
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .and_then(linear_unit_name)
}

fn angle_unit_to_degree_from_json(value: &Value) -> Option<f64> {
    if let Some(unit) = value.as_str() {
        return angle_unit_name_to_degree(unit);
    }

    if let Some(factor) = value.get("conversion_factor").and_then(Value::as_f64) {
        return Some(radians_to_degrees_factor(factor));
    }
    if let Some(factor) = value.get("unit_conversion_factor").and_then(Value::as_f64) {
        return Some(radians_to_degrees_factor(factor));
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .and_then(angle_unit_name_to_degree)
}

fn linear_unit_name(name: &str) -> Option<LinearUnit> {
    match normalize_key(name).as_str() {
        "metre" | "meter" => Some(LinearUnit::metre()),
        "kilometre" | "kilometer" => Some(LinearUnit::kilometre()),
        "foot" | "internationalfoot" | "ft" => Some(LinearUnit::foot()),
        "ussurveyfoot" | "usfoot" | "usft" => Some(LinearUnit::us_survey_foot()),
        "yard" => LinearUnit::from_meters_per_unit(0.9144).ok(),
        "nauticalmile" => LinearUnit::from_meters_per_unit(1852.0).ok(),
        _ => None,
    }
}

fn angle_unit_name_to_degree(name: &str) -> Option<f64> {
    match normalize_key(name).as_str() {
        "degree" => Some(1.0),
        "radian" => Some(radians_to_degrees_factor(1.0)),
        "grad" | "gon" => Some(0.9),
        _ => None,
    }
}

fn radians_to_degrees_factor(radians_per_unit: f64) -> f64 {
    radians_per_unit.to_degrees()
}

fn first_param(params: &HashMap<String, f64>, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| params.get(&normalize_key(name)).copied())
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }

    haystack
        .char_indices()
        .any(|(idx, _)| matches!(haystack.get(idx..idx + needle.len()), Some(slice) if slice.eq_ignore_ascii_case(needle)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

    #[test]
    fn parses_projjson_with_top_level_epsg_id() {
        let crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "WGS 84 / Pseudo-Mercator",
                "id": { "authority": "EPSG", "code": 3857 }
            }"#,
        )
        .unwrap();

        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn parses_projjson_wgs84_without_epsg_id() {
        let crs = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "WGS 84",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378137,
                        "inverse_flattening": 298.257223563
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(crs.is_geographic());
        assert_eq!(crs.datum().ellipsoid.a, proj_core::datum::WGS84.ellipsoid.a);
    }

    #[test]
    fn rejects_projjson_without_supported_definition() {
        let err = parse_projjson(r#"{ "type": "ProjectedCRS", "name": "Custom" }"#).unwrap_err();
        assert!(err.to_string().contains("missing conversion"));
    }

    #[test]
    fn parses_projected_projjson_without_epsg_id() {
        let crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": {
                        "name": "World Geodetic System 1984"
                    }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": -75 },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        { "name": "False easting", "value": 500000 },
                        { "name": "False northing", "value": 0 }
                    ]
                }
            }"#,
        )
        .unwrap();

        assert!(crs.is_projected());
    }

    #[test]
    fn parses_projected_projjson_with_foot_units() {
        let meter_crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N metre",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": { "name": "World Geodetic System 1984" }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": -75 },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        { "name": "False easting", "value": 500000, "unit": "metre" },
                        { "name": "False northing", "value": 0, "unit": "metre" }
                    ]
                },
                "coordinate_system": {
                    "subtype": "Cartesian",
                    "axis": [
                        { "name": "Easting", "direction": "east", "unit": "metre" },
                        { "name": "Northing", "direction": "north", "unit": "metre" }
                    ]
                }
            }"#,
        )
        .unwrap();
        let foot_crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N ftUS",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": { "name": "World Geodetic System 1984" }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": -75 },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        {
                            "name": "False easting",
                            "value": 1640416.6666666667,
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        },
                        {
                            "name": "False northing",
                            "value": 0,
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        }
                    ]
                },
                "coordinate_system": {
                    "subtype": "Cartesian",
                    "axis": [
                        {
                            "name": "Easting",
                            "direction": "east",
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        },
                        {
                            "name": "Northing",
                            "direction": "north",
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let from = proj_core::lookup_epsg(4326).unwrap();
        let meter_tx = proj_core::Transform::from_crs_defs(&from, &meter_crs).unwrap();
        let foot_tx = proj_core::Transform::from_crs_defs(&from, &foot_crs).unwrap();

        let (mx, my) = meter_tx.convert((-74.006, 40.7128)).unwrap();
        let (fx, fy) = foot_tx.convert((-74.006, 40.7128)).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02, "x mismatch");
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02, "y mismatch");
    }
}

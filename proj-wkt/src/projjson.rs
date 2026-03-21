use serde_json::Value;
use std::collections::HashMap;

use crate::{ParseError, Result};
use proj_core::{CrsDef, Datum, GeographicCrsDef, ProjectedCrsDef, ProjectionMethod};

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
            Ok(CrsDef::Geographic(GeographicCrsDef {
                epsg: 0,
                datum,
                name: "",
            }))
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
    let method_name = conversion
        .get("method")
        .and_then(|method| method.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ParseError::Parse("PROJJSON projected CRS is missing conversion.method.name".into())
        })?;
    let params = parse_parameters(conversion);

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

    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum,
        method,
        name: "",
    }))
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
    let mut text = String::new();
    collect_names(value, &mut text);
    let upper = text.to_uppercase();

    if upper.contains("WORLD GEODETIC SYSTEM 1984")
        || upper.contains("WGS 84")
        || upper.contains("WGS84")
    {
        return Ok(proj_core::datum::WGS84);
    }
    if upper.contains("NORTH AMERICAN DATUM 1983") || upper.contains("NAD83") {
        return Ok(proj_core::datum::NAD83);
    }
    if upper.contains("NORTH AMERICAN DATUM 1927") || upper.contains("NAD27") {
        return Ok(proj_core::datum::NAD27);
    }
    if upper.contains("ETRS89") || upper.contains("ETRS 89") {
        return Ok(proj_core::datum::ETRS89);
    }
    if upper.contains("OSGB") || upper.contains("ORDNANCE SURVEY GREAT BRITAIN 1936") {
        return Ok(proj_core::datum::OSGB36);
    }
    if upper.contains("ED50") || upper.contains("EUROPEAN DATUM 1950") {
        return Ok(proj_core::datum::ED50);
    }
    if upper.contains("PULKOVO") {
        return Ok(proj_core::datum::PULKOVO1942);
    }
    if upper.contains("TOKYO") {
        return Ok(proj_core::datum::TOKYO);
    }

    Err(ParseError::Parse(
        "unsupported PROJJSON datum or CRS definition".into(),
    ))
}

fn collect_names(value: &Value, text: &mut String) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                if key == "name" {
                    if let Some(s) = val.as_str() {
                        text.push_str(s);
                        text.push('\n');
                    }
                } else {
                    collect_names(val, text);
                }
            }
        }
        Value::Array(values) => {
            for val in values {
                collect_names(val, text);
            }
        }
        _ => {}
    }
}

fn parse_parameters(conversion: &Value) -> HashMap<String, f64> {
    let mut params = HashMap::new();
    let values = match conversion.get("parameters").and_then(Value::as_array) {
        Some(values) => values,
        None => return params,
    };

    for param in values {
        let Some(name) = param.get("name").and_then(Value::as_str) else {
            continue;
        };
        let value = match param.get("value") {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.parse::<f64>().ok(),
            _ => None,
        };
        if let Some(value) = value {
            params.insert(normalize_key(name), value);
        }
    }

    params
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

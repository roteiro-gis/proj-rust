use serde_json::Value;

use crate::{ParseError, Result};
use proj_core::{CrsDef, Datum, GeographicCrsDef};

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
        other => Err(ParseError::Parse(format!(
            "unsupported PROJJSON CRS without an EPSG id: {other}"
        ))),
    }
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
        assert!(err.to_string().contains("unsupported PROJJSON"));
    }
}

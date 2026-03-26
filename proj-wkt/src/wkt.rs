use crate::{ParseError, Result};
use proj_core::CrsDef;
use std::collections::HashMap;

/// Parse a WKT CRS string.
///
/// Strategy:
/// 1. Extract a top-level AUTHORITY["EPSG","XXXX"] or ID["EPSG",XXXX] if present
///    → look up in registry
/// 2. Otherwise, extract projection parameters from the WKT structure
pub(crate) fn parse_wkt(s: &str) -> Result<CrsDef> {
    // Try to extract a top-level CRS identifier first — most reliable approach.
    if let Some(epsg) = extract_top_level_epsg(s) {
        if let Some(crs) = proj_core::lookup_epsg(epsg) {
            return Ok(crs);
        }
    }

    // Try to extract projection info from WKT structure
    parse_wkt_structure(s)
}

/// Extract a top-level EPSG code from AUTHORITY[...] or ID[...].
fn extract_top_level_epsg(s: &str) -> Option<u32> {
    let root_start = s.find('[')?;
    let mut depth = 1usize;
    let mut i = root_start + 1;
    let bytes = s.as_bytes();
    let mut in_string = false;

    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                } else {
                    in_string = !in_string;
                    i += 1;
                }
            }
            _ if in_string => i += 1,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
            }
            _ if depth == 1 => {
                if let Some((name, inner, next)) = parse_wkt_element(s, i) {
                    if name.eq_ignore_ascii_case("AUTHORITY") || name.eq_ignore_ascii_case("ID") {
                        if let Some(epsg) = parse_epsg_element(inner) {
                            return Some(epsg);
                        }
                    }
                    i = next;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    None
}

fn parse_wkt_element(s: &str, start: usize) -> Option<(&str, &str, usize)> {
    let bytes = s.as_bytes();
    let first = *bytes.get(start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }

    let mut name_end = start + 1;
    while let Some(byte) = bytes.get(name_end) {
        if byte.is_ascii_alphanumeric() || *byte == b'_' {
            name_end += 1;
        } else {
            break;
        }
    }

    let mut bracket_start = name_end;
    while let Some(byte) = bytes.get(bracket_start) {
        if byte.is_ascii_whitespace() {
            bracket_start += 1;
        } else {
            break;
        }
    }
    if bytes.get(bracket_start) != Some(&b'[') {
        return None;
    }

    let mut depth = 1usize;
    let mut i = bracket_start + 1;
    let mut in_string = false;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                } else {
                    in_string = !in_string;
                    i += 1;
                }
            }
            _ if in_string => i += 1,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    let name = &s[start..name_end];
                    let inner = &s[bracket_start + 1..i - 1];
                    return Some((name, inner, i));
                }
            }
            _ => i += 1,
        }
    }

    None
}

fn parse_epsg_element(inner: &str) -> Option<u32> {
    let (authority, code) = first_two_fields(inner)?;
    if !trim_wkt_token(authority).eq_ignore_ascii_case("EPSG") {
        return None;
    }
    trim_wkt_token(code).parse().ok()
}

fn first_two_fields(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut in_string = false;
    let bytes = s.as_bytes();

    for (idx, byte) in bytes.iter().enumerate() {
        match *byte {
            b'"' => {
                if in_string && bytes.get(idx + 1) == Some(&b'"') {
                    continue;
                }
                in_string = !in_string;
            }
            _ if in_string => {}
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                return Some((&s[..idx], &s[idx + 1..]));
            }
            _ => {}
        }
    }

    None
}

fn trim_wkt_token(token: &str) -> &str {
    token.trim().trim_matches('"')
}

/// Attempt to parse WKT structure to extract projection parameters.
fn parse_wkt_structure(s: &str) -> Result<CrsDef> {
    let upper = s.to_uppercase();

    if upper.starts_with("GEOGCS") || upper.starts_with("GEODCRS") || upper.starts_with("GEOGCRS") {
        return parse_wkt_geographic(s);
    }

    if upper.starts_with("PROJCS") || upper.starts_with("PROJCRS") {
        return parse_wkt_projected(s);
    }

    Err(ParseError::Parse(format!(
        "unrecognized WKT root element: {:.40}",
        s
    )))
}

fn parse_wkt_geographic(s: &str) -> Result<CrsDef> {
    // Extract datum name to determine which datum to use
    let upper = s.to_uppercase();
    let datum = infer_datum(&upper)?;

    Ok(CrsDef::Geographic(proj_core::GeographicCrsDef {
        epsg: 0,
        datum,
        name: "",
    }))
}

fn parse_wkt_projected(s: &str) -> Result<CrsDef> {
    let upper = s.to_uppercase();

    // WKT1 uses PROJECTION["name"], WKT2 uses METHOD["name"].
    let proj_name =
        extract_wkt_value(&upper, "PROJECTION").or_else(|| extract_wkt_value(&upper, "METHOD"));
    let normalized_method = proj_name.as_deref().map(normalize_key).ok_or_else(|| {
        ParseError::Parse("WKT projected CRS is missing a projection method".into())
    })?;

    let params = parse_wkt_parameters(s);

    // Extract common parameters
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

    // Determine datum from GEOGCS section
    let datum = infer_datum(&upper)?;

    let method = match normalized_method.as_str() {
        "transversemercator" => proj_core::ProjectionMethod::TransverseMercator {
            lon0,
            lat0,
            k0,
            false_easting: fe,
            false_northing: fn_,
        },
        name if name.starts_with("mercator") => proj_core::ProjectionMethod::Mercator {
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
            proj_core::ProjectionMethod::LambertConformalConic {
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
        "albersequalareaconic" | "albersequalarea" => {
            proj_core::ProjectionMethod::AlbersEqualArea {
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
        "polarstereographicvarianta" | "polarstereographicvariantb" | "polarstereographic" => {
            proj_core::ProjectionMethod::PolarStereographic {
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
        "equidistantcylindrical" | "platecarree" => {
            proj_core::ProjectionMethod::EquidistantCylindrical {
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
            }
        }
        _ => {
            return Err(ParseError::Parse(format!(
                "unsupported WKT projection: {}",
                proj_name.as_deref().unwrap_or("(none)")
            )));
        }
    };

    Ok(CrsDef::Projected(proj_core::ProjectedCrsDef {
        epsg: 0,
        datum,
        method,
        name: "",
    }))
}

fn infer_datum(upper: &str) -> Result<proj_core::Datum> {
    if upper.contains("WGS 84") || upper.contains("WGS84") || upper.contains("WGS_1984") {
        return Ok(proj_core::datum::WGS84);
    }
    if upper.contains("NAD83") || upper.contains("NAD 83") || upper.contains("NORTH_AMERICAN_1983")
    {
        return Ok(proj_core::datum::NAD83);
    }
    if upper.contains("NAD27") || upper.contains("NAD 27") || upper.contains("NORTH_AMERICAN_1927")
    {
        return Ok(proj_core::datum::NAD27);
    }
    if upper.contains("ETRS89") || upper.contains("ETRS 89") {
        return Ok(proj_core::datum::ETRS89);
    }
    if upper.contains("OSGB")
        || upper.contains("AIRY")
        || upper.contains("ORDNANCE_SURVEY_GREAT_BRITAIN_1936")
    {
        return Ok(proj_core::datum::OSGB36);
    }
    if upper.contains("ED50") || upper.contains("EUROPEAN_DATUM_1950") {
        return Ok(proj_core::datum::ED50);
    }
    if upper.contains("PULKOVO") {
        return Ok(proj_core::datum::PULKOVO1942);
    }
    if upper.contains("TOKYO") {
        return Ok(proj_core::datum::TOKYO);
    }

    Err(ParseError::Parse(
        "unsupported or unrecognized WKT datum".into(),
    ))
}

/// Extract a quoted value like PROJECTION["Transverse_Mercator"]
fn extract_wkt_value(upper: &str, key: &str) -> Option<String> {
    let marker = format!("{key}[\"");
    let pos = upper.find(&marker)?;
    let start = pos + marker.len();
    let rest = &upper[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_wkt_parameters(s: &str) -> HashMap<String, f64> {
    let upper = s.to_uppercase();
    let mut params = HashMap::new();
    let mut search_start = 0usize;

    while let Some(rel) = upper[search_start..].find("PARAMETER[") {
        let start = search_start + rel + "PARAMETER[".len();
        let rest = &s[start..];
        let quote_start = match rest.find('"') {
            Some(pos) => pos,
            None => break,
        };
        let name_start = start + quote_start + 1;
        let name_rest = &s[name_start..];
        let name_end = match name_rest.find('"') {
            Some(pos) => pos,
            None => break,
        };
        let name = &name_rest[..name_end];
        let after_name = &name_rest[name_end + 1..];
        let comma = match after_name.find(',') {
            Some(pos) => pos,
            None => break,
        };
        let value_rest = after_name[comma + 1..].trim_start();
        let value_len = value_rest
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
            .unwrap_or(value_rest.len());

        if let Ok(value) = value_rest[..value_len].parse::<f64>() {
            params.insert(normalize_key(name), value);
        }

        search_start = name_start + name_end + 1;
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
    fn extract_top_level_epsg_from_wkt() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(extract_top_level_epsg(wkt), Some(4326));
    }

    #[test]
    fn parse_wkt_geogcs_with_authority() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn parse_wkt_projcs_utm() {
        let wkt = r#"PROJCS["WGS 84 / UTM zone 18N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],AUTHORITY["EPSG","32618"]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 32618);
    }

    #[test]
    fn parse_wkt_without_authority() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
    }

    #[test]
    fn parse_wkt_projcs_no_authority() {
        let wkt = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn parse_wkt_projcs_ignores_nested_base_authority() {
        let wkt = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 0);
    }

    #[test]
    fn reject_unknown_geographic_datum() {
        let err =
            parse_wkt(r#"GEOGCS["Unknown",DATUM["Custom",SPHEROID["Custom",1,1]]]"#).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported or unrecognized WKT datum"));
    }

    #[test]
    fn parse_wkt2_projected_without_authority() {
        let wkt = r#"PROJCRS["WGS 84 / UTM zone 18N",BASEGEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]]],CONVERSION["UTM zone 18N",METHOD["Transverse Mercator"],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",-75,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1]],PARAMETER["False northing",0,LENGTHUNIT["metre",1]]],CS[Cartesian,2],AXIS["easting",east],AXIS["northing",north],LENGTHUNIT["metre",1]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn parse_wkt2_geographic_with_id() {
        let wkt = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }
}

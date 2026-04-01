use crate::{ParseError, Result};
use proj_core::{CrsDef, GeographicCrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod};
use std::collections::HashMap;

/// Parse a WKT CRS string.
///
/// Strategy:
/// 1. Extract a top-level AUTHORITY["EPSG","XXXX"] or ID["EPSG",XXXX] if present
///    → look up in registry
/// 2. Otherwise, extract projection parameters from the WKT structure
pub(crate) fn parse_wkt(s: &str) -> Result<CrsDef> {
    let s = s.trim();

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
    let Some((root_name, _, _)) = parse_wkt_element(s, 0) else {
        return Err(ParseError::Parse(format!(
            "unrecognized WKT root element: {:.40}",
            s
        )));
    };

    if root_name.eq_ignore_ascii_case("GEOGCS")
        || root_name.eq_ignore_ascii_case("GEODCRS")
        || root_name.eq_ignore_ascii_case("GEOGCRS")
    {
        return parse_wkt_geographic(s);
    }

    if root_name.eq_ignore_ascii_case("PROJCS") || root_name.eq_ignore_ascii_case("PROJCRS") {
        return parse_wkt_projected(s);
    }

    Err(ParseError::Parse(format!(
        "unrecognized WKT root element: {:.40}",
        s
    )))
}

fn parse_wkt_geographic(s: &str) -> Result<CrsDef> {
    // Extract datum name to determine which datum to use
    let datum = infer_datum(s)?;

    Ok(CrsDef::Geographic(GeographicCrsDef::new(0, datum, "")))
}

fn parse_wkt_projected(s: &str) -> Result<CrsDef> {
    // WKT1 uses PROJECTION["name"], WKT2 uses METHOD["name"].
    let proj_name = extract_wkt_value_case_insensitive(s, "PROJECTION")
        .or_else(|| extract_wkt_value_case_insensitive(s, "METHOD"));
    let normalized_method = proj_name.as_deref().map(normalize_key).ok_or_else(|| {
        ParseError::Parse("WKT projected CRS is missing a projection method".into())
    })?;

    let projected_linear_unit = extract_projected_linear_unit(s).unwrap_or_else(LinearUnit::metre);
    let base_angle_unit_to_degree = extract_base_geographic_angle_unit_to_degree(s).unwrap_or(1.0);
    let params = parse_wkt_parameters(s, projected_linear_unit, base_angle_unit_to_degree);

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
    let datum = infer_datum(s)?;

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
        "albersequalareaconic" | "albersequalarea" => ProjectionMethod::AlbersEqualArea {
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
                "unsupported WKT projection: {}",
                proj_name.as_deref().unwrap_or("(none)")
            )));
        }
    };

    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        datum,
        method,
        projected_linear_unit,
        "",
    )))
}

fn infer_datum(s: &str) -> Result<proj_core::Datum> {
    if contains_ascii_case_insensitive(s, "WGS 84")
        || contains_ascii_case_insensitive(s, "WGS84")
        || contains_ascii_case_insensitive(s, "WGS_1984")
    {
        return Ok(proj_core::datum::WGS84);
    }
    if contains_ascii_case_insensitive(s, "NAD83")
        || contains_ascii_case_insensitive(s, "NAD 83")
        || contains_ascii_case_insensitive(s, "NORTH_AMERICAN_1983")
    {
        return Ok(proj_core::datum::NAD83);
    }
    if contains_ascii_case_insensitive(s, "NAD27")
        || contains_ascii_case_insensitive(s, "NAD 27")
        || contains_ascii_case_insensitive(s, "NORTH_AMERICAN_1927")
    {
        return Ok(proj_core::datum::NAD27);
    }
    if contains_ascii_case_insensitive(s, "ETRS89") || contains_ascii_case_insensitive(s, "ETRS 89")
    {
        return Ok(proj_core::datum::ETRS89);
    }
    if contains_ascii_case_insensitive(s, "OSGB")
        || contains_ascii_case_insensitive(s, "AIRY")
        || contains_ascii_case_insensitive(s, "ORDNANCE_SURVEY_GREAT_BRITAIN_1936")
    {
        return Ok(proj_core::datum::OSGB36);
    }
    if contains_ascii_case_insensitive(s, "ED50")
        || contains_ascii_case_insensitive(s, "EUROPEAN_DATUM_1950")
    {
        return Ok(proj_core::datum::ED50);
    }
    if contains_ascii_case_insensitive(s, "PULKOVO") {
        return Ok(proj_core::datum::PULKOVO1942);
    }
    if contains_ascii_case_insensitive(s, "TOKYO") {
        return Ok(proj_core::datum::TOKYO);
    }

    Err(ParseError::Parse(
        "unsupported or unrecognized WKT datum".into(),
    ))
}

/// Extract a quoted value like PROJECTION["Transverse_Mercator"].
fn extract_wkt_value_case_insensitive(s: &str, key: &str) -> Option<String> {
    let marker = format!("{key}[\"");
    let pos = find_ascii_case_insensitive(s, &marker)?;
    let start = pos + marker.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_wkt_parameters(
    s: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> HashMap<String, f64> {
    let mut params = HashMap::new();
    let mut search_start = 0usize;

    while let Some(rel) = find_ascii_case_insensitive(&s[search_start..], "PARAMETER[") {
        let start = search_start + rel;
        if let Some((name, inner, next)) = parse_wkt_element(s, start) {
            if name.eq_ignore_ascii_case("PARAMETER") {
                if let Some((key, value)) =
                    parse_parameter_element(inner, projected_linear_unit, base_angle_unit_to_degree)
                {
                    params.insert(key, value);
                }
                search_start = next;
                continue;
            }
        }
        search_start = start + "PARAMETER[".len();
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

fn parse_parameter_element(
    inner: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> Option<(String, f64)> {
    let fields = split_top_level_fields(inner);
    if fields.len() < 2 {
        return None;
    }

    let name = trim_wkt_token(fields[0]);
    let normalized_name = normalize_key(name);
    let value = fields[1].trim().parse::<f64>().ok()?;
    let unit_kind = parameter_unit_kind(&normalized_name);

    let nested_factor = fields.iter().skip(2).find_map(|field| {
        let field = field.trim();
        let (unit_name, unit_inner, _) = parse_wkt_element(field, 0)?;
        match unit_name.to_ascii_uppercase().as_str() {
            "ANGLEUNIT" => parse_unit_factor(unit_inner).map(radians_to_degrees_factor),
            "LENGTHUNIT" | "UNIT" => parse_unit_factor(unit_inner),
            "SCALEUNIT" => parse_unit_factor(unit_inner),
            _ => None,
        }
    });

    let default_factor = match unit_kind {
        ParameterUnitKind::Angle => base_angle_unit_to_degree,
        ParameterUnitKind::Length => projected_linear_unit.meters_per_unit(),
        ParameterUnitKind::Scale | ParameterUnitKind::Other => 1.0,
    };

    Some((
        normalized_name,
        value * nested_factor.unwrap_or(default_factor),
    ))
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

fn split_top_level_fields(s: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut field_start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                    continue;
                }
                in_string = !in_string;
            }
            _ if in_string => {}
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                fields.push(s[field_start..i].trim());
                field_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }

    fields.push(s[field_start..].trim());
    fields
}

fn root_inner(s: &str) -> Option<&str> {
    parse_wkt_element(s, 0).map(|(_, inner, _)| inner)
}

fn extract_projected_linear_unit(s: &str) -> Option<LinearUnit> {
    let inner = root_inner(s)?;
    let mut linear_unit = None;
    for_each_top_level_element(inner, |name, element_inner| {
        if name.eq_ignore_ascii_case("UNIT") || name.eq_ignore_ascii_case("LENGTHUNIT") {
            linear_unit = parse_unit_factor(element_inner)
                .and_then(|factor| LinearUnit::from_meters_per_unit(factor).ok());
        }
    });
    linear_unit
}

fn extract_base_geographic_angle_unit_to_degree(s: &str) -> Option<f64> {
    for name in ["BASEGEOGCRS", "GEOGCRS", "GEODCRS", "GEOGCS"] {
        if let Some(start) = find_ascii_case_insensitive(s, name) {
            if let Some((_, inner, _)) = parse_wkt_element(s, start) {
                let mut factor = None;
                for_each_top_level_element(inner, |unit_name, unit_inner| {
                    if unit_name.eq_ignore_ascii_case("UNIT")
                        || unit_name.eq_ignore_ascii_case("ANGLEUNIT")
                    {
                        factor = parse_unit_factor(unit_inner).map(radians_to_degrees_factor);
                    }
                });
                if factor.is_some() {
                    return factor;
                }
            }
        }
    }
    None
}

fn for_each_top_level_element<'a, F>(inner: &'a str, mut f: F)
where
    F: FnMut(&'a str, &'a str),
{
    let mut i = 0usize;
    let bytes = inner.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                    continue;
                }
                in_string = !in_string;
                i += 1;
            }
            _ if in_string => i += 1,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ if depth == 0 => {
                if let Some((name, element_inner, next)) = parse_wkt_element(inner, i) {
                    f(name, element_inner);
                    i = next;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

fn parse_unit_factor(inner: &str) -> Option<f64> {
    let fields = split_top_level_fields(inner);
    fields.get(1)?.trim().parse::<f64>().ok()
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
    find_ascii_case_insensitive(haystack, needle).is_some()
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    haystack.char_indices().find_map(|(idx, _)| {
        haystack
            .get(idx..idx + needle.len())
            .filter(|slice| slice.eq_ignore_ascii_case(needle))
            .map(|_| idx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

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
    fn parse_wkt_projcs_with_foot_units() {
        let meter_wkt = r#"PROJCS["UTM 18N metre",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
        let foot_wkt = r#"PROJCS["UTM 18N ftUS",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",1640416.6666666667],PARAMETER["false_northing",0],UNIT["Foot_US",0.3048006096012192]]"#;

        let meter_crs = parse_wkt(meter_wkt).unwrap();
        let foot_crs = parse_wkt(foot_wkt).unwrap();
        let from = proj_core::lookup_epsg(4326).unwrap();

        let meter_tx = proj_core::Transform::from_crs_defs(&from, &meter_crs).unwrap();
        let foot_tx = proj_core::Transform::from_crs_defs(&from, &foot_crs).unwrap();

        let (mx, my) = meter_tx.convert((-74.006, 40.7128)).unwrap();
        let (fx, fy) = foot_tx.convert((-74.006, 40.7128)).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02, "x mismatch");
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02, "y mismatch");
    }

    #[test]
    fn parse_wkt2_geographic_with_id() {
        let wkt = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }
}

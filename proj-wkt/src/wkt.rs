use crate::{ParseError, Result};
use proj_core::CrsDef;

/// Parse a WKT CRS string.
///
/// Strategy:
/// 1. Extract AUTHORITY["EPSG","XXXX"] if present → look up in registry
/// 2. Otherwise, extract projection parameters from the WKT structure
pub(crate) fn parse_wkt(s: &str) -> Result<CrsDef> {
    // Try to extract AUTHORITY tag first — most reliable approach
    if let Some(epsg) = extract_authority_epsg(s) {
        if let Some(crs) = proj_core::lookup_epsg(epsg) {
            return Ok(crs);
        }
    }

    // Try to extract projection info from WKT structure
    parse_wkt_structure(s)
}

/// Extract EPSG code from AUTHORITY["EPSG","XXXX"] anywhere in the string.
fn extract_authority_epsg(s: &str) -> Option<u32> {
    // Look for AUTHORITY["EPSG","XXXX"] pattern
    let upper = s.to_uppercase();
    let auth_pos = upper.find("AUTHORITY[\"EPSG\"")?;
    let rest = &s[auth_pos..];

    // Find the code between the second quote pair
    let mut in_quotes = false;
    let mut quote_count = 0;
    let mut code_start = 0;
    let mut code_end = 0;

    for (i, ch) in rest.char_indices() {
        if ch == '"' {
            if in_quotes {
                in_quotes = false;
                quote_count += 1;
                if quote_count == 4 {
                    code_end = i;
                    break;
                }
            } else {
                in_quotes = true;
                if quote_count == 2 {
                    // This is the opening quote of the code value
                    // But we need to handle AUTHORITY["EPSG","4326"]
                    // quote_count 0,1 = EPSG quotes, 2,3 = code quotes
                }
                if quote_count == 3 {
                    code_start = i + 1;
                }
            }
        }
    }

    // Simpler approach: use regex-like extraction
    // Find pattern: AUTHORITY["EPSG","NNNN"]
    let marker = "AUTHORITY[\"EPSG\",\"";
    let upper_marker = marker.to_uppercase();
    let pos = upper.find(&upper_marker)?;
    let start = pos + marker.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    let code_str = &rest[..end];
    let _ = (code_start, code_end); // suppress unused warnings from earlier approach
    code_str.parse().ok()
}

/// Attempt to parse WKT structure to extract projection parameters.
fn parse_wkt_structure(s: &str) -> Result<CrsDef> {
    let upper = s.to_uppercase();

    if upper.starts_with("GEOGCS") || upper.starts_with("GEODCRS") {
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

    let datum = if upper.contains("WGS 84") || upper.contains("WGS84") || upper.contains("WGS_1984")
    {
        proj_core::datum::WGS84
    } else if upper.contains("NAD83")
        || upper.contains("NAD 83")
        || upper.contains("NORTH_AMERICAN_1983")
    {
        proj_core::datum::NAD83
    } else if upper.contains("NAD27")
        || upper.contains("NAD 27")
        || upper.contains("NORTH_AMERICAN_1927")
    {
        proj_core::datum::NAD27
    } else if upper.contains("ETRS89") || upper.contains("ETRS 89") {
        proj_core::datum::ETRS89
    } else if upper.contains("OSGB") || upper.contains("AIRY") {
        proj_core::datum::OSGB36
    } else {
        // Default to WGS84 if we can't identify the datum
        proj_core::datum::WGS84
    };

    Ok(CrsDef::Geographic(proj_core::GeographicCrsDef {
        epsg: 0,
        datum,
        name: "",
    }))
}

fn parse_wkt_projected(s: &str) -> Result<CrsDef> {
    let upper = s.to_uppercase();

    // Try to identify the projection method from WKT PROJECTION["name"]
    let proj_name = extract_wkt_value(&upper, "PROJECTION");

    // Extract common parameters
    let lon0 = extract_wkt_parameter(&upper, "CENTRAL_MERIDIAN")
        .or_else(|| extract_wkt_parameter(&upper, "LONGITUDE_OF_CENTER"))
        .unwrap_or(0.0);
    let lat0 = extract_wkt_parameter(&upper, "LATITUDE_OF_ORIGIN")
        .or_else(|| extract_wkt_parameter(&upper, "LATITUDE_OF_CENTER"))
        .unwrap_or(0.0);
    let k0 = extract_wkt_parameter(&upper, "SCALE_FACTOR").unwrap_or(1.0);
    let fe = extract_wkt_parameter(&upper, "FALSE_EASTING").unwrap_or(0.0);
    let fn_ = extract_wkt_parameter(&upper, "FALSE_NORTHING").unwrap_or(0.0);

    // Determine datum from GEOGCS section
    let datum = if upper.contains("WGS 84") || upper.contains("WGS84") || upper.contains("WGS_1984")
    {
        proj_core::datum::WGS84
    } else if upper.contains("NAD83") || upper.contains("NAD 83") {
        proj_core::datum::NAD83
    } else if upper.contains("NAD27") || upper.contains("NAD 27") {
        proj_core::datum::NAD27
    } else if upper.contains("OSGB") {
        proj_core::datum::OSGB36
    } else {
        proj_core::datum::WGS84
    };

    let method = match proj_name.as_deref() {
        Some(name)
            if name.contains("TRANSVERSE_MERCATOR") || name.contains("TRANSVERSE MERCATOR") =>
        {
            proj_core::ProjectionMethod::TransverseMercator {
                lon0,
                lat0,
                k0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        Some(name) if name.contains("MERCATOR") && !name.contains("TRANSVERSE") => {
            proj_core::ProjectionMethod::Mercator {
                lon0,
                lat_ts: extract_wkt_parameter(&upper, "STANDARD_PARALLEL_1").unwrap_or(0.0),
                k0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        Some(name) if name.contains("LAMBERT") && name.contains("CONIC") => {
            proj_core::ProjectionMethod::LambertConformalConic {
                lon0,
                lat0,
                lat1: extract_wkt_parameter(&upper, "STANDARD_PARALLEL_1").unwrap_or(lat0),
                lat2: extract_wkt_parameter(&upper, "STANDARD_PARALLEL_2").unwrap_or(lat0),
                false_easting: fe,
                false_northing: fn_,
            }
        }
        Some(name) if name.contains("ALBERS") => proj_core::ProjectionMethod::AlbersEqualArea {
            lon0,
            lat0,
            lat1: extract_wkt_parameter(&upper, "STANDARD_PARALLEL_1").unwrap_or(lat0),
            lat2: extract_wkt_parameter(&upper, "STANDARD_PARALLEL_2").unwrap_or(lat0),
            false_easting: fe,
            false_northing: fn_,
        },
        Some(name) if name.contains("POLAR") && name.contains("STEREOGRAPHIC") => {
            proj_core::ProjectionMethod::PolarStereographic {
                lon0,
                lat_ts: extract_wkt_parameter(&upper, "STANDARD_PARALLEL").unwrap_or(lat0),
                k0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        Some(name) if name.contains("EQUIDISTANT") || name.contains("PLATE") => {
            proj_core::ProjectionMethod::EquidistantCylindrical {
                lon0,
                lat_ts: extract_wkt_parameter(&upper, "STANDARD_PARALLEL_1").unwrap_or(0.0),
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

/// Extract a quoted value like PROJECTION["Transverse_Mercator"]
fn extract_wkt_value(upper: &str, key: &str) -> Option<String> {
    let marker = format!("{key}[\"");
    let pos = upper.find(&marker)?;
    let start = pos + marker.len();
    let rest = &upper[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract a numeric parameter like PARAMETER["false_easting",500000]
fn extract_wkt_parameter(upper: &str, param_name: &str) -> Option<f64> {
    let marker = format!("\"{param_name}\",");
    let pos = upper.find(&marker)?;
    let start = pos + marker.len();
    let rest = &upper[start..];
    // Find the end of the number (next ] or ,)
    let end = rest.find(']').unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_epsg_from_wkt() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(extract_authority_epsg(wkt), Some(4326));
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
}

use std::collections::HashMap;

use proj_core::crs::*;
use proj_core::datum;
use proj_core::ellipsoid;

use crate::{ParseError, Result};

/// Parse a PROJ format string like `+proj=utm +zone=18 +datum=WGS84 +units=m`.
pub(crate) fn parse_proj_string(s: &str) -> Result<CrsDef> {
    let params = parse_params(s)?;

    let proj = params.get("proj").map(|s| s.as_str()).unwrap_or("longlat");

    match proj {
        "longlat" | "lonlat" | "latlong" | "latlon" => parse_geographic(&params),
        "utm" => parse_utm(&params),
        "tmerc" => parse_tmerc(&params),
        "merc" => parse_merc(&params),
        "stere" | "sterea" => parse_stereo(&params),
        "lcc" => parse_lcc(&params),
        "aea" => parse_aea(&params),
        "eqc" => parse_eqc(&params),
        other => Err(ParseError::Parse(format!(
            "unsupported PROJ projection: {other}"
        ))),
    }
}

fn parse_params(s: &str) -> Result<HashMap<String, String>> {
    let mut params = HashMap::new();
    for token in s.split_whitespace() {
        let token = token.trim_start_matches('+');
        if let Some((key, val)) = token.split_once('=') {
            params.insert(key.to_lowercase(), val.to_string());
        } else {
            // Flag-style parameter (e.g., +no_defs, +south)
            params.insert(token.to_lowercase(), String::new());
        }
    }
    Ok(params)
}

fn resolve_datum(params: &HashMap<String, String>) -> proj_core::Datum {
    if let Some(d) = params.get("datum") {
        match d.to_uppercase().as_str() {
            "WGS84" => return datum::WGS84,
            "NAD83" => return datum::NAD83,
            "NAD27" => return datum::NAD27,
            "OSGB36" => return datum::OSGB36,
            _ => {}
        }
    }

    if let Some(e) = params.get("ellps") {
        let ellps = match e.to_uppercase().as_str() {
            "WGS84" => ellipsoid::WGS84,
            "GRS80" => ellipsoid::GRS80,
            "CLRK66" | "CLARKE1866" => ellipsoid::CLARKE1866,
            "INTL" | "INTL1924" => ellipsoid::INTL1924,
            "BESSEL" | "BESSEL1841" => ellipsoid::BESSEL1841,
            "KRASS" | "KRASSOWSKY" => ellipsoid::KRASSOWSKY,
            "AIRY" => ellipsoid::AIRY1830,
            _ => ellipsoid::WGS84,
        };
        return proj_core::Datum {
            ellipsoid: ellps,
            to_wgs84: parse_towgs84(params),
        };
    }

    datum::WGS84
}

fn parse_towgs84(params: &HashMap<String, String>) -> Option<proj_core::HelmertParams> {
    let s = params.get("towgs84")?;
    let vals: Vec<f64> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    if vals.len() >= 3 {
        Some(proj_core::HelmertParams {
            dx: vals[0],
            dy: vals[1],
            dz: vals[2],
            rx: *vals.get(3).unwrap_or(&0.0),
            ry: *vals.get(4).unwrap_or(&0.0),
            rz: *vals.get(5).unwrap_or(&0.0),
            ds: *vals.get(6).unwrap_or(&0.0),
        })
    } else {
        None
    }
}

fn get_f64(params: &HashMap<String, String>, key: &str) -> f64 {
    params.get(key).and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

fn parse_geographic(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Geographic(GeographicCrsDef {
        epsg: 0,
        datum: d,
        name: "",
    }))
}

fn parse_utm(params: &HashMap<String, String>) -> Result<CrsDef> {
    let zone: u8 = params
        .get("zone")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| ParseError::Parse("UTM requires +zone parameter".into()))?;

    let south = params.contains_key("south");
    let d = resolve_datum(params);

    let lon0 = (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0;
    let false_northing = if south { 10_000_000.0 } else { 0.0 };

    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::TransverseMercator {
            lon0,
            lat0: 0.0,
            k0: 0.9996,
            false_easting: 500_000.0,
            false_northing,
        },
        name: "",
    }))
}

fn parse_tmerc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::TransverseMercator {
            lon0: get_f64(params, "lon_0"),
            lat0: get_f64(params, "lat_0"),
            k0: params
                .get("k_0")
                .or(params.get("k"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            false_easting: get_f64(params, "x_0"),
            false_northing: get_f64(params, "y_0"),
        },
        name: "",
    }))
}

fn parse_merc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::Mercator {
            lon0: get_f64(params, "lon_0"),
            lat_ts: get_f64(params, "lat_ts"),
            k0: params
                .get("k_0")
                .or(params.get("k"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            false_easting: get_f64(params, "x_0"),
            false_northing: get_f64(params, "y_0"),
        },
        name: "",
    }))
}

fn parse_stereo(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::PolarStereographic {
            lon0: get_f64(params, "lon_0"),
            lat_ts: get_f64(params, "lat_ts"),
            k0: params
                .get("k_0")
                .or(params.get("k"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            false_easting: get_f64(params, "x_0"),
            false_northing: get_f64(params, "y_0"),
        },
        name: "",
    }))
}

fn parse_lcc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::LambertConformalConic {
            lon0: get_f64(params, "lon_0"),
            lat0: get_f64(params, "lat_0"),
            lat1: get_f64(params, "lat_1"),
            lat2: get_f64(params, "lat_2"),
            false_easting: get_f64(params, "x_0"),
            false_northing: get_f64(params, "y_0"),
        },
        name: "",
    }))
}

fn parse_aea(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::AlbersEqualArea {
            lon0: get_f64(params, "lon_0"),
            lat0: get_f64(params, "lat_0"),
            lat1: get_f64(params, "lat_1"),
            lat2: get_f64(params, "lat_2"),
            false_easting: get_f64(params, "x_0"),
            false_northing: get_f64(params, "y_0"),
        },
        name: "",
    }))
}

fn parse_eqc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params);
    Ok(CrsDef::Projected(ProjectedCrsDef {
        epsg: 0,
        datum: d,
        method: ProjectionMethod::EquidistantCylindrical {
            lon0: get_f64(params, "lon_0"),
            lat_ts: get_f64(params, "lat_ts"),
            false_easting: get_f64(params, "x_0"),
            false_northing: get_f64(params, "y_0"),
        },
        name: "",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_longlat_wgs84() {
        let crs = parse_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        assert!(crs.is_geographic());
    }

    #[test]
    fn parse_utm_zone18n() {
        let crs = parse_proj_string("+proj=utm +zone=18 +datum=WGS84 +units=m +no_defs").unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn parse_utm_zone18s() {
        let crs = parse_proj_string("+proj=utm +zone=18 +south +datum=WGS84 +units=m").unwrap();
        if let CrsDef::Projected(p) = &crs {
            if let ProjectionMethod::TransverseMercator { false_northing, .. } = &p.method {
                assert_eq!(*false_northing, 10_000_000.0);
            } else {
                panic!("expected TM");
            }
        } else {
            panic!("expected projected");
        }
    }

    #[test]
    fn parse_tmerc() {
        let crs = parse_proj_string(
            "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.9996012717 +x_0=400000 +y_0=-100000 +ellps=airy +towgs84=446.448,-125.157,542.06,0.1502,0.247,0.8421,-20.4894 +units=m",
        ).unwrap();
        assert!(crs.is_projected());
        if let CrsDef::Projected(p) = &crs {
            assert!(p.datum.to_wgs84.is_some());
        }
    }

    #[test]
    fn roundtrip_proj_string_utm() {
        let from = parse_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        let to = parse_proj_string("+proj=utm +zone=18 +datum=WGS84 +units=m +no_defs").unwrap();
        let t = proj_core::Transform::from_crs_defs(&from, &to).unwrap();
        let (x, _y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - 583960.0).abs() < 1.0, "easting = {x}");
    }
}

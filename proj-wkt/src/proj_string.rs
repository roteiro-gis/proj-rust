use std::collections::HashMap;

use proj_core::crs::*;
use proj_core::datum;
use proj_core::ellipsoid;
use proj_core::Datum;

use crate::{ParseError, Result};

/// Parse a PROJ format string like `+proj=utm +zone=18 +datum=WGS84 +units=m`.
pub(crate) fn parse_proj_string(s: &str) -> Result<CrsDef> {
    let params = parse_params(s)?;

    if !params.contains_key("proj") {
        if let Some(crs) = parse_init_authority(&params)? {
            return Ok(crs);
        }
    }

    let proj = params.get("proj").map(|s| s.as_str()).unwrap_or("longlat");

    match proj {
        "longlat" | "lonlat" | "latlong" | "latlon" => parse_geographic(&params),
        "utm" => parse_utm(&params),
        "tmerc" => parse_tmerc(&params),
        "merc" => parse_merc(&params),
        "stere" => parse_stereo(&params),
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

fn parse_init_authority(params: &HashMap<String, String>) -> Result<Option<CrsDef>> {
    let Some(init) = params.get("init") else {
        return Ok(None);
    };
    let Some((authority, code)) = init.split_once(':') else {
        return Err(ParseError::Parse(format!(
            "unsupported +init authority reference: {init}"
        )));
    };
    if !authority.eq_ignore_ascii_case("epsg") {
        return Err(ParseError::Parse(format!(
            "unsupported +init authority reference: {init}"
        )));
    }

    let code = code
        .parse::<u32>()
        .map_err(|_| ParseError::Parse(format!("invalid EPSG code in +init: {init}")))?;
    let crs = proj_core::lookup_epsg(code)
        .ok_or_else(|| ParseError::Parse(format!("unknown EPSG code in +init: {code}")))?;
    Ok(Some(crs))
}

fn resolve_datum(params: &HashMap<String, String>) -> Result<Datum> {
    if let Some(d) = params.get("datum") {
        match d.to_uppercase().as_str() {
            "WGS84" => return Ok(datum::WGS84),
            "NAD83" => return Ok(datum::NAD83),
            "NAD27" => return Ok(datum::NAD27),
            "OSGB36" => return Ok(datum::OSGB36),
            "ETRS89" => return Ok(datum::ETRS89),
            "ED50" => return Ok(datum::ED50),
            "TOKYO" => return Ok(datum::TOKYO),
            other => {
                return Err(ParseError::Parse(format!(
                    "unsupported PROJ datum: {other}"
                )));
            }
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
            other => {
                return Err(ParseError::Parse(format!(
                    "unsupported PROJ ellipsoid: {other}"
                )));
            }
        };
        return Ok(proj_core::Datum {
            ellipsoid: ellps,
            to_wgs84: parse_towgs84(params),
        });
    }

    Ok(datum::WGS84)
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

fn resolve_linear_unit_to_meter(params: &HashMap<String, String>) -> Result<f64> {
    if let Some(to_meter) = params.get("to_meter") {
        return to_meter
            .parse::<f64>()
            .map_err(|_| ParseError::Parse(format!("invalid +to_meter value: {to_meter}")));
    }

    let factor = match params.get("units").map(|s| s.as_str()) {
        None | Some("m") => 1.0,
        Some("km") => 1000.0,
        Some("ft") => 0.3048,
        Some("us-ft") => 0.3048006096012192,
        Some("yd") => 0.9144,
        Some("ch") => 20.1168,
        Some("link") => 0.201168,
        Some("mi") => 1609.344,
        Some("nmi") => 1852.0,
        Some(other) => {
            return Err(ParseError::Parse(format!(
                "unsupported PROJ linear unit: {other}"
            )));
        }
    };

    Ok(factor)
}

fn resolve_linear_unit(params: &HashMap<String, String>) -> Result<LinearUnit> {
    let meters_per_unit = resolve_linear_unit_to_meter(params)?;
    LinearUnit::from_meters_per_unit(meters_per_unit).map_err(ParseError::Core)
}

fn parse_geographic(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params)?;
    Ok(CrsDef::Geographic(GeographicCrsDef::new(0, d, "")))
}

fn parse_utm(params: &HashMap<String, String>) -> Result<CrsDef> {
    let zone: u8 = params
        .get("zone")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| ParseError::Parse("UTM requires +zone parameter".into()))?;
    if !(1..=60).contains(&zone) {
        return Err(ParseError::Parse(format!(
            "UTM zone out of range: {zone} (expected 1..=60)"
        )));
    }

    let south = params.contains_key("south");
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;

    let lon0 = (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0;
    let false_northing = if south { 10_000_000.0 } else { 0.0 };

    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::TransverseMercator {
            lon0,
            lat0: 0.0,
            k0: 0.9996,
            false_easting: linear_unit.to_meters(500_000.0),
            false_northing: linear_unit.to_meters(false_northing),
        },
        linear_unit,
        "",
    )))
}

fn parse_tmerc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::TransverseMercator {
            lon0: get_f64(params, "lon_0"),
            lat0: get_f64(params, "lat_0"),
            k0: params
                .get("k_0")
                .or(params.get("k"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            false_easting: linear_unit.to_meters(get_f64(params, "x_0")),
            false_northing: linear_unit.to_meters(get_f64(params, "y_0")),
        },
        linear_unit,
        "",
    )))
}

fn parse_merc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::Mercator {
            lon0: get_f64(params, "lon_0"),
            lat_ts: get_f64(params, "lat_ts"),
            k0: params
                .get("k_0")
                .or(params.get("k"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            false_easting: linear_unit.to_meters(get_f64(params, "x_0")),
            false_northing: linear_unit.to_meters(get_f64(params, "y_0")),
        },
        linear_unit,
        "",
    )))
}

fn parse_stereo(params: &HashMap<String, String>) -> Result<CrsDef> {
    let lat0 = get_f64(params, "lat_0");
    let lat_ts = if params.contains_key("lat_ts") {
        get_f64(params, "lat_ts")
    } else {
        lat0
    };
    let pole = if lat0.abs() > 89.999_999 {
        lat0
    } else if lat_ts.abs() > 89.999_999 {
        lat_ts
    } else {
        return Err(ParseError::Parse(
            "only polar stereographic PROJ strings are supported".into(),
        ));
    };
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::PolarStereographic {
            lon0: get_f64(params, "lon_0"),
            lat_ts: lat_ts.copysign(pole),
            k0: params
                .get("k_0")
                .or(params.get("k"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
            false_easting: linear_unit.to_meters(get_f64(params, "x_0")),
            false_northing: linear_unit.to_meters(get_f64(params, "y_0")),
        },
        linear_unit,
        "",
    )))
}

fn parse_lcc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::LambertConformalConic {
            lon0: get_f64(params, "lon_0"),
            lat0: get_f64(params, "lat_0"),
            lat1: get_f64(params, "lat_1"),
            lat2: get_f64(params, "lat_2"),
            false_easting: linear_unit.to_meters(get_f64(params, "x_0")),
            false_northing: linear_unit.to_meters(get_f64(params, "y_0")),
        },
        linear_unit,
        "",
    )))
}

fn parse_aea(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::AlbersEqualArea {
            lon0: get_f64(params, "lon_0"),
            lat0: get_f64(params, "lat_0"),
            lat1: get_f64(params, "lat_1"),
            lat2: get_f64(params, "lat_2"),
            false_easting: linear_unit.to_meters(get_f64(params, "x_0")),
            false_northing: linear_unit.to_meters(get_f64(params, "y_0")),
        },
        linear_unit,
        "",
    )))
}

fn parse_eqc(params: &HashMap<String, String>) -> Result<CrsDef> {
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::EquidistantCylindrical {
            lon0: get_f64(params, "lon_0"),
            lat_ts: get_f64(params, "lat_ts"),
            false_easting: linear_unit.to_meters(get_f64(params, "x_0")),
            false_northing: linear_unit.to_meters(get_f64(params, "y_0")),
        },
        linear_unit,
        "",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

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
            if let ProjectionMethod::TransverseMercator { false_northing, .. } = p.method() {
                assert_eq!(false_northing, 10_000_000.0);
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
            assert!(p.datum().to_wgs84.is_some());
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

    #[test]
    fn proj_string_projected_units_roundtrip_through_native_feet() {
        let from = parse_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        let to_feet = parse_proj_string(
            "+proj=tmerc +lat_0=0 +lon_0=-75 +k=0.9996 +x_0=1640416.6666666667 +y_0=0 +datum=WGS84 +units=us-ft +no_defs",
        )
        .unwrap();
        let to_meters = parse_proj_string(
            "+proj=tmerc +lat_0=0 +lon_0=-75 +k=0.9996 +x_0=500000 +y_0=0 +datum=WGS84 +units=m +no_defs",
        )
        .unwrap();

        let feet_tx = proj_core::Transform::from_crs_defs(&from, &to_feet).unwrap();
        let meter_tx = proj_core::Transform::from_crs_defs(&from, &to_meters).unwrap();

        let (fx, fy) = feet_tx.convert((-74.006, 40.7128)).unwrap();
        let (mx, my) = meter_tx.convert((-74.006, 40.7128)).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02, "x mismatch");
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02, "y mismatch");
    }

    #[test]
    fn reject_unknown_datum() {
        let err = parse_proj_string("+proj=longlat +datum=FOO").unwrap_err();
        assert!(err.to_string().contains("unsupported PROJ datum"));
    }

    #[test]
    fn reject_invalid_utm_zone() {
        let err = parse_proj_string("+proj=utm +zone=0 +datum=WGS84").unwrap_err();
        assert!(err.to_string().contains("UTM zone out of range"));
    }

    #[test]
    fn parse_init_epsg() {
        let crs = parse_proj_string("+init=epsg:3857 +type=crs").unwrap();
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn reject_oblique_stereographic() {
        let err = parse_proj_string("+proj=sterea +lat_0=52 +lon_0=5 +k=0.9999").unwrap_err();
        assert!(err.to_string().contains("unsupported PROJ projection"));
    }

    #[test]
    fn reject_non_polar_stereographic() {
        let err = parse_proj_string("+proj=stere +lat_0=52 +lon_0=5 +k=0.9999").unwrap_err();
        assert!(err
            .to_string()
            .contains("only polar stereographic PROJ strings are supported"));
    }
}

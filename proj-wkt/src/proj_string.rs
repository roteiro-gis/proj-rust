use std::collections::HashMap;
use std::path::{Component, Path};

use proj_core::crs::*;
use proj_core::datum;
use proj_core::ellipsoid;
use proj_core::Datum;
use proj_core::DatumGridShift;
use proj_core::DatumGridShiftEntry;
use proj_core::DatumToWgs84;
use proj_core::Ellipsoid;
use proj_core::GridDefinition;
use proj_core::GridFormat;
use proj_core::GridId;
use proj_core::GridInterpolation;

use crate::semantics::normalize_key;
use crate::ParsedCrs;
use crate::{ParseError, Result};

const COMMON_PROJ_PARAMS: &[&str] = &[
    "proj", "datum", "ellps", "a", "b", "rf", "f", "r", "towgs84", "nadgrids", "pm", "axis",
    "lon_wrap", "over", "no_defs", "wktext", "type",
];
const LINEAR_UNIT_PARAMS: &[&str] = &["units", "to_meter"];
const GEOGRAPHIC_UNIT_PARAMS: &[&str] = &["units", "to_meter"];
const UTM_PARAMS: &[&str] = &["zone", "south"];
const TMERC_PARAMS: &[&str] = &["lat_0", "lon_0", "k_0", "k", "x_0", "y_0"];
const MERC_PARAMS: &[&str] = &["lon_0", "lat_ts", "k_0", "k", "x_0", "y_0"];
const STERE_PARAMS: &[&str] = &["lat_0", "lon_0", "lat_ts", "k_0", "k", "x_0", "y_0"];
const STEREA_PARAMS: &[&str] = &["lat_0", "lon_0", "k_0", "k", "x_0", "y_0"];
const CONIC_PARAMS: &[&str] = &["lat_0", "lon_0", "lat_1", "lat_2", "k_0", "k", "x_0", "y_0"];
const EQC_PARAMS: &[&str] = &["lat_0", "lon_0", "lat_ts", "x_0", "y_0"];
const LAEA_PARAMS: &[&str] = &["lat_0", "lon_0", "x_0", "y_0"];
const WEBMERC_PARAMS: &[&str] = &["lon_0", "x_0", "y_0"];
const EQEARTH_PARAMS: &[&str] = &["lon_0", "x_0", "y_0"];
const POLY_PARAMS: &[&str] = &["lat_0", "lon_0", "x_0", "y_0"];
const AEQD_PARAMS: &[&str] = &["lat_0", "lon_0", "x_0", "y_0", "guam"];
const KROVAK_PARAMS: &[&str] = &["lat_0", "lon_0", "alpha", "k_0", "k", "x_0", "y_0"];
const OMERC_PARAMS: &[&str] = &[
    "lat_0", "lonc", "lon_0", "alpha", "gamma", "k_0", "k", "x_0", "y_0", "no_uoff", "no_off",
];
const CASS_PARAMS: &[&str] = &["lat_0", "lon_0", "x_0", "y_0"];

/// Parse a PROJ format string like `+proj=utm +zone=18 +datum=WGS84 +units=m`.
#[cfg(test)]
pub(crate) fn parse_proj_string(s: &str) -> Result<CrsDef> {
    Ok(parse_proj_string_with_operations(s)?.crs)
}

pub(crate) fn parse_proj_string_with_operations(s: &str) -> Result<ParsedCrs> {
    let params = parse_params(s)?;
    let grid_shift_to_wgs84 = parse_nadgrids(&params)?.filter(DatumGridShift::uses_grid_shift);
    let crs = parse_proj_params(&params)?;

    Ok(ParsedCrs {
        crs,
        grid_shift_to_wgs84,
    })
}

fn parse_proj_params(params: &HashMap<String, String>) -> Result<CrsDef> {
    if !params.contains_key("proj") && params.contains_key("init") {
        validate_supported_proj_init_params(params)?;
        if let Some(crs) = parse_init_authority(params)? {
            return Ok(crs);
        }
    }

    let proj = params.get("proj").map(|s| s.as_str()).unwrap_or("longlat");

    match proj {
        "longlat" | "lonlat" | "latlong" | "latlon" => parse_geographic(params),
        "cart" | "geocent" => parse_geocentric(params),
        "utm" => parse_utm(params),
        "tmerc" => parse_tmerc(params),
        "merc" => parse_merc(params),
        "stere" => parse_stereo(params),
        "sterea" => parse_sterea(params),
        "lcc" => parse_lcc(params),
        "aea" => parse_aea(params),
        "eqc" => parse_eqc(params),
        "laea" => parse_laea(params),
        "omerc" => parse_omerc(params),
        "cass" => parse_cass(params),
        "webmerc" => parse_webmerc(params),
        "eqearth" => parse_eqearth(params),
        "poly" => parse_poly(params),
        "aeqd" => parse_aeqd(params),
        "krovak" => parse_krovak(params),
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
    let towgs84 = parse_towgs84(params)?;
    let nadgrids = parse_nadgrids(params)?;
    if towgs84.is_some() && nadgrids.is_some() {
        return Err(ParseError::UnsupportedSemantics(
            "PROJ datum definition cannot combine +towgs84 and +nadgrids".into(),
        ));
    }

    if let Some(d) = params.get("datum") {
        let datum = match d.to_uppercase().as_str() {
            "WGS84" => datum::WGS84,
            "NAD83" => datum::NAD83,
            "NAD27" => datum::NAD27,
            "OSGB36" => datum::OSGB36,
            "ETRS89" => datum::ETRS89,
            "ED50" => datum::ED50,
            "TOKYO" => datum::TOKYO,
            other => {
                return Err(ParseError::Parse(format!(
                    "unsupported PROJ datum: {other}"
                )));
            }
        };
        if let Some(to_wgs84) = towgs84 {
            return Ok(proj_core::Datum::new(datum.ellipsoid(), to_wgs84)?);
        }
        if let Some(grid_shift) = nadgrids {
            return Ok(proj_core::Datum::new(
                datum.ellipsoid(),
                datum_grid_shift_to_crs_datum_transform(grid_shift),
            )?);
        }
        return Ok(datum);
    }

    if let Some(ellps) = resolve_ellipsoid(params)? {
        let to_wgs84 = towgs84.unwrap_or_else(|| {
            if let Some(grid_shift) = nadgrids {
                datum_grid_shift_to_crs_datum_transform(grid_shift)
            } else if (ellps.semi_major_axis() - ellipsoid::WGS84.semi_major_axis()).abs() < 1e-9
                && (ellps.flattening() - ellipsoid::WGS84.flattening()).abs() < 1e-15
            {
                DatumToWgs84::Identity
            } else {
                DatumToWgs84::Unknown
            }
        });
        return Ok(proj_core::Datum::new(ellps, to_wgs84)?);
    }

    if let Some(grid_shift) = nadgrids {
        return Ok(proj_core::Datum::new(
            ellipsoid::WGS84,
            datum_grid_shift_to_crs_datum_transform(grid_shift),
        )?);
    }

    if let Some(to_wgs84) = towgs84 {
        return Ok(proj_core::Datum::new(ellipsoid::WGS84, to_wgs84)?);
    }

    Ok(datum::WGS84)
}

/// Resolve `+ellps` or the custom `+a`/`+b`/`+rf`/`+f`/`+R` parameters.
fn resolve_ellipsoid(params: &HashMap<String, String>) -> Result<Option<Ellipsoid>> {
    let custom = custom_ellipsoid(params)?;
    let Some(name) = params.get("ellps") else {
        return Ok(custom);
    };
    if custom.is_some() {
        return Err(ParseError::UnsupportedSemantics(
            "PROJ ellipsoid definition cannot combine +ellps with custom ellipsoid parameters"
                .into(),
        ));
    }
    let named = |a: f64, rf: f64| Ellipsoid::from_a_rf(a, rf).map_err(ParseError::Core);
    Ok(Some(match name.to_uppercase().as_str() {
        "WGS84" => ellipsoid::WGS84,
        "GRS80" => ellipsoid::GRS80,
        "CLRK66" | "CLARKE1866" => ellipsoid::CLARKE1866,
        "INTL" | "INTL1924" => ellipsoid::INTL1924,
        "BESSEL" | "BESSEL1841" => ellipsoid::BESSEL1841,
        "KRASS" | "KRASSOWSKY" => ellipsoid::KRASSOWSKY,
        "AIRY" => ellipsoid::AIRY1830,
        "GRS67" => named(6_378_160.0, 298.247_167_427)?,
        "WGS72" => named(6_378_135.0, 298.26)?,
        "CLRK80" => named(6_378_249.145, 293.4663)?,
        "CLRK80IGN" => named(6_378_249.2, 293.466_021_293_626_9)?,
        "AUST_SA" => named(6_378_160.0, 298.25)?,
        "MOD_AIRY" => ellipsoid_from_a_b(6_377_340.189, 6_356_034.446)?,
        "BESS_NAM" => named(6_377_483.865, 299.152_812_8)?,
        "EVRST30" => named(6_377_276.345, 300.8017)?,
        "SPHERE" => Ellipsoid::sphere(6_370_997.0)?,
        other => {
            return Err(ParseError::Parse(format!(
                "unsupported PROJ ellipsoid: {other}"
            )));
        }
    }))
}

fn custom_ellipsoid(params: &HashMap<String, String>) -> Result<Option<Ellipsoid>> {
    let value = |key: &str| {
        params
            .get(key)
            .map(|value| parse_f64_param(key, value))
            .transpose()
    };
    let (a, b, rf, f) = (value("a")?, value("b")?, value("rf")?, value("f")?);
    if let Some(radius) = value("r")? {
        if a.is_some() || b.is_some() || rf.is_some() || f.is_some() {
            return Err(ParseError::UnsupportedSemantics(
                "PROJ ellipsoid definition cannot combine +R with other ellipsoid parameters"
                    .into(),
            ));
        }
        return Ok(Some(Ellipsoid::sphere(radius)?));
    }
    let Some(a) = a else {
        if b.is_some() || rf.is_some() || f.is_some() {
            return Err(ParseError::Parse(
                "PROJ ellipsoid definition requires +a with +b, +rf, or +f".into(),
            ));
        }
        return Ok(None);
    };
    Ok(Some(match (b, rf, f) {
        (None, None, None) => Ellipsoid::sphere(a)?,
        (Some(b), None, None) => ellipsoid_from_a_b(a, b)?,
        (None, Some(rf), None) => Ellipsoid::from_a_rf(a, rf)?,
        (None, None, Some(f)) => Ellipsoid::from_a_f(a, f)?,
        _ => {
            return Err(ParseError::UnsupportedSemantics(
                "PROJ ellipsoid definition must use only one of +b, +rf, or +f".into(),
            ));
        }
    }))
}

fn ellipsoid_from_a_b(a: f64, b: f64) -> Result<Ellipsoid> {
    Ok(if a == b {
        Ellipsoid::sphere(a)?
    } else {
        Ellipsoid::from_a_f(a, (a - b) / a)?
    })
}

fn datum_grid_shift_to_crs_datum_transform(grid_shift: DatumGridShift) -> DatumToWgs84 {
    if grid_shift.uses_grid_shift() {
        DatumToWgs84::Unknown
    } else {
        DatumToWgs84::Identity
    }
}

fn parse_nadgrids(params: &HashMap<String, String>) -> Result<Option<DatumGridShift>> {
    let Some(value) = params.get("nadgrids") else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Err(ParseError::Parse(
            "+nadgrids requires at least one grid name".into(),
        ));
    }

    let mut entries = Vec::new();
    for raw_entry in value.split(',') {
        let raw_entry = raw_entry.trim();
        if raw_entry.is_empty() {
            return Err(ParseError::Parse(
                "+nadgrids contains an empty grid name".into(),
            ));
        }

        let (optional, resource_name) = raw_entry
            .strip_prefix('@')
            .map(|name| (true, name))
            .unwrap_or((false, raw_entry));
        if resource_name.is_empty() {
            return Err(ParseError::Parse(
                "+nadgrids contains an empty grid name".into(),
            ));
        }
        if resource_name.eq_ignore_ascii_case("null") {
            entries.push(DatumGridShiftEntry::Null);
            continue;
        }
        validate_grid_resource_name(resource_name)?;
        entries.push(DatumGridShiftEntry::Grid {
            definition: GridDefinition {
                id: GridId(parsed_grid_id(resource_name)),
                name: resource_name.to_string(),
                format: infer_grid_format(resource_name),
                interpolation: GridInterpolation::Bilinear,
                area_of_use: None,
                resource_names: smallvec_from_one(resource_name.to_string()),
            },
            optional,
        });
    }

    if entries.is_empty() {
        return Err(ParseError::Parse(
            "+nadgrids requires at least one grid name".into(),
        ));
    }

    Ok(Some(DatumGridShift::from_vec(entries)))
}

fn smallvec_from_one(value: String) -> smallvec::SmallVec<[String; 2]> {
    smallvec::SmallVec::from_vec(vec![value])
}

fn validate_grid_resource_name(resource_name: &str) -> Result<()> {
    let path = Path::new(resource_name);
    if path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir
        )
    }) {
        return Err(ParseError::UnsupportedSemantics(format!(
            "+nadgrids resource `{resource_name}` must be a relative grid resource name"
        )));
    }
    Ok(())
}

fn infer_grid_format(resource_name: &str) -> GridFormat {
    let normalized = resource_name.to_ascii_lowercase();
    if normalized.ends_with(".gsb") || normalized.ends_with(".ntv2") {
        GridFormat::Ntv2
    } else {
        GridFormat::Unsupported
    }
}

fn parsed_grid_id(resource_name: &str) -> u32 {
    const FNV_OFFSET: u32 = 0x811c9dc5;
    const FNV_PRIME: u32 = 0x01000193;
    let hash = resource_name
        .as_bytes()
        .iter()
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(FNV_PRIME)
        });
    0x8000_0000 | (hash & 0x7fff_ffff)
}

fn parse_towgs84(params: &HashMap<String, String>) -> Result<Option<DatumToWgs84>> {
    let Some(s) = params.get("towgs84") else {
        return Ok(None);
    };
    let vals = s
        .split(',')
        .map(|value| {
            let value = value.trim();
            let parsed = value.parse::<f64>().map_err(|_| {
                ParseError::Parse(format!("invalid +towgs84 numeric value: {value}"))
            })?;
            if parsed.is_finite() {
                Ok(parsed)
            } else {
                Err(ParseError::Parse(format!(
                    "invalid +towgs84 numeric value: {value}"
                )))
            }
        })
        .collect::<Result<Vec<_>>>()?;

    if !matches!(vals.len(), 3 | 7) {
        return Err(ParseError::Parse(format!(
            "+towgs84 requires 3 or 7 comma-separated numeric values, got {}",
            vals.len()
        )));
    }

    let helmert = proj_core::HelmertParams::new(
        vals[0],
        vals[1],
        vals[2],
        *vals.get(3).unwrap_or(&0.0),
        *vals.get(4).unwrap_or(&0.0),
        *vals.get(5).unwrap_or(&0.0),
        *vals.get(6).unwrap_or(&0.0),
    )?;
    Ok(Some(if vals.iter().all(|value| *value == 0.0) {
        DatumToWgs84::Identity
    } else {
        DatumToWgs84::Helmert(helmert)
    }))
}

fn get_f64(params: &HashMap<String, String>, key: &str) -> Result<f64> {
    match params.get(key) {
        Some(value) => parse_f64_param(key, value),
        None => Ok(0.0),
    }
}

fn get_meter_param(params: &HashMap<String, String>, key: &str) -> Result<f64> {
    // PROJ false easting/northing parameters are always meters; +units only
    // controls the native projected coordinates accepted and returned.
    get_f64(params, key)
}

fn get_scale(params: &HashMap<String, String>) -> Result<f64> {
    if let Some(value) = params.get("k_0") {
        return parse_f64_param("k_0", value);
    }
    if let Some(value) = params.get("k") {
        return parse_f64_param("k", value);
    }
    Ok(1.0)
}

fn parse_f64_param(key: &str, value: &str) -> Result<f64> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| ParseError::Parse(format!("invalid +{key} value: {value}")))?;
    if !parsed.is_finite() {
        return Err(ParseError::Parse(format!("invalid +{key} value: {value}")));
    }
    Ok(parsed)
}

fn resolve_linear_unit_to_meter(params: &HashMap<String, String>) -> Result<f64> {
    if let Some(to_meter) = params.get("to_meter") {
        return parse_f64_param("to_meter", to_meter);
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
    validate_supported_proj_params(
        params,
        &[],
        GEOGRAPHIC_UNIT_PARAMS,
        "PROJ geographic CRS definition",
    )?;
    validate_supported_proj_geographic_semantics(params)?;
    let d = resolve_datum(params)?;
    Ok(CrsDef::Geographic(GeographicCrsDef::new(0, d, "")))
}

fn parse_geocentric(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        &[],
        LINEAR_UNIT_PARAMS,
        "PROJ geocentric CRS definition",
    )?;
    validate_supported_proj_geocentric_semantics(params)?;
    let d = resolve_datum(params)?;
    Ok(CrsDef::Geocentric(GeocentricCrsDef::new(0, 0, d, "")))
}

fn parse_utm(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        UTM_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ UTM definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ UTM definition")?;
    validate_empty_flag(params, "south", "PROJ UTM definition")?;
    let zone_value = params
        .get("zone")
        .ok_or_else(|| ParseError::Parse("UTM requires +zone parameter".into()))?;
    let zone: u8 = zone_value
        .parse()
        .map_err(|_| ParseError::Parse(format!("invalid UTM zone: {zone_value}")))?;
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
            false_easting: 500_000.0,
            false_northing,
        },
        linear_unit,
        "",
    )))
}

fn parse_tmerc(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        TMERC_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Transverse Mercator definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Transverse Mercator definition")?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::TransverseMercator {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            k0: get_scale(params)?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_merc(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        MERC_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Mercator definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Mercator definition")?;
    if is_legacy_web_mercator(params)? {
        return proj_core::lookup_epsg(3857)
            .ok_or_else(|| ParseError::Parse("EPSG:3857 not found in registry".into()));
    }
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::Mercator {
            lon0: get_f64(params, "lon_0")?,
            lat_ts: get_f64(params, "lat_ts")?,
            k0: get_scale(params)?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

/// The spherical `+proj=merc` form GDAL and QGIS wrote for EPSG:3857 before
/// `+proj=webmerc`: the sphere has WGS 84's radius and `+nadgrids=@null` keeps
/// the ellipsoidal WGS 84 coordinates from being shifted onto it.
fn is_legacy_web_mercator(params: &HashMap<String, String>) -> Result<bool> {
    let is = |key: &str, expected: f64| -> Result<bool> {
        Ok(match params.get(key) {
            Some(value) => parse_f64_param(key, value)? == expected,
            None => expected == 0.0,
        })
    };
    let towgs84_is_null = parse_helmert_is_null(params)?;
    Ok(params.get("nadgrids").map(String::as_str) == Some("@null")
        && is("a", 6_378_137.0)?
        && is("b", 6_378_137.0)?
        && is("lat_ts", 0.0)?
        && is("lon_0", 0.0)?
        && is("x_0", 0.0)?
        && is("y_0", 0.0)?
        && get_scale(params)? == 1.0
        && resolve_linear_unit_to_meter(params)? == 1.0
        && towgs84_is_null)
}

fn parse_helmert_is_null(params: &HashMap<String, String>) -> Result<bool> {
    Ok(matches!(
        parse_towgs84(params)?,
        None | Some(DatumToWgs84::Identity)
    ))
}

fn parse_stereo(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        STERE_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Polar Stereographic definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Polar Stereographic definition")?;
    let lat0 = get_f64(params, "lat_0")?;
    let lat_ts = if params.contains_key("lat_ts") {
        get_f64(params, "lat_ts")?
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
            lon0: get_f64(params, "lon_0")?,
            lat_ts: lat_ts.copysign(pole),
            k0: get_scale(params)?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_lcc(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        CONIC_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Lambert Conformal Conic definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Lambert Conformal Conic definition")?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::LambertConformalConic {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            lat1: get_f64(params, "lat_1")?,
            lat2: get_f64(params, "lat_2")?,
            k0: get_scale(params)?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_laea(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        LAEA_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Lambert Azimuthal Equal Area definition",
    )?;
    validate_supported_proj_common_semantics(
        params,
        "PROJ Lambert Azimuthal Equal Area definition",
    )?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::LambertAzimuthalEqualArea {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_sterea(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        STEREA_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Oblique Stereographic definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Oblique Stereographic definition")?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::ObliqueStereographic {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            k0: get_scale(params)?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_aea(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        CONIC_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Albers Equal Area definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Albers Equal Area definition")?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::AlbersEqualArea {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            lat1: get_f64(params, "lat_1")?,
            lat2: get_f64(params, "lat_2")?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_omerc(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        OMERC_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Hotine Oblique Mercator definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Hotine Oblique Mercator definition")?;
    validate_empty_flag(params, "no_uoff", "PROJ Hotine Oblique Mercator definition")?;
    validate_empty_flag(params, "no_off", "PROJ Hotine Oblique Mercator definition")?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    let azimuth = if params.contains_key("alpha") {
        get_f64(params, "alpha")?
    } else {
        get_f64(params, "gamma")?
    };
    let rectified_grid_angle = if params.contains_key("gamma") {
        get_f64(params, "gamma")?
    } else {
        azimuth
    };
    let lonc = if params.contains_key("lonc") {
        get_f64(params, "lonc")?
    } else {
        get_f64(params, "lon_0")?
    };
    let variant_b = !params.contains_key("no_uoff") && !params.contains_key("no_off");

    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::HotineObliqueMercator {
            latc: get_f64(params, "lat_0")?,
            lonc,
            azimuth,
            rectified_grid_angle,
            k0: get_scale(params)?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
            variant_b,
        },
        linear_unit,
        "",
    )))
}

fn parse_cass(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        CASS_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Cassini-Soldner definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Cassini-Soldner definition")?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::CassiniSoldner {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_eqc(params: &HashMap<String, String>) -> Result<CrsDef> {
    validate_supported_proj_params(
        params,
        EQC_PARAMS,
        LINEAR_UNIT_PARAMS,
        "PROJ Equidistant Cylindrical definition",
    )?;
    validate_supported_proj_common_semantics(params, "PROJ Equidistant Cylindrical definition")?;
    if get_f64(params, "lat_0")? != 0.0 {
        return Err(ParseError::UnsupportedSemantics(
            "PROJ Equidistant Cylindrical definition uses a nonzero +lat_0".into(),
        ));
    }
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::EquidistantCylindrical {
            lon0: get_f64(params, "lon_0")?,
            lat_ts: get_f64(params, "lat_ts")?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_webmerc(params: &HashMap<String, String>) -> Result<CrsDef> {
    const CONTEXT: &str = "PROJ Web Mercator definition";
    validate_supported_proj_params(params, WEBMERC_PARAMS, LINEAR_UNIT_PARAMS, CONTEXT)?;
    validate_supported_proj_common_semantics(params, CONTEXT)?;
    for key in WEBMERC_PARAMS {
        if get_f64(params, key)? != 0.0 {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{CONTEXT} uses a nonzero +{key}"
            )));
        }
    }
    let d = resolve_datum(params)?;
    if d.ellipsoid().semi_major_axis() != ellipsoid::WGS84.semi_major_axis() {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{CONTEXT} requires a WGS 84 or GRS80 semi-major axis"
        )));
    }
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::WebMercator,
        linear_unit,
        "",
    )))
}

fn parse_eqearth(params: &HashMap<String, String>) -> Result<CrsDef> {
    const CONTEXT: &str = "PROJ Equal Earth definition";
    validate_supported_proj_params(params, EQEARTH_PARAMS, LINEAR_UNIT_PARAMS, CONTEXT)?;
    validate_supported_proj_common_semantics(params, CONTEXT)?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::EqualEarth {
            lon0: get_f64(params, "lon_0")?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_poly(params: &HashMap<String, String>) -> Result<CrsDef> {
    const CONTEXT: &str = "PROJ American Polyconic definition";
    validate_supported_proj_params(params, POLY_PARAMS, LINEAR_UNIT_PARAMS, CONTEXT)?;
    validate_supported_proj_common_semantics(params, CONTEXT)?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::AmericanPolyconic {
            lon0: get_f64(params, "lon_0")?,
            lat0: get_f64(params, "lat_0")?,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn parse_aeqd(params: &HashMap<String, String>) -> Result<CrsDef> {
    const CONTEXT: &str = "PROJ Azimuthal Equidistant definition";
    validate_supported_proj_params(params, AEQD_PARAMS, LINEAR_UNIT_PARAMS, CONTEXT)?;
    validate_supported_proj_common_semantics(params, CONTEXT)?;
    validate_empty_flag(params, "guam", CONTEXT)?;
    let d = resolve_datum(params)?;
    let linear_unit = resolve_linear_unit(params)?;
    let (lon0, lat0) = (get_f64(params, "lon_0")?, get_f64(params, "lat_0")?);
    let false_easting = get_meter_param(params, "x_0")?;
    let false_northing = get_meter_param(params, "y_0")?;
    let method = if params.contains_key("guam") {
        ProjectionMethod::GuamProjection {
            lon0,
            lat0,
            false_easting,
            false_northing,
        }
    } else {
        ProjectionMethod::AzimuthalEquidistant {
            lon0,
            lat0,
            false_easting,
            false_northing,
        }
    };
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        method,
        linear_unit,
        "",
    )))
}

/// C PROJ's `krovak` is the north-orientated form on a fixed Bessel ellipsoid
/// with a fixed cone: it reads `+alpha` but does not apply it.
fn parse_krovak(params: &HashMap<String, String>) -> Result<CrsDef> {
    const CONTEXT: &str = "PROJ Krovak definition";
    validate_supported_proj_params(params, KROVAK_PARAMS, LINEAR_UNIT_PARAMS, CONTEXT)?;
    validate_supported_proj_common_semantics(params, CONTEXT)?;
    let d = resolve_datum(params)?;
    let bessel = ellipsoid::BESSEL1841;
    if (d.ellipsoid().semi_major_axis() - bessel.semi_major_axis()).abs() > 1e-6
        || (d.ellipsoid().flattening() - bessel.flattening()).abs() > 1e-12
    {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{CONTEXT} requires the Bessel 1841 ellipsoid"
        )));
    }
    let linear_unit = resolve_linear_unit(params)?;
    let or_default = |key: &str, default: f64| match params.get(key) {
        Some(value) => parse_f64_param(key, value),
        None => Ok(default),
    };
    let k0 = if params.contains_key("k_0") || params.contains_key("k") {
        get_scale(params)?
    } else {
        0.9999
    };
    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        d,
        ProjectionMethod::KrovakNorthOrientated {
            lon0: or_default("lon_0", 24.0 + 50.0 / 60.0)?,
            lat0: or_default("lat_0", 49.5)?,
            co_latitude_cone_axis: 30.288_139_752_777_78,
            lat_pseudo_standard_parallel: 78.5,
            k0,
            false_easting: get_meter_param(params, "x_0")?,
            false_northing: get_meter_param(params, "y_0")?,
        },
        linear_unit,
        "",
    )))
}

fn validate_supported_proj_init_params(params: &HashMap<String, String>) -> Result<()> {
    for key in params.keys() {
        if matches!(key.as_str(), "init" | "type" | "no_defs") {
            continue;
        }
        return Err(unsupported_proj_parameter_error(
            "PROJ init authority reference",
            key,
        ));
    }
    validate_empty_flag(params, "no_defs", "PROJ init authority reference")?;
    validate_proj_type(params, "PROJ init authority reference")
}

fn validate_supported_proj_params(
    params: &HashMap<String, String>,
    projection_params: &[&str],
    unit_params: &[&str],
    context: &str,
) -> Result<()> {
    for key in params.keys() {
        if COMMON_PROJ_PARAMS.contains(&key.as_str())
            || projection_params.contains(&key.as_str())
            || unit_params.contains(&key.as_str())
        {
            continue;
        }
        return Err(unsupported_proj_parameter_error(context, key));
    }
    Ok(())
}

fn unsupported_proj_parameter_error(context: &str, key: &str) -> ParseError {
    let detail = match key {
        "nadgrids" => {
            "grid-based horizontal datum shifts are only supported on full PROJ CRS definitions"
        }
        "geoidgrids" => "vertical geoid grid shifts are not supported in PROJ strings",
        "vunits" | "vto_meter" => "vertical coordinate units are not supported in PROJ strings",
        "es" => "define a custom ellipsoid with +a and one of +b, +rf, or +f",
        _ => "parameter is not supported by this PROJ parser",
    };
    ParseError::UnsupportedSemantics(format!(
        "{context} uses unsupported PROJ parameter `+{key}`: {detail}"
    ))
}

fn validate_supported_proj_geographic_semantics(params: &HashMap<String, String>) -> Result<()> {
    validate_supported_proj_common_semantics(params, "PROJ geographic CRS definition")?;

    if let Some(units) = params.get("units") {
        let normalized_units = normalize_key(units);
        if !matches!(normalized_units.as_str(), "deg" | "degree" | "degrees") {
            return Err(ParseError::UnsupportedSemantics(format!(
                "PROJ geographic CRS definition uses unsupported angular unit `{units}`"
            )));
        }
    }

    if params.contains_key("to_meter") {
        return Err(ParseError::UnsupportedSemantics(
            "PROJ geographic CRS definition uses unsupported angular unit conversion".into(),
        ));
    }

    Ok(())
}

fn validate_supported_proj_geocentric_semantics(params: &HashMap<String, String>) -> Result<()> {
    validate_supported_proj_common_semantics(params, "PROJ geocentric CRS definition")?;

    // ECEF is always X/Y/Z; reject ENU-style +axis that geographic CRS allow.
    if let Some(axis) = params.get("axis") {
        let normalized_axis = normalize_key(axis);
        if !normalized_axis.is_empty() {
            return Err(ParseError::UnsupportedSemantics(format!(
                "PROJ geocentric CRS definition uses unsupported axis order `{axis}`"
            )));
        }
    }

    if let Some(units) = params.get("units") {
        let normalized_units = normalize_key(units);
        if !matches!(
            normalized_units.as_str(),
            "m" | "meter" | "metre" | "meters" | "metres"
        ) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "PROJ geocentric CRS definition uses linear units other than metres (`{units}`)"
            )));
        }
    }

    if let Some(to_meter) = params.get("to_meter") {
        let factor = parse_f64_param("to_meter", to_meter)?;
        if (factor - 1.0).abs() > 1e-12 {
            return Err(ParseError::UnsupportedSemantics(format!(
                "PROJ geocentric CRS definition uses linear units other than metres (+to_meter={to_meter})"
            )));
        }
    }

    Ok(())
}

fn validate_supported_proj_common_semantics(
    params: &HashMap<String, String>,
    context: &str,
) -> Result<()> {
    validate_empty_flag(params, "no_defs", context)?;
    validate_proj_type(params, context)?;

    if let Some(prime_meridian) = params.get("pm") {
        let normalized_prime_meridian = normalize_key(prime_meridian);
        let is_greenwich = normalized_prime_meridian.is_empty()
            || normalized_prime_meridian == "greenwich"
            || prime_meridian
                .parse::<f64>()
                .ok()
                .is_some_and(|value| value.abs() < 1e-12);
        if !is_greenwich {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses unsupported prime meridian `{prime_meridian}`"
            )));
        }
    }

    if let Some(axis) = params.get("axis") {
        let normalized_axis = normalize_key(axis);
        if !matches!(normalized_axis.as_str(), "" | "enu" | "en") {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses unsupported axis order `{axis}`"
            )));
        }
    }

    if params.contains_key("lon_wrap") {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} uses unsupported longitude wrapping semantics"
        )));
    }

    if params.contains_key("over") {
        return Err(ParseError::UnsupportedSemantics(format!(
            "{context} uses unsupported over-range longitude semantics"
        )));
    }

    Ok(())
}

fn validate_empty_flag(params: &HashMap<String, String>, key: &str, context: &str) -> Result<()> {
    if let Some(value) = params.get(key) {
        if !value.is_empty() {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses unsupported +{key} value `{value}`"
            )));
        }
    }
    Ok(())
}

fn validate_proj_type(params: &HashMap<String, String>, context: &str) -> Result<()> {
    if let Some(value) = params.get("type") {
        if normalize_key(value) != "crs" {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} uses unsupported PROJ object type `{value}`"
            )));
        }
    }
    Ok(())
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
    fn parse_cart_wgs84() {
        let crs = parse_proj_string("+proj=cart +datum=WGS84 +type=crs").unwrap();
        assert!(crs.is_geocentric());
        assert_eq!(crs.as_geocentric().unwrap().epsg(), 0);
    }

    #[test]
    fn parse_geocent_alias() {
        let crs = parse_proj_string("+proj=geocent +ellps=WGS84 +units=m +no_defs").unwrap();
        assert!(crs.is_geocentric());
    }

    #[test]
    fn roundtrip_proj_string_cart() {
        let from = parse_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        let to = parse_proj_string("+proj=cart +datum=WGS84 +type=crs").unwrap();
        let t = proj_core::Transform::from_crs_defs(&from, &to).unwrap();
        let (x, y, z) = t.convert_3d((-74.006, 40.7128, 10.0)).unwrap();
        assert!((x - 1_334_000.544_686_07).abs() < 1e-4, "x = {x}");
        assert!((y - -4_654_052.129_206_82).abs() < 1e-4, "y = {y}");
        assert!((z - 4_138_306.761_372_84).abs() < 1e-4, "z = {z}");
    }

    #[test]
    fn reject_cart_non_metre_units() {
        let err = parse_proj_string("+proj=cart +datum=WGS84 +units=us-ft").unwrap_err();
        assert!(err.to_string().contains("linear units other than metres"));
    }

    #[test]
    fn reject_cart_axis_order() {
        let err = parse_proj_string("+proj=cart +datum=WGS84 +axis=enu").unwrap_err();
        assert!(err.to_string().contains("unsupported axis order"));
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
            assert!(matches!(p.datum().to_wgs84(), DatumToWgs84::Helmert(_)));
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
    fn proj_string_projected_units_apply_output_units_only() {
        let from = parse_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        let to_feet = parse_proj_string(
            "+proj=tmerc +lat_0=0 +lon_0=-75 +k=0.9996 +x_0=500000 +y_0=0 +datum=WGS84 +units=us-ft +no_defs",
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
    fn proj_string_utm_units_keep_meter_false_offsets() {
        let from = parse_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        let to_feet =
            parse_proj_string("+proj=utm +zone=18 +datum=WGS84 +units=us-ft +no_defs").unwrap();
        let to_meters =
            parse_proj_string("+proj=utm +zone=18 +datum=WGS84 +units=m +no_defs").unwrap();

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
    fn reject_non_greenwich_prime_meridian() {
        let err = parse_proj_string("+proj=longlat +datum=WGS84 +pm=paris").unwrap_err();
        assert!(err.to_string().contains("unsupported prime meridian"));
    }

    #[test]
    fn reject_non_default_axis_order() {
        let err = parse_proj_string("+proj=longlat +datum=WGS84 +axis=neu").unwrap_err();
        assert!(err.to_string().contains("unsupported axis order"));
    }

    #[test]
    fn parse_nadgrids_datum_shift() {
        let parsed = parse_proj_string_with_operations(
            "+proj=longlat +ellps=clrk66 +nadgrids=@missing.gsb,ntv2_0.gsb",
        )
        .unwrap();
        assert!(matches!(
            parsed.crs.datum().to_wgs84(),
            DatumToWgs84::Unknown
        ));
        let grids = parsed
            .grid_shift_to_wgs84
            .as_ref()
            .expect("expected parsed grid shift");
        assert_eq!(grids.entries().len(), 2);
    }

    #[test]
    fn parse_nadgrids_null_as_identity_shift() {
        let crs = parse_proj_string("+proj=longlat +ellps=WGS84 +nadgrids=@null").unwrap();
        assert!(matches!(crs.datum().to_wgs84(), DatumToWgs84::Identity));
    }

    #[test]
    fn nadgrids_transform_uses_grid_provider_path() {
        let transform = crate::transform_from_crs_strings(
            "+proj=longlat +ellps=clrk66 +nadgrids=@missing.gsb,ntv2_0.gsb",
            "+proj=longlat +datum=WGS84",
        )
        .unwrap();
        assert_eq!(
            transform.selection_diagnostics().selected_match_kind,
            proj_core::OperationMatchKind::Custom
        );

        let (lon, lat) = transform.convert((-80.5041667, 44.5458333)).unwrap();

        assert!((lon - (-80.50401615833)).abs() < 1e-6, "lon={lon}");
        assert!((lat - 44.5458827236).abs() < 3e-6, "lat={lat}");

        let inverse = transform.inverse().unwrap();
        let (back_lon, back_lat) = inverse.convert((lon, lat)).unwrap();

        assert!((back_lon - (-80.5041667)).abs() < 1e-6, "lon={back_lon}");
        assert!((back_lat - 44.5458333).abs() < 3e-6, "lat={back_lat}");
    }

    #[test]
    fn same_nadgrids_definition_does_not_load_grid() {
        let definition = "+proj=longlat +ellps=clrk66 +nadgrids=missing.gsb";
        let transform = crate::transform_from_crs_strings(definition, definition).unwrap();
        assert_eq!(
            transform.selection_diagnostics().selected_match_kind,
            proj_core::OperationMatchKind::Custom
        );

        let (lon, lat) = transform.convert((-80.5, 44.5)).unwrap();

        assert_eq!(lon, -80.5);
        assert_eq!(lat, 44.5);
    }

    #[test]
    fn required_nadgrids_resource_must_load() {
        let err = match crate::transform_from_crs_strings(
            "+proj=longlat +ellps=clrk66 +nadgrids=missing.gsb",
            "+proj=longlat +datum=WGS84",
        ) {
            Ok(_) => panic!("expected missing grid to fail"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("grid resource unavailable"));
    }

    #[test]
    fn reject_unsupported_grid_shift_proj_params() {
        let err =
            parse_proj_string("+proj=longlat +ellps=WGS84 +geoidgrids=egm96_15.gtx").unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported PROJ parameter `+geoidgrids`"));

        let err =
            parse_proj_string("+proj=longlat +ellps=clrk66 +nadgrids=../ntv2_0.gsb").unwrap_err();
        assert!(err.to_string().contains("relative grid resource name"));

        let err =
            parse_proj_string("+proj=longlat +ellps=clrk66 +nadgrids=./ntv2_0.gsb").unwrap_err();
        assert!(err.to_string().contains("relative grid resource name"));

        let err =
            parse_proj_string("+proj=longlat +ellps=clrk66 +towgs84=1,2,3 +nadgrids=ntv2_0.gsb")
                .unwrap_err();
        assert!(err
            .to_string()
            .contains("cannot combine +towgs84 and +nadgrids"));
    }

    #[test]
    fn reject_over_range_longitude_semantics() {
        let err = parse_proj_string("+proj=longlat +datum=WGS84 +over").unwrap_err();
        assert!(err.to_string().contains("over-range longitude semantics"));
    }

    #[test]
    fn reject_unknown_proj_parameter() {
        let err = parse_proj_string("+proj=utm +zone=18 +datum=WGS84 +foo=bar").unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported PROJ parameter `+foo`"));
    }

    #[test]
    fn reject_malformed_towgs84() {
        let err =
            parse_proj_string("+proj=longlat +ellps=WGS84 +towgs84=1,not-a-number,3").unwrap_err();
        assert!(err.to_string().contains("invalid +towgs84 numeric value"));

        let err = parse_proj_string("+proj=longlat +ellps=WGS84 +towgs84=1,2,3,4").unwrap_err();
        assert!(err.to_string().contains("+towgs84 requires 3 or 7"));
    }

    #[test]
    fn reject_invalid_numeric_proj_parameter() {
        let err = parse_proj_string("+proj=tmerc +lat_0=not-a-number +lon_0=-2").unwrap_err();
        assert!(err.to_string().contains("invalid +lat_0 value"));
    }

    #[test]
    fn parse_init_epsg() {
        let crs = parse_proj_string("+init=epsg:3857 +type=crs").unwrap();
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn parse_oblique_stereographic() {
        let crs = parse_proj_string(
            "+proj=sterea +lat_0=52.1561605555556 +lon_0=5.38763888888889 +k=0.9999079 +x_0=155000 +y_0=463000 +ellps=bessel +units=m",
        )
        .unwrap();
        assert!(matches!(
            crs,
            CrsDef::Projected(p)
                if matches!(p.method(), ProjectionMethod::ObliqueStereographic { .. })
        ));
    }

    #[test]
    fn parse_laea_omerc_and_cass() {
        let laea = parse_proj_string(
            "+proj=laea +lat_0=52 +lon_0=10 +x_0=4321000 +y_0=3210000 +ellps=GRS80 +units=m",
        )
        .unwrap();
        assert!(matches!(
            laea,
            CrsDef::Projected(p)
                if matches!(p.method(), ProjectionMethod::LambertAzimuthalEqualArea { .. })
        ));

        let omerc = parse_proj_string(
            "+proj=omerc +no_uoff +lat_0=45.3091666666667 +lonc=-86 +alpha=337.25556 +gamma=337.25556 +k=0.9996 +x_0=2546731.496 +y_0=-4354009.816 +datum=NAD83 +units=m",
        )
        .unwrap();
        assert!(matches!(
            omerc,
            CrsDef::Projected(p)
                if matches!(
                    p.method(),
                    ProjectionMethod::HotineObliqueMercator {
                        variant_b: false,
                        ..
                    }
                )
        ));

        let cass = parse_proj_string(
            "+proj=cass +lat_0=10.4416666667 +lon_0=-61.3333333333 +x_0=430000 +y_0=325000 +ellps=WGS84 +units=m",
        )
        .unwrap();
        assert!(matches!(
            cass,
            CrsDef::Projected(p) if matches!(p.method(), ProjectionMethod::CassiniSoldner { .. })
        ));
    }

    /// Convert a WGS 84 longitude/latitude into `definition`.
    fn project(source: &str, definition: &str, input: (f64, f64)) -> (f64, f64) {
        let source = parse_proj_string(source).unwrap();
        let target =
            parse_proj_string(definition).unwrap_or_else(|err| panic!("{definition}: {err}"));
        proj_core::Transform::from_crs_defs(&source, &target)
            .unwrap_or_else(|err| panic!("{definition}: {err}"))
            .convert(input)
            .unwrap_or_else(|err| panic!("{definition}: {err}"))
    }

    fn assert_near(actual: (f64, f64), expected: (f64, f64), tolerance: f64, label: &str) {
        assert!(
            (actual.0 - expected.0).abs() < tolerance && (actual.1 - expected.1).abs() < tolerance,
            "{label}: {actual:?} != {expected:?}"
        );
    }

    #[test]
    fn implemented_projection_families_match_c_proj() {
        // Reference values from C PROJ 9.6 `proj_create_crs_to_crs`.
        const WGS84: &str = "+proj=longlat +datum=WGS84";
        const BESSEL: &str = "+proj=longlat +ellps=bessel +towgs84=0,0,0";
        const KROVAK: &str = "+proj=krovak +lat_0=49.5 +lon_0=24.83333333333333 +alpha=30.28813972222222 +k=0.9999 +x_0=0 +y_0=0 +ellps=bessel +towgs84=0,0,0 +units=m +no_defs";
        for (source, definition, input, expected) in [
            (
                WGS84,
                "+proj=webmerc +datum=WGS84 +type=crs",
                (10.0, 45.0),
                (1113194.9079327357, 5621521.486192066),
            ),
            (
                WGS84,
                "+proj=eqearth +lon_0=10 +x_0=1000 +y_0=2000 +datum=WGS84 +type=crs",
                (30.0, 45.0),
                (1644608.3054439558, 5468867.760213718),
            ),
            (
                WGS84,
                "+proj=poly +lat_0=10 +lon_0=-54 +x_0=5000000 +y_0=10000000 +datum=WGS84 +type=crs",
                (-50.0, -20.0),
                (5418548.571027056, 6676781.7262324765),
            ),
            (
                WGS84,
                "+proj=aeqd +lat_0=40 +lon_0=-100 +x_0=100 +y_0=200 +datum=WGS84 +type=crs",
                (-90.0, 45.0),
                (787729.8931984723, 601446.6024790345),
            ),
            (
                WGS84,
                "+proj=aeqd +guam +lat_0=13.4724663 +lon_0=144.748750 +x_0=50000 +y_0=50000 +datum=WGS84 +type=crs",
                (144.8, 13.5),
                (55548.50307553842, 53046.761721513125),
            ),
            (
                WGS84,
                "+proj=eqc +lat_ts=30 +lat_0=0 +lon_0=10 +x_0=100 +y_0=200 +datum=WGS84 +type=crs",
                (20.0, 40.0),
                (964155.0696332284, 4452979.631730943),
            ),
            (
                BESSEL,
                KROVAK,
                (15.0, 50.0),
                (-703105.6899367272, -1058219.6008260138),
            ),
            (
                BESSEL,
                "+proj=krovak +ellps=bessel +towgs84=0,0,0 +type=crs",
                (15.0, 50.0),
                (-703105.6899367272, -1058219.6008260138),
            ),
        ] {
            assert_near(project(source, definition, input), expected, 1e-4, definition);
        }
    }

    #[test]
    fn named_ellipsoids_match_c_proj() {
        // `+proj=merc` of (10, 45) from C PROJ 9.6.
        for (name, expected) in [
            ("GRS67", (1113198.9221900152, 5591315.06259037)),
            ("WGS72", (1113194.5588668853, 5591294.446489025)),
            ("clrk80", (1113214.4809276322, 5590901.0607191995)),
            ("clrk80ign", (1113214.490526943, 5590901.079772257)),
            ("aust_SA", (1113198.9221900152, 5591315.34950132)),
            ("mod_airy", (1113055.8381780747, 5590705.14936335)),
            ("bess_nam", (1113080.9143706355, 5590813.785649478)),
            ("evrst30", (1113044.6952979981, 5590796.939130907)),
            ("sphere", (1111948.7428468117, 5615228.478780747)),
        ] {
            let source = format!("+proj=longlat +ellps={name} +towgs84=0,0,0");
            let target = format!("+proj=merc +ellps={name} +towgs84=0,0,0");
            assert_near(
                project(&source, &target, (10.0, 45.0)),
                expected,
                1e-6,
                name,
            );
        }
    }

    #[test]
    fn custom_ellipsoid_parameters_define_the_figure() {
        let ellipsoid = |definition: &str| {
            parse_proj_string(definition)
                .unwrap_or_else(|err| panic!("{definition}: {err}"))
                .datum()
                .ellipsoid()
        };
        let reference = ellipsoid::BESSEL1841;
        let rf = reference.inverse_flattening();
        let a = reference.semi_major_axis();
        for definition in [
            format!("+proj=longlat +a={a} +rf={rf}"),
            format!("+proj=longlat +a={a} +f={}", 1.0 / rf),
            format!("+proj=longlat +a={a} +b={}", reference.b()),
        ] {
            let parsed = ellipsoid(&definition);
            assert_eq!(parsed.semi_major_axis(), a, "{definition}");
            assert!((parsed.flattening() - reference.flattening()).abs() < 1e-14);
        }

        for sphere in ["+proj=longlat +R=6371000", "+proj=longlat +a=6371000"] {
            let parsed = ellipsoid(sphere);
            assert_eq!(parsed.semi_major_axis(), 6_371_000.0, "{sphere}");
            assert_eq!(parsed.flattening(), 0.0, "{sphere}");
        }
    }

    #[test]
    fn custom_ellipsoids_stay_datum_unknown() {
        // A figure alone does not identify a datum, so no transform is implied.
        let crs = parse_proj_string("+proj=longlat +a=6378137 +rf=298.257222101").unwrap();
        assert!(matches!(crs.datum().to_wgs84(), DatumToWgs84::Unknown));
    }

    #[test]
    fn invalid_ellipsoid_combinations_are_rejected() {
        for (definition, expected) in [
            (
                "+proj=longlat +ellps=GRS80 +a=6378137",
                "cannot combine +ellps",
            ),
            ("+proj=longlat +R=6371000 +a=6371000", "cannot combine +R"),
            ("+proj=longlat +b=6356752", "requires +a"),
            ("+proj=longlat +a=6378137 +b=6356752 +rf=298", "only one of"),
            ("+proj=longlat +a=6378137 +es=0.006", "+es"),
            ("+proj=longlat +a=-1", "semi-major"),
        ] {
            let err = parse_proj_string(definition).unwrap_err().to_string();
            assert!(err.contains(expected), "{definition}: {err}");
        }
    }

    #[test]
    fn legacy_web_mercator_resolves_to_epsg_3857() {
        for definition in [
            "+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +nadgrids=@null +wktext +no_defs",
            "+proj=merc +a=6378137 +b=6378137 +lat_ts=0.0 +lon_0=0.0 +x_0=0.0 +y_0=0 +k=1.0 +units=m +nadgrids=@null +no_defs",
            "+proj=merc +a=6378137 +b=6378137 +towgs84=0,0,0,0,0,0,0 +nadgrids=@null +wktext",
        ] {
            let crs = parse_proj_string(definition)
                .unwrap_or_else(|err| panic!("{definition}: {err}"));
            assert_eq!(crs.epsg(), 3857, "{definition}");
        }
    }

    #[test]
    fn spherical_mercator_without_null_grids_is_not_web_mercator() {
        let crs = parse_proj_string("+proj=merc +a=6378137 +b=6378137").unwrap();
        assert_eq!(crs.epsg(), 0);
        assert!(matches!(
            crs.as_projected().unwrap().method(),
            ProjectionMethod::Mercator { .. }
        ));
    }

    #[test]
    fn projection_constraints_are_enforced() {
        for (definition, expected) in [
            ("+proj=webmerc +lon_0=10 +datum=WGS84", "nonzero +lon_0"),
            ("+proj=webmerc +x_0=1 +datum=WGS84", "nonzero +x_0"),
            ("+proj=webmerc +ellps=bessel", "semi-major axis"),
            ("+proj=eqc +lat_0=10 +datum=WGS84", "nonzero +lat_0"),
            ("+proj=krovak +datum=WGS84", "requires the Bessel"),
            ("+proj=krovak +czech +ellps=bessel", "+czech"),
            ("+proj=aeqd +guam=1 +datum=WGS84", "+guam value"),
        ] {
            let err = parse_proj_string(definition).unwrap_err().to_string();
            assert!(err.contains(expected), "{definition}: {err}");
        }
    }

    #[test]
    fn reject_non_polar_stereographic() {
        let err = parse_proj_string("+proj=stere +lat_0=52 +lon_0=5 +k=0.9999").unwrap_err();
        assert!(err
            .to_string()
            .contains("only polar stereographic PROJ strings are supported"));
    }
}

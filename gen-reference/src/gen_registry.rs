/// Generate comprehensive EPSG registry from proj.db.
///
/// Run: cd gen-reference && cargo run --bin gen-registry > ../proj-core/src/registry_data_gen.rs

use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

/// Wrapper that always prints f64 with a decimal point (valid Rust float literal).
struct F(f64);
impl fmt::Display for F {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0.0 {
            write!(f, "0.0")
        } else if self.0.fract() == 0.0 {
            write!(f, "{:.1}", self.0)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

fn find_proj_db() -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    for entry in walkdir(&target_dir, "proj.db") {
        if !entry.to_string_lossy().contains("for_tests") {
            return entry;
        }
    }
    panic!("proj.db not found. Run `cargo build` first.");
}

fn walkdir(dir: &std::path::Path, name: &str) -> Vec<PathBuf> {
    let mut r = Vec::new();
    if let Ok(es) = std::fs::read_dir(dir) {
        for e in es.flatten() {
            let p = e.path();
            if p.is_dir() { r.extend(walkdir(&p, name)); }
            else if p.file_name().and_then(|n| n.to_str()) == Some(name) { r.push(p); }
        }
    }
    r
}

// EPSG parameter codes
const LAT_ORIGIN: i64 = 8801;
const LON_ORIGIN: i64 = 8802;
const SCALE_FACTOR: i64 = 8805;
const FALSE_EASTING: i64 = 8806;
const FALSE_NORTHING: i64 = 8807;
const LAT_1ST_PARALLEL: i64 = 8823;
const LAT_2ND_PARALLEL: i64 = 8824;
const LAT_FALSE_ORIGIN: i64 = 8821;
const LON_FALSE_ORIGIN: i64 = 8822;
const EASTING_FALSE_ORIGIN: i64 = 8826;
const NORTHING_FALSE_ORIGIN: i64 = 8827;
const LAT_STD_PARALLEL: i64 = 8832;
const LON_OF_ORIGIN: i64 = 8833;

// EPSG method codes → our variant names
fn method_code_to_variant(code: i64) -> Option<&'static str> {
    match code {
        9807 => Some("TransverseMercator"),         // Transverse Mercator
        9804 => Some("Mercator"),                   // Mercator (variant A)
        9805 => Some("Mercator"),                   // Mercator (variant B)
        9801 => Some("LambertConformalConic"),       // Lambert Conic Conformal (1SP)
        9802 => Some("LambertConformalConic"),       // Lambert Conic Conformal (2SP)
        9822 => Some("AlbersEqualArea"),            // Albers Equal Area
        9810 => Some("PolarStereographic"),         // Polar Stereographic (variant A)
        9829 => Some("PolarStereographic"),         // Polar Stereographic (variant B)
        9842 => Some("EquidistantCylindrical"),     // Equidistant Cylindrical
        9843 => Some("EquidistantCylindrical"),     // Equidistant Cylindrical (Spherical)
        1024 => Some("WebMercator"),                // Popular Visualisation Pseudo Mercator
        _ => None,
    }
}

// UOM conversion to degrees (for angular params)
fn uom_to_degrees(uom_code: i64) -> f64 {
    match uom_code {
        9102 => 1.0,            // degree
        9110 => 1.0,            // sexagesimal DMS (we'll handle this separately)
        9101 => 180.0 / std::f64::consts::PI, // radian
        9105 => 1.0 / 3600.0,  // arc-second
        9104 => 1.0 / 60.0,    // arc-minute
        9122 => 1.0,            // degree (supplier to define representation)
        9107 => 180.0 / 200.0,  // grad
        _ => 1.0,               // assume degrees
    }
}

fn convert_dms_to_degrees(dms_value: f64) -> f64 {
    // EPSG DMS format: degrees.MMSSsss
    let sign = dms_value.signum();
    let abs_val = dms_value.abs();
    let degrees = abs_val.trunc();
    let rest = (abs_val - degrees) * 100.0;
    let minutes = rest.trunc();
    let seconds = (rest - minutes) * 100.0;
    sign * (degrees + minutes / 60.0 + seconds / 3600.0)
}

struct ConversionParams {
    method_code: i64,
    params: BTreeMap<i64, (f64, i64)>, // param_code → (value, uom_code)
}

fn get_param_degrees(cp: &ConversionParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(val, uom)) = cp.params.get(&code) {
            if uom == 9110 {
                return convert_dms_to_degrees(val);
            }
            return val * uom_to_degrees(uom);
        }
    }
    0.0
}

fn get_param_meters(cp: &ConversionParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(val, _uom)) = cp.params.get(&code) {
            return val; // assume meters or unitless for scale
        }
    }
    0.0
}

fn get_param_scale(cp: &ConversionParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(val, _uom)) = cp.params.get(&code) {
            return val;
        }
    }
    1.0
}

fn main() {
    let db_path = find_proj_db();
    eprintln!("Using proj.db: {}", db_path.display());
    let conn = Connection::open(&db_path).unwrap();

    // Extract ellipsoids
    let mut ellipsoids: BTreeMap<u32, (f64, f64, bool)> = BTreeMap::new(); // code → (a, rf, is_sphere)
    {
        let mut s = conn.prepare(
            "SELECT code, semi_major_axis, inv_flattening, semi_minor_axis FROM ellipsoid WHERE auth_name='EPSG'"
        ).unwrap();
        for row in s.query_map([], |r| {
            let code: u32 = r.get(0)?;
            let a: f64 = r.get(1)?;
            let inv_f: Option<f64> = r.get(2)?;
            let b: Option<f64> = r.get(3)?;
            let (rf, is_sphere) = match inv_f {
                Some(rf) if rf != 0.0 => (rf, false),
                _ => match b {
                    Some(bv) if (a - bv).abs() > 0.001 => (a / (a - bv), false),
                    _ => (0.0, true),
                },
            };
            Ok((code, a, rf, is_sphere))
        }).unwrap().flatten() {
            ellipsoids.insert(row.0, (row.1, row.2, row.3));
        }
    }

    // Extract datums with Helmert to WGS84
    struct DatumInfo { ellipsoid_code: u32, helmert: Option<[f64; 7]> }
    let mut datums: BTreeMap<u32, DatumInfo> = BTreeMap::new();
    {
        let mut s = conn.prepare(
            "SELECT code, ellipsoid_code FROM geodetic_datum WHERE auth_name='EPSG'"
        ).unwrap();
        for row in s.query_map([], |r| Ok((r.get::<_,u32>(0)?, r.get::<_,u32>(1)?))).unwrap().flatten() {
            datums.insert(row.0, DatumInfo { ellipsoid_code: row.1, helmert: None });
        }
    }
    // Get Helmert parameters
    {
        let mut s = conn.prepare(
            "SELECT source_crs_code, tx, ty, tz, rx, ry, rz, px
             FROM helmert_transformation_table
             WHERE source_crs_auth_name='EPSG' AND target_crs_auth_name='EPSG' AND target_crs_code=4326
               AND deprecated=0"
        ).unwrap();
        for row in s.query_map([], |r| {
            Ok((r.get::<_,u32>(0)?,
                [r.get::<_,f64>(1)?, r.get::<_,f64>(2)?, r.get::<_,f64>(3)?,
                 r.get::<_,f64>(4)?, r.get::<_,f64>(5)?, r.get::<_,f64>(6)?,
                 r.get::<_,f64>(7)?]))
        }).unwrap().flatten() {
            // helmert_transformation_table has source_crs_code → find corresponding datum
            // The source_crs_code is a geodetic CRS code, we need to map to datum
            if let Ok(datum_code) = conn.query_row(
                "SELECT datum_code FROM geodetic_crs WHERE auth_name='EPSG' AND code=?1",
                [row.0], |r| r.get::<_,u32>(0)
            ) {
                if let Some(d) = datums.get_mut(&datum_code) {
                    if d.helmert.is_none() {
                        d.helmert = Some(row.1);
                    }
                }
            }
        }
    }

    // Geographic CRS
    struct GeoCrs { code: u32, name: String, datum_code: u32 }
    let geo_crs: Vec<GeoCrs> = {
        let mut s = conn.prepare(
            "SELECT code, name, datum_code FROM geodetic_crs
             WHERE auth_name='EPSG' AND type='geographic 2D' AND deprecated=0"
        ).unwrap();
        s.query_map([], |r| Ok(GeoCrs { code: r.get(0)?, name: r.get(1)?, datum_code: r.get(2)? }))
            .unwrap().filter_map(|r| r.ok()).collect()
    };

    // Projected CRS with conversion parameters
    struct ProjCrs { code: u32, name: String, datum_code: u32, variant: String, conv: ConversionParams }
    let mut proj_crs: Vec<ProjCrs> = Vec::new();
    {
        let mut s = conn.prepare(
            "SELECT pc.code, pc.name, gc.datum_code, pc.conversion_auth_name, pc.conversion_code
             FROM projected_crs pc
             JOIN geodetic_crs gc ON pc.geodetic_crs_code = gc.code AND pc.geodetic_crs_auth_name = gc.auth_name
             WHERE pc.auth_name='EPSG' AND pc.deprecated=0"
        ).unwrap();
        let raw: Vec<(u32, String, u32, String, i64)> = s.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        }).unwrap().filter_map(|r| r.ok()).collect();

        for (code, name, datum_code, conv_auth, conv_code) in raw {
            // Get conversion from conversion_table
            let conv = match conn.query_row(
                "SELECT method_code,
                        param1_code, param1_value, param1_uom_code,
                        param2_code, param2_value, param2_uom_code,
                        param3_code, param3_value, param3_uom_code,
                        param4_code, param4_value, param4_uom_code,
                        param5_code, param5_value, param5_uom_code,
                        param6_code, param6_value, param6_uom_code,
                        param7_code, param7_value, param7_uom_code
                 FROM conversion_table WHERE auth_name=?1 AND code=?2",
                rusqlite::params![conv_auth, conv_code],
                |r| {
                    let method_code: i64 = r.get(0)?;
                    let mut params = BTreeMap::new();
                    for i in 0..7 {
                        let base = 1 + i * 3;
                        let pc: Option<i64> = r.get(base)?;
                        let pv: Option<f64> = r.get(base + 1)?;
                        let pu: Option<i64> = r.get(base + 2)?;
                        if let (Some(c), Some(v), Some(u)) = (pc, pv, pu) {
                            params.insert(c, (v, u));
                        }
                    }
                    Ok(ConversionParams { method_code, params })
                }
            ) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let variant = match method_code_to_variant(conv.method_code) {
                Some(v) => v.to_string(),
                None => continue,
            };

            proj_crs.push(ProjCrs { code, name, datum_code, variant, conv });
        }
    }

    eprintln!("Ellipsoids: {}", ellipsoids.len());
    eprintln!("Datums: {}", datums.len());
    eprintln!("Geographic CRS: {}", geo_crs.len());
    eprintln!("Projected CRS (supported): {}", proj_crs.len());

    // Determine used ellipsoids/datums
    let used_datum_codes: BTreeSet<u32> = geo_crs.iter().map(|c| c.datum_code)
        .chain(proj_crs.iter().map(|c| c.datum_code)).collect();
    let used_ellipsoid_codes: BTreeSet<u32> = used_datum_codes.iter()
        .filter_map(|dc| datums.get(dc).map(|d| d.ellipsoid_code)).collect();

    eprintln!("Used datums: {}", used_datum_codes.len());
    eprintln!("Used ellipsoids: {}", used_ellipsoid_codes.len());

    // Generate output
    println!("//! Auto-generated EPSG registry from proj.db.");
    println!("//! DO NOT EDIT — regenerate with gen-reference/src/gen_registry.rs");
    println!("//!");
    println!("//! Geographic CRS: {}", geo_crs.len());
    println!("//! Projected CRS: {}", proj_crs.len());
    println!("//! Ellipsoids: {}", used_ellipsoid_codes.len());
    println!("//! Datums: {}", used_datum_codes.len());
    println!();
    println!("use crate::crs::*;");
    println!("use crate::datum::{{Datum, HelmertParams}};");
    println!("use crate::ellipsoid::Ellipsoid;");
    println!();

    // Ellipsoids
    for &ec in &used_ellipsoid_codes {
        if let Some(&(a, rf, is_sphere)) = ellipsoids.get(&ec) {
            if is_sphere {
                println!("const E{ec}: Ellipsoid = Ellipsoid::sphere({});", F(a));
            } else {
                println!("const E{ec}: Ellipsoid = Ellipsoid::from_a_rf({}, {});", F(a), F(rf));
            }
        }
    }
    println!();

    // Datums
    // WGS84 datum code is 6326
    for &dc in &used_datum_codes {
        if let Some(d) = datums.get(&dc) {
            let eref = format!("E{}", d.ellipsoid_code);
            match &d.helmert {
                Some(h) if h.iter().any(|&v| v != 0.0) => {
                    let is_trans = h[3] == 0.0 && h[4] == 0.0 && h[5] == 0.0 && h[6] == 0.0;
                    if is_trans {
                        println!("const D{dc}: Datum = Datum {{ ellipsoid: {eref}, to_wgs84: Some(HelmertParams::translation({}, {}, {})) }};", F(h[0]), F(h[1]), F(h[2]));
                    } else {
                        println!("const D{dc}: Datum = Datum {{ ellipsoid: {eref}, to_wgs84: Some(HelmertParams {{ dx: {}, dy: {}, dz: {}, rx: {}, ry: {}, rz: {}, ds: {} }}) }};",
                            F(h[0]), F(h[1]), F(h[2]), F(h[3]), F(h[4]), F(h[5]), F(h[6]));
                    }
                }
                _ => {
                    println!("const D{dc}: Datum = Datum {{ ellipsoid: {eref}, to_wgs84: None }};");
                }
            }
        }
    }
    println!();

    // Geographic CRS
    println!("pub(crate) const GEOGRAPHIC_CRS: &[(u32, GeographicCrsDef)] = &[");
    for c in &geo_crs {
        let name = c.name.replace('"', "\\\"");
        println!("    ({}, GeographicCrsDef {{ epsg: {}, datum: D{}, name: \"{}\" }}),", c.code, c.code, c.datum_code, name);
    }
    println!("];");
    println!();

    // Projected CRS
    println!("pub(crate) const PROJECTED_CRS: &[(u32, ProjectedCrsDef)] = &[");
    for c in &proj_crs {
        let name = c.name.replace('"', "\\\"");
        let method = format_method(&c.variant, &c.conv);
        println!("    ({}, ProjectedCrsDef {{ epsg: {}, datum: D{}, method: {}, name: \"{}\" }}),", c.code, c.code, c.datum_code, method, name);
    }
    println!("];");
}

fn format_method(variant: &str, cp: &ConversionParams) -> String {
    match variant {
        "WebMercator" => "ProjectionMethod::WebMercator".to_string(),
        "TransverseMercator" => format!(
            "ProjectionMethod::TransverseMercator {{ lon0: {}, lat0: {}, k0: {}, false_easting: {}, false_northing: {} }}",
            get_param_degrees(cp, &[LON_ORIGIN, LON_FALSE_ORIGIN, LON_OF_ORIGIN]),
            get_param_degrees(cp, &[LAT_ORIGIN, LAT_FALSE_ORIGIN]),
            get_param_scale(cp, &[SCALE_FACTOR]),
            get_param_meters(cp, &[FALSE_EASTING, EASTING_FALSE_ORIGIN]),
            get_param_meters(cp, &[FALSE_NORTHING, NORTHING_FALSE_ORIGIN]),
        ),
        "Mercator" => format!(
            "ProjectionMethod::Mercator {{ lon0: {}, lat_ts: {}, k0: {}, false_easting: {}, false_northing: {} }}",
            get_param_degrees(cp, &[LON_ORIGIN]),
            get_param_degrees(cp, &[LAT_1ST_PARALLEL, LAT_STD_PARALLEL]),
            get_param_scale(cp, &[SCALE_FACTOR]),
            get_param_meters(cp, &[FALSE_EASTING]),
            get_param_meters(cp, &[FALSE_NORTHING]),
        ),
        "LambertConformalConic" => format!(
            "ProjectionMethod::LambertConformalConic {{ lon0: {}, lat0: {}, lat1: {}, lat2: {}, false_easting: {}, false_northing: {} }}",
            get_param_degrees(cp, &[LON_FALSE_ORIGIN, LON_ORIGIN]),
            get_param_degrees(cp, &[LAT_FALSE_ORIGIN, LAT_ORIGIN]),
            get_param_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_param_degrees(cp, &[LAT_2ND_PARALLEL]),
            get_param_meters(cp, &[EASTING_FALSE_ORIGIN, FALSE_EASTING]),
            get_param_meters(cp, &[NORTHING_FALSE_ORIGIN, FALSE_NORTHING]),
        ),
        "AlbersEqualArea" => format!(
            "ProjectionMethod::AlbersEqualArea {{ lon0: {}, lat0: {}, lat1: {}, lat2: {}, false_easting: {}, false_northing: {} }}",
            get_param_degrees(cp, &[LON_FALSE_ORIGIN]),
            get_param_degrees(cp, &[LAT_FALSE_ORIGIN]),
            get_param_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_param_degrees(cp, &[LAT_2ND_PARALLEL]),
            get_param_meters(cp, &[EASTING_FALSE_ORIGIN, FALSE_EASTING]),
            get_param_meters(cp, &[NORTHING_FALSE_ORIGIN, FALSE_NORTHING]),
        ),
        "PolarStereographic" => format!(
            "ProjectionMethod::PolarStereographic {{ lon0: {}, lat_ts: {}, k0: {}, false_easting: {}, false_northing: {} }}",
            get_param_degrees(cp, &[LON_ORIGIN, LON_OF_ORIGIN]),
            get_param_degrees(cp, &[LAT_STD_PARALLEL, LAT_ORIGIN]),
            get_param_scale(cp, &[SCALE_FACTOR]),
            get_param_meters(cp, &[FALSE_EASTING]),
            get_param_meters(cp, &[FALSE_NORTHING]),
        ),
        "EquidistantCylindrical" => format!(
            "ProjectionMethod::EquidistantCylindrical {{ lon0: {}, lat_ts: {}, false_easting: {}, false_northing: {} }}",
            get_param_degrees(cp, &[LON_ORIGIN]),
            get_param_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_param_meters(cp, &[FALSE_EASTING]),
            get_param_meters(cp, &[FALSE_NORTHING]),
        ),
        _ => format!("/* unsupported: {variant} */"),
    }
}

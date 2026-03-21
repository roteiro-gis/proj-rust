/// Generate compact binary EPSG registry from proj.db.
///
/// Run: cd gen-reference && cargo run --bin gen-registry
///
/// Outputs: ../proj-core/data/epsg.bin

use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

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
            if p.is_dir() {
                r.extend(walkdir(&p, name));
            } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
                r.push(p);
            }
        }
    }
    r
}

// Binary format constants (must match epsg_db.rs)
const MAGIC: u32 = 0x45505347;
const ELLIPSOID_RECORD_SIZE: usize = 20;
const DATUM_RECORD_SIZE: usize = 64;
const GEO_CRS_RECORD_SIZE: usize = 8;
const PROJ_CRS_RECORD_SIZE: usize = 72;

const METHOD_WEB_MERCATOR: u8 = 1;
const METHOD_TRANSVERSE_MERCATOR: u8 = 2;
const METHOD_MERCATOR: u8 = 3;
const METHOD_LCC: u8 = 4;
const METHOD_ALBERS: u8 = 5;
const METHOD_POLAR_STEREO: u8 = 6;
const METHOD_EQUIDISTANT_CYL: u8 = 7;

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

fn method_code_to_id(code: i64) -> Option<u8> {
    match code {
        9807 => Some(METHOD_TRANSVERSE_MERCATOR),
        9804 | 9805 => Some(METHOD_MERCATOR),
        9801 | 9802 => Some(METHOD_LCC),
        9822 => Some(METHOD_ALBERS),
        9810 | 9829 => Some(METHOD_POLAR_STEREO),
        9842 | 9843 => Some(METHOD_EQUIDISTANT_CYL),
        1024 => Some(METHOD_WEB_MERCATOR),
        _ => None,
    }
}

fn convert_dms_to_degrees(dms_value: f64) -> f64 {
    // EPSG DMS format: DD.MMSSsss
    // Must round intermediate values to avoid floating point truncation errors
    // (e.g., 46.3 → 29.999... minutes instead of 30)
    let sign = dms_value.signum();
    let abs_val = dms_value.abs();
    let degrees = abs_val.trunc();
    let mm_ss = (abs_val - degrees) * 100.0;
    let minutes = mm_ss.round().min(mm_ss.trunc() + 1.0); // guard against 29.9999→30
    let minutes = if (mm_ss - mm_ss.round()).abs() < 1e-6 {
        mm_ss.round()
    } else {
        mm_ss.trunc()
    };
    let seconds = (mm_ss - minutes) * 100.0;
    sign * (degrees + minutes / 60.0 + seconds / 3600.0)
}

fn uom_to_degrees(uom_code: i64) -> f64 {
    match uom_code {
        9102 | 9122 => 1.0,
        9101 => 180.0 / std::f64::consts::PI,
        9105 => 1.0 / 3600.0,
        9104 => 1.0 / 60.0,
        9107 => 180.0 / 200.0,
        _ => 1.0,
    }
}

struct ConvParams {
    method_code: i64,
    params: BTreeMap<i64, (f64, i64)>, // param_code → (value, uom_code)
}

fn get_degrees(cp: &ConvParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(val, uom)) = cp.params.get(&code) {
            return if uom == 9110 {
                convert_dms_to_degrees(val)
            } else {
                val * uom_to_degrees(uom)
            };
        }
    }
    0.0
}

fn get_meters(cp: &ConvParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(val, _)) = cp.params.get(&code) {
            return val;
        }
    }
    0.0
}

fn get_scale(cp: &ConvParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(val, _)) = cp.params.get(&code) {
            return val;
        }
    }
    1.0
}

fn main() {
    let db_path = find_proj_db();
    eprintln!("Using proj.db: {}", db_path.display());
    let conn = Connection::open(&db_path).unwrap();

    // --- Ellipsoids ---
    let mut ellipsoids: BTreeMap<u32, (f64, f64)> = BTreeMap::new(); // code → (a, inv_f)
    {
        let mut s = conn
            .prepare(
                "SELECT code, semi_major_axis, inv_flattening, semi_minor_axis
             FROM ellipsoid WHERE auth_name='EPSG'",
            )
            .unwrap();
        for row in s
            .query_map([], |r| {
                let code: u32 = r.get(0)?;
                let a: f64 = r.get(1)?;
                let inv_f: Option<f64> = r.get(2)?;
                let b: Option<f64> = r.get(3)?;
                let rf = match inv_f {
                    Some(rf) if rf != 0.0 => rf,
                    _ => match b {
                        Some(bv) if (a - bv).abs() > 0.001 => a / (a - bv),
                        _ => 0.0,
                    },
                };
                Ok((code, a, rf))
            })
            .unwrap()
            .flatten()
        {
            ellipsoids.insert(row.0, (row.1, row.2));
        }
    }

    // --- Datums ---
    struct DatumInfo {
        ellipsoid_code: u32,
        helmert: [f64; 7],
    }
    let mut datums: BTreeMap<u32, DatumInfo> = BTreeMap::new();
    {
        let mut s = conn
            .prepare("SELECT code, ellipsoid_code FROM geodetic_datum WHERE auth_name='EPSG'")
            .unwrap();
        for (code, ec) in s
            .query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, u32>(1)?)))
            .unwrap()
            .flatten()
        {
            datums.insert(
                code,
                DatumInfo {
                    ellipsoid_code: ec,
                    helmert: [0.0; 7],
                },
            );
        }
    }
    // Helmert parameters
    {
        // Select the best Helmert transformation for each CRS→WGS84 path.
        // Prefer 7-parameter over 3-parameter (higher accuracy).
        // Order by: has rotation params DESC, then accuracy ASC.
        let mut s = conn
            .prepare(
                "SELECT source_crs_code, tx, ty, tz,
                        COALESCE(rx, 0.0), COALESCE(ry, 0.0), COALESCE(rz, 0.0),
                        COALESCE(scale_difference, 0.0),
                        COALESCE(accuracy, 999.0)
                 FROM helmert_transformation_table
                 WHERE source_crs_auth_name='EPSG' AND target_crs_auth_name='EPSG'
                   AND target_crs_code=4326 AND deprecated=0
                 ORDER BY source_crs_code,
                          (CASE WHEN rx IS NOT NULL AND rx != 0 THEN 0 ELSE 1 END),
                          COALESCE(accuracy, 999.0)",
            )
            .unwrap();
        // Track best accuracy per datum to avoid overwriting good params with worse ones
        let mut datum_accuracy: BTreeMap<u32, f64> = BTreeMap::new();
        for row in s
            .query_map([], |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    [
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, f64>(3)?,
                        r.get::<_, f64>(4)?,
                        r.get::<_, f64>(5)?,
                        r.get::<_, f64>(6)?,
                        r.get::<_, f64>(7)?,
                    ],
                    r.get::<_, f64>(8)?,
                ))
            })
            .unwrap()
            .flatten()
        {
            let (crs_code, helmert, accuracy) = row;
            if let Ok(datum_code) = conn.query_row(
                "SELECT datum_code FROM geodetic_crs WHERE auth_name='EPSG' AND code=?1",
                [crs_code],
                |r| r.get::<_, u32>(0),
            ) {
                if let Some(d) = datums.get_mut(&datum_code) {
                    let prev_acc = datum_accuracy.get(&datum_code).copied().unwrap_or(999.0);
                    let has_rotation = helmert[3] != 0.0 || helmert[4] != 0.0 || helmert[5] != 0.0;
                    let prev_has_rotation = d.helmert[3] != 0.0 || d.helmert[4] != 0.0 || d.helmert[5] != 0.0;

                    // Prefer 7-parameter over 3-parameter, then lower accuracy value
                    let is_first = !datum_accuracy.contains_key(&datum_code);
                    let is_better = is_first
                        || (has_rotation && !prev_has_rotation)
                        || (has_rotation == prev_has_rotation && accuracy < prev_acc);

                    if is_better {
                        d.helmert = helmert;
                        datum_accuracy.insert(datum_code, accuracy);
                    }
                }
            }
        }
    }

    // --- Geographic CRS ---
    struct GeoCrs {
        code: u32,
        datum_code: u32,
    }
    let geo_crs: Vec<GeoCrs> = {
        let mut s = conn
            .prepare(
                "SELECT code, datum_code FROM geodetic_crs
             WHERE auth_name='EPSG' AND type='geographic 2D' AND deprecated=0
             ORDER BY code",
            )
            .unwrap();
        s.query_map([], |r| {
            Ok(GeoCrs {
                code: r.get(0)?,
                datum_code: r.get(1)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    };

    // --- Projected CRS ---
    struct ProjCrs {
        code: u32,
        datum_code: u32,
        method_id: u8,
        params: [f64; 7],
    }
    let mut proj_crs: Vec<ProjCrs> = Vec::new();
    {
        let mut s = conn
            .prepare(
                "SELECT pc.code, gc.datum_code, pc.conversion_auth_name, pc.conversion_code
             FROM projected_crs pc
             JOIN geodetic_crs gc ON pc.geodetic_crs_code = gc.code
               AND pc.geodetic_crs_auth_name = gc.auth_name
             WHERE pc.auth_name='EPSG' AND pc.deprecated=0
             ORDER BY pc.code",
            )
            .unwrap();
        let raw: Vec<(u32, u32, String, i64)> = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        for (code, datum_code, conv_auth, conv_code) in raw {
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
                    let mc: i64 = r.get(0)?;
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
                    Ok(ConvParams {
                        method_code: mc,
                        params,
                    })
                },
            ) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let method_id = match method_code_to_id(conv.method_code) {
                Some(id) => id,
                None => continue,
            };

            let params = encode_params(method_id, &conv);
            proj_crs.push(ProjCrs {
                code,
                datum_code,
                method_id,
                params,
            });
        }
    }

    // --- Determine used ellipsoids/datums ---
    let used_datum_codes: BTreeSet<u32> = geo_crs
        .iter()
        .map(|c| c.datum_code)
        .chain(proj_crs.iter().map(|c| c.datum_code))
        .collect();
    let used_ellipsoid_codes: BTreeSet<u32> = used_datum_codes
        .iter()
        .filter_map(|dc| datums.get(dc).map(|d| d.ellipsoid_code))
        .collect();

    // Filter to used only, sorted
    let used_ellipsoids: Vec<(u32, f64, f64)> = used_ellipsoid_codes
        .iter()
        .filter_map(|&ec| ellipsoids.get(&ec).map(|&(a, rf)| (ec, a, rf)))
        .collect();
    let used_datums: Vec<(u32, &DatumInfo)> = used_datum_codes
        .iter()
        .filter_map(|&dc| datums.get(&dc).map(|d| (dc, d)))
        .collect();

    eprintln!("Ellipsoids: {}", used_ellipsoids.len());
    eprintln!("Datums: {}", used_datums.len());
    eprintln!("Geographic CRS: {}", geo_crs.len());
    eprintln!("Projected CRS: {}", proj_crs.len());

    // --- Write binary ---
    let out_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../proj-core/data/epsg.bin");
    let mut buf: Vec<u8> = Vec::new();

    // Header (16 bytes)
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // version
    buf.extend_from_slice(&(used_ellipsoids.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(used_datums.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(geo_crs.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(proj_crs.len() as u32).to_le_bytes());

    // Ellipsoid table
    for &(code, a, inv_f) in &used_ellipsoids {
        let mut rec = [0u8; ELLIPSOID_RECORD_SIZE];
        rec[0..4].copy_from_slice(&code.to_le_bytes());
        rec[4..12].copy_from_slice(&a.to_le_bytes());
        rec[12..20].copy_from_slice(&inv_f.to_le_bytes());
        buf.extend_from_slice(&rec);
    }

    // Datum table
    for &(code, datum) in &used_datums {
        let mut rec = [0u8; DATUM_RECORD_SIZE];
        rec[0..4].copy_from_slice(&code.to_le_bytes());
        rec[4..8].copy_from_slice(&datum.ellipsoid_code.to_le_bytes());
        for (i, &val) in datum.helmert.iter().enumerate() {
            let offset = 8 + i * 8;
            rec[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
        }
        buf.extend_from_slice(&rec);
    }

    // Geographic CRS table
    for c in &geo_crs {
        let mut rec = [0u8; GEO_CRS_RECORD_SIZE];
        rec[0..4].copy_from_slice(&c.code.to_le_bytes());
        rec[4..8].copy_from_slice(&c.datum_code.to_le_bytes());
        buf.extend_from_slice(&rec);
    }

    // Projected CRS table
    for c in &proj_crs {
        let mut rec = [0u8; PROJ_CRS_RECORD_SIZE];
        rec[0..4].copy_from_slice(&c.code.to_le_bytes());
        rec[4..8].copy_from_slice(&c.datum_code.to_le_bytes());
        rec[8] = c.method_id;
        // rec[9..16] = padding (zeros)
        for (i, &val) in c.params.iter().enumerate() {
            let offset = 16 + i * 8;
            rec[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
        }
        buf.extend_from_slice(&rec);
    }

    std::fs::write(&out_path, &buf).unwrap();
    eprintln!(
        "Wrote {} bytes ({:.1} KB) to {}",
        buf.len(),
        buf.len() as f64 / 1024.0,
        out_path.display()
    );
}

fn encode_params(method_id: u8, cp: &ConvParams) -> [f64; 7] {
    match method_id {
        METHOD_WEB_MERCATOR => [0.0; 7],
        METHOD_TRANSVERSE_MERCATOR => [
            get_degrees(cp, &[LON_ORIGIN, LON_FALSE_ORIGIN, LON_OF_ORIGIN]),
            get_degrees(cp, &[LAT_ORIGIN, LAT_FALSE_ORIGIN]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING, EASTING_FALSE_ORIGIN]),
            get_meters(cp, &[FALSE_NORTHING, NORTHING_FALSE_ORIGIN]),
            0.0,
            0.0,
        ],
        METHOD_MERCATOR => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL, LAT_STD_PARALLEL]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING]),
            get_meters(cp, &[FALSE_NORTHING]),
            0.0,
            0.0,
        ],
        METHOD_LCC => [
            get_degrees(cp, &[LON_FALSE_ORIGIN, LON_ORIGIN]),
            get_degrees(cp, &[LAT_FALSE_ORIGIN, LAT_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_meters(cp, &[EASTING_FALSE_ORIGIN, FALSE_EASTING]),
            0.0, // unused slot
            get_degrees(cp, &[LAT_2ND_PARALLEL]),
            get_meters(cp, &[NORTHING_FALSE_ORIGIN, FALSE_NORTHING]),
        ],
        METHOD_ALBERS => [
            get_degrees(cp, &[LON_FALSE_ORIGIN]),
            get_degrees(cp, &[LAT_FALSE_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_meters(cp, &[EASTING_FALSE_ORIGIN, FALSE_EASTING]),
            0.0,
            get_degrees(cp, &[LAT_2ND_PARALLEL]),
            get_meters(cp, &[NORTHING_FALSE_ORIGIN, FALSE_NORTHING]),
        ],
        METHOD_POLAR_STEREO => [
            get_degrees(cp, &[LON_ORIGIN, LON_OF_ORIGIN]),
            get_degrees(cp, &[LAT_STD_PARALLEL, LAT_ORIGIN]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING]),
            get_meters(cp, &[FALSE_NORTHING]),
            0.0,
            0.0,
        ],
        METHOD_EQUIDISTANT_CYL => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL]),
            0.0,
            get_meters(cp, &[FALSE_EASTING]),
            get_meters(cp, &[FALSE_NORTHING]),
            0.0,
            0.0,
        ],
        _ => [0.0; 7],
    }
}

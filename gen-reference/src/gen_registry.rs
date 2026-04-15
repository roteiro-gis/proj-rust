/// Generate compact binary EPSG registry from proj.db.
///
/// Run: cd gen-reference && cargo run --bin gen-registry
///
/// Outputs: ../proj-core/data/epsg.bin

use rusqlite::{params, Connection};
use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::PI;
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
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                results.extend(walkdir(&path, name));
            } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
                results.push(path);
            }
        }
    }
    results
}

const MAGIC: u32 = 0x4550_5347;
const VERSION: u16 = 5;

const ELLIPSOID_RECORD_SIZE: usize = 20;
const DATUM_RECORD_SIZE: usize = 72;
const GEO_CRS_RECORD_BASE_SIZE: usize = 8;
const PROJ_CRS_RECORD_BASE_SIZE: usize = 80;

const METHOD_WEB_MERCATOR: u8 = 1;
const METHOD_TRANSVERSE_MERCATOR: u8 = 2;
const METHOD_MERCATOR: u8 = 3;
const METHOD_LCC: u8 = 4;
const METHOD_ALBERS: u8 = 5;
const METHOD_POLAR_STEREO: u8 = 6;
const METHOD_EQUIDISTANT_CYL: u8 = 7;

const DATUM_SHIFT_UNKNOWN: u8 = 0;
const DATUM_SHIFT_IDENTITY: u8 = 1;
const DATUM_SHIFT_HELMERT: u8 = 2;

const OP_HELMERT: u8 = 1;
const OP_GRID_SHIFT: u8 = 2;
const OP_CONCATENATED: u8 = 3;

const FLAG_DEPRECATED: u8 = 1 << 0;
const FLAG_PREFERRED: u8 = 1 << 1;
const FLAG_APPROXIMATE: u8 = 1 << 2;

const GRID_FORMAT_NTV2: u8 = 1;
const GRID_FORMAT_UNSUPPORTED: u8 = 255;

const GRID_INTERPOLATION_BILINEAR: u8 = 1;

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

#[derive(Clone, Copy)]
enum DatumShiftKind {
    Unknown,
    Identity,
    Helmert,
}

struct DatumInfo {
    ellipsoid_code: u32,
    shift_kind: DatumShiftKind,
    helmert: [f64; 7],
}

struct GeoCrs {
    code: u32,
    datum_code: u32,
    name: String,
}

struct ProjCrs {
    code: u32,
    base_geographic_crs_code: u32,
    datum_code: u32,
    method_id: u8,
    linear_unit_to_meter: f64,
    params: [f64; 7],
    name: String,
}

#[derive(Clone)]
struct ExtentRecord {
    code: u32,
    name: String,
    west: f64,
    south: f64,
    east: f64,
    north: f64,
}

#[derive(Clone)]
struct GridRecord {
    id: u32,
    name: String,
    format: u8,
    interpolation: u8,
    area_code: u32,
    resource_names: Vec<String>,
}

#[derive(Clone)]
enum OperationPayload {
    Helmert([f64; 7]),
    GridShift { grid_id: u32, direction: u8, interpolation: u8 },
    Concatenated { steps: Vec<(u32, u8)> },
}

#[derive(Clone)]
struct OperationRecord {
    table_name: &'static str,
    code: u32,
    name: String,
    source_crs_code: u32,
    target_crs_code: u32,
    source_datum_code: u32,
    target_datum_code: u32,
    accuracy: Option<f64>,
    deprecated: bool,
    preferred: bool,
    approximate: bool,
    area_codes: Vec<u32>,
    payload: OperationPayload,
}

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
    let sign = dms_value.signum();
    let abs_val = dms_value.abs();
    let degrees = abs_val.trunc();
    let mm_ss = (abs_val - degrees) * 100.0;
    let minutes = if (mm_ss - mm_ss.round()).abs() < 1e-6 {
        mm_ss.round()
    } else {
        mm_ss.trunc()
    };
    let seconds = (mm_ss - minutes) * 100.0;
    sign * (degrees + minutes / 60.0 + seconds / 3600.0)
}

struct ConvParams {
    method_code: i64,
    params: BTreeMap<i64, (f64, i64)>,
}

fn get_degrees(cp: &ConvParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(value, uom)) = cp.params.get(&code) {
            return if uom == 9110 {
                convert_dms_to_degrees(value)
            } else {
                let factor = match uom {
                    9102 | 9122 => 1.0,
                    9101 => 180.0 / PI,
                    9105 => 1.0 / 3600.0,
                    9104 => 1.0 / 60.0,
                    9107 => 180.0 / 200.0,
                    _ => panic!(
                        "unsupported angular unit EPSG:{uom} for parameter EPSG:{code} in conversion method EPSG:{}",
                        cp.method_code
                    ),
                };
                value * factor
            };
        }
    }
    0.0
}

fn get_meters(cp: &ConvParams, codes: &[i64], linear_uoms: &BTreeMap<i64, f64>) -> f64 {
    for &code in codes {
        if let Some(&(value, uom)) = cp.params.get(&code) {
            let factor = linear_uoms.get(&uom).copied().unwrap_or_else(|| {
                panic!(
                    "unsupported linear unit EPSG:{uom} for parameter EPSG:{code} in conversion method EPSG:{}",
                    cp.method_code
                )
            });
            return value * factor;
        }
    }
    0.0
}

fn get_scale(cp: &ConvParams, codes: &[i64]) -> f64 {
    for &code in codes {
        if let Some(&(value, _)) = cp.params.get(&code) {
            return value;
        }
    }
    1.0
}

fn parse_u32_code(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok()
}

fn encode_params(method_id: u8, cp: &ConvParams, linear_uoms: &BTreeMap<i64, f64>) -> [f64; 7] {
    match method_id {
        METHOD_WEB_MERCATOR => [0.0; 7],
        METHOD_TRANSVERSE_MERCATOR => [
            get_degrees(cp, &[LON_ORIGIN, LON_FALSE_ORIGIN, LON_OF_ORIGIN]),
            get_degrees(cp, &[LAT_ORIGIN, LAT_FALSE_ORIGIN]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING, EASTING_FALSE_ORIGIN], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING, NORTHING_FALSE_ORIGIN], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_MERCATOR => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL, LAT_STD_PARALLEL]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_LCC => [
            get_degrees(cp, &[LON_FALSE_ORIGIN, LON_ORIGIN]),
            get_degrees(cp, &[LAT_FALSE_ORIGIN, LAT_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_meters(cp, &[EASTING_FALSE_ORIGIN, FALSE_EASTING], linear_uoms),
            0.0,
            get_degrees(cp, &[LAT_2ND_PARALLEL]),
            get_meters(cp, &[NORTHING_FALSE_ORIGIN, FALSE_NORTHING], linear_uoms),
        ],
        METHOD_ALBERS => [
            get_degrees(cp, &[LON_FALSE_ORIGIN]),
            get_degrees(cp, &[LAT_FALSE_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL]),
            get_meters(cp, &[EASTING_FALSE_ORIGIN, FALSE_EASTING], linear_uoms),
            0.0,
            get_degrees(cp, &[LAT_2ND_PARALLEL]),
            get_meters(cp, &[NORTHING_FALSE_ORIGIN, FALSE_NORTHING], linear_uoms),
        ],
        METHOD_POLAR_STEREO => [
            get_degrees(cp, &[LON_ORIGIN, LON_OF_ORIGIN]),
            get_degrees(cp, &[LAT_STD_PARALLEL, LAT_ORIGIN]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_EQUIDISTANT_CYL => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL]),
            0.0,
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
            0.0,
            0.0,
        ],
        _ => [0.0; 7],
    }
}

fn grid_format_from_method(method_name: &str) -> u8 {
    if method_name == "NTv2" {
        GRID_FORMAT_NTV2
    } else {
        GRID_FORMAT_UNSUPPORTED
    }
}

fn main() {
    let db_path = find_proj_db();
    eprintln!("Using proj.db: {}", db_path.display());
    let conn = Connection::open(&db_path).unwrap();

    let mut ellipsoids: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT code, semi_major_axis, inv_flattening, semi_minor_axis
                 FROM ellipsoid
                 WHERE auth_name='EPSG'",
            )
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                let code: u32 = row.get(0)?;
                let a: f64 = row.get(1)?;
                let inv_f: Option<f64> = row.get(2)?;
                let b: Option<f64> = row.get(3)?;
                let rf = match inv_f {
                    Some(value) if value != 0.0 => value,
                    _ => match b {
                        Some(semi_minor) if (a - semi_minor).abs() > 0.001 => a / (a - semi_minor),
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

    let mut datums: BTreeMap<u32, DatumInfo> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT code, ellipsoid_code FROM geodetic_datum WHERE auth_name='EPSG'")
            .unwrap();
        for (code, ellipsoid_code) in stmt
            .query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))
            .unwrap()
            .flatten()
        {
            datums.insert(
                code,
                DatumInfo {
                    ellipsoid_code,
                    shift_kind: DatumShiftKind::Unknown,
                    helmert: [0.0; 7],
                },
            );
        }
    }

    if let Ok(wgs84_datum_code) = conn.query_row(
        "SELECT datum_code FROM geodetic_crs WHERE auth_name='EPSG' AND code=4326",
        [],
        |row| row.get::<_, u32>(0),
    ) {
        if let Some(datum) = datums.get_mut(&wgs84_datum_code) {
            datum.shift_kind = DatumShiftKind::Identity;
        }
    }

    let linear_uoms: BTreeMap<i64, f64> = {
        let mut stmt = conn
            .prepare(
                "SELECT code, conv_factor
                 FROM unit_of_measure
                 WHERE auth_name='EPSG' AND type='length'",
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?)))
            .unwrap()
            .filter_map(|row| row.ok())
            .filter_map(|(code, factor)| factor.map(|factor| (code, factor)))
            .collect()
    };
    let angle_uoms: BTreeMap<i64, f64> = {
        let mut stmt = conn
            .prepare(
                "SELECT code, conv_factor
                 FROM unit_of_measure
                 WHERE auth_name='EPSG' AND type='angle'",
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?)))
            .unwrap()
            .filter_map(|row| row.ok())
            .filter_map(|(code, factor)| factor.map(|factor| (code, factor)))
            .collect()
    };
    let scale_uoms: BTreeMap<i64, f64> = {
        let mut stmt = conn
            .prepare(
                "SELECT code, conv_factor
                 FROM unit_of_measure
                 WHERE auth_name='EPSG' AND type='scale'",
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?)))
            .unwrap()
            .filter_map(|row| row.ok())
            .filter_map(|(code, factor)| factor.map(|factor| (code, factor)))
            .collect()
    };

    {
        let mut stmt = conn
            .prepare(
                "SELECT CAST(source_crs_code AS TEXT),
                        tx, ty, tz,
                        COALESCE(rx, 0.0), COALESCE(ry, 0.0), COALESCE(rz, 0.0),
                        rotation_uom_code,
                        COALESCE(scale_difference, 0.0),
                        scale_difference_uom_code,
                        COALESCE(accuracy, 999.0)
                 FROM helmert_transformation_table
                 WHERE auth_name='EPSG'
                   AND source_crs_auth_name='EPSG'
                   AND target_crs_auth_name='EPSG'
                   AND target_crs_code=4326
                   AND deprecated=0
                 ORDER BY CAST(source_crs_code AS INTEGER),
                          (CASE WHEN rx IS NOT NULL AND rx != 0 THEN 0 ELSE 1 END),
                          COALESCE(accuracy, 999.0)",
            )
            .unwrap();
        let mut datum_accuracy: BTreeMap<u32, f64> = BTreeMap::new();
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, f64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, f64>(10)?,
                ))
            })
            .unwrap()
            .flatten()
        {
            let Some(crs_code) = parse_u32_code(&row.0) else {
                continue;
            };
            let rx = match row.7.and_then(|uom| angle_uoms.get(&uom).copied()) {
                Some(conv) => row.4 * conv * 180.0 / PI * 3600.0,
                None if row.4 == 0.0 => 0.0,
                None => continue,
            };
            let ry = match row.7.and_then(|uom| angle_uoms.get(&uom).copied()) {
                Some(conv) => row.5 * conv * 180.0 / PI * 3600.0,
                None if row.5 == 0.0 => 0.0,
                None => continue,
            };
            let rz = match row.7.and_then(|uom| angle_uoms.get(&uom).copied()) {
                Some(conv) => row.6 * conv * 180.0 / PI * 3600.0,
                None if row.6 == 0.0 => 0.0,
                None => continue,
            };
            let ds = match row.9.and_then(|uom| scale_uoms.get(&uom).copied()) {
                Some(conv) => row.8 * conv * 1_000_000.0,
                None if row.8 == 0.0 => 0.0,
                None => continue,
            };
            let helmert = [row.1, row.2, row.3, rx, ry, rz, ds];

            if let Ok(datum_code) = conn.query_row(
                "SELECT datum_code FROM geodetic_crs WHERE auth_name='EPSG' AND code=?1",
                [crs_code],
                |row| row.get::<_, u32>(0),
            ) {
                if let Some(datum) = datums.get_mut(&datum_code) {
                    let prev_accuracy = datum_accuracy.get(&datum_code).copied().unwrap_or(999.0);
                    let has_rotation = helmert[3] != 0.0 || helmert[4] != 0.0 || helmert[5] != 0.0;
                    let prev_has_rotation =
                        datum.helmert[3] != 0.0 || datum.helmert[4] != 0.0 || datum.helmert[5] != 0.0;
                    let first = !datum_accuracy.contains_key(&datum_code);
                    let better = first
                        || (has_rotation && !prev_has_rotation)
                        || (has_rotation == prev_has_rotation && row.10 < prev_accuracy);
                    if better {
                        datum.shift_kind = if helmert.iter().all(|value| *value == 0.0) {
                            DatumShiftKind::Identity
                        } else {
                            DatumShiftKind::Helmert
                        };
                        datum.helmert = helmert;
                        datum_accuracy.insert(datum_code, row.10);
                    }
                }
            }
        }
    }

    let geo_crs: Vec<GeoCrs> = {
        let mut stmt = conn
            .prepare(
                "SELECT code, datum_code, name
                 FROM geodetic_crs
                 WHERE auth_name='EPSG' AND type='geographic 2D' AND deprecated=0
                 ORDER BY code",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok(GeoCrs {
                code: row.get(0)?,
                datum_code: row.get(1)?,
                name: row.get(2)?,
            })
        })
        .unwrap()
        .filter_map(|row| row.ok())
        .collect()
    };
    let geo_codes: BTreeSet<u32> = geo_crs.iter().map(|crs| crs.code).collect();

    let mut proj_crs: Vec<ProjCrs> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT pc.code,
                        pc.geodetic_crs_code,
                        gc.datum_code,
                        pc.name,
                        pc.conversion_auth_name,
                        pc.conversion_code,
                        a.uom_code,
                        u.conv_factor
                 FROM projected_crs pc
                 JOIN geodetic_crs gc
                   ON gc.auth_name = pc.geodetic_crs_auth_name
                  AND gc.code = pc.geodetic_crs_code
                 LEFT JOIN axis a
                   ON a.coordinate_system_auth_name = pc.coordinate_system_auth_name
                  AND a.coordinate_system_code = pc.coordinate_system_code
                  AND a.coordinate_system_order = 1
                 LEFT JOIN unit_of_measure u
                   ON u.auth_name = a.uom_auth_name
                  AND u.code = a.uom_code
                 WHERE pc.auth_name='EPSG' AND pc.deprecated=0
                 ORDER BY pc.code",
            )
            .unwrap();
        let rows: Vec<(u32, u32, u32, String, String, i64, Option<i64>, Option<f64>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .unwrap()
            .filter_map(|row| row.ok())
            .collect();

        for (
            code,
            base_geographic_crs_code,
            datum_code,
            name,
            conv_auth,
            conv_code,
            axis_uom_code,
            axis_unit_factor,
        ) in rows
        {
            let linear_unit_to_meter = match (axis_uom_code, axis_unit_factor) {
                (Some(_), Some(factor)) => factor,
                (Some(uom_code), None) => {
                    panic!("projected CRS EPSG:{code} uses unsupported axis linear unit EPSG:{uom_code}")
                }
                (None, _) => panic!("projected CRS EPSG:{code} is missing axis linear unit metadata"),
            };
            let conv = match conn.query_row(
                "SELECT method_code,
                        param1_code, param1_value, param1_uom_code,
                        param2_code, param2_value, param2_uom_code,
                        param3_code, param3_value, param3_uom_code,
                        param4_code, param4_value, param4_uom_code,
                        param5_code, param5_value, param5_uom_code,
                        param6_code, param6_value, param6_uom_code,
                        param7_code, param7_value, param7_uom_code
                 FROM conversion_table
                 WHERE auth_name=?1 AND code=?2",
                params![conv_auth, conv_code],
                |row| {
                    let method_code: i64 = row.get(0)?;
                    let mut params = BTreeMap::new();
                    for index in 0..7 {
                        let base = 1 + index * 3;
                        let param_code: Option<i64> = row.get(base)?;
                        let param_value: Option<f64> = row.get(base + 1)?;
                        let param_uom: Option<i64> = row.get(base + 2)?;
                        if let (Some(code), Some(value), Some(uom)) = (param_code, param_value, param_uom) {
                            params.insert(code, (value, uom));
                        }
                    }
                    Ok(ConvParams { method_code, params })
                },
            ) {
                Ok(value) => value,
                Err(_) => continue,
            };

            let Some(method_id) = method_code_to_id(conv.method_code) else {
                continue;
            };
            let params = encode_params(method_id, &conv, &linear_uoms);
            proj_crs.push(ProjCrs {
                code,
                base_geographic_crs_code,
                datum_code,
                method_id,
                linear_unit_to_meter,
                params,
                name,
            });
        }
    }

    let used_datum_codes: BTreeSet<u32> = geo_crs
        .iter()
        .map(|crs| crs.datum_code)
        .chain(proj_crs.iter().map(|crs| crs.datum_code))
        .collect();
    let used_ellipsoid_codes: BTreeSet<u32> = used_datum_codes
        .iter()
        .filter_map(|datum_code| datums.get(datum_code).map(|datum| datum.ellipsoid_code))
        .collect();

    let used_ellipsoids: Vec<(u32, f64, f64)> = used_ellipsoid_codes
        .iter()
        .filter_map(|code| ellipsoids.get(code).map(|(a, rf)| (*code, *a, *rf)))
        .collect();
    let used_datums: Vec<(u32, &DatumInfo)> = used_datum_codes
        .iter()
        .filter_map(|code| datums.get(code).map(|datum| (*code, datum)))
        .collect();

    let mut grid_resources: Vec<GridRecord> = Vec::new();
    let mut grid_resource_ids: BTreeMap<(String, String, String), u32> = BTreeMap::new();
    let mut operations: Vec<OperationRecord> = Vec::new();

    {
        let mut stmt = conn
            .prepare(
                "SELECT CAST(gt.code AS TEXT),
                        gt.name,
                        CAST(gt.source_crs_code AS TEXT),
                        CAST(gt.target_crs_code AS TEXT),
                        src.datum_code,
                        tgt.datum_code,
                        gt.accuracy,
                        gt.method_name,
                        gt.grid_name,
                        COALESCE(gt.grid2_name, ''),
                        gt.deprecated
                 FROM grid_transformation gt
                 JOIN geodetic_crs src
                   ON src.auth_name = gt.source_crs_auth_name
                  AND src.code = gt.source_crs_code
                  AND src.type = 'geographic 2D'
                 JOIN geodetic_crs tgt
                   ON tgt.auth_name = gt.target_crs_auth_name
                  AND tgt.code = gt.target_crs_code
                  AND tgt.type = 'geographic 2D'
                 WHERE gt.auth_name='EPSG'
                   AND gt.source_crs_auth_name='EPSG'
                   AND gt.target_crs_auth_name='EPSG'
                 ORDER BY CAST(gt.code AS INTEGER)",
            )
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, bool>(10)?,
                ))
            })
            .unwrap()
            .flatten()
        {
            let Some(code) = parse_u32_code(&row.0) else {
                continue;
            };
            let Some(source_crs_code) = parse_u32_code(&row.2) else {
                continue;
            };
            let Some(target_crs_code) = parse_u32_code(&row.3) else {
                continue;
            };
            if !geo_codes.contains(&source_crs_code) || !geo_codes.contains(&target_crs_code) {
                continue;
            }

            let grid_key = (row.7.clone(), row.8.clone(), row.9.clone());
            let grid_id = if let Some(id) = grid_resource_ids.get(&grid_key).copied() {
                id
            } else {
                let id = grid_resources.len() as u32 + 1;
                let mut resource_names = vec![row.8.clone()];
                if !row.9.is_empty() {
                    resource_names.push(row.9.clone());
                }
                grid_resources.push(GridRecord {
                    id,
                    name: row.8.clone(),
                    format: grid_format_from_method(&row.7),
                    interpolation: GRID_INTERPOLATION_BILINEAR,
                    area_code: 0,
                    resource_names,
                });
                grid_resource_ids.insert(grid_key, id);
                id
            };

            operations.push(OperationRecord {
                table_name: "grid_transformation",
                code,
                name: row.1,
                source_crs_code,
                target_crs_code,
                source_datum_code: row.4,
                target_datum_code: row.5,
                accuracy: row.6,
                deprecated: row.10,
                preferred: true,
                approximate: false,
                area_codes: Vec::new(),
                payload: OperationPayload::GridShift {
                    grid_id,
                    direction: 0,
                    interpolation: GRID_INTERPOLATION_BILINEAR,
                },
            });
        }
    }

    {
        let mut stmt = conn
            .prepare(
                "SELECT CAST(ht.code AS TEXT),
                        ht.name,
                        CAST(ht.source_crs_code AS TEXT),
                        CAST(ht.target_crs_code AS TEXT),
                        src.datum_code,
                        tgt.datum_code,
                        ht.accuracy,
                        ht.tx,
                        ht.ty,
                        ht.tz,
                        COALESCE(ht.rx, 0.0),
                        COALESCE(ht.ry, 0.0),
                        COALESCE(ht.rz, 0.0),
                        ht.rotation_uom_code,
                        COALESCE(ht.scale_difference, 0.0),
                        ht.scale_difference_uom_code,
                        ht.deprecated
                 FROM helmert_transformation_table ht
                 JOIN geodetic_crs src
                   ON src.auth_name = ht.source_crs_auth_name
                  AND src.code = ht.source_crs_code
                  AND src.type = 'geographic 2D'
                 JOIN geodetic_crs tgt
                   ON tgt.auth_name = ht.target_crs_auth_name
                  AND tgt.code = ht.target_crs_code
                  AND tgt.type = 'geographic 2D'
                 WHERE ht.auth_name='EPSG'
                   AND ht.source_crs_auth_name='EPSG'
                   AND ht.target_crs_auth_name='EPSG'
                 ORDER BY CAST(ht.code AS INTEGER)",
            )
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, f64>(7)?,
                    row.get::<_, f64>(8)?,
                    row.get::<_, f64>(9)?,
                    row.get::<_, f64>(10)?,
                    row.get::<_, f64>(11)?,
                    row.get::<_, f64>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, f64>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, bool>(16)?,
                ))
            })
            .unwrap()
            .flatten()
        {
            let Some(code) = parse_u32_code(&row.0) else {
                continue;
            };
            let Some(source_crs_code) = parse_u32_code(&row.2) else {
                continue;
            };
            let Some(target_crs_code) = parse_u32_code(&row.3) else {
                continue;
            };
            if !geo_codes.contains(&source_crs_code) || !geo_codes.contains(&target_crs_code) {
                continue;
            }

            let rotation_factor = row.13.and_then(|uom| angle_uoms.get(&uom).copied()).unwrap_or(0.0);
            let scale_factor = row.15.and_then(|uom| scale_uoms.get(&uom).copied()).unwrap_or(0.0);
            let params = [
                row.7,
                row.8,
                row.9,
                if row.10 == 0.0 { 0.0 } else { row.10 * rotation_factor * 180.0 / PI * 3600.0 },
                if row.11 == 0.0 { 0.0 } else { row.11 * rotation_factor * 180.0 / PI * 3600.0 },
                if row.12 == 0.0 { 0.0 } else { row.12 * rotation_factor * 180.0 / PI * 3600.0 },
                if row.14 == 0.0 { 0.0 } else { row.14 * scale_factor * 1_000_000.0 },
            ];

            operations.push(OperationRecord {
                table_name: "helmert_transformation",
                code,
                name: row.1,
                source_crs_code,
                target_crs_code,
                source_datum_code: row.4,
                target_datum_code: row.5,
                accuracy: row.6,
                deprecated: row.16,
                preferred: true,
                approximate: false,
                area_codes: Vec::new(),
                payload: OperationPayload::Helmert(params),
            });
        }
    }

    {
        let mut stmt = conn
            .prepare(
                "SELECT CAST(co.code AS TEXT),
                        co.name,
                        CAST(co.source_crs_code AS TEXT),
                        CAST(co.target_crs_code AS TEXT),
                        src.datum_code,
                        tgt.datum_code,
                        co.accuracy,
                        co.deprecated
                 FROM concatenated_operation co
                 JOIN geodetic_crs src
                   ON src.auth_name = co.source_crs_auth_name
                  AND src.code = co.source_crs_code
                  AND src.type = 'geographic 2D'
                 JOIN geodetic_crs tgt
                   ON tgt.auth_name = co.target_crs_auth_name
                  AND tgt.code = co.target_crs_code
                  AND tgt.type = 'geographic 2D'
                 WHERE co.auth_name='EPSG'
                   AND co.source_crs_auth_name='EPSG'
                   AND co.target_crs_auth_name='EPSG'
                 ORDER BY CAST(co.code AS INTEGER)",
            )
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            })
            .unwrap()
            .flatten()
        {
            let Some(code) = parse_u32_code(&row.0) else {
                continue;
            };
            let Some(source_crs_code) = parse_u32_code(&row.2) else {
                continue;
            };
            let Some(target_crs_code) = parse_u32_code(&row.3) else {
                continue;
            };
            if !geo_codes.contains(&source_crs_code) || !geo_codes.contains(&target_crs_code) {
                continue;
            }

            let mut steps = Vec::new();
            let mut step_stmt = conn
                .prepare(
                    "SELECT CAST(step_code AS TEXT), COALESCE(step_direction, 'forward')
                     FROM concatenated_operation_step
                     WHERE operation_auth_name='EPSG' AND operation_code=?1
                     ORDER BY step_number",
                )
                .unwrap();
            let mut valid = true;
            for step in step_stmt
                .query_map([code], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .unwrap()
                .flatten()
            {
                let Some(step_id) = parse_u32_code(&step.0) else {
                    valid = false;
                    break;
                };
                let direction = match step.1.as_str() {
                    "forward" => 0,
                    "reverse" => 1,
                    _ => {
                        valid = false;
                        break;
                    }
                };
                steps.push((step_id, direction));
            }
            if !valid || steps.is_empty() {
                continue;
            }

            operations.push(OperationRecord {
                table_name: "concatenated_operation",
                code,
                name: row.1,
                source_crs_code,
                target_crs_code,
                source_datum_code: row.4,
                target_datum_code: row.5,
                accuracy: row.6,
                deprecated: row.7,
                preferred: true,
                approximate: false,
                area_codes: Vec::new(),
                payload: OperationPayload::Concatenated { steps },
            });
        }
    }

    let mut extent_records: BTreeMap<u32, ExtentRecord> = BTreeMap::new();
    let operation_lookup: BTreeMap<(&'static str, u32), usize> = operations
        .iter()
        .enumerate()
        .map(|(index, operation)| ((operation.table_name, operation.code), index))
        .collect();
    {
        let mut stmt = conn
            .prepare(
                "SELECT object_table_name,
                        CAST(object_code AS TEXT),
                        CAST(extent.code AS TEXT),
                        extent.name,
                        extent.west_lon,
                        extent.south_lat,
                        extent.east_lon,
                        extent.north_lat
                 FROM usage
                 JOIN extent
                   ON extent.auth_name = usage.extent_auth_name
                  AND extent.code = usage.extent_code
                 WHERE object_auth_name='EPSG'
                   AND object_table_name IN ('grid_transformation','helmert_transformation','concatenated_operation')",
            )
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, f64>(7)?,
                ))
            })
            .unwrap()
            .flatten()
        {
            let Some(operation_code) = parse_u32_code(&row.1) else {
                continue;
            };
            let Some(extent_code) = parse_u32_code(&row.2) else {
                continue;
            };
            let table_name = row.0.as_str();
            let Some(&index) = operation_lookup.get(&(table_name, operation_code)) else {
                continue;
            };
            operations[index].area_codes.push(extent_code);
            extent_records.entry(extent_code).or_insert(ExtentRecord {
                code: extent_code,
                name: row.3,
                west: row.4,
                south: row.5,
                east: row.6,
                north: row.7,
            });
        }
    }

    let mut grid_area_by_id: BTreeMap<u32, u32> = BTreeMap::new();
    for operation in &operations {
        if let OperationPayload::GridShift { grid_id, .. } = operation.payload {
            if let Some(area_code) = operation.area_codes.first().copied() {
                grid_area_by_id.entry(grid_id).or_insert(area_code);
            }
        }
    }
    for grid in &mut grid_resources {
        grid.area_code = grid_area_by_id.get(&grid.id).copied().unwrap_or(0);
    }

    let extent_list: Vec<ExtentRecord> = extent_records.into_values().collect();

    eprintln!("Ellipsoids: {}", used_ellipsoids.len());
    eprintln!("Datums: {}", used_datums.len());
    eprintln!("Geographic CRS: {}", geo_crs.len());
    eprintln!("Projected CRS: {}", proj_crs.len());
    eprintln!("Extents: {}", extent_list.len());
    eprintln!("Grid resources: {}", grid_resources.len());
    eprintln!("Operations: {}", operations.len());

    let out_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../proj-core/data/epsg.bin");
    let mut buf = Vec::<u8>::new();
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&(used_ellipsoids.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(used_datums.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(geo_crs.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(proj_crs.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(extent_list.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(grid_resources.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(operations.len() as u32).to_le_bytes());

    for (code, a, inv_f) in &used_ellipsoids {
        let mut rec = [0u8; ELLIPSOID_RECORD_SIZE];
        rec[0..4].copy_from_slice(&code.to_le_bytes());
        rec[4..12].copy_from_slice(&a.to_le_bytes());
        rec[12..20].copy_from_slice(&inv_f.to_le_bytes());
        buf.extend_from_slice(&rec);
    }

    for (code, datum) in &used_datums {
        let mut rec = [0u8; DATUM_RECORD_SIZE];
        rec[0..4].copy_from_slice(&code.to_le_bytes());
        rec[4..8].copy_from_slice(&datum.ellipsoid_code.to_le_bytes());
        rec[8] = match datum.shift_kind {
            DatumShiftKind::Unknown => DATUM_SHIFT_UNKNOWN,
            DatumShiftKind::Identity => DATUM_SHIFT_IDENTITY,
            DatumShiftKind::Helmert => DATUM_SHIFT_HELMERT,
        };
        for (index, value) in datum.helmert.iter().enumerate() {
            let offset = 16 + index * 8;
            rec[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        buf.extend_from_slice(&rec);
    }

    for crs in &geo_crs {
        let mut rec = [0u8; GEO_CRS_RECORD_BASE_SIZE];
        rec[0..4].copy_from_slice(&crs.code.to_le_bytes());
        rec[4..8].copy_from_slice(&crs.datum_code.to_le_bytes());
        buf.extend_from_slice(&rec);
        write_string_u16(&mut buf, &crs.name);
    }

    for crs in &proj_crs {
        let mut rec = [0u8; PROJ_CRS_RECORD_BASE_SIZE];
        rec[0..4].copy_from_slice(&crs.code.to_le_bytes());
        rec[4..8].copy_from_slice(&crs.base_geographic_crs_code.to_le_bytes());
        rec[8..12].copy_from_slice(&crs.datum_code.to_le_bytes());
        rec[12] = crs.method_id;
        rec[16..24].copy_from_slice(&crs.linear_unit_to_meter.to_le_bytes());
        for (index, value) in crs.params.iter().enumerate() {
            let offset = 24 + index * 8;
            rec[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        buf.extend_from_slice(&rec);
        write_string_u16(&mut buf, &crs.name);
    }

    for extent in &extent_list {
        buf.extend_from_slice(&extent.code.to_le_bytes());
        buf.extend_from_slice(&extent.west.to_le_bytes());
        buf.extend_from_slice(&extent.south.to_le_bytes());
        buf.extend_from_slice(&extent.east.to_le_bytes());
        buf.extend_from_slice(&extent.north.to_le_bytes());
        write_string_u16(&mut buf, &extent.name);
    }

    for grid in &grid_resources {
        buf.extend_from_slice(&grid.id.to_le_bytes());
        buf.push(grid.format);
        buf.push(grid.interpolation);
        buf.extend_from_slice(&(grid.resource_names.len() as u16).to_le_bytes());
        buf.extend_from_slice(&grid.area_code.to_le_bytes());
        write_string_u16(&mut buf, &grid.name);
        for name in &grid.resource_names {
            write_string_u16(&mut buf, name);
        }
    }

    for operation in &operations {
        buf.extend_from_slice(&operation.code.to_le_bytes());
        buf.push(match operation.payload {
            OperationPayload::Helmert(_) => OP_HELMERT,
            OperationPayload::GridShift { .. } => OP_GRID_SHIFT,
            OperationPayload::Concatenated { .. } => OP_CONCATENATED,
        });
        let mut flags = 0u8;
        if operation.deprecated {
            flags |= FLAG_DEPRECATED;
        }
        if operation.preferred {
            flags |= FLAG_PREFERRED;
        }
        if operation.approximate {
            flags |= FLAG_APPROXIMATE;
        }
        buf.push(flags);
        buf.extend_from_slice(&(operation.area_codes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&operation.source_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.target_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.source_datum_code.to_le_bytes());
        buf.extend_from_slice(&operation.target_datum_code.to_le_bytes());
        buf.extend_from_slice(&operation.accuracy.unwrap_or(f64::NAN).to_le_bytes());
        write_string_u16(&mut buf, &operation.name);
        for area_code in &operation.area_codes {
            buf.extend_from_slice(&area_code.to_le_bytes());
        }
        match &operation.payload {
            OperationPayload::Helmert(params) => {
                for value in params {
                    buf.extend_from_slice(&value.to_le_bytes());
                }
            }
            OperationPayload::GridShift {
                grid_id,
                direction,
                interpolation,
            } => {
                buf.extend_from_slice(&grid_id.to_le_bytes());
                buf.push(*direction);
                buf.push(*interpolation);
                buf.extend_from_slice(&0u16.to_le_bytes());
            }
            OperationPayload::Concatenated { steps } => {
                buf.extend_from_slice(&(steps.len() as u16).to_le_bytes());
                for (step_id, direction) in steps {
                    buf.extend_from_slice(&step_id.to_le_bytes());
                    buf.push(*direction);
                    buf.extend_from_slice(&[0u8; 3]);
                }
            }
        }
    }

    std::fs::write(&out_path, &buf).unwrap();
    eprintln!(
        "Wrote {} bytes ({:.1} KB) to {}",
        buf.len(),
        buf.len() as f64 / 1024.0,
        out_path.display()
    );
}

fn write_string_u16(buf: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    let len = u16::try_from(bytes.len()).expect("string too long for embedded registry");
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(bytes);
}

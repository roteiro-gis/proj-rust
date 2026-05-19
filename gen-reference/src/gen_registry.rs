//! Generate compact binary EPSG registry from proj.db.
//!
//! Run: cargo run --manifest-path gen-reference/Cargo.toml --bin gen-registry
//! Check: cargo run --manifest-path gen-reference/Cargo.toml --bin gen-registry -- --check
//!
//! Outputs: proj-core/data/epsg.bin and proj-core/data/epsg.provenance.json

use rusqlite::{params, types::ValueRef, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};

type ProjectedCrsRow = (u32, u32, u32, String, String, i64, Option<i64>, Option<f64>);

const EPSG_BIN_FILE: &str = "epsg.bin";
const PROVENANCE_FILE: &str = "epsg.provenance.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistryMode {
    Write,
    Check,
}

#[derive(Debug)]
struct RegistryArgs {
    mode: RegistryMode,
    proj_db: Option<PathBuf>,
    out_dir: PathBuf,
}

impl RegistryArgs {
    fn parse() -> Self {
        Self::parse_from(env::args().skip(1)).unwrap_or_else(|message| {
            eprintln!("{message}");
            std::process::exit(2);
        })
    }

    fn parse_from<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let mut mode = RegistryMode::Write;
        let mut proj_db = None;
        let mut out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../proj-core/data");

        let mut iter = args.into_iter().map(Into::into);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--check" => mode = RegistryMode::Check,
                "--write" => mode = RegistryMode::Write,
                "--proj-db" => {
                    let path = iter
                        .next()
                        .ok_or_else(|| "--proj-db requires a path".to_string())?;
                    proj_db = Some(PathBuf::from(path));
                }
                "--out-dir" => {
                    let path = iter
                        .next()
                        .ok_or_else(|| "--out-dir requires a path".to_string())?;
                    out_dir = PathBuf::from(path);
                }
                "--help" | "-h" => {
                    return Err(format!(
                        "usage: gen-registry [--write|--check] [--proj-db PATH] [--out-dir DIR]\n\
                         default: write {EPSG_BIN_FILE} and {PROVENANCE_FILE}"
                    ));
                }
                _ => return Err(format!("unknown argument: {arg}")),
            }
        }

        Ok(Self {
            mode,
            proj_db,
            out_dir,
        })
    }
}

fn find_proj_db() -> Result<PathBuf, String> {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let mut candidates: Vec<PathBuf> = walkdir(&target_dir, "proj.db")
        .into_iter()
        .filter(|entry| !entry.to_string_lossy().contains("for_tests"))
        .collect();
    candidates.sort();
    if candidates.is_empty() {
        return Err(format!(
            "proj.db not found below {}. Run `cargo build --manifest-path gen-reference/Cargo.toml --bin gen-registry` first.",
            target_dir.display()
        ));
    }

    let mut checksums = BTreeMap::<String, PathBuf>::new();
    for candidate in &candidates {
        let digest = normalized_proj_db_sha256_for_path(candidate)?;
        checksums.entry(digest).or_insert_with(|| candidate.clone());
    }
    if checksums.len() > 1 {
        let entries = checksums
            .into_iter()
            .map(|(checksum, path)| format!("{checksum} {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "multiple distinct proj.db files were found; pass --proj-db explicitly:\n{entries}"
        ));
    }

    Ok(candidates[0].clone())
}

fn walkdir(dir: &Path, name: &str) -> Vec<PathBuf> {
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
const VERSION: u16 = 7;
const PROVENANCE_SCHEMA_VERSION: u16 = 3;
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const CANONICAL_FLOAT_DECIMAL_PLACES: usize = 13;

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
const METHOD_LAEA: u8 = 8;
const METHOD_OBLIQUE_STEREO: u8 = 9;
const METHOD_HOTINE_OBLIQUE_MERCATOR_A: u8 = 10;
const METHOD_HOTINE_OBLIQUE_MERCATOR_B: u8 = 11;
const METHOD_CASSINI_SOLDNER: u8 = 12;
const METHOD_LAEA_SPHERICAL: u8 = 13;

const DATUM_SHIFT_UNKNOWN: u8 = 0;
const DATUM_SHIFT_IDENTITY: u8 = 1;
const DATUM_SHIFT_HELMERT: u8 = 2;

const OP_HELMERT: u8 = 1;
const OP_GRID_SHIFT: u8 = 2;
const OP_CONCATENATED: u8 = 3;

const VERTICAL_OFFSET_GEOID_HEIGHT_METERS: u8 = 1;

const FLAG_DEPRECATED: u8 = 1 << 0;
const FLAG_PREFERRED: u8 = 1 << 1;
const FLAG_APPROXIMATE: u8 = 1 << 2;

const GRID_FORMAT_NTV2: u8 = 1;
const GRID_FORMAT_GTX: u8 = 2;
const GRID_FORMAT_UNSUPPORTED: u8 = 255;

const GRID_INTERPOLATION_BILINEAR: u8 = 1;

// EPSG:32662 is deprecated upstream but remains part of this crate's documented
// public support set.
const EXPLICITLY_SUPPORTED_DEPRECATED_PROJECTED_CRS: &[u32] = &[32662];

#[derive(Serialize)]
struct RegistryProvenance {
    schema_version: u16,
    generator: &'static str,
    registry_format: RegistryFormatProvenance,
    source_database: SourceDatabaseProvenance,
    output: RegistryOutputProvenance,
    counts: RegistryCounts,
    supported_projection_methods: BTreeMap<String, u8>,
    supported_grid_formats: BTreeMap<String, u8>,
    supported_operation_payloads: BTreeMap<String, u8>,
}

#[derive(Serialize)]
struct RegistryFormatProvenance {
    magic: String,
    version: u16,
}

#[derive(Serialize)]
struct SourceDatabaseProvenance {
    kind: &'static str,
    file_name: &'static str,
    normalized_content_sha256: String,
    metadata: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct RegistryOutputProvenance {
    file_name: &'static str,
    byte_len: usize,
    sha256: String,
}

#[derive(Clone, Copy, Serialize)]
struct RegistryCounts {
    ellipsoids: usize,
    datums: usize,
    geographic_crs: usize,
    projected_crs: usize,
    extents: usize,
    grid_resources: usize,
    operations: usize,
    vertical_operations: usize,
}

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
const LAT_PROJECTION_CENTRE: i64 = 8811;
const LON_PROJECTION_CENTRE: i64 = 8812;
const AZIMUTH_INITIAL_LINE: i64 = 8813;
const RECTIFIED_GRID_ANGLE: i64 = 8814;
const SCALE_FACTOR_PROJECTION_CENTRE: i64 = 8815;
const EASTING_PROJECTION_CENTRE: i64 = 8816;
const NORTHING_PROJECTION_CENTRE: i64 = 8817;

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
    GridShift {
        grid_id: u32,
        direction: u8,
        interpolation: u8,
    },
    Concatenated {
        steps: Vec<(u32, u8)>,
    },
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

#[derive(Clone)]
struct VerticalOperationRecord {
    table_name: &'static str,
    code: u32,
    name: String,
    source_horizontal_crs_code: u32,
    target_horizontal_crs_code: u32,
    grid_horizontal_crs_code: u32,
    source_vertical_crs_code: u32,
    target_vertical_crs_code: u32,
    source_vertical_datum_code: u32,
    target_vertical_datum_code: u32,
    grid_id: u32,
    accuracy: Option<f64>,
    deprecated: bool,
    area_codes: Vec<u32>,
}

fn method_code_to_id(code: i64) -> Option<u8> {
    match code {
        9807 => Some(METHOD_TRANSVERSE_MERCATOR),
        9804 | 9805 => Some(METHOD_MERCATOR),
        9801 | 9802 => Some(METHOD_LCC),
        9822 => Some(METHOD_ALBERS),
        9810 | 9829 => Some(METHOD_POLAR_STEREO),
        1028 | 1029 | 9823 | 9842 => Some(METHOD_EQUIDISTANT_CYL),
        9820 => Some(METHOD_LAEA),
        1027 => Some(METHOD_LAEA_SPHERICAL),
        9809 => Some(METHOD_OBLIQUE_STEREO),
        9812 => Some(METHOD_HOTINE_OBLIQUE_MERCATOR_A),
        9815 => Some(METHOD_HOTINE_OBLIQUE_MERCATOR_B),
        9806 => Some(METHOD_CASSINI_SOLDNER),
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
            get_degrees(cp, &[LON_ORIGIN, LON_FALSE_ORIGIN, LON_OF_ORIGIN]),
            get_degrees(cp, &[LAT_1ST_PARALLEL, LAT_STD_PARALLEL, LAT_ORIGIN]),
            0.0,
            get_meters(cp, &[FALSE_EASTING, EASTING_FALSE_ORIGIN], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING, NORTHING_FALSE_ORIGIN], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_LAEA => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_ORIGIN]),
            0.0,
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_LAEA_SPHERICAL => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_ORIGIN]),
            0.0,
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_OBLIQUE_STEREO => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_ORIGIN]),
            get_scale(cp, &[SCALE_FACTOR]),
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
            0.0,
            0.0,
        ],
        METHOD_HOTINE_OBLIQUE_MERCATOR_A => [
            get_degrees(cp, &[LAT_PROJECTION_CENTRE]),
            get_degrees(cp, &[LON_PROJECTION_CENTRE]),
            get_degrees(cp, &[AZIMUTH_INITIAL_LINE]),
            get_degrees(cp, &[RECTIFIED_GRID_ANGLE]),
            get_scale(cp, &[SCALE_FACTOR_PROJECTION_CENTRE]),
            get_meters(cp, &[FALSE_EASTING], linear_uoms),
            get_meters(cp, &[FALSE_NORTHING], linear_uoms),
        ],
        METHOD_HOTINE_OBLIQUE_MERCATOR_B => [
            get_degrees(cp, &[LAT_PROJECTION_CENTRE]),
            get_degrees(cp, &[LON_PROJECTION_CENTRE]),
            get_degrees(cp, &[AZIMUTH_INITIAL_LINE]),
            get_degrees(cp, &[RECTIFIED_GRID_ANGLE]),
            get_scale(cp, &[SCALE_FACTOR_PROJECTION_CENTRE]),
            get_meters(cp, &[EASTING_PROJECTION_CENTRE], linear_uoms),
            get_meters(cp, &[NORTHING_PROJECTION_CENTRE], linear_uoms),
        ],
        METHOD_CASSINI_SOLDNER => [
            get_degrees(cp, &[LON_ORIGIN]),
            get_degrees(cp, &[LAT_ORIGIN]),
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
    } else if method_name.to_ascii_lowercase().contains("(gtx)") {
        GRID_FORMAT_GTX
    } else {
        GRID_FORMAT_UNSUPPORTED
    }
}

fn read_proj_db_metadata(conn: &Connection) -> BTreeMap<String, String> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM metadata ORDER BY key")
        .unwrap_or_else(|err| fatal(format!("failed to read proj.db metadata schema: {err}")));
    stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
    .unwrap_or_else(|err| fatal(format!("failed to query proj.db metadata: {err}")))
    .filter_map(|row| row.ok())
    .collect()
}

fn normalized_proj_db_sha256_for_path(path: &Path) -> Result<String, String> {
    let conn = Connection::open(path)
        .map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    normalized_proj_db_sha256(&conn)
}

fn normalized_proj_db_sha256(conn: &Connection) -> Result<String, String> {
    let mut payload = Vec::new();
    let tables = query_strings(
        conn,
        "SELECT name
         FROM sqlite_schema
         WHERE type='table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;

    for table in tables {
        append_bytes(&mut payload, b't', table.as_bytes());
        let columns = table_columns(conn, &table)?;
        for column in &columns {
            append_bytes(&mut payload, b'c', column.as_bytes());
        }

        let quoted_columns = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {quoted_columns} FROM {} ORDER BY {quoted_columns}",
            quote_identifier(&table)
        );
        let mut stmt = conn.prepare(&sql).map_err(|err| {
            format!("failed to prepare normalized digest query for {table}: {err}")
        })?;
        let mut rows = stmt
            .query([])
            .map_err(|err| format!("failed to query {table} for normalized digest: {err}"))?;
        while let Some(row) = rows
            .next()
            .map_err(|err| format!("failed to read {table} row for normalized digest: {err}"))?
        {
            payload.push(b'r');
            for (index, column) in columns.iter().enumerate() {
                match row
                    .get_ref(index)
                    .map_err(|err| format!("failed to read {table}.{column}: {err}"))?
                {
                    ValueRef::Null => payload.push(b'n'),
                    ValueRef::Integer(value) => {
                        payload.push(b'i');
                        payload.extend_from_slice(&value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        payload.push(b'f');
                        payload.extend_from_slice(&canonical_f64(value).to_le_bytes());
                    }
                    ValueRef::Text(value) => append_bytes(&mut payload, b's', value),
                    ValueRef::Blob(value) => append_bytes(&mut payload, b'b', value),
                }
            }
        }
    }

    Ok(sha256_hex(&payload))
}

fn query_strings(conn: &Connection, sql: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|err| format!("failed to prepare query `{sql}`: {err}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| format!("failed to run query `{sql}`: {err}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read query `{sql}`: {err}"))?;
    Ok(rows)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|err| format!("failed to prepare column query for {table}: {err}"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| format!("failed to query columns for {table}: {err}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read columns for {table}: {err}"))?;
    Ok(columns)
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn append_bytes(payload: &mut Vec<u8>, marker: u8, bytes: &[u8]) {
    payload.push(marker);
    payload.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    payload.extend_from_slice(bytes);
}

fn named_codes(items: &[(&str, u8)]) -> BTreeMap<String, u8> {
    items
        .iter()
        .map(|(name, code)| ((*name).to_string(), *code))
        .collect()
}

fn supported_projection_methods() -> BTreeMap<String, u8> {
    named_codes(&[
        ("Albers Equal Area", METHOD_ALBERS),
        ("Cassini-Soldner", METHOD_CASSINI_SOLDNER),
        ("Equidistant Cylindrical", METHOD_EQUIDISTANT_CYL),
        (
            "Hotine Oblique Mercator A",
            METHOD_HOTINE_OBLIQUE_MERCATOR_A,
        ),
        (
            "Hotine Oblique Mercator B",
            METHOD_HOTINE_OBLIQUE_MERCATOR_B,
        ),
        ("Lambert Azimuthal Equal Area", METHOD_LAEA),
        (
            "Lambert Azimuthal Equal Area (Spherical)",
            METHOD_LAEA_SPHERICAL,
        ),
        ("Lambert Conformal Conic", METHOD_LCC),
        ("Mercator", METHOD_MERCATOR),
        ("Oblique Stereographic", METHOD_OBLIQUE_STEREO),
        ("Polar Stereographic", METHOD_POLAR_STEREO),
        ("Transverse Mercator", METHOD_TRANSVERSE_MERCATOR),
        ("Web Mercator", METHOD_WEB_MERCATOR),
    ])
}

fn supported_grid_formats() -> BTreeMap<String, u8> {
    named_codes(&[
        ("GTX", GRID_FORMAT_GTX),
        ("NTv2", GRID_FORMAT_NTV2),
        ("Unsupported", GRID_FORMAT_UNSUPPORTED),
    ])
}

fn supported_operation_payloads() -> BTreeMap<String, u8> {
    named_codes(&[
        ("Concatenated", OP_CONCATENATED),
        ("GridShift", OP_GRID_SHIFT),
        ("Helmert", OP_HELMERT),
    ])
}

fn explicitly_supported_deprecated_projected_crs_sql_list() -> String {
    EXPLICITLY_SUPPORTED_DEPRECATED_PROJECTED_CRS
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn main() {
    let args = RegistryArgs::parse();
    let db_path = args
        .proj_db
        .clone()
        .unwrap_or_else(|| find_proj_db().unwrap_or_else(|message| fatal(message)));
    eprintln!("Using proj.db: {}", db_path.display());
    let conn = Connection::open(&db_path)
        .unwrap_or_else(|err| fatal(format!("failed to open {}: {err}", db_path.display())));
    let db_sha256 = normalized_proj_db_sha256(&conn)
        .unwrap_or_else(|err| fatal(format!("failed to digest proj.db content: {err}")));
    let proj_db_metadata = read_proj_db_metadata(&conn);

    let mut ellipsoids: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT e.code,
                        e.semi_major_axis,
                        e.inv_flattening,
                        e.semi_minor_axis,
                        u.conv_factor
                 FROM ellipsoid e
                 JOIN unit_of_measure u
                   ON u.auth_name = e.uom_auth_name
                  AND u.code = e.uom_code
                 WHERE e.auth_name='EPSG'
                 ORDER BY CAST(e.code AS INTEGER)",
            )
            .unwrap();
        for row in stmt
            .query_map([], |row| {
                let code: u32 = row.get(0)?;
                let a: f64 = row.get(1)?;
                let inv_f: Option<f64> = row.get(2)?;
                let b: Option<f64> = row.get(3)?;
                let unit_to_meter: f64 = row.get(4)?;
                let a_m = a * unit_to_meter;
                let b_m = b.map(|value| value * unit_to_meter);
                let rf = match inv_f {
                    Some(value) if value != 0.0 => value,
                    _ => match b_m {
                        Some(semi_minor) if (a_m - semi_minor).abs() > 0.001 => {
                            a_m / (a_m - semi_minor)
                        }
                        _ => 0.0,
                    },
                };
                Ok((code, a_m, rf))
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
            .prepare(
                "SELECT code, ellipsoid_code
                 FROM geodetic_datum
                 WHERE auth_name='EPSG'
                 ORDER BY CAST(code AS INTEGER)",
            )
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
                 WHERE auth_name='EPSG' AND type='length'
                 ORDER BY CAST(code AS INTEGER)",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?))
        })
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
                 WHERE auth_name='EPSG' AND type='angle'
                 ORDER BY CAST(code AS INTEGER)",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?))
        })
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
                 WHERE auth_name='EPSG' AND type='scale'
                 ORDER BY CAST(code AS INTEGER)",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<f64>>(1)?))
        })
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
                          COALESCE(accuracy, 999.0),
                          CAST(code AS INTEGER)",
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
                    let prev_has_rotation = datum.helmert[3] != 0.0
                        || datum.helmert[4] != 0.0
                        || datum.helmert[5] != 0.0;
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
    let mut geo_2d_by_datum: BTreeMap<u32, u32> = BTreeMap::new();
    for crs in &geo_crs {
        geo_2d_by_datum.entry(crs.datum_code).or_insert(crs.code);
    }

    let mut proj_crs: Vec<ProjCrs> = Vec::new();
    {
        let sql = format!(
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
                 WHERE pc.auth_name='EPSG'
                   AND (pc.deprecated=0 OR pc.code IN ({}))
                 ORDER BY CAST(pc.code AS INTEGER)",
            explicitly_supported_deprecated_projected_crs_sql_list()
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows: Vec<ProjectedCrsRow> = stmt
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
            .map(|row| row.unwrap())
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
                (None, _) => {
                    panic!("projected CRS EPSG:{code} is missing axis linear unit metadata")
                }
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
                        if let (Some(code), Some(value), Some(uom)) =
                            (param_code, param_value, param_uom)
                        {
                            params.insert(code, (value, uom));
                        }
                    }
                    Ok(ConvParams {
                        method_code,
                        params,
                    })
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
    let mut vertical_operations: Vec<VerticalOperationRecord> = Vec::new();

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
                "SELECT CAST(gt.code AS TEXT),
                        gt.name,
                        CAST(gt.source_crs_code AS TEXT),
                        src.type,
                        src.datum_code,
                        COALESCE(CAST(tgt_v.code AS TEXT), CAST(comp_v.code AS TEXT), ''),
                        COALESCE(tgt_v.datum_code, comp_v.datum_code),
                        COALESCE(CAST(cc.horiz_crs_code AS TEXT), ''),
                        gt.accuracy,
                        gt.method_name,
                        gt.grid_name,
                        COALESCE(gt.grid2_name, ''),
                        COALESCE(CAST(gt.interpolation_crs_code AS TEXT), ''),
                        gt.deprecated
                 FROM grid_transformation gt
                 JOIN geodetic_crs src
                   ON src.auth_name = gt.source_crs_auth_name
                  AND src.code = gt.source_crs_code
                  AND src.type IN ('geographic 2D', 'geographic 3D')
                 LEFT JOIN vertical_crs tgt_v
                   ON tgt_v.auth_name = gt.target_crs_auth_name
                  AND tgt_v.code = gt.target_crs_code
                 LEFT JOIN compound_crs cc
                   ON cc.auth_name = gt.target_crs_auth_name
                  AND cc.code = gt.target_crs_code
                 LEFT JOIN vertical_crs comp_v
                   ON comp_v.auth_name = cc.vertical_crs_auth_name
                  AND comp_v.code = cc.vertical_crs_code
                 WHERE gt.auth_name='EPSG'
                   AND gt.source_crs_auth_name='EPSG'
                   AND gt.target_crs_auth_name='EPSG'
                   AND gt.method_name LIKE '%GravityRelatedHeight%'
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
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<u32>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<f64>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, bool>(13)?,
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
            let Some(target_vertical_crs_code) = parse_u32_code(&row.5) else {
                continue;
            };
            let Some(target_vertical_datum_code) = row.6 else {
                continue;
            };
            let format = grid_format_from_method(&row.9);
            if format == GRID_FORMAT_UNSUPPORTED {
                continue;
            }

            let source_horizontal_crs_code = if row.3 == "geographic 2D" {
                source_crs_code
            } else {
                match geo_2d_by_datum.get(&row.4).copied() {
                    Some(code) => code,
                    None => continue,
                }
            };
            let target_horizontal_crs_code = parse_u32_code(&row.7).unwrap_or(0);
            let default_grid_horizontal_crs_code = if target_horizontal_crs_code != 0 {
                target_horizontal_crs_code
            } else {
                source_horizontal_crs_code
            };
            let grid_horizontal_crs_code =
                parse_u32_code(&row.12).unwrap_or(default_grid_horizontal_crs_code);

            let grid_key = (row.9.clone(), row.10.clone(), row.11.clone());
            let grid_id = if let Some(id) = grid_resource_ids.get(&grid_key).copied() {
                id
            } else {
                let id = grid_resources.len() as u32 + 1;
                let mut resource_names = vec![row.10.clone()];
                if !row.11.is_empty() {
                    resource_names.push(row.11.clone());
                }
                grid_resources.push(GridRecord {
                    id,
                    name: row.10.clone(),
                    format,
                    interpolation: GRID_INTERPOLATION_BILINEAR,
                    area_code: 0,
                    resource_names,
                });
                grid_resource_ids.insert(grid_key, id);
                id
            };

            vertical_operations.push(VerticalOperationRecord {
                table_name: "grid_transformation",
                code,
                name: row.1,
                source_horizontal_crs_code,
                target_horizontal_crs_code,
                grid_horizontal_crs_code,
                source_vertical_crs_code: 0,
                target_vertical_crs_code,
                source_vertical_datum_code: 0,
                target_vertical_datum_code,
                grid_id,
                accuracy: row.8,
                deprecated: row.13,
                area_codes: Vec::new(),
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

            let rotation_factor = row
                .13
                .and_then(|uom| angle_uoms.get(&uom).copied())
                .unwrap_or(0.0);
            let scale_factor = row
                .15
                .and_then(|uom| scale_uoms.get(&uom).copied())
                .unwrap_or(0.0);
            let params = [
                row.7,
                row.8,
                row.9,
                if row.10 == 0.0 {
                    0.0
                } else {
                    row.10 * rotation_factor * 180.0 / PI * 3600.0
                },
                if row.11 == 0.0 {
                    0.0
                } else {
                    row.11 * rotation_factor * 180.0 / PI * 3600.0
                },
                if row.12 == 0.0 {
                    0.0
                } else {
                    row.12 * rotation_factor * 180.0 / PI * 3600.0
                },
                if row.14 == 0.0 {
                    0.0
                } else {
                    row.14 * scale_factor * 1_000_000.0
                },
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
                .query_map([code], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
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
    let vertical_operation_lookup: BTreeMap<(&'static str, u32), usize> = vertical_operations
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
                   AND object_table_name IN ('grid_transformation','helmert_transformation','concatenated_operation')
                 ORDER BY object_table_name,
                          CAST(object_code AS INTEGER),
                          CAST(extent.code AS INTEGER)",
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
            let mut used = false;
            if let Some(&index) = operation_lookup.get(&(table_name, operation_code)) {
                operations[index].area_codes.push(extent_code);
                used = true;
            }
            if let Some(&index) = vertical_operation_lookup.get(&(table_name, operation_code)) {
                vertical_operations[index].area_codes.push(extent_code);
                used = true;
            }
            if !used {
                continue;
            }
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
    for operation in &mut operations {
        operation.area_codes.sort_unstable();
        operation.area_codes.dedup();
    }
    for operation in &mut vertical_operations {
        operation.area_codes.sort_unstable();
        operation.area_codes.dedup();
    }

    let mut grid_area_by_id: BTreeMap<u32, u32> = BTreeMap::new();
    for operation in &operations {
        if let OperationPayload::GridShift { grid_id, .. } = operation.payload {
            if let Some(area_code) = operation.area_codes.first().copied() {
                grid_area_by_id.entry(grid_id).or_insert(area_code);
            }
        }
    }
    for operation in &vertical_operations {
        if let Some(area_code) = operation.area_codes.first().copied() {
            grid_area_by_id
                .entry(operation.grid_id)
                .or_insert(area_code);
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
    eprintln!("Vertical operations: {}", vertical_operations.len());

    let counts = RegistryCounts {
        ellipsoids: used_ellipsoids.len(),
        datums: used_datums.len(),
        geographic_crs: geo_crs.len(),
        projected_crs: proj_crs.len(),
        extents: extent_list.len(),
        grid_resources: grid_resources.len(),
        operations: operations.len(),
        vertical_operations: vertical_operations.len(),
    };

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
    buf.extend_from_slice(&(vertical_operations.len() as u32).to_le_bytes());

    for (code, a, inv_f) in &used_ellipsoids {
        let mut rec = [0u8; ELLIPSOID_RECORD_SIZE];
        rec[0..4].copy_from_slice(&code.to_le_bytes());
        rec[4..12].copy_from_slice(&canonical_f64(*a).to_le_bytes());
        rec[12..20].copy_from_slice(&canonical_f64(*inv_f).to_le_bytes());
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
            rec[offset..offset + 8].copy_from_slice(&canonical_f64(*value).to_le_bytes());
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
        rec[16..24].copy_from_slice(&canonical_f64(crs.linear_unit_to_meter).to_le_bytes());
        for (index, value) in crs.params.iter().enumerate() {
            let offset = 24 + index * 8;
            rec[offset..offset + 8].copy_from_slice(&canonical_f64(*value).to_le_bytes());
        }
        buf.extend_from_slice(&rec);
        write_string_u16(&mut buf, &crs.name);
    }

    for extent in &extent_list {
        buf.extend_from_slice(&extent.code.to_le_bytes());
        write_f64(&mut buf, extent.west);
        write_f64(&mut buf, extent.south);
        write_f64(&mut buf, extent.east);
        write_f64(&mut buf, extent.north);
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
        write_optional_f64(&mut buf, operation.accuracy);
        write_string_u16(&mut buf, &operation.name);
        for area_code in &operation.area_codes {
            buf.extend_from_slice(&area_code.to_le_bytes());
        }
        match &operation.payload {
            OperationPayload::Helmert(params) => {
                for value in params {
                    write_f64(&mut buf, *value);
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

    for operation in &vertical_operations {
        buf.extend_from_slice(&operation.code.to_le_bytes());
        let mut flags = 0u8;
        if operation.deprecated {
            flags |= FLAG_DEPRECATED;
        }
        buf.push(flags);
        buf.push(VERTICAL_OFFSET_GEOID_HEIGHT_METERS);
        buf.extend_from_slice(&(operation.area_codes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&operation.source_horizontal_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.target_horizontal_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.grid_horizontal_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.source_vertical_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.target_vertical_crs_code.to_le_bytes());
        buf.extend_from_slice(&operation.source_vertical_datum_code.to_le_bytes());
        buf.extend_from_slice(&operation.target_vertical_datum_code.to_le_bytes());
        buf.extend_from_slice(&operation.grid_id.to_le_bytes());
        write_optional_f64(&mut buf, operation.accuracy);
        write_string_u16(&mut buf, &operation.name);
        for area_code in &operation.area_codes {
            buf.extend_from_slice(&area_code.to_le_bytes());
        }
    }

    let bin_sha256 = sha256_hex(&buf);
    let provenance = RegistryProvenance {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        generator: "gen-reference gen-registry",
        registry_format: RegistryFormatProvenance {
            magic: format!("0x{MAGIC:08x}"),
            version: VERSION,
        },
        source_database: SourceDatabaseProvenance {
            kind: "PROJ proj.db",
            file_name: "proj.db",
            normalized_content_sha256: db_sha256,
            metadata: proj_db_metadata,
        },
        output: RegistryOutputProvenance {
            file_name: EPSG_BIN_FILE,
            byte_len: buf.len(),
            sha256: bin_sha256,
        },
        counts,
        supported_projection_methods: supported_projection_methods(),
        supported_grid_formats: supported_grid_formats(),
        supported_operation_payloads: supported_operation_payloads(),
    };
    let mut provenance_bytes = serde_json::to_vec_pretty(&provenance)
        .unwrap_or_else(|err| fatal(format!("failed to serialize provenance: {err}")));
    provenance_bytes.push(b'\n');

    let out_path = args.out_dir.join(EPSG_BIN_FILE);
    let provenance_path = args.out_dir.join(PROVENANCE_FILE);
    match args.mode {
        RegistryMode::Write => {
            fs::create_dir_all(&args.out_dir).unwrap_or_else(|err| {
                fatal(format!(
                    "failed to create output directory {}: {err}",
                    args.out_dir.display()
                ))
            });
            fs::write(&out_path, &buf).unwrap_or_else(|err| {
                fatal(format!("failed to write {}: {err}", out_path.display()))
            });
            fs::write(&provenance_path, &provenance_bytes).unwrap_or_else(|err| {
                fatal(format!(
                    "failed to write {}: {err}",
                    provenance_path.display()
                ))
            });
            eprintln!(
                "Wrote {} bytes ({:.1} KB) to {}",
                buf.len(),
                buf.len() as f64 / 1024.0,
                out_path.display()
            );
            eprintln!("Wrote provenance to {}", provenance_path.display());
        }
        RegistryMode::Check => {
            assert_reproducible(&out_path, &buf, EPSG_BIN_FILE);
            assert_reproducible(&provenance_path, &provenance_bytes, PROVENANCE_FILE);
            eprintln!(
                "Registry artifacts are reproducible: {}, {}",
                out_path.display(),
                provenance_path.display()
            );
        }
    }
}

fn write_string_u16(buf: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    let len = u16::try_from(bytes.len()).expect("string too long for embedded registry");
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn write_f64(buf: &mut Vec<u8>, value: f64) {
    buf.extend_from_slice(&canonical_f64(value).to_le_bytes());
}

fn write_optional_f64(buf: &mut Vec<u8>, value: Option<f64>) {
    match value {
        Some(value) => write_f64(buf, value),
        None => buf.extend_from_slice(&CANONICAL_NAN_BITS.to_le_bytes()),
    }
}

fn canonical_f64(value: f64) -> f64 {
    assert!(value.is_finite(), "registry value must be finite");
    if value == 0.0 {
        return 0.0;
    }
    format!("{value:.CANONICAL_FLOAT_DECIMAL_PLACES$e}")
        .parse()
        .expect("formatted finite f64 should parse")
}

fn assert_reproducible(path: &Path, expected: &[u8], label: &str) {
    let existing = fs::read(path).unwrap_or_else(|err| {
        fatal(format!(
            "{label} is missing or unreadable at {}: {err}",
            path.display()
        ))
    });
    if existing != expected {
        fatal(format!(
            "{label} is not reproducible from the pinned proj.db\n\
             expected: {} ({} bytes)\n\
             actual:   {} ({} bytes)\n\
             regenerate with: cargo run --manifest-path gen-reference/Cargo.toml --bin gen-registry",
            sha256_hex(expected),
            expected.len(),
            sha256_hex(&existing),
            existing.len()
        ));
    }
}

fn fatal(message: impl AsRef<str>) -> ! {
    eprintln!("error: {}", message.as_ref());
    std::process::exit(1);
}

fn sha256_hex(bytes: &[u8]) -> String {
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((bytes.len() + 72).div_ceil(64) * 64);
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut h = H0;
    let mut w = [0u32; 64];
    for chunk in padded.chunks_exact(64) {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes(
                chunk[i * 4..i * 4 + 4]
                    .try_into()
                    .expect("slice length checked"),
            );
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for word in h {
        use std::fmt::Write as _;
        write!(&mut out, "{word:08x}").expect("writing to string cannot fail");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn canonical_nan_encoding_is_fixed() {
        assert_eq!(
            CANONICAL_NAN_BITS.to_le_bytes(),
            [0, 0, 0, 0, 0, 0, 248, 127]
        );
    }

    #[test]
    fn canonical_f64_rounds_to_stable_decimal_precision() {
        assert_eq!(canonical_f64(-0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(canonical_f64(6378137.0000000001), 6378137.0);
        assert_eq!(
            canonical_f64(1.2345678901234567).to_bits(),
            1.2345678901235f64.to_bits()
        );
    }

    #[test]
    fn parses_check_mode_with_explicit_paths() {
        let args = RegistryArgs::parse_from([
            "--check",
            "--proj-db",
            "/tmp/proj.db",
            "--out-dir",
            "/tmp/out",
        ])
        .unwrap();
        assert_eq!(args.mode, RegistryMode::Check);
        assert_eq!(args.proj_db.as_deref(), Some(Path::new("/tmp/proj.db")));
        assert_eq!(args.out_dir, PathBuf::from("/tmp/out"));
    }
}

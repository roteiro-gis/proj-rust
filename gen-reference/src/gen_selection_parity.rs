//! Generate the operation-selection parity corpus.
//!
//! For geographic CRS pairs that have several candidate EPSG transformations,
//! probe C PROJ at each candidate's area-of-use centroid and record which
//! operation C PROJ's late-binding selection actually used. The committed
//! output (`testdata/selection_parity.json`) lets proj-core assert that its
//! area-of-interest-driven selection picks the same operation.
//!
//! Usage: `cargo run --release --bin gen-selection-parity [--proj-db PATH]`
//! (writes JSON to stdout).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

const MAX_ENTRIES: usize = 250;

#[derive(Serialize)]
struct ParityEntry {
    source_epsg: u32,
    target_epsg: u32,
    probe_lon: f64,
    probe_lat: f64,
    /// EPSG code of the operation C PROJ used, when it exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_operation_epsg: Option<u32>,
    expected_operation_name: String,
    /// Extent whose centroid produced the probe (provenance only).
    probe_extent_name: String,
}

mod c_proj {
    use std::ffi::{CStr, CString};

    use proj_sys::{
        proj_area_create, proj_area_destroy, proj_concatoperation_get_step,
        proj_concatoperation_get_step_count, proj_context_create, proj_context_destroy,
        proj_context_errno, proj_create_crs_to_crs, proj_destroy, proj_errno_string,
        proj_get_id_auth_name, proj_get_id_code, proj_get_name, proj_get_type,
        proj_normalize_for_visualization, proj_trans, proj_trans_get_last_used_operation, PJ_COORD,
        PJ_DIRECTION_PJ_FWD, PJ_TYPE_PJ_TYPE_CONCATENATED_OPERATION,
        PJ_TYPE_PJ_TYPE_TRANSFORMATION, PJ_XYZT,
    };

    pub struct UsedOperation {
        pub auth: String,
        pub code: Option<u32>,
        pub name: String,
    }

    fn string_at(ptr: *const std::os::raw::c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// Transform the probe point and report the operation C PROJ used.
    pub fn used_operation(
        source_epsg: u32,
        target_epsg: u32,
        probe: (f64, f64),
    ) -> Result<UsedOperation, String> {
        let from = CString::new(format!("EPSG:{source_epsg}")).expect("no NUL");
        let to = CString::new(format!("EPSG:{target_epsg}")).expect("no NUL");
        unsafe {
            let ctx = proj_context_create();
            if ctx.is_null() {
                return Err("failed to create PROJ context".into());
            }
            let result = (|| {
                let area = proj_area_create();
                let raw = proj_create_crs_to_crs(ctx, from.as_ptr(), to.as_ptr(), area);
                proj_area_destroy(area);
                if raw.is_null() {
                    return Err(format!(
                        "failed to create transform EPSG:{source_epsg}->EPSG:{target_epsg}: {}",
                        string_at(proj_errno_string(proj_context_errno(ctx))).unwrap_or_default()
                    ));
                }
                let pj = proj_normalize_for_visualization(ctx, raw);
                proj_destroy(raw);
                if pj.is_null() {
                    return Err("failed to normalize transform".into());
                }

                let out = proj_trans(
                    pj,
                    PJ_DIRECTION_PJ_FWD,
                    PJ_COORD {
                        xyzt: PJ_XYZT {
                            x: probe.0,
                            y: probe.1,
                            z: 0.0,
                            t: f64::INFINITY,
                        },
                    },
                );
                if !out.xyzt.x.is_finite() || !out.xyzt.y.is_finite() {
                    proj_destroy(pj);
                    return Err("probe transform produced non-finite output".into());
                }

                let last = proj_trans_get_last_used_operation(pj);
                proj_destroy(pj);
                if last.is_null() {
                    return Err("C PROJ did not report a last-used operation".into());
                }
                let auth = string_at(proj_get_id_auth_name(last, 0)).unwrap_or_default();
                let mut code =
                    string_at(proj_get_id_code(last, 0)).and_then(|code| code.parse::<u32>().ok());
                let name = string_at(proj_get_name(last)).unwrap_or_default();

                // The used operation is usually a synthesized pipeline (axis
                // order changes around the datum shift) without its own EPSG
                // identity; recover the identity from its single EPSG
                // transformation step.
                if code.is_none() && proj_get_type(last) == PJ_TYPE_PJ_TYPE_CONCATENATED_OPERATION {
                    let mut step_codes = Vec::new();
                    let count = proj_concatoperation_get_step_count(ctx, last);
                    for index in 0..count {
                        let step = proj_concatoperation_get_step(ctx, last, index);
                        if step.is_null() {
                            continue;
                        }
                        if proj_get_type(step) == PJ_TYPE_PJ_TYPE_TRANSFORMATION {
                            if let Some(step_code) = string_at(proj_get_id_code(step, 0))
                                .and_then(|value| value.parse::<u32>().ok())
                            {
                                if string_at(proj_get_id_auth_name(step, 0)).as_deref()
                                    == Some("EPSG")
                                {
                                    step_codes.push(step_code);
                                }
                            }
                        }
                        proj_destroy(step);
                    }
                    if let [single] = step_codes.as_slice() {
                        code = Some(*single);
                    }
                }

                proj_destroy(last);
                Ok(UsedOperation { auth, code, name })
            })();
            proj_context_destroy(ctx);
            result
        }
    }
}

struct CandidateRow {
    op_code: u32,
    source_epsg: u32,
    target_epsg: u32,
    west: f64,
    south: f64,
    east: f64,
    north: f64,
    extent_name: String,
}

fn candidate_rows(conn: &rusqlite::Connection) -> Vec<CandidateRow> {
    let mut stmt = conn
        .prepare(
            "SELECT CAST(h.code AS TEXT),
                    CAST(h.source_crs_code AS TEXT),
                    CAST(h.target_crs_code AS TEXT),
                    e.west_lon, e.south_lat, e.east_lon, e.north_lat,
                    e.name
             FROM helmert_transformation h
             JOIN usage u
               ON u.object_table_name = 'helmert_transformation'
              AND u.object_auth_name = 'EPSG'
              AND u.object_code = h.code
             JOIN extent e
               ON e.auth_name = u.extent_auth_name
              AND e.code = u.extent_code
             JOIN geodetic_crs s
               ON s.auth_name = h.source_crs_auth_name
              AND s.code = h.source_crs_code
              AND s.type = 'geographic 2D'
             JOIN geodetic_crs t
               ON t.auth_name = h.target_crs_auth_name
              AND t.code = h.target_crs_code
              AND t.type = 'geographic 2D'
             WHERE h.auth_name = 'EPSG'
               AND h.deprecated = 0
             ORDER BY CAST(h.source_crs_code AS INTEGER),
                      CAST(h.target_crs_code AS INTEGER),
                      CAST(h.code AS INTEGER)",
        )
        .expect("prepare candidate query");
    stmt.query_map([], |row| {
        Ok(CandidateRow {
            op_code: row.get::<_, String>(0)?.parse::<u32>().unwrap_or(0),
            source_epsg: row.get::<_, String>(1)?.parse::<u32>().unwrap_or(0),
            target_epsg: row.get::<_, String>(2)?.parse::<u32>().unwrap_or(0),
            west: row.get(3)?,
            south: row.get(4)?,
            east: row.get(5)?,
            north: row.get(6)?,
            extent_name: row.get(7)?,
        })
    })
    .expect("query candidates")
    .flatten()
    .filter(|row| row.op_code != 0 && row.source_epsg != 0 && row.target_epsg != 0)
    .collect()
}

fn extent_centroid(row: &CandidateRow) -> (f64, f64) {
    let lat = (row.south + row.north) / 2.0;
    let lon = if row.east >= row.west {
        (row.west + row.east) / 2.0
    } else {
        // Antimeridian-crossing extent.
        let mid = (row.west + row.east + 360.0) / 2.0;
        if mid > 180.0 {
            mid - 360.0
        } else {
            mid
        }
    };
    (lon, lat)
}

fn find_proj_db() -> Result<PathBuf, String> {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let mut candidates: Vec<PathBuf> = walkdir(&target_dir, "proj.db")
        .into_iter()
        .filter(|entry| !entry.to_string_lossy().contains("for_tests"))
        .collect();
    candidates.sort();
    candidates.into_iter().next().ok_or_else(|| {
        format!(
            "proj.db not found below {}. Build a bundled-proj binary first.",
            target_dir.display()
        )
    })
}

fn walkdir(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return results;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            results.extend(walkdir(&path, name));
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            results.push(path);
        }
    }
    results
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let proj_db = match args.iter().position(|arg| arg == "--proj-db") {
        Some(index) => PathBuf::from(args.get(index + 1).expect("--proj-db requires a path")),
        None => find_proj_db().expect("locate proj.db"),
    };
    eprintln!("Using proj.db at {}", proj_db.display());
    let conn = rusqlite::Connection::open(&proj_db).expect("open proj.db");

    let rows = candidate_rows(&conn);

    // Keep only CRS pairs with several candidate operations: those are the
    // pairs where ranking actually decides something.
    let mut per_pair: BTreeMap<(u32, u32), Vec<&CandidateRow>> = BTreeMap::new();
    for row in &rows {
        per_pair
            .entry((row.source_epsg, row.target_epsg))
            .or_default()
            .push(row);
    }

    let mut entries = Vec::new();
    'outer: for ((source, target), pair_rows) in &per_pair {
        if pair_rows.len() < 2 {
            continue;
        }
        for row in pair_rows {
            if entries.len() >= MAX_ENTRIES {
                break 'outer;
            }
            let probe = extent_centroid(row);
            match c_proj::used_operation(*source, *target, probe) {
                Ok(used) if used.auth == "EPSG" || used.code.is_some() => {
                    entries.push(ParityEntry {
                        source_epsg: *source,
                        target_epsg: *target,
                        probe_lon: probe.0,
                        probe_lat: probe.1,
                        expected_operation_epsg: used.code,
                        expected_operation_name: used.name,
                        probe_extent_name: row.extent_name.clone(),
                    });
                }
                Ok(used) => {
                    // Operations without EPSG identity (e.g. ad-hoc pipelines)
                    // still carry a name worth asserting against.
                    entries.push(ParityEntry {
                        source_epsg: *source,
                        target_epsg: *target,
                        probe_lon: probe.0,
                        probe_lat: probe.1,
                        expected_operation_epsg: None,
                        expected_operation_name: used.name,
                        probe_extent_name: row.extent_name.clone(),
                    });
                }
                Err(err) => {
                    eprintln!(
                        "Skipping EPSG:{source}->EPSG:{target} at ({:.4}, {:.4}): {err}",
                        probe.0, probe.1
                    );
                }
            }
        }
    }

    eprintln!("Generated {} selection parity entries", entries.len());
    let json = serde_json::to_string_pretty(&entries).unwrap();
    println!("{json}");
}

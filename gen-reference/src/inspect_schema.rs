use rusqlite::Connection;
use std::path::PathBuf;

fn find_proj_db() -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    for entry in walkdir(&target_dir, "proj.db") {
        if !entry.to_string_lossy().contains("for_tests") {
            return entry;
        }
    }
    panic!("proj.db not found");
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

fn main() {
    let db = find_proj_db();
    let conn = Connection::open(&db).unwrap();

    // List all tables
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap();
    let tables: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect();
    println!("Tables ({}):", tables.len());
    for t in &tables { println!("  {t}"); }

    // For conversion-related tables, show columns
    for table in &["conversion", "conversion_table", "projected_crs"] {
        if let Ok(mut s) = conn.prepare(&format!("PRAGMA table_info({table})")) {
            let cols: Vec<String> = s.query_map([], |r| r.get::<_, String>(1)).unwrap().filter_map(|r| r.ok()).collect();
            if !cols.is_empty() {
                println!("\n{table} columns: {}", cols.join(", "));
            }
        }
    }

    // Sample a conversion to see parameter storage
    let mut s = conn.prepare(
        "SELECT * FROM conversion_table WHERE auth_name='EPSG' LIMIT 1"
    ).unwrap_or_else(|_| conn.prepare("SELECT 'no conversion_table'").unwrap());
    let cols = s.column_names().iter().map(|c| c.to_string()).collect::<Vec<_>>();
    println!("\nconversion_table columns: {}", cols.join(", "));

    // Check a specific UTM zone
    println!("\n--- EPSG:32618 (UTM 18N) ---");
    let mut s = conn.prepare(
        "SELECT * FROM conversion_table WHERE auth_name='EPSG' AND code=16018"
    ).unwrap();
    let cols = s.column_names().iter().map(|c| c.to_string()).collect::<Vec<_>>();
    println!("Columns: {}", cols.join(", "));

    // Try to find the conversion code for UTM 18N
    let mut s = conn.prepare(
        "SELECT conversion_auth_name, conversion_code FROM projected_crs WHERE auth_name='EPSG' AND code=32618"
    ).unwrap();
    let row: (String, i64) = s.query_row([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    println!("UTM 18N conversion: {} {}", row.0, row.1);

    // Get the conversion details
    let mut s = conn.prepare(
        "SELECT * FROM conversion_table WHERE auth_name=?1 AND code=?2"
    ).unwrap();
    let n = s.column_count();
    let cols = s.column_names().iter().map(|c| c.to_string()).collect::<Vec<_>>();
    println!("Columns: {}", cols.join(", "));
    s.query_row(rusqlite::params![row.0, row.1], |r| {
        for i in 0..n {
            let val: String = r.get::<_, rusqlite::types::Value>(i).map(|v| format!("{:?}", v)).unwrap_or_default();
            println!("  {}: {}", cols[i], val);
        }
        Ok(())
    }).unwrap();
}

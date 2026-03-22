#![cfg(feature = "c-proj-compat")]

use proj::Proj;
use proj_core::Transform;
use serde::Deserialize;

#[derive(Deserialize)]
struct ReferencePoint {
    from_epsg: u32,
    to_epsg: u32,
    input_x: f64,
    input_y: f64,
    expected_x: f64,
    expected_y: f64,
    tolerance: f64,
    description: String,
}

fn load_corpus() -> Vec<ReferencePoint> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../testdata/reference_values.json"
    );
    let data =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"))
}

fn live_c_proj(from_epsg: u32, to_epsg: u32, x: f64, y: f64) -> Result<(f64, f64), String> {
    let from = format!("EPSG:{from_epsg}");
    let to = format!("EPSG:{to_epsg}");
    let proj = Proj::new_known_crs(&from, &to, None)
        .map_err(|e| format!("failed to create C PROJ transform {from}->{to}: {e}"))?;
    proj.convert((x, y))
        .map_err(|e| format!("C PROJ convert failed for {from}->{to}: {e}"))
}

fn assert_within_tolerance(
    description: &str,
    expected: (f64, f64),
    actual: (f64, f64),
    tolerance: f64,
) -> Option<String> {
    let dx = (actual.0 - expected.0).abs();
    let dy = (actual.1 - expected.1).abs();
    if dx <= tolerance && dy <= tolerance {
        return None;
    }

    Some(format!(
        "{description}: expected ({}, {}), got ({}, {}), delta ({:e}, {:e}), tol {:e}",
        expected.0, expected.1, actual.0, actual.1, dx, dy, tolerance
    ))
}

#[test]
fn reference_corpus_stays_in_sync_with_live_c_proj() {
    let corpus = load_corpus();
    assert!(!corpus.is_empty(), "corpus is empty");

    let mut failures = Vec::new();

    for point in &corpus {
        let actual = match live_c_proj(point.from_epsg, point.to_epsg, point.input_x, point.input_y)
        {
            Ok(result) => result,
            Err(error) => {
                failures.push(format!("{}: {error}", point.description));
                continue;
            }
        };

        if let Some(failure) = assert_within_tolerance(
            &point.description,
            (point.expected_x, point.expected_y),
            actual,
            point.tolerance,
        ) {
            failures.push(failure);
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} of {} corpus points drifted from live C PROJ:\n{}",
            failures.len(),
            corpus.len(),
            failures.join("\n")
        );
    }
}

#[test]
fn proj_core_matches_live_c_proj_for_supported_corpus_cases() {
    let corpus = load_corpus();
    assert!(!corpus.is_empty(), "corpus is empty");

    let mut compared = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for point in &corpus {
        let transform = match Transform::from_epsg(point.from_epsg, point.to_epsg) {
            Ok(transform) => transform,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let expected =
            match live_c_proj(point.from_epsg, point.to_epsg, point.input_x, point.input_y) {
                Ok(result) => result,
                Err(error) => {
                    failures.push(format!("{}: {error}", point.description));
                    continue;
                }
            };

        let actual = match transform.convert((point.input_x, point.input_y)) {
            Ok(result) => result,
            Err(error) => {
                failures.push(format!(
                    "{}: proj-core convert failed: {error}",
                    point.description
                ));
                continue;
            }
        };

        if let Some(failure) =
            assert_within_tolerance(&point.description, expected, actual, point.tolerance)
        {
            failures.push(failure);
        } else {
            compared += 1;
        }
    }

    assert!(
        compared >= 100,
        "expected broad live coverage, only compared {compared} points ({skipped} skipped)"
    );

    if !failures.is_empty() {
        panic!(
            "{} of {} live C PROJ comparisons failed ({} skipped):\n{}",
            failures.len(),
            corpus.len(),
            skipped,
            failures.join("\n")
        );
    }
}

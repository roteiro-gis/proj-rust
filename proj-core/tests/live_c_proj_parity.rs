#![cfg(feature = "c-proj-compat")]

#[path = "common/c_proj_ffi.rs"]
mod c_proj_ffi;

use c_proj_ffi::CProjTransform;
use proj::Proj;
use proj_core::Transform;
use serde::Deserialize;

#[derive(Deserialize)]
struct ReferencePoint {
    from_epsg: u32,
    to_epsg: u32,
    input_x: f64,
    input_y: f64,
    input_z: Option<f64>,
    expected_x: f64,
    expected_y: f64,
    expected_z: Option<f64>,
    tolerance: f64,
    tolerance_z: Option<f64>,
    description: String,
}

impl ReferencePoint {
    fn is_3d(&self) -> bool {
        self.input_z.is_some() && self.expected_z.is_some()
    }
}

fn load_corpus() -> Vec<ReferencePoint> {
    c_proj_ffi::assert_reference_version();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../testdata/reference_values.json"
    );
    let data =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"))
}

/// Live C PROJ result: XY, plus Z when the corpus record is 3D.
fn live_c_proj(point: &ReferencePoint) -> Result<(f64, f64, Option<f64>), String> {
    let from = format!("EPSG:{}", point.from_epsg);
    let to = format!("EPSG:{}", point.to_epsg);
    if point.is_3d() {
        let z = point.input_z.expect("3D record has input_z");
        CProjTransform::new_known_crs(&from, &to)
            .and_then(|proj| proj.convert_3d((point.input_x, point.input_y, z)))
            .map(|(x, y, z)| (x, y, Some(z)))
            .map_err(|e| format!("C PROJ convert_3d failed for {from}->{to}: {e}"))
    } else {
        let proj = Proj::new_known_crs(&from, &to, None)
            .map_err(|e| format!("failed to create C PROJ transform {from}->{to}: {e}"))?;
        proj.convert((point.input_x, point.input_y))
            .map(|(x, y)| (x, y, None))
            .map_err(|e| format!("C PROJ convert failed for {from}->{to}: {e}"))
    }
}

fn rust_transform(point: &ReferencePoint) -> proj_core::Result<Transform> {
    Transform::from_epsg(point.from_epsg, point.to_epsg)
}

fn rust_convert(
    transform: &Transform,
    point: &ReferencePoint,
) -> Result<(f64, f64, Option<f64>), String> {
    if point.is_3d() {
        let z = point.input_z.expect("3D record has input_z");
        transform
            .convert_3d((point.input_x, point.input_y, z))
            .map(|(x, y, z)| (x, y, Some(z)))
            .map_err(|e| format!("{}: proj-core convert_3d failed: {e}", point.description))
    } else {
        transform
            .convert((point.input_x, point.input_y))
            .map(|(x, y)| (x, y, None))
            .map_err(|e| format!("{}: proj-core convert failed: {e}", point.description))
    }
}

fn assert_within_tolerance(
    point: &ReferencePoint,
    expected: (f64, f64, Option<f64>),
    actual: (f64, f64, Option<f64>),
) -> Option<String> {
    let dx = (actual.0 - expected.0).abs();
    let dy = (actual.1 - expected.1).abs();
    let dz = match (expected.2, actual.2) {
        (Some(e), Some(a)) => Some((a - e).abs()),
        _ => None,
    };
    let tol_z = point.tolerance_z.unwrap_or(point.tolerance);
    if dx <= point.tolerance && dy <= point.tolerance && dz.is_none_or(|d| d <= tol_z) {
        return None;
    }

    Some(format!(
        "{}: expected {:?}, got {:?}, delta ({:e}, {:e}, {:?}), tol {:e}, tol_z {:e}",
        point.description,
        (expected.0, expected.1, expected.2),
        (actual.0, actual.1, actual.2),
        dx,
        dy,
        dz,
        point.tolerance,
        tol_z
    ))
}

#[test]
fn reference_corpus_stays_in_sync_with_live_c_proj() {
    let corpus = load_corpus();
    assert!(!corpus.is_empty(), "corpus is empty");

    let mut failures = Vec::new();

    for point in &corpus {
        let actual = match live_c_proj(point) {
            Ok(actual) => actual,
            Err(error) => {
                failures.push(format!("{}: {error}", point.description));
                continue;
            }
        };
        if let Some(failure) = assert_within_tolerance(
            point,
            (point.expected_x, point.expected_y, point.expected_z),
            actual,
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
        let transform = match rust_transform(point) {
            Ok(transform) => transform,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let expected = match live_c_proj(point) {
            Ok(expected) => expected,
            Err(error) => {
                failures.push(format!("{}: {error}", point.description));
                continue;
            }
        };
        let actual = match rust_convert(&transform, point) {
            Ok(actual) => actual,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };

        if let Some(failure) = assert_within_tolerance(point, expected, actual) {
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

/// These give their scale at the pole, which the corpus's standard-parallel
/// polar CRSs cannot exercise.
#[test]
fn polar_stereographic_scale_at_the_pole_matches_c_proj() {
    for (code, lon, lat) in [
        (5041, 10.0, 85.0),
        (5042, 10.0, -85.0),
        (32661, -120.0, 88.0),
        (32761, 150.0, -88.0),
    ] {
        let ours = Transform::from_epsg(4326, code)
            .unwrap()
            .convert((lon, lat))
            .unwrap();
        let (x, y, _) = CProjTransform::new_known_crs("EPSG:4326", &format!("EPSG:{code}"))
            .unwrap()
            .convert_3d((lon, lat, 0.0))
            .unwrap();
        assert!(
            (ours.0 - x).abs() < 1e-3 && (ours.1 - y).abs() < 1e-3,
            "EPSG:{code}"
        );
    }
}

/// These cones lie south of the equator, so their cone constant is negative,
/// which the corpus's northern conic CRSs cannot exercise.
#[test]
fn southern_conic_inverse_matches_c_proj() {
    for (code, geographic, lon, lat) in [
        (8058, 7844, 141.625, -37.061),
        (9473, 7844, 135.0, -25.0),
        (3033, 4326, 80.0, -70.0),
        (3249, 4326, 30.0, -72.0),
    ] {
        let (x, y, _) =
            CProjTransform::new_known_crs(&format!("EPSG:{geographic}"), &format!("EPSG:{code}"))
                .unwrap()
                .convert_3d((lon, lat, 0.0))
                .unwrap();
        let ours = Transform::from_epsg(code, geographic)
            .unwrap()
            .convert((x, y))
            .unwrap();
        assert!(
            (ours.0 - lon).abs() < 1e-9 && (ours.1 - lat).abs() < 1e-9,
            "EPSG:{code}: {ours:?}"
        );
    }
}

/// PROJ before 9.8 used the spherical formula on ellipsoids. Check the
/// corrected method in both directions for the modern and deprecated CRS codes.
#[test]
fn equidistant_cylindrical_matches_c_proj() {
    c_proj_ffi::assert_reference_version();
    for (code, geographic) in [(4087, 4326), (32662, 4326)] {
        let ours = Transform::from_epsg(geographic, code).unwrap();
        let inverse = ours.inverse().unwrap();
        let forward =
            CProjTransform::new_known_crs(&format!("EPSG:{geographic}"), &format!("EPSG:{code}"))
                .unwrap();
        let backward =
            CProjTransform::new_known_crs(&format!("EPSG:{code}"), &format!("EPSG:{geographic}"))
                .unwrap();
        for (lon, lat) in [
            (10.0, 50.0),
            (-74.006, 40.7128),
            (151.2093, -33.8688),
            (0.0, 90.0),
            (0.0, -90.0),
            (180.0, 89.999),
            (-180.0, -89.999),
            (0.0, 0.0),
        ] {
            let (x, y, _) = forward.convert_3d((lon, lat, 0.0)).unwrap();
            let actual = ours.convert((lon, lat)).unwrap();
            assert!(
                (actual.0 - x).abs() < 1e-6 && (actual.1 - y).abs() < 1e-6,
                "EPSG:{code} forward ({lon}, {lat}): {actual:?} vs ({x}, {y})"
            );
            let (lon2, lat2, _) = backward.convert_3d((x, y, 0.0)).unwrap();
            let actual = inverse.convert((x, y)).unwrap();
            // The antimeridian can normalize to either -180 or +180 degrees.
            let delta_lon = (actual.0 - lon2 + 180.0).rem_euclid(360.0) - 180.0;
            assert!(
                delta_lon.abs() < 1e-9 && (actual.1 - lat2).abs() < 1e-9,
                "EPSG:{code} inverse ({x}, {y}): {actual:?} vs ({lon2}, {lat2})"
            );
        }
    }
}

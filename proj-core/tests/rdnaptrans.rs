//! RDNAPTRANS2018 parity: ETRS89/WGS 84 3D (EPSG:4979) <-> RD New + NAP
//! (EPSG:7415) using PROJ's GeoTIFF grids, checked against PROJ 9.8 output.
//!
//! Requires the `geotiff` feature. Grid files are not bundled; point
//! `PROJ_RDNAP_GRID_DIR` at a directory containing `nl_nsgi_rdtrans2018.tif`
//! and `nl_nsgi_nlgeo2018.tif` (e.g. downloaded from <https://cdn.proj.org/>).
//! When the variable is unset or the grids are missing, the tests no-op.
#![cfg(feature = "geotiff")]

use std::path::PathBuf;
use std::sync::Arc;

use proj_core::{FilesystemGridProvider, SelectionOptions, Transform};

const GRIDS: [&str; 2] = ["nl_nsgi_rdtrans2018.tif", "nl_nsgi_nlgeo2018.tif"];

/// Returns the grid directory if it exists and contains both grids, else `None`.
fn grid_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("PROJ_RDNAP_GRID_DIR")?);
    GRIDS
        .iter()
        .all(|name| dir.join(name).is_file())
        .then_some(dir)
}

fn rd_transform(dir: PathBuf) -> Transform {
    let provider = Arc::new(FilesystemGridProvider::new(vec![dir]));
    let options = SelectionOptions::new().with_grid_provider(provider);
    Transform::with_selection_options("EPSG:4979", "EPSG:7415", options)
        .expect("EPSG:4979 -> EPSG:7415 transform")
}

// Reference produced by:
//   echo "53.294378 6.605585 50" | PROJ_NETWORK=ON \
//     cs2cs -f "%.6f" EPSG:4979 EPSG:7415
// (PROJ 9.8.0, operation: RD New via nl_nsgi_rdtrans2018 + NAP via nl_nsgi_nlgeo2018)
const PROJ_RD_X: f64 = 236238.891848;
const PROJ_RD_Y: f64 = 590452.359452;
const PROJ_NAP: f64 = 9.409506;

// Input is lon, lat, ellipsoidal height (proj-core geographic axis order).
const INPUT_LON: f64 = 6.605585;
const INPUT_LAT: f64 = 53.294378;
const INPUT_H: f64 = 50.0;

#[test]
fn rdnaptrans_forward_matches_proj() {
    let Some(dir) = grid_dir() else {
        eprintln!("skipping: set PROJ_RDNAP_GRID_DIR to the RDNAPTRANS2018 grid directory");
        return;
    };
    let transform = rd_transform(dir);

    let metadata = transform.selected_operation();
    assert!(
        metadata.uses_grids,
        "expected the grid-backed RDNAPTRANS operation, got {:?}",
        metadata.name
    );

    let (x, y, z) = transform
        .convert_3d((INPUT_LON, INPUT_LAT, INPUT_H))
        .expect("forward convert_3d");

    // Sub-millimetre agreement with PROJ (residual is float32 grid storage and
    // independent rounding in the inverse grid-shift iteration / projection).
    assert!(
        (x - PROJ_RD_X).abs() < 1e-3,
        "RD X {x:.6} vs PROJ {PROJ_RD_X:.6}"
    );
    assert!(
        (y - PROJ_RD_Y).abs() < 1e-3,
        "RD Y {y:.6} vs PROJ {PROJ_RD_Y:.6}"
    );
    assert!(
        (z - PROJ_NAP).abs() < 1e-3,
        "NAP height {z:.6} vs PROJ {PROJ_NAP:.6}"
    );
}

#[test]
fn rdnaptrans_round_trips() {
    let Some(dir) = grid_dir() else {
        eprintln!("skipping: set PROJ_RDNAP_GRID_DIR to the RDNAPTRANS2018 grid directory");
        return;
    };
    let forward = rd_transform(dir);
    let inverse = forward.inverse().expect("inverse transform");

    let projected = forward
        .convert_3d((INPUT_LON, INPUT_LAT, INPUT_H))
        .expect("forward convert_3d");
    let (lon, lat, h) = inverse.convert_3d(projected).expect("inverse convert_3d");

    assert!((lon - INPUT_LON).abs() < 1e-8, "lon {lon} vs {INPUT_LON}");
    assert!((lat - INPUT_LAT).abs() < 1e-8, "lat {lat} vs {INPUT_LAT}");
    assert!((h - INPUT_H).abs() < 1e-3, "height {h} vs {INPUT_H}");
}

#[test]
fn rdnaptrans_without_grids_does_not_select_grid_operation() {
    // With no grid provider the grid-backed operation cannot compile, so
    // selection must not pick it (the vertical geoid step also has no grid, so
    // construction is expected to fail outright).
    if let Ok(transform) = Transform::new("EPSG:4979", "EPSG:7415") {
        assert!(
            !transform.selected_operation().uses_grids,
            "no grids were provided, so a grid-backed operation must not be selected"
        );
    }
}

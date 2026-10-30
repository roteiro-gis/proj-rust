//! RDNAPTRANS2018 parity: ETRS89/WGS 84 3D (EPSG:4979) <-> RD New + NAP
//! (EPSG:7415) using PROJ's GeoTIFF grids, checked against PROJ 9.8 output.
//!
//! Requires the `geotiff` feature. Grid files are not bundled. Either point
//! `PROJ_RDNAP_GRID_DIR` at a directory containing `nl_nsgi_rdtrans2018.tif`
//! and `nl_nsgi_nlgeo2018.tif`, or set `PROJ_NETWORK=ON` (as for C PROJ) to
//! fetch them on demand from <https://cdn.proj.org/> into
//! `CARGO_TARGET_TMPDIR`, checked against pinned SHA-256 checksums.
//! Without either variable the tests no-op.
#![cfg(feature = "geotiff")]

#[path = "common/cdn_grids.rs"]
mod cdn_grids;

use std::sync::Arc;

use cdn_grids::PinnedGrid;
use proj_core::{GridProvider, SelectionOptions, SelectionPolicy, Transform};

const GRIDS: &[PinnedGrid] = &[
    (
        "nl_nsgi_rdtrans2018.tif",
        "sha256:7653831191b424e715a906468962fc60071cfb71a3186b2a58f098bab8bf41de",
    ),
    (
        "nl_nsgi_nlgeo2018.tif",
        "sha256:f8e32c56bf8940fc3fefbc0e413eb4546633ed598b669c63856cddf8328992c0",
    ),
];

fn grid_provider() -> Option<Arc<dyn GridProvider>> {
    cdn_grids::grid_provider("PROJ_RDNAP_GRID_DIR", GRIDS)
}

fn rd_transform(provider: Arc<dyn GridProvider>) -> Transform {
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
    let Some(provider) = grid_provider() else {
        eprintln!("skipping: set PROJ_RDNAP_GRID_DIR or PROJ_NETWORK=ON");
        return;
    };
    let transform = rd_transform(provider);

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
    let Some(provider) = grid_provider() else {
        eprintln!("skipping: set PROJ_RDNAP_GRID_DIR or PROJ_NETWORK=ON");
        return;
    };
    let forward = rd_transform(provider);
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
fn rdnaptrans_requiring_grids_matches_proj() {
    let Some(provider) = grid_provider() else {
        eprintln!("skipping: set PROJ_RDNAP_GRID_DIR or PROJ_NETWORK=ON");
        return;
    };
    // The geoid sampling step must not need a grid-backed WGS 84 to ETRS89
    // operation: that shift is null.
    let options = SelectionOptions::new()
        .with_grid_provider(provider)
        .with_policy(SelectionPolicy::RequireGrids);
    let transform = Transform::with_selection_options("EPSG:4979", "EPSG:7415", options)
        .expect("EPSG:4979 -> EPSG:7415 transform");

    let (x, y, z) = transform
        .convert_3d((INPUT_LON, INPUT_LAT, INPUT_H))
        .expect("forward convert_3d");

    assert!((x - PROJ_RD_X).abs() < 1e-3, "RD X {x:.6}");
    assert!((y - PROJ_RD_Y).abs() < 1e-3, "RD Y {y:.6}");
    assert!((z - PROJ_NAP).abs() < 1e-3, "NAP height {z:.6}");
}

#[test]
fn wgs84_to_rd_new_keeps_the_rdtrans_grid() {
    let Some(provider) = grid_provider() else {
        eprintln!("skipping: set PROJ_RDNAP_GRID_DIR or PROJ_NETWORK=ON");
        return;
    };
    // Unlike the BD72 grid, "Amersfoort to ETRS89 (9)" adds no stated
    // uncertainty to the bridge, so it ties the direct Helmerts and wins.
    let options = SelectionOptions::new().with_grid_provider(provider);
    let transform = Transform::with_selection_options("EPSG:4326", "EPSG:28992", options)
        .expect("EPSG:4326 -> EPSG:28992 transform");

    assert!(transform.selected_operation().uses_grids);
    let (x, y) = transform
        .convert((INPUT_LON, INPUT_LAT))
        .expect("forward convert");
    assert!((x - PROJ_RD_X).abs() < 1e-3, "RD X {x:.6}");
    assert!((y - PROJ_RD_Y).abs() < 1e-3, "RD Y {y:.6}");
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

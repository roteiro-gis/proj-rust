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

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use proj_core::{
    FilesystemGridProvider, GridDefinition, GridError, GridHandle, GridProvider, SelectionOptions,
    Transform,
};

/// Grid resources and their checksums as reported by [`GridHandle::checksum`].
const GRIDS: [(&str, &str); 2] = [
    (
        "nl_nsgi_rdtrans2018.tif",
        "sha256:7653831191b424e715a906468962fc60071cfb71a3186b2a58f098bab8bf41de",
    ),
    (
        "nl_nsgi_nlgeo2018.tif",
        "sha256:f8e32c56bf8940fc3fefbc0e413eb4546633ed598b669c63856cddf8328992c0",
    ),
];

/// Fetches the pinned RDNAPTRANS grids from the PROJ CDN on first use, like
/// C PROJ with `PROJ_NETWORK=ON`. Other grids are reported as unavailable.
struct CdnGridProvider {
    cache_dir: PathBuf,
}

impl CdnGridProvider {
    fn pinned(grid: &GridDefinition) -> Option<(&'static str, &'static str)> {
        GRIDS.into_iter().find(|(name, _)| {
            grid.name == *name || grid.resource_names.iter().any(|resource| resource == name)
        })
    }

    fn fetch(&self, name: &str) -> PathBuf {
        // Tests share the cache; serialize downloads so none reads a partial file.
        static DOWNLOAD: Mutex<()> = Mutex::new(());
        let _guard = DOWNLOAD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.cache_dir.join(name);
        if !path.is_file() {
            std::fs::create_dir_all(&self.cache_dir).expect("create grid cache dir");
            let partial = path.with_extension("part");
            let status = Command::new("curl")
                .args(["--fail", "--silent", "--show-error", "--location"])
                .args(["--retry", "3", "--output"])
                .arg(&partial)
                .arg(format!("https://cdn.proj.org/{name}"))
                .status()
                .expect("run curl");
            assert!(status.success(), "downloading {name} failed: {status}");
            std::fs::rename(&partial, &path).expect("move downloaded grid into cache");
        }
        path
    }
}

impl GridProvider for CdnGridProvider {
    fn definition(&self, grid: &GridDefinition) -> Result<Option<GridDefinition>, GridError> {
        Ok(Self::pinned(grid).map(|_| grid.clone()))
    }

    fn load(&self, grid: &GridDefinition) -> Result<Option<GridHandle>, GridError> {
        let Some((name, checksum)) = Self::pinned(grid) else {
            return Ok(None);
        };
        let path = self.fetch(name);
        let bytes = std::fs::read(&path).expect("read cached grid");
        let handle = GridHandle::from_bytes(grid.clone(), &bytes)?;
        assert_eq!(
            handle.checksum(),
            checksum,
            "{name} checksum mismatch; delete {} to download it again",
            path.display()
        );
        Ok(Some(handle))
    }
}

/// Grid provider from `PROJ_RDNAP_GRID_DIR` or `PROJ_NETWORK`, else `None`.
fn grid_provider() -> Option<Arc<dyn GridProvider>> {
    if let Some(dir) = std::env::var_os("PROJ_RDNAP_GRID_DIR") {
        let dir = PathBuf::from(dir);
        for (name, _) in GRIDS {
            assert!(
                dir.join(name).is_file(),
                "PROJ_RDNAP_GRID_DIR is set but {name} is missing from {}",
                dir.display()
            );
        }
        return Some(Arc::new(FilesystemGridProvider::new(vec![dir])));
    }
    let network = std::env::var("PROJ_NETWORK").ok()?;
    ["ON", "YES", "TRUE"]
        .iter()
        .any(|value| network.eq_ignore_ascii_case(value))
        .then(|| {
            Arc::new(CdnGridProvider {
                cache_dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("proj-grids"),
            }) as Arc<dyn GridProvider>
        })
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

//! Checksum-pinned PROJ grids for parity tests.
//!
//! Either point `dir_env` at a directory containing the grids, or set
//! `PROJ_NETWORK=ON` (as for C PROJ) to fetch them on demand from
//! <https://cdn.proj.org/> into `CARGO_TARGET_TMPDIR`, checked against pinned
//! SHA-256 checksums. Without either variable [`grid_provider`] returns `None`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use proj_core::{FilesystemGridProvider, GridDefinition, GridError, GridHandle, GridProvider};

/// Grid resource name and its checksum as reported by [`GridHandle::checksum`].
pub type PinnedGrid = (&'static str, &'static str);

/// Fetches the pinned grids from the PROJ CDN on first use, like C PROJ with
/// `PROJ_NETWORK=ON`. Other grids are reported as unavailable.
struct CdnGridProvider {
    cache_dir: PathBuf,
    grids: &'static [PinnedGrid],
}

impl CdnGridProvider {
    fn pinned(&self, grid: &GridDefinition) -> Option<PinnedGrid> {
        self.grids.iter().copied().find(|(name, _)| {
            grid.name == *name || grid.resource_names.iter().any(|resource| resource == name)
        })
    }

    /// Returns the cached grid, downloading it first when missing. Downloads
    /// go to a per-process `.part` file and are checked before the rename,
    /// so concurrent test processes never install a partial or corrupt grid.
    fn fetch(&self, grid: &GridDefinition, name: &str, checksum: &str) -> GridHandle {
        // Tests in this process share the cache; download each grid once.
        static DOWNLOAD: Mutex<()> = Mutex::new(());
        let _guard = DOWNLOAD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.cache_dir.join(name);
        if path.is_file() {
            let bytes = std::fs::read(&path).expect("read cached grid");
            let handle = GridHandle::from_bytes(grid.clone(), &bytes).expect("parse cached grid");
            assert_eq!(
                handle.checksum(),
                checksum,
                "{name} checksum mismatch; delete {} to download it again",
                path.display()
            );
            return handle;
        }
        std::fs::create_dir_all(&self.cache_dir).expect("create grid cache dir");
        let partial = path.with_extension(format!("{}.part", std::process::id()));
        let status = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--retry", "3", "--output"])
            .arg(&partial)
            .arg(format!("https://cdn.proj.org/{name}"))
            .status()
            .expect("run curl");
        assert!(status.success(), "downloading {name} failed: {status}");
        let bytes = std::fs::read(&partial).expect("read downloaded grid");
        let handle = GridHandle::from_bytes(grid.clone(), &bytes);
        let verified = handle
            .as_ref()
            .is_ok_and(|handle| handle.checksum() == checksum);
        if !verified {
            let _ = std::fs::remove_file(&partial);
            panic!("downloaded {name} failed verification against {checksum}");
        }
        std::fs::rename(&partial, &path).expect("move downloaded grid into cache");
        handle.unwrap()
    }
}

impl GridProvider for CdnGridProvider {
    fn definition(&self, grid: &GridDefinition) -> Result<Option<GridDefinition>, GridError> {
        Ok(self.pinned(grid).map(|_| grid.clone()))
    }

    fn load(&self, grid: &GridDefinition) -> Result<Option<GridHandle>, GridError> {
        Ok(self
            .pinned(grid)
            .map(|(name, checksum)| self.fetch(grid, name, checksum)))
    }
}

/// Grid provider from `dir_env` or `PROJ_NETWORK`, else `None`.
///
/// A non-empty `dir_env` missing a grid falls back to the network when
/// enabled, and otherwise fails rather than skipping.
pub fn grid_provider(dir_env: &str, grids: &'static [PinnedGrid]) -> Option<Arc<dyn GridProvider>> {
    let network = std::env::var("PROJ_NETWORK").is_ok_and(|value| {
        ["ON", "YES", "TRUE"]
            .iter()
            .any(|enabled| value.eq_ignore_ascii_case(enabled))
    });
    if let Some(dir) = std::env::var_os(dir_env).filter(|dir| !dir.is_empty()) {
        let dir = PathBuf::from(dir);
        let missing = grids
            .iter()
            .find(|(name, _)| !dir.join(name).is_file())
            .map(|(name, _)| *name);
        match missing {
            None => return Some(Arc::new(FilesystemGridProvider::new(vec![dir]))),
            Some(name) => assert!(
                network,
                "{dir_env} is set but {name} is missing from {}",
                dir.display()
            ),
        }
    }
    network.then(|| {
        Arc::new(CdnGridProvider {
            cache_dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("proj-grids"),
            grids,
        }) as Arc<dyn GridProvider>
    })
}

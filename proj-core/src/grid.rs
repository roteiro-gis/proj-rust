use crate::operation::{AreaOfUse, GridId, GridInterpolation, GridShiftDirection};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use thiserror::Error;

const NTV2_HEADER_LEN: usize = 11 * 16;
const NTV2_RECORD_LEN: usize = 4 * 4;
const MAX_NTV2_SUBFILES: usize = 4_096;
const MAX_NTV2_CELLS_PER_SUBGRID: usize = 16_777_216;
const MAX_NTV2_TOTAL_CELLS: usize = 16_777_216;
const MAX_NTV2_TOTAL_DATA_BYTES: usize = MAX_NTV2_TOTAL_CELLS * NTV2_RECORD_LEN;
const MAX_NTV2_GRID_BYTES: usize =
    MAX_NTV2_TOTAL_DATA_BYTES + (MAX_NTV2_SUBFILES + 1) * NTV2_HEADER_LEN;
const GTX_HEADER_LEN: usize = 40;
const GTX_RECORD_LEN: usize = 4;
const MAX_GTX_CELLS: usize = 16_777_216;
const MAX_GTX_GRID_BYTES: usize = GTX_HEADER_LEN + MAX_GTX_CELLS * GTX_RECORD_LEN;
/// Upper bound on an accepted GeoTIFF grid resource. Compressed PROJ grids are
/// well under this; the cap simply bounds untrusted input before decoding.
const MAX_GEOTIFF_GRID_BYTES: usize = 256 * 1024 * 1024;
#[cfg(feature = "geotiff")]
const MAX_GEOTIFF_IFDS: usize = 4_096;
#[cfg(feature = "geotiff")]
const MAX_GEOTIFF_CELLS_PER_IMAGE: usize = 16_777_216;
#[cfg(feature = "geotiff")]
const MAX_GEOTIFF_TOTAL_CELLS: usize = 16_777_216;
#[cfg(feature = "geotiff")]
const MAX_GEOTIFF_BANDS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridFormat {
    /// NTv2 horizontal datum-shift grid (`.gsb`).
    Ntv2,
    /// NOAA/VDatum binary GTX vertical offset grid (`.gtx`).
    Gtx,
    /// PROJ-format GeoTIFF/COG grid (`.tif`), as distributed on the PROJ CDN.
    ///
    /// The `TYPE` GDAL metadata item selects horizontal (NTv2-equivalent
    /// latitude/longitude offsets) or vertical (geoid undulation) semantics;
    /// both are decoded into the same internal representation as the binary
    /// NTv2/GTX formats. Requires the `geotiff` crate feature.
    GeoTiff,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GridDefinition {
    pub id: GridId,
    pub name: String,
    pub format: GridFormat,
    pub interpolation: GridInterpolation,
    pub area_of_use: Option<AreaOfUse>,
    pub resource_names: SmallVec<[String; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSample {
    pub lon_shift_radians: f64,
    pub lat_shift_radians: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VerticalGridSample {
    /// Vertical offset in meters at the sampled horizontal position.
    pub offset_meters: f64,
}

#[derive(Debug, Error, Clone)]
pub enum GridError {
    #[error("grid not found: {0}")]
    NotFound(String),
    #[error("grid resource unavailable: {0}")]
    Unavailable(String),
    #[error("grid parse error: {0}")]
    Parse(String),
    #[error("grid point outside coverage: {0}")]
    OutsideCoverage(String),
    #[error("unsupported grid format: {0}")]
    UnsupportedFormat(String),
}

pub trait GridProvider: Send + Sync {
    fn definition(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<Option<GridDefinition>, GridError>;
    fn load(&self, grid: &GridDefinition) -> std::result::Result<Option<GridHandle>, GridError>;
}

#[derive(Clone)]
pub struct GridHandle {
    definition: GridDefinition,
    data: Arc<CachedGridData>,
}

impl GridHandle {
    /// Parse a grid resource into a handle.
    ///
    /// Custom [`GridProvider`] implementations can use this constructor after
    /// loading bytes from their own package, object store, or manifest.
    pub fn from_bytes(
        definition: GridDefinition,
        bytes: &[u8],
    ) -> std::result::Result<Self, GridError> {
        Ok(Self {
            data: Arc::new(parse_cached_grid_data(
                definition.format,
                &definition.name,
                bytes,
            )?),
            definition,
        })
    }

    pub fn definition(&self) -> &GridDefinition {
        &self.definition
    }

    pub fn checksum(&self) -> &str {
        &self.data.checksum
    }

    pub fn sample(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<GridSample, GridError> {
        match &self.data.data {
            GridData::Ntv2(set) => set.sample(lon_radians, lat_radians),
            GridData::Gtx(_) => Err(GridError::UnsupportedFormat(format!(
                "{} is a vertical grid",
                self.definition.name
            ))),
        }
    }

    pub fn sample_vertical_offset_meters(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<VerticalGridSample, GridError> {
        match &self.data.data {
            GridData::Gtx(grid) => grid.sample(lon_radians, lat_radians),
            GridData::Ntv2(_) => Err(GridError::UnsupportedFormat(format!(
                "{} is a horizontal grid",
                self.definition.name
            ))),
        }
    }

    pub fn apply(
        &self,
        lon_radians: f64,
        lat_radians: f64,
        direction: GridShiftDirection,
    ) -> std::result::Result<(f64, f64), GridError> {
        match &self.data.data {
            GridData::Ntv2(set) => set.apply(lon_radians, lat_radians, direction),
            GridData::Gtx(_) => Err(GridError::UnsupportedFormat(format!(
                "{} is a vertical grid",
                self.definition.name
            ))),
        }
    }
}

pub(crate) struct GridRuntime {
    providers: Vec<Arc<dyn GridProvider>>,
    definition_cache: Mutex<HashMap<String, GridDefinition>>,
    handle_cache: Mutex<HashMap<String, GridHandle>>,
}

impl GridRuntime {
    pub(crate) fn new(app_provider: Option<Arc<dyn GridProvider>>) -> Self {
        let mut providers: Vec<Arc<dyn GridProvider>> = Vec::with_capacity(2);
        if let Some(provider) = app_provider {
            providers.push(provider);
        }
        providers.push(Arc::new(EmbeddedGridProvider));
        Self {
            providers,
            definition_cache: Mutex::new(HashMap::new()),
            handle_cache: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn resolve_definition(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<GridDefinition, GridError> {
        let cache_key = grid_runtime_cache_key(grid);
        if let Some(cached) = self
            .definition_cache
            .lock()
            .expect("grid definition cache poisoned")
            .get(&cache_key)
            .cloned()
        {
            return Ok(cached);
        }

        for provider in &self.providers {
            if let Some(definition) = provider.definition(grid)? {
                self.definition_cache
                    .lock()
                    .expect("grid definition cache poisoned")
                    .insert(cache_key, definition.clone());
                return Ok(definition);
            }
        }

        Err(GridError::Unavailable(grid.name.clone()))
    }

    pub(crate) fn resolve_handle(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<GridHandle, GridError> {
        let cache_key = grid_runtime_cache_key(grid);
        if let Some(cached) = self
            .handle_cache
            .lock()
            .expect("grid handle cache poisoned")
            .get(&cache_key)
            .cloned()
        {
            return Ok(cached);
        }

        let definition = self.resolve_definition(grid)?;
        for provider in &self.providers {
            if let Some(handle) = provider.load(&definition)? {
                self.handle_cache
                    .lock()
                    .expect("grid handle cache poisoned")
                    .insert(cache_key, handle.clone());
                return Ok(handle);
            }
        }

        Err(GridError::Unavailable(definition.name))
    }
}

fn grid_runtime_cache_key(grid: &GridDefinition) -> String {
    let mut key = format!("{}|{:?}", grid.id.0, grid.format);
    for resource in &grid.resource_names {
        key.push('|');
        key.push_str(resource);
    }
    key
}

#[derive(Default)]
pub struct EmbeddedGridProvider;

impl GridProvider for EmbeddedGridProvider {
    fn definition(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<Option<GridDefinition>, GridError> {
        if embedded_grid_resource(&grid.resource_names).is_some() {
            return Ok(Some(grid.clone()));
        }
        Ok(None)
    }

    fn load(&self, grid: &GridDefinition) -> std::result::Result<Option<GridHandle>, GridError> {
        let Some((resource_name, bytes)) = embedded_grid_resource(&grid.resource_names) else {
            return Ok(None);
        };

        let key = GridDataCacheKey::new(grid.format, resource_name);
        let data = cached_grid_data(embedded_grid_data_cache(), key, || {
            parse_cached_grid_data(grid.format, &grid.name, bytes)
        })?;

        Ok(Some(GridHandle {
            definition: grid.clone(),
            data,
        }))
    }
}

pub struct FilesystemGridProvider {
    roots: Mutex<Vec<FilesystemGridRoot>>,
    location_cache: Mutex<HashMap<String, FilesystemGridLocation>>,
    data_cache: GridDataCache,
    #[cfg(test)]
    locate_searches: std::sync::atomic::AtomicUsize,
}

enum FilesystemGridRoot {
    Canonical(PathBuf),
    // Retain roots that do not exist yet so callers can construct a provider
    // before mounting or creating the grid directory.
    Unresolved(PathBuf),
}

#[derive(Clone)]
struct FilesystemGridLocation {
    root: PathBuf,
    path: PathBuf,
}

impl FilesystemGridProvider {
    pub fn new<I>(roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        Self {
            roots: Mutex::new(
                roots
                    .into_iter()
                    .map(|root| match root.canonicalize() {
                        Ok(canonical_root) => FilesystemGridRoot::Canonical(canonical_root),
                        Err(_) => FilesystemGridRoot::Unresolved(root),
                    })
                    .collect(),
            ),
            location_cache: Mutex::new(HashMap::new()),
            data_cache: Mutex::new(HashMap::new()),
            #[cfg(test)]
            locate_searches: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn locate(&self, grid: &GridDefinition) -> Option<FilesystemGridLocation> {
        let cache_key = grid_runtime_cache_key(grid);
        let cached_location = {
            self.location_cache
                .lock()
                .expect("filesystem grid location cache poisoned")
                .get(&cache_key)
                .cloned()
        };
        if let Some(location) = cached_location {
            if let Some(validated) = self.revalidate_location(&location) {
                if validated.path != location.path {
                    self.location_cache
                        .lock()
                        .expect("filesystem grid location cache poisoned")
                        .insert(cache_key, validated.clone());
                }
                return Some(validated);
            }

            self.location_cache
                .lock()
                .expect("filesystem grid location cache poisoned")
                .remove(&cache_key);
        }

        let location = self.locate_uncached(grid)?;
        self.location_cache
            .lock()
            .expect("filesystem grid location cache poisoned")
            .insert(cache_key, location.clone());
        Some(location)
    }

    fn locate_uncached(&self, grid: &GridDefinition) -> Option<FilesystemGridLocation> {
        #[cfg(test)]
        self.locate_searches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let safe_resource_names = grid
            .resource_names
            .iter()
            .filter(|name| is_safe_grid_resource_name(name))
            .collect::<Vec<_>>();
        if safe_resource_names.is_empty() {
            return None;
        }

        for root in self.canonical_roots_for_lookup() {
            for name in &safe_resource_names {
                let candidate = root.join(name);
                let Ok(canonical_candidate) = candidate.canonicalize() else {
                    continue;
                };
                if canonical_candidate.starts_with(&root) && canonical_candidate.is_file() {
                    return Some(FilesystemGridLocation {
                        root,
                        path: canonical_candidate,
                    });
                }
            }
        }
        None
    }

    fn revalidate_location(
        &self,
        location: &FilesystemGridLocation,
    ) -> Option<FilesystemGridLocation> {
        let Ok(canonical_path) = location.path.canonicalize() else {
            return None;
        };
        if !canonical_path.starts_with(&location.root) || !canonical_path.is_file() {
            return None;
        }
        Some(FilesystemGridLocation {
            root: location.root.clone(),
            path: canonical_path,
        })
    }

    fn canonical_roots_for_lookup(&self) -> Vec<PathBuf> {
        let mut roots = self.roots.lock().expect("filesystem grid roots poisoned");
        let mut canonical_roots = Vec::with_capacity(roots.len());
        for root in roots.iter_mut() {
            match root {
                FilesystemGridRoot::Canonical(canonical_root) => {
                    canonical_roots.push(canonical_root.clone());
                }
                FilesystemGridRoot::Unresolved(unresolved_root) => {
                    let Ok(canonical_root) = unresolved_root.canonicalize() else {
                        continue;
                    };
                    *root = FilesystemGridRoot::Canonical(canonical_root.clone());
                    canonical_roots.push(canonical_root);
                }
            }
        }
        canonical_roots
    }
}

impl GridProvider for FilesystemGridProvider {
    fn definition(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<Option<GridDefinition>, GridError> {
        if self.locate(grid).is_some() {
            return Ok(Some(grid.clone()));
        }
        Ok(None)
    }

    fn load(&self, grid: &GridDefinition) -> std::result::Result<Option<GridHandle>, GridError> {
        let Some(location) = self.locate(grid) else {
            return Ok(None);
        };

        let key = GridDataCacheKey::new(grid.format, location.path.to_string_lossy());
        let data = cached_grid_data(&self.data_cache, key, || {
            let bytes = read_filesystem_grid_resource_bytes(&location, grid.format)?;
            parse_cached_grid_data(grid.format, &grid.name, &bytes)
        })?;

        Ok(Some(GridHandle {
            definition: grid.clone(),
            data,
        }))
    }
}

fn is_safe_grid_resource_name(name: &str) -> bool {
    let path = Path::new(name);
    if path.as_os_str().is_empty() {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn read_filesystem_grid_resource_bytes(
    location: &FilesystemGridLocation,
    format: GridFormat,
) -> std::result::Result<Vec<u8>, GridError> {
    let canonical_path = location
        .path
        .canonicalize()
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", location.path.display())))?;
    if canonical_path != location.path || !canonical_path.starts_with(&location.root) {
        return Err(GridError::Unavailable(format!(
            "{} is no longer contained by {}",
            location.path.display(),
            location.root.display()
        )));
    }

    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", canonical_path.display())))?;
    if !metadata.is_file() {
        return Err(GridError::Unavailable(format!(
            "{} is not a regular file",
            canonical_path.display()
        )));
    }

    let file = open_filesystem_grid_resource_file(location, &canonical_path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", canonical_path.display())))?;
    ensure_same_grid_resource_file(&canonical_path, &metadata, &opened_metadata)?;

    read_grid_resource_file(file, &canonical_path, format)
}

#[cfg(unix)]
fn open_filesystem_grid_resource_file(
    location: &FilesystemGridLocation,
    canonical_path: &Path,
) -> std::result::Result<std::fs::File, GridError> {
    use rustix::fs::{open, openat, Mode, OFlags};

    let relative_path = canonical_path.strip_prefix(&location.root).map_err(|_| {
        GridError::Unavailable(format!(
            "{} is no longer contained by {}",
            canonical_path.display(),
            location.root.display()
        ))
    })?;

    let mut components = relative_path.components().peekable();
    let Some(_) = components.peek() else {
        return Err(GridError::Unavailable(format!(
            "{} is not a grid file path",
            canonical_path.display()
        )));
    };

    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let empty_mode = Mode::empty();
    let mut dir = open(&location.root, directory_flags, empty_mode)
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", location.root.display())))?;

    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(GridError::Unavailable(format!(
                "{} is not a normal relative grid path",
                canonical_path.display()
            )));
        };

        if components.peek().is_some() {
            dir = openat(&dir, name, directory_flags, empty_mode).map_err(|err| {
                GridError::Unavailable(format!("{}: {err}", canonical_path.display()))
            })?;
        } else {
            let file = openat(&dir, name, file_flags, empty_mode).map_err(|err| {
                GridError::Unavailable(format!("{}: {err}", canonical_path.display()))
            })?;
            return Ok(std::fs::File::from(file));
        }
    }

    Err(GridError::Unavailable(format!(
        "{} is not a grid file path",
        canonical_path.display()
    )))
}

#[cfg(not(unix))]
fn open_filesystem_grid_resource_file(
    _location: &FilesystemGridLocation,
    canonical_path: &Path,
) -> std::result::Result<std::fs::File, GridError> {
    std::fs::File::open(canonical_path)
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", canonical_path.display())))
}

fn read_grid_resource_file(
    mut file: std::fs::File,
    path: &Path,
    format: GridFormat,
) -> std::result::Result<Vec<u8>, GridError> {
    if let Some(max_bytes) = max_grid_resource_bytes(format) {
        return read_bounded_grid_resource_file(file, path, format, max_bytes);
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", path.display())))?;
    Ok(bytes)
}

#[cfg(test)]
fn read_bounded_grid_resource_bytes(
    path: &Path,
    format: GridFormat,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, GridError> {
    let file = std::fs::File::open(path)
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", path.display())))?;
    read_bounded_grid_resource_file(file, path, format, max_bytes)
}

fn read_bounded_grid_resource_file(
    file: std::fs::File,
    path: &Path,
    format: GridFormat,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, GridError> {
    let read_limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut reader = file.take(read_limit);
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    reader
        .read_to_end(&mut bytes)
        .map_err(|err| GridError::Unavailable(format!("{}: {err}", path.display())))?;

    if bytes.len() > max_bytes {
        return Err(GridError::Parse(format!(
            "{} exceeds maximum {format:?} grid size of {max_bytes} bytes",
            path.display()
        )));
    }

    Ok(bytes)
}

#[cfg(unix)]
fn ensure_same_grid_resource_file(
    path: &Path,
    expected: &std::fs::Metadata,
    opened: &std::fs::Metadata,
) -> std::result::Result<(), GridError> {
    use std::os::unix::fs::MetadataExt;

    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err(GridError::Unavailable(format!(
            "{} changed while opening",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_same_grid_resource_file(
    _path: &Path,
    _expected: &std::fs::Metadata,
    _opened: &std::fs::Metadata,
) -> std::result::Result<(), GridError> {
    Ok(())
}

fn validate_grid_resource_size(
    resource: impl std::fmt::Display,
    format: GridFormat,
    len: u64,
) -> std::result::Result<(), GridError> {
    if let Some(max_bytes) = max_grid_resource_bytes(format) {
        let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        if len > max_bytes_u64 {
            return Err(GridError::Parse(format!(
                "{resource} exceeds maximum {format:?} grid size of {max_bytes} bytes"
            )));
        }
    }
    Ok(())
}

fn max_grid_resource_bytes(format: GridFormat) -> Option<usize> {
    match format {
        GridFormat::Ntv2 => Some(MAX_NTV2_GRID_BYTES),
        GridFormat::Gtx => Some(MAX_GTX_GRID_BYTES),
        GridFormat::GeoTiff => Some(MAX_GEOTIFF_GRID_BYTES),
        GridFormat::Unsupported => None,
    }
}

enum GridData {
    Ntv2(Ntv2GridSet),
    Gtx(GtxGrid),
}

struct CachedGridData {
    data: GridData,
    checksum: String,
}

type GridDataCache = Mutex<HashMap<GridDataCacheKey, Arc<GridDataCacheSlot>>>;

struct GridDataCacheSlot {
    state: Mutex<GridDataCacheState>,
    ready: Condvar,
}

enum GridDataCacheState {
    Loading,
    Ready(Arc<CachedGridData>),
    Failed(GridError),
}

impl GridDataCacheSlot {
    fn loading() -> Self {
        Self {
            state: Mutex::new(GridDataCacheState::Loading),
            ready: Condvar::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GridDataCacheKey {
    format: GridFormat,
    resource: String,
}

impl GridDataCacheKey {
    fn new(format: GridFormat, resource: impl AsRef<str>) -> Self {
        Self {
            format,
            resource: resource.as_ref().to_string(),
        }
    }
}

fn embedded_grid_data_cache() -> &'static GridDataCache {
    static CACHE: OnceLock<GridDataCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_grid_data(
    cache: &GridDataCache,
    key: GridDataCacheKey,
    parse: impl FnOnce() -> std::result::Result<CachedGridData, GridError>,
) -> std::result::Result<Arc<CachedGridData>, GridError> {
    let (slot, should_load) = {
        let mut cache = cache.lock().expect("grid data cache poisoned");
        if let Some(slot) = cache.get(&key) {
            (Arc::clone(slot), false)
        } else {
            let slot = Arc::new(GridDataCacheSlot::loading());
            cache.insert(key.clone(), Arc::clone(&slot));
            (slot, true)
        }
    };

    if should_load {
        let result = parse().map(Arc::new);
        if result.is_err() {
            let mut cache = cache.lock().expect("grid data cache poisoned");
            let should_remove = cache
                .get(&key)
                .map(|cached_slot| Arc::ptr_eq(cached_slot, &slot))
                .unwrap_or(false);
            if should_remove {
                cache.remove(&key);
            }
        }

        let mut state = slot.state.lock().expect("grid data cache slot poisoned");
        match &result {
            Ok(data) => *state = GridDataCacheState::Ready(Arc::clone(data)),
            Err(error) => *state = GridDataCacheState::Failed(error.clone()),
        }
        slot.ready.notify_all();
        return result;
    }

    let mut state = slot.state.lock().expect("grid data cache slot poisoned");
    loop {
        match &*state {
            GridDataCacheState::Ready(data) => return Ok(Arc::clone(data)),
            GridDataCacheState::Failed(error) => return Err(error.clone()),
            GridDataCacheState::Loading => {
                state = slot
                    .ready
                    .wait(state)
                    .expect("grid data cache slot poisoned");
            }
        }
    }
}

fn parse_grid_data(
    format: GridFormat,
    name: &str,
    bytes: &[u8],
) -> std::result::Result<GridData, GridError> {
    validate_grid_resource_size(name, format, u64::try_from(bytes.len()).unwrap_or(u64::MAX))?;

    match format {
        GridFormat::Ntv2 => Ok(GridData::Ntv2(Ntv2GridSet::parse(bytes)?)),
        GridFormat::Gtx => Ok(GridData::Gtx(GtxGrid::parse(bytes)?)),
        GridFormat::GeoTiff => parse_geotiff_grid_data(name, bytes),
        GridFormat::Unsupported => Err(GridError::UnsupportedFormat(name.into())),
    }
}

#[cfg(not(feature = "geotiff"))]
fn parse_geotiff_grid_data(name: &str, _bytes: &[u8]) -> std::result::Result<GridData, GridError> {
    Err(GridError::UnsupportedFormat(format!(
        "{name}: GeoTIFF grid support requires the `geotiff` crate feature"
    )))
}

#[cfg(feature = "geotiff")]
fn parse_geotiff_grid_data(name: &str, bytes: &[u8]) -> std::result::Result<GridData, GridError> {
    geotiff::parse(name, bytes)
}

/// Decode PROJ-format GeoTIFF grids into the same internal representation as the
/// binary NTv2 (`Ntv2GridSet`) and GTX (`GtxGrid`) formats, so all sampling,
/// bilinear interpolation, nested-grid selection, and inverse iteration is
/// shared with those code paths.
///
/// PROJ stores its grids as cloud-optimized GeoTIFFs: a horizontal datum-shift
/// grid carries `latitude_offset`/`longitude_offset` bands in arc-seconds (with
/// nested finer subgrids as additional IFDs), and a geoid grid carries a single
/// `geoid_undulation` band in metres. The grid role is taken from the `TYPE`
/// item of the `GDAL_METADATA` tag.
#[cfg(feature = "geotiff")]
mod geotiff {
    use super::{
        GridData, GridError, GridExtent, GtxGrid, Ntv2Grid, Ntv2GridSet, MAX_GEOTIFF_BANDS,
        MAX_GEOTIFF_CELLS_PER_IMAGE, MAX_GEOTIFF_IFDS, MAX_GEOTIFF_TOTAL_CELLS,
    };
    use geotiff_reader::{GeoTiffFile, GeoTiffOpenOptions};
    use std::f64::consts::PI;
    use tiff_core::TagValue;

    const TIFFTAG_MODEL_PIXEL_SCALE: u16 = 33550;
    const TIFFTAG_MODEL_TIEPOINT: u16 = 33922;
    const TIFFTAG_GDAL_METADATA: u16 = 42112;
    const ARCSEC_TO_RAD: f64 = PI / 180.0 / 3600.0;
    const DEG_TO_RAD: f64 = PI / 180.0;

    #[derive(Clone, Copy)]
    enum Kind {
        Horizontal,
        Vertical,
    }

    struct ImageMetadata {
        west_node_deg: f64,
        north_node_deg: f64,
        scale_lon_deg: f64,
        scale_lat_deg: f64,
        width: usize,
        height: usize,
        cell_count: usize,
    }

    /// One decoded image (IFD): node-origin georeferencing plus per-band values
    /// laid out row-major, north-to-south (TIFF raster order).
    struct Image {
        west_node_deg: f64,
        north_node_deg: f64,
        scale_lon_deg: f64,
        scale_lat_deg: f64,
        width: usize,
        height: usize,
        bands: Vec<Vec<f64>>,
    }

    pub(super) fn parse(name: &str, bytes: &[u8]) -> Result<GridData, GridError> {
        let mut options = GeoTiffOpenOptions::default();
        options.parse_budgets.max_ifds = MAX_GEOTIFF_IFDS;
        let file = GeoTiffFile::from_bytes_with_options(bytes.to_vec(), options)
            .map_err(|err| GridError::Parse(format!("{name}: {err}")))?;
        let tiff = file.tiff();
        let ifd_count = tiff.ifd_count();
        if ifd_count == 0 {
            return Err(GridError::Parse(format!("{name}: no images in GeoTIFF")));
        }
        if ifd_count > MAX_GEOTIFF_IFDS {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF IFD count {ifd_count} exceeds limit {MAX_GEOTIFF_IFDS}"
            )));
        }

        let base_index = file.base_ifd_index();
        let base_ifd = tiff
            .ifd(base_index)
            .map_err(|err| GridError::Parse(format!("{name}: {err}")))?;
        let kind = grid_kind(
            base_ifd.tag(TIFFTAG_GDAL_METADATA).map(|tag| &tag.value),
            name,
        )?;

        match kind {
            Kind::Vertical => {
                let metadata = read_image_metadata(&file, base_index, kind, name)?;
                let image = read_image(&file, base_index, kind, &metadata, name)?;
                Ok(GridData::Gtx(build_gtx(&image)))
            }
            Kind::Horizontal => {
                let mut metadata = Vec::with_capacity(ifd_count);
                let mut total_cells = 0usize;
                for index in 0..ifd_count {
                    let image_metadata = read_image_metadata(&file, index, kind, name)?;
                    total_cells = total_cells
                        .checked_add(image_metadata.cell_count)
                        .ok_or_else(|| {
                            GridError::Parse(format!("{name}: GeoTIFF total cell count overflow"))
                        })?;
                    if total_cells > MAX_GEOTIFF_TOTAL_CELLS {
                        return Err(GridError::Parse(format!(
                            "{name}: GeoTIFF total cell count {total_cells} exceeds limit {MAX_GEOTIFF_TOTAL_CELLS}"
                        )));
                    }
                    metadata.push(image_metadata);
                }

                let mut images = Vec::with_capacity(metadata.len());
                for (index, image_metadata) in metadata.iter().enumerate() {
                    images.push(read_image(&file, index, kind, image_metadata, name)?);
                }
                Ok(GridData::Ntv2(build_ntv2(&images, name)?))
            }
        }
    }

    fn grid_kind(metadata: Option<&TagValue>, name: &str) -> Result<Kind, GridError> {
        let text = match metadata {
            Some(TagValue::Ascii(text)) => text.to_ascii_uppercase(),
            _ => String::new(),
        };
        if text.contains("VERTICAL") {
            Ok(Kind::Vertical)
        } else if text.contains("HORIZONTAL") {
            Ok(Kind::Horizontal)
        } else {
            Err(GridError::Parse(format!(
                "{name}: GeoTIFF grid is missing a recognised GDAL `TYPE` (HORIZONTAL_OFFSET / VERTICAL_OFFSET)"
            )))
        }
    }

    fn read_image_metadata(
        file: &GeoTiffFile,
        index: usize,
        kind: Kind,
        name: &str,
    ) -> Result<ImageMetadata, GridError> {
        let tiff = file.tiff();
        let ifd = tiff
            .ifd(index)
            .map_err(|err| GridError::Parse(format!("{name}: {err}")))?;
        let width = ifd.width() as usize;
        let height = ifd.height() as usize;
        if width < 2 || height < 2 {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} is smaller than 2x2"
            )));
        }
        if width > MAX_GEOTIFF_CELLS_PER_IMAGE {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} width {width} exceeds limit {MAX_GEOTIFF_CELLS_PER_IMAGE}"
            )));
        }
        if height > MAX_GEOTIFF_CELLS_PER_IMAGE {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} height {height} exceeds limit {MAX_GEOTIFF_CELLS_PER_IMAGE}"
            )));
        }
        let cell_count = width.checked_mul(height).ok_or_else(|| {
            GridError::Parse(format!("{name}: GeoTIFF image {index} cell count overflow"))
        })?;
        if cell_count > MAX_GEOTIFF_CELLS_PER_IMAGE {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} cell count {cell_count} exceeds limit {MAX_GEOTIFF_CELLS_PER_IMAGE}"
            )));
        }

        let scale = doubles(ifd.tag(TIFFTAG_MODEL_PIXEL_SCALE).map(|tag| &tag.value))
            .ok_or_else(|| GridError::Parse(format!("{name}: missing ModelPixelScale")))?;
        let tiepoint = doubles(ifd.tag(TIFFTAG_MODEL_TIEPOINT).map(|tag| &tag.value))
            .ok_or_else(|| GridError::Parse(format!("{name}: missing ModelTiepoint")))?;
        if scale.len() < 2 || tiepoint.len() < 6 {
            return Err(GridError::Parse(format!(
                "{name}: malformed GeoTIFF georeferencing tags"
            )));
        }
        let scale_lon_deg = scale[0];
        let scale_lat_deg = scale[1];
        // Tiepoint maps raster (i, j) -> model (x, y). PROJ grids use a Point
        // raster, so the (0, 0) tiepoint is the node coordinate directly.
        let west_node_deg = tiepoint[3] - tiepoint[0] * scale_lon_deg;
        let north_node_deg = tiepoint[4] + tiepoint[1] * scale_lat_deg;
        if !(scale_lon_deg.is_finite()
            && scale_lat_deg.is_finite()
            && scale_lon_deg > 0.0
            && scale_lat_deg > 0.0
            && west_node_deg.is_finite()
            && north_node_deg.is_finite())
        {
            return Err(GridError::Parse(format!(
                "{name}: invalid GeoTIFF georeferencing"
            )));
        }

        let band_count = ifd.samples_per_pixel() as usize;
        if band_count == 0 {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} has no bands"
            )));
        }
        if band_count > MAX_GEOTIFF_BANDS {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} band count {band_count} exceeds limit {MAX_GEOTIFF_BANDS}"
            )));
        }
        let required_bands = required_band_count(kind);
        if band_count < required_bands {
            return Err(GridError::Parse(format!(
                "{name}: GeoTIFF image {index} has {band_count} bands, needs at least {required_bands}"
            )));
        }

        Ok(ImageMetadata {
            west_node_deg,
            north_node_deg,
            scale_lon_deg,
            scale_lat_deg,
            width,
            height,
            cell_count,
        })
    }

    fn read_image(
        file: &GeoTiffFile,
        index: usize,
        kind: Kind,
        metadata: &ImageMetadata,
        name: &str,
    ) -> Result<Image, GridError> {
        let tiff = file.tiff();
        let ifd = tiff
            .ifd(index)
            .map_err(|err| GridError::Parse(format!("{name}: {err}")))?;
        let required_bands = required_band_count(kind);
        let mut bands = Vec::with_capacity(required_bands);
        // Horizontal grids need bands 0 (latitude) and 1 (longitude); vertical
        // grids need band 0. Accuracy bands, if present, are ignored.
        for band_index in 0..required_bands {
            let array = tiff
                .read_band_from_ifd::<f32>(ifd, band_index)
                .map_err(|err| GridError::Parse(format!("{name}: band {band_index}: {err}")))?;
            let values: Vec<f64> = array.iter().map(|&value| value as f64).collect();
            if values.len() != metadata.cell_count {
                return Err(GridError::Parse(format!(
                    "{name}: band {band_index} has {} samples, expected {}",
                    values.len(),
                    metadata.cell_count
                )));
            }
            bands.push(values);
        }

        Ok(Image {
            west_node_deg: metadata.west_node_deg,
            north_node_deg: metadata.north_node_deg,
            scale_lon_deg: metadata.scale_lon_deg,
            scale_lat_deg: metadata.scale_lat_deg,
            width: metadata.width,
            height: metadata.height,
            bands,
        })
    }

    fn required_band_count(kind: Kind) -> usize {
        match kind {
            Kind::Horizontal => 2,
            Kind::Vertical => 1,
        }
    }

    /// Sample value at output node (x from west, y from south), flipping the
    /// row-major north-to-south raster into the south-to-north node order used
    /// by `Ntv2Grid`/`GtxGrid`.
    fn at(image: &Image, band: usize, x: usize, y: usize) -> f64 {
        let row = image.height - 1 - y;
        image.bands[band][row * image.width + x]
    }

    fn build_gtx(image: &Image) -> GtxGrid {
        let width = image.width;
        let height = image.height;
        let mut offsets_meters = vec![0.0f64; width * height];
        for y in 0..height {
            for x in 0..width {
                offsets_meters[y * width + x] = at(image, 0, x, y);
            }
        }
        let west_degrees = image.west_node_deg;
        let south_degrees = image.north_node_deg - image.scale_lat_deg * (height - 1) as f64;
        GtxGrid {
            west_degrees,
            south_degrees,
            east_degrees: west_degrees + image.scale_lon_deg * (width - 1) as f64,
            north_degrees: image.north_node_deg,
            delta_lon_degrees: image.scale_lon_deg,
            delta_lat_degrees: image.scale_lat_deg,
            width,
            height,
            offsets_meters,
        }
    }

    fn build_ntv2(images: &[Image], name: &str) -> Result<Ntv2GridSet, GridError> {
        let mut grids = Vec::with_capacity(images.len());
        for image in images {
            if image.bands.len() < 2 {
                return Err(GridError::Parse(format!(
                    "{name}: horizontal GeoTIFF grid needs latitude and longitude offset bands"
                )));
            }
            let width = image.width;
            let height = image.height;
            let mut lat_shift = vec![0.0f64; width * height];
            let mut lon_shift = vec![0.0f64; width * height];
            for y in 0..height {
                for x in 0..width {
                    let dest = y * width + x;
                    // Band 0: latitude offset (arc-sec, +north).
                    // Band 1: longitude offset (arc-sec, +east).
                    lat_shift[dest] = at(image, 0, x, y) * ARCSEC_TO_RAD;
                    lon_shift[dest] = at(image, 1, x, y) * ARCSEC_TO_RAD;
                }
            }
            let west = image.west_node_deg * DEG_TO_RAD;
            let north = image.north_node_deg * DEG_TO_RAD;
            let res_x = image.scale_lon_deg * DEG_TO_RAD;
            let res_y = image.scale_lat_deg * DEG_TO_RAD;
            let extent = GridExtent {
                west,
                south: north - res_y * (height - 1) as f64,
                east: west + res_x * (width - 1) as f64,
                north,
                res_x,
                res_y,
            };
            grids.push(Ntv2Grid {
                name: name.into(),
                extent,
                width,
                height,
                lat_shift,
                lon_shift,
                children: Vec::new(),
            });
        }

        // Establish nesting: a grid's parent is the smallest other grid that
        // fully contains it. Grids without a parent are roots. PROJ stores
        // coarse parent grids before their finer nested children.
        let mut roots = Vec::new();
        for child in 0..grids.len() {
            let mut parent: Option<usize> = None;
            for candidate in 0..grids.len() {
                if candidate == child || !extent_contains(&grids[candidate], &grids[child]) {
                    continue;
                }
                match parent {
                    Some(current) if !extent_contains(&grids[current], &grids[candidate]) => {}
                    _ => parent = Some(candidate),
                }
            }
            match parent {
                Some(parent_index) => grids[parent_index].children.push(child),
                None => roots.push(child),
            }
        }
        if roots.is_empty() {
            return Err(GridError::Parse(format!(
                "{name}: horizontal GeoTIFF grid has no root subgrid"
            )));
        }

        Ok(Ntv2GridSet { grids, roots })
    }

    fn extent_contains(outer: &Ntv2Grid, inner: &Ntv2Grid) -> bool {
        let tol = (outer.extent.res_x + outer.extent.res_y) * 1e-9;
        outer.extent.west <= inner.extent.west + tol
            && outer.extent.east >= inner.extent.east - tol
            && outer.extent.south <= inner.extent.south + tol
            && outer.extent.north >= inner.extent.north - tol
            // A strictly larger cell is a coarser (parent) grid.
            && outer.extent.res_x > inner.extent.res_x * (1.0 + 1e-9)
    }

    fn doubles(value: Option<&TagValue>) -> Option<Vec<f64>> {
        match value? {
            TagValue::Double(values) => Some(values.clone()),
            TagValue::Float(values) => Some(values.iter().map(|&v| v as f64).collect()),
            _ => None,
        }
    }
}

fn parse_cached_grid_data(
    format: GridFormat,
    name: &str,
    bytes: &[u8],
) -> std::result::Result<CachedGridData, GridError> {
    Ok(CachedGridData {
        data: parse_grid_data(format, name, bytes)?,
        checksum: sha256_hex(bytes),
    })
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

fn embedded_grid_resource(names: &[String]) -> Option<(&'static str, &'static [u8])> {
    for name in names {
        if name.eq_ignore_ascii_case("ntv2_0.gsb") {
            return Some(("ntv2_0.gsb", include_bytes!("../data/grids/ntv2_0.gsb")));
        }
    }
    None
}

#[derive(Clone)]
struct Ntv2GridSet {
    grids: Vec<Ntv2Grid>,
    roots: Vec<usize>,
}

impl Ntv2GridSet {
    fn parse(bytes: &[u8]) -> std::result::Result<Self, GridError> {
        if bytes.len() < NTV2_HEADER_LEN {
            return Err(GridError::Parse("NTv2 file too small".into()));
        }
        if bytes.len() > MAX_NTV2_GRID_BYTES {
            return Err(GridError::Parse(format!(
                "NTv2 grid exceeds maximum size of {MAX_NTV2_GRID_BYTES} bytes"
            )));
        }

        let endian = if u32::from_le_bytes(bytes[8..12].try_into().expect("slice length checked"))
            == 11
        {
            Endian::Little
        } else if u32::from_be_bytes(bytes[8..12].try_into().expect("slice length checked")) == 11 {
            Endian::Big
        } else {
            return Err(GridError::Parse(
                "invalid NTv2 header endianness marker".into(),
            ));
        };

        if &bytes[56..63] != b"SECONDS" {
            return Err(GridError::Parse(
                "only NTv2 GS_TYPE=SECONDS is supported".into(),
            ));
        }

        let num_subfiles = read_u32(bytes, 40, endian)? as usize;
        if num_subfiles == 0 || num_subfiles > MAX_NTV2_SUBFILES {
            return Err(GridError::Parse(format!(
                "NTv2 subfile count {num_subfiles} exceeds limit {MAX_NTV2_SUBFILES}"
            )));
        }

        let mut offset = NTV2_HEADER_LEN;
        let mut grids = Vec::with_capacity(num_subfiles);
        let mut name_to_index = HashMap::new();
        let mut parent_links: Vec<Option<String>> = Vec::with_capacity(num_subfiles);
        let mut total_cells = 0usize;
        let mut total_data_bytes = 0usize;

        for _ in 0..num_subfiles {
            let header_end = offset
                .checked_add(NTV2_HEADER_LEN)
                .ok_or_else(|| GridError::Parse("NTv2 header offset overflow".into()))?;
            let header = bytes
                .get(offset..header_end)
                .ok_or_else(|| GridError::Parse("truncated NTv2 subfile header".into()))?;
            if &header[0..8] != b"SUB_NAME" {
                return Err(GridError::Parse("invalid NTv2 subfile header tag".into()));
            }

            let name = parse_label(&header[8..16]);
            let parent = parse_label(&header[24..32]);
            let south = read_f64(header, 72, endian)? * PI / 180.0 / 3600.0;
            let north = read_f64(header, 88, endian)? * PI / 180.0 / 3600.0;
            let east = -read_f64(header, 104, endian)? * PI / 180.0 / 3600.0;
            let west = -read_f64(header, 120, endian)? * PI / 180.0 / 3600.0;
            let res_y = read_f64(header, 136, endian)? * PI / 180.0 / 3600.0;
            let res_x = read_f64(header, 152, endian)? * PI / 180.0 / 3600.0;
            let gs_count = read_u32(header, 168, endian)? as usize;

            if !(west.is_finite()
                && east.is_finite()
                && south.is_finite()
                && north.is_finite()
                && res_x.is_finite()
                && res_y.is_finite()
                && west < east
                && south < north
                && res_x > 0.0
                && res_y > 0.0)
            {
                return Err(GridError::Parse(format!(
                    "invalid NTv2 georeferencing for subgrid {name}"
                )));
            }

            let width = ntv2_axis_cell_count(east - west, res_x, "longitude", &name)?;
            let height = ntv2_axis_cell_count(north - south, res_y, "latitude", &name)?;
            let derived_cells = width
                .checked_mul(height)
                .ok_or_else(|| GridError::Parse("NTv2 cell count overflow".into()))?;
            if derived_cells > MAX_NTV2_CELLS_PER_SUBGRID {
                return Err(GridError::Parse(format!(
                    "NTv2 subgrid {name} has {derived_cells} cells, exceeding limit {MAX_NTV2_CELLS_PER_SUBGRID}"
                )));
            }
            if derived_cells != gs_count {
                return Err(GridError::Parse(format!(
                    "NTv2 subgrid {name} cell count mismatch: expected {} got {gs_count}",
                    derived_cells
                )));
            }

            total_cells = total_cells
                .checked_add(gs_count)
                .ok_or_else(|| GridError::Parse("NTv2 total cell count overflow".into()))?;
            if total_cells > MAX_NTV2_TOTAL_CELLS {
                return Err(GridError::Parse(format!(
                    "NTv2 total cell count {total_cells} exceeds limit {MAX_NTV2_TOTAL_CELLS}"
                )));
            }

            let data_len = gs_count
                .checked_mul(NTV2_RECORD_LEN)
                .ok_or_else(|| GridError::Parse("NTv2 data size overflow".into()))?;
            total_data_bytes = total_data_bytes
                .checked_add(data_len)
                .ok_or_else(|| GridError::Parse("NTv2 total data size overflow".into()))?;
            if total_data_bytes > MAX_NTV2_TOTAL_DATA_BYTES {
                return Err(GridError::Parse(format!(
                    "NTv2 data size {total_data_bytes} exceeds limit {MAX_NTV2_TOTAL_DATA_BYTES}"
                )));
            }
            let data_end = header_end
                .checked_add(data_len)
                .ok_or_else(|| GridError::Parse("NTv2 data offset overflow".into()))?;
            let data = bytes.get(header_end..data_end).ok_or_else(|| {
                GridError::Parse(format!("truncated NTv2 data for subgrid {name}"))
            })?;

            let mut lat_shift = vec![0.0f64; gs_count];
            let mut lon_shift = vec![0.0f64; gs_count];
            for y in 0..height {
                for x in 0..width {
                    let source_x = width - 1 - x;
                    let record_offset = (y * width + source_x) * NTV2_RECORD_LEN;
                    let lat = read_f32(data, record_offset, endian)? as f64 * PI / 180.0 / 3600.0;
                    let lon =
                        -(read_f32(data, record_offset + 4, endian)? as f64) * PI / 180.0 / 3600.0;
                    if !(lat.is_finite() && lon.is_finite()) {
                        return Err(GridError::Parse(format!(
                            "non-finite NTv2 shift value in subgrid {name}"
                        )));
                    }
                    let dest = y * width + x;
                    lat_shift[dest] = lat;
                    lon_shift[dest] = lon;
                }
            }

            let index = grids.len();
            name_to_index.insert(name.clone(), index);
            parent_links.push(
                if parent.eq_ignore_ascii_case("none") || parent.is_empty() {
                    None
                } else {
                    Some(parent)
                },
            );
            grids.push(Ntv2Grid {
                name,
                extent: GridExtent {
                    west,
                    south,
                    east,
                    north,
                    res_x,
                    res_y,
                },
                width,
                height,
                lat_shift,
                lon_shift,
                children: Vec::new(),
            });
            offset = data_end;
        }

        let mut roots = Vec::new();
        for (idx, parent) in parent_links.into_iter().enumerate() {
            if let Some(parent_name) = parent {
                let Some(parent_idx) = name_to_index.get(&parent_name).copied() else {
                    return Err(GridError::Parse(format!(
                        "missing NTv2 parent subgrid {parent_name} for {}",
                        grids[idx].name
                    )));
                };
                grids[parent_idx].children.push(idx);
            } else {
                roots.push(idx);
            }
        }

        Ok(Self { grids, roots })
    }

    fn sample(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<GridSample, GridError> {
        let (grid_idx, local_lon, local_lat) = self.grid_at(lon_radians, lat_radians)?;
        let (lon_shift, lat_shift) = interpolate(&self.grids[grid_idx], local_lon, local_lat)?;
        Ok(GridSample {
            lon_shift_radians: lon_shift,
            lat_shift_radians: lat_shift,
        })
    }

    fn apply(
        &self,
        lon_radians: f64,
        lat_radians: f64,
        direction: GridShiftDirection,
    ) -> std::result::Result<(f64, f64), GridError> {
        match direction {
            GridShiftDirection::Forward => {
                let shift = self.sample(lon_radians, lat_radians)?;
                Ok((
                    lon_radians + shift.lon_shift_radians,
                    lat_radians + shift.lat_shift_radians,
                ))
            }
            GridShiftDirection::Reverse => self.apply_inverse(lon_radians, lat_radians),
        }
    }

    fn apply_inverse(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<(f64, f64), GridError> {
        const MAX_ITERATIONS: usize = 10;
        const TOLERANCE: f64 = 1e-12;

        let mut estimate_lon = lon_radians;
        let mut estimate_lat = lat_radians;

        for _ in 0..MAX_ITERATIONS {
            let shift = self.sample(estimate_lon, estimate_lat)?;
            let next_lon = lon_radians - shift.lon_shift_radians;
            let next_lat = lat_radians - shift.lat_shift_radians;
            let diff_lon = next_lon - estimate_lon;
            let diff_lat = next_lat - estimate_lat;
            estimate_lon = next_lon;
            estimate_lat = next_lat;
            if diff_lon * diff_lon + diff_lat * diff_lat <= TOLERANCE * TOLERANCE {
                return Ok((estimate_lon, estimate_lat));
            }
        }

        // Matches C PROJ's nad_cvt, which fails the point after MAX_TRY
        // instead of returning a non-converged estimate. A wandering fixed
        // point means the pull-back left reliable coverage, so surface it as
        // a coverage error and let grid fallbacks handle it.
        Err(GridError::OutsideCoverage(format!(
            "NTv2 inverse shift did not converge at longitude {:.8} latitude {:.8}",
            lon_radians.to_degrees(),
            lat_radians.to_degrees()
        )))
    }

    fn grid_at(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<(usize, f64, f64), GridError> {
        for &root in &self.roots {
            if self.grids[root].extent.contains(lon_radians, lat_radians) {
                let idx = self.deepest_child(root, lon_radians, lat_radians);
                let extent = &self.grids[idx].extent;
                return Ok((idx, lon_radians - extent.west, lat_radians - extent.south));
            }
        }
        Err(GridError::OutsideCoverage(format!(
            "longitude {:.8} latitude {:.8}",
            lon_radians.to_degrees(),
            lat_radians.to_degrees()
        )))
    }

    fn deepest_child(&self, index: usize, lon_radians: f64, lat_radians: f64) -> usize {
        for &child in &self.grids[index].children {
            if self.grids[child].extent.contains(lon_radians, lat_radians) {
                return self.deepest_child(child, lon_radians, lat_radians);
            }
        }
        index
    }
}

fn ntv2_axis_cell_count(
    span: f64,
    resolution: f64,
    axis: &str,
    name: &str,
) -> std::result::Result<usize, GridError> {
    let intervals = span / resolution;
    if !intervals.is_finite() || intervals < 0.0 {
        return Err(GridError::Parse(format!(
            "invalid NTv2 {axis} spacing for subgrid {name}"
        )));
    }

    let rounded_intervals = (intervals + 0.5).floor();
    if !rounded_intervals.is_finite() || rounded_intervals > (MAX_NTV2_CELLS_PER_SUBGRID - 1) as f64
    {
        return Err(GridError::Parse(format!(
            "NTv2 subgrid {name} {axis} cell count exceeds limit {MAX_NTV2_CELLS_PER_SUBGRID}"
        )));
    }

    let count = rounded_intervals as usize + 1;
    if count < 2 {
        return Err(GridError::Parse(format!(
            "NTv2 subgrid {name} has fewer than two {axis} cells"
        )));
    }
    Ok(count)
}

#[derive(Clone)]
struct Ntv2Grid {
    name: String,
    extent: GridExtent,
    width: usize,
    height: usize,
    lat_shift: Vec<f64>,
    lon_shift: Vec<f64>,
    children: Vec<usize>,
}

#[derive(Clone, Copy)]
struct GridExtent {
    west: f64,
    south: f64,
    east: f64,
    north: f64,
    res_x: f64,
    res_y: f64,
}

impl GridExtent {
    fn contains(&self, lon_radians: f64, lat_radians: f64) -> bool {
        let epsilon = (self.res_x + self.res_y) * 1e-10;
        lon_radians >= self.west - epsilon
            && lon_radians <= self.east + epsilon
            && lat_radians >= self.south - epsilon
            && lat_radians <= self.north + epsilon
    }
}

fn interpolate(
    grid: &Ntv2Grid,
    local_lon: f64,
    local_lat: f64,
) -> std::result::Result<(f64, f64), GridError> {
    let lam = local_lon / grid.extent.res_x;
    let phi = local_lat / grid.extent.res_y;
    let mut x = lam.floor() as isize;
    let mut y = phi.floor() as isize;
    let mut fx = lam - x as f64;
    let mut fy = phi - y as f64;

    if x < 0 {
        if x == -1 && fx > 1.0 - 1e-9 {
            x = 0;
            fx = 0.0;
        } else {
            return Err(GridError::OutsideCoverage(grid.name.clone()));
        }
    }
    if y < 0 {
        if y == -1 && fy > 1.0 - 1e-9 {
            y = 0;
            fy = 0.0;
        } else {
            return Err(GridError::OutsideCoverage(grid.name.clone()));
        }
    }
    if x as usize + 1 >= grid.width {
        if x as usize + 1 == grid.width && fx < 1e-9 {
            x -= 1;
            fx = 1.0;
        } else {
            return Err(GridError::OutsideCoverage(grid.name.clone()));
        }
    }
    if y as usize + 1 >= grid.height {
        if y as usize + 1 == grid.height && fy < 1e-9 {
            y -= 1;
            fy = 1.0;
        } else {
            return Err(GridError::OutsideCoverage(grid.name.clone()));
        }
    }

    let idx = |xx: usize, yy: usize| yy * grid.width + xx;
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = x0 + 1;
    let y1 = y0 + 1;

    let m00 = (1.0 - fx) * (1.0 - fy);
    let m10 = fx * (1.0 - fy);
    let m01 = (1.0 - fx) * fy;
    let m11 = fx * fy;

    let lon = m00 * grid.lon_shift[idx(x0, y0)]
        + m10 * grid.lon_shift[idx(x1, y0)]
        + m01 * grid.lon_shift[idx(x0, y1)]
        + m11 * grid.lon_shift[idx(x1, y1)];
    let lat = m00 * grid.lat_shift[idx(x0, y0)]
        + m10 * grid.lat_shift[idx(x1, y0)]
        + m01 * grid.lat_shift[idx(x0, y1)]
        + m11 * grid.lat_shift[idx(x1, y1)];

    Ok((lon, lat))
}

#[derive(Clone)]
struct GtxGrid {
    west_degrees: f64,
    south_degrees: f64,
    east_degrees: f64,
    north_degrees: f64,
    delta_lon_degrees: f64,
    delta_lat_degrees: f64,
    width: usize,
    height: usize,
    offsets_meters: Vec<f64>,
}

impl GtxGrid {
    fn parse(bytes: &[u8]) -> std::result::Result<Self, GridError> {
        if bytes.len() < GTX_HEADER_LEN {
            return Err(GridError::Parse("GTX file too small".into()));
        }
        if bytes.len() > MAX_GTX_GRID_BYTES {
            return Err(GridError::Parse(format!(
                "GTX grid exceeds maximum size of {MAX_GTX_GRID_BYTES} bytes"
            )));
        }

        let south_degrees = read_f64(bytes, 0, Endian::Big)?;
        let west_degrees = read_f64(bytes, 8, Endian::Big)?;
        let delta_lat_degrees = read_f64(bytes, 16, Endian::Big)?;
        let delta_lon_degrees = read_f64(bytes, 24, Endian::Big)?;
        let height_i32 = read_i32(bytes, 32, Endian::Big)?;
        let width_i32 = read_i32(bytes, 36, Endian::Big)?;

        if !(west_degrees.is_finite()
            && south_degrees.is_finite()
            && delta_lon_degrees.is_finite()
            && delta_lat_degrees.is_finite()
            && delta_lon_degrees > 0.0
            && delta_lat_degrees > 0.0
            && width_i32 >= 2
            && height_i32 >= 2)
        {
            return Err(GridError::Parse("invalid GTX georeferencing".into()));
        }
        let height = height_i32 as usize;
        let width = width_i32 as usize;

        let count = width
            .checked_mul(height)
            .ok_or_else(|| GridError::Parse("GTX data size overflow".into()))?;
        if count > MAX_GTX_CELLS {
            return Err(GridError::Parse(format!(
                "GTX cell count {count} exceeds limit {MAX_GTX_CELLS}"
            )));
        }
        let data_len = count
            .checked_mul(GTX_RECORD_LEN)
            .ok_or_else(|| GridError::Parse("GTX data size overflow".into()))?;
        let expected_len = GTX_HEADER_LEN
            .checked_add(data_len)
            .ok_or_else(|| GridError::Parse("GTX data size overflow".into()))?;
        if expected_len > MAX_GTX_GRID_BYTES {
            return Err(GridError::Parse(format!(
                "GTX data size {expected_len} exceeds limit {MAX_GTX_GRID_BYTES}"
            )));
        }
        if bytes.len() < expected_len {
            return Err(GridError::Parse("truncated GTX data".into()));
        }

        let mut offsets_meters = Vec::with_capacity(count);
        for index in 0..count {
            let value =
                read_f32(bytes, GTX_HEADER_LEN + index * GTX_RECORD_LEN, Endian::Big)? as f64;
            if (value + 88.8888).abs() <= 1e-4 {
                offsets_meters.push(f64::NAN);
            } else {
                offsets_meters.push(value);
            }
        }

        let east_degrees = west_degrees + delta_lon_degrees * (width - 1) as f64;
        let north_degrees = south_degrees + delta_lat_degrees * (height - 1) as f64;

        Ok(Self {
            west_degrees,
            south_degrees,
            east_degrees,
            north_degrees,
            delta_lon_degrees,
            delta_lat_degrees,
            width,
            height,
            offsets_meters,
        })
    }

    fn sample(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<VerticalGridSample, GridError> {
        let raw_lon_degrees = lon_radians.to_degrees();
        let lat_degrees = lat_radians.to_degrees();

        if !(raw_lon_degrees.is_finite() && lat_degrees.is_finite()) {
            return Err(GridError::OutsideCoverage(format!(
                "non-finite longitude {:.8} latitude {:.8}",
                raw_lon_degrees, lat_degrees
            )));
        }

        let lon_degrees = self.normalize_lon_degrees(raw_lon_degrees);

        if !self.contains(lon_degrees, lat_degrees) {
            return Err(GridError::OutsideCoverage(format!(
                "longitude {:.8} latitude {:.8}",
                raw_lon_degrees, lat_degrees
            )));
        }

        let lam = (lon_degrees - self.west_degrees) / self.delta_lon_degrees;
        let phi = (lat_degrees - self.south_degrees) / self.delta_lat_degrees;
        let mut x = lam.floor() as isize;
        let mut y = phi.floor() as isize;
        let mut fx = lam - x as f64;
        let mut fy = phi - y as f64;

        if x < 0 {
            if x == -1 && fx > 1.0 - 1e-9 {
                x = 0;
                fx = 0.0;
            } else {
                return Err(GridError::OutsideCoverage("GTX negative grid index".into()));
            }
        }
        if y < 0 {
            if y == -1 && fy > 1.0 - 1e-9 {
                y = 0;
                fy = 0.0;
            } else {
                return Err(GridError::OutsideCoverage("GTX negative grid index".into()));
            }
        }
        if x as usize + 1 >= self.width {
            if x as usize + 1 == self.width && fx < 1e-9 {
                x -= 1;
                fx = 1.0;
            } else {
                return Err(GridError::OutsideCoverage("GTX longitude edge".into()));
            }
        }
        if y as usize + 1 >= self.height {
            if y as usize + 1 == self.height && fy < 1e-9 {
                y -= 1;
                fy = 1.0;
            } else {
                return Err(GridError::OutsideCoverage("GTX latitude edge".into()));
            }
        }

        let x0 = x as usize;
        let y0 = y as usize;
        let x1 = x0 + 1;
        let y1 = y0 + 1;
        let idx = |xx: usize, yy: usize| yy * self.width + xx;
        let z00 = self.offsets_meters[idx(x0, y0)];
        let z10 = self.offsets_meters[idx(x1, y0)];
        let z01 = self.offsets_meters[idx(x0, y1)];
        let z11 = self.offsets_meters[idx(x1, y1)];

        if !(z00.is_finite() && z10.is_finite() && z01.is_finite() && z11.is_finite()) {
            return Err(GridError::OutsideCoverage(
                "GTX interpolation touches a null cell".into(),
            ));
        }

        let m00 = (1.0 - fx) * (1.0 - fy);
        let m10 = fx * (1.0 - fy);
        let m01 = (1.0 - fx) * fy;
        let m11 = fx * fy;
        Ok(VerticalGridSample {
            offset_meters: m00 * z00 + m10 * z10 + m01 * z01 + m11 * z11,
        })
    }

    fn contains(&self, lon_degrees: f64, lat_degrees: f64) -> bool {
        let epsilon = (self.delta_lon_degrees + self.delta_lat_degrees) * 1e-10;
        lon_degrees >= self.west_degrees - epsilon
            && lon_degrees <= self.east_degrees + epsilon
            && lat_degrees >= self.south_degrees - epsilon
            && lat_degrees <= self.north_degrees + epsilon
    }

    fn normalize_lon_degrees(&self, lon_degrees: f64) -> f64 {
        if self.contains(lon_degrees, self.south_degrees) {
            return lon_degrees;
        }

        self.west_degrees + (lon_degrees - self.west_degrees).rem_euclid(360.0)
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

fn parse_label(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn read_u32(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<u32, GridError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| GridError::Parse("integer offset overflow".into()))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| GridError::Parse("truncated integer".into()))?;
    Ok(match endian {
        Endian::Little => u32::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => u32::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

fn read_i32(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<i32, GridError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| GridError::Parse("integer offset overflow".into()))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| GridError::Parse("truncated integer".into()))?;
    Ok(match endian {
        Endian::Little => i32::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => i32::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

fn read_f64(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<f64, GridError> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| GridError::Parse("float64 offset overflow".into()))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| GridError::Parse("truncated float64".into()))?;
    Ok(match endian {
        Endian::Little => f64::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => f64::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

fn read_f32(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<f32, GridError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| GridError::Parse("float32 offset overflow".into()))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| GridError::Parse("truncated float32".into()))?;
    Ok(match endian {
        Endian::Little => f32::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => f32::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::time::Duration;

    #[test]
    fn embedded_ntv2_grid_samples_known_point() {
        let provider = EmbeddedGridProvider;
        let definition = GridDefinition {
            id: GridId(1),
            name: "ntv2_0.gsb".into(),
            format: GridFormat::Ntv2,
            interpolation: GridInterpolation::Bilinear,
            area_of_use: None,
            resource_names: SmallVec::from_vec(vec!["ntv2_0.gsb".into()]),
        };
        let handle = provider.load(&definition).unwrap().expect("embedded grid");
        let (lon, lat) = handle
            .apply(
                (-80.5041667f64).to_radians(),
                44.5458333f64.to_radians(),
                GridShiftDirection::Forward,
            )
            .unwrap();
        assert!(
            (lon.to_degrees() - (-80.50401615833)).abs() < 1e-6,
            "lon={lon}"
        );
        assert!((lat.to_degrees() - 44.5458827236).abs() < 3e-6, "lat={lat}");
    }

    #[test]
    fn embedded_provider_reuses_parsed_grid_data() {
        let provider = EmbeddedGridProvider;
        let definition = test_grid_definition();

        let first = provider.load(&definition).unwrap().expect("embedded grid");
        let mut renamed = definition.clone();
        renamed.name = "renamed ntv2 grid".into();
        let second = provider.load(&renamed).unwrap().expect("embedded grid");

        assert!(Arc::ptr_eq(&first.data, &second.data));
        assert_eq!(second.definition().name, "renamed ntv2 grid");
    }

    #[test]
    fn grid_handle_reports_sha256_checksum() {
        let provider = EmbeddedGridProvider;
        let handle = provider
            .load(&test_grid_definition())
            .unwrap()
            .expect("embedded grid");

        assert!(handle.checksum().starts_with("sha256:"));
        assert_eq!(handle.checksum().len(), 71);
        assert_eq!(
            sha256_hex(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    struct SingleFlightTrackingProvider {
        data_cache: GridDataCache,
        parse_calls: Arc<AtomicUsize>,
        bytes: Vec<u8>,
    }

    impl GridProvider for SingleFlightTrackingProvider {
        fn definition(
            &self,
            grid: &GridDefinition,
        ) -> std::result::Result<Option<GridDefinition>, GridError> {
            Ok(Some(grid.clone()))
        }

        fn load(
            &self,
            grid: &GridDefinition,
        ) -> std::result::Result<Option<GridHandle>, GridError> {
            let key = GridDataCacheKey::new(grid.format, "single-flight-test-grid");
            let data = cached_grid_data(&self.data_cache, key, || {
                self.parse_calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(25));
                parse_cached_grid_data(grid.format, &grid.name, &self.bytes)
            })?;

            Ok(Some(GridHandle {
                definition: grid.clone(),
                data,
            }))
        }
    }

    #[test]
    fn cached_grid_data_single_flights_concurrent_loads() {
        const THREADS: usize = 12;

        let parse_calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(SingleFlightTrackingProvider {
            data_cache: Mutex::new(HashMap::new()),
            parse_calls: Arc::clone(&parse_calls),
            bytes: test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
        });
        let definition = GridDefinition {
            id: GridId(9_999),
            name: "single-flight-test.gtx".into(),
            format: GridFormat::Gtx,
            interpolation: GridInterpolation::Bilinear,
            area_of_use: None,
            resource_names: SmallVec::from_vec(vec!["single-flight-test.gtx".into()]),
        };
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles = std::thread::scope(|scope| {
            let mut joins = Vec::new();
            for _ in 0..THREADS {
                let provider = Arc::clone(&provider);
                let definition = definition.clone();
                let barrier = Arc::clone(&barrier);
                joins.push(scope.spawn(move || {
                    barrier.wait();
                    provider.load(&definition).unwrap().unwrap()
                }));
            }

            joins
                .into_iter()
                .map(|join| join.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(parse_calls.load(Ordering::SeqCst), 1);
        for handle in &handles[1..] {
            assert!(Arc::ptr_eq(&handles[0].data, &handle.data));
            assert_eq!(handles[0].checksum(), handle.checksum());
        }
    }

    struct TrackingGridProvider {
        override_definition: bool,
        definition_calls: Arc<AtomicUsize>,
        load_calls: Arc<AtomicUsize>,
    }

    impl GridProvider for TrackingGridProvider {
        fn definition(
            &self,
            grid: &GridDefinition,
        ) -> std::result::Result<Option<GridDefinition>, GridError> {
            self.definition_calls.fetch_add(1, Ordering::SeqCst);
            if self.override_definition {
                let mut overridden = grid.clone();
                overridden.name = "custom override".into();
                Ok(Some(overridden))
            } else {
                Ok(None)
            }
        }

        fn load(
            &self,
            grid: &GridDefinition,
        ) -> std::result::Result<Option<GridHandle>, GridError> {
            self.load_calls.fetch_add(1, Ordering::SeqCst);
            EmbeddedGridProvider.load(grid)
        }
    }

    fn test_grid_definition() -> GridDefinition {
        GridDefinition {
            id: GridId(1),
            name: "ntv2_0.gsb".into(),
            format: GridFormat::Ntv2,
            interpolation: GridInterpolation::Bilinear,
            area_of_use: None,
            resource_names: SmallVec::from_vec(vec!["ntv2_0.gsb".into()]),
        }
    }

    fn write_ntv2_global_header(header: &mut [u8], num_subfiles: u32) {
        header[8..12].copy_from_slice(&11u32.to_le_bytes());
        header[40..44].copy_from_slice(&num_subfiles.to_le_bytes());
        header[56..63].copy_from_slice(b"SECONDS");
    }

    fn write_ntv2_label(header: &mut [u8], offset: usize, value: &str) {
        header[offset..offset + 8].fill(b' ');
        let bytes = value.as_bytes();
        let len = bytes.len().min(8);
        header[offset..offset + len].copy_from_slice(&bytes[..len]);
    }

    fn write_ntv2_f64(header: &mut [u8], offset: usize, value: f64) {
        header[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_ntv2_f64_bits(header: &mut [u8], offset: usize, bits: u64) {
        header[offset..offset + 8].copy_from_slice(&bits.to_le_bytes());
    }

    fn write_ntv2_f32(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_ntv2_u32(header: &mut [u8], offset: usize, value: u32) {
        header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn minimal_ntv2_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; NTV2_HEADER_LEN * 2 + 4 * NTV2_RECORD_LEN];
        write_ntv2_global_header(&mut bytes[..NTV2_HEADER_LEN], 1);

        let header = &mut bytes[NTV2_HEADER_LEN..NTV2_HEADER_LEN * 2];
        header[0..8].copy_from_slice(b"SUB_NAME");
        write_ntv2_label(header, 8, "TEST");
        write_ntv2_label(header, 24, "NONE");
        write_ntv2_f64(header, 72, 0.0);
        write_ntv2_f64(header, 88, 3600.0);
        write_ntv2_f64(header, 104, 0.0);
        write_ntv2_f64(header, 120, 3600.0);
        write_ntv2_f64(header, 136, 3600.0);
        write_ntv2_f64(header, 152, 3600.0);
        write_ntv2_u32(header, 168, 4);

        bytes
    }

    /// Write one NTv2 subfile (header + constant-shift data) at `offset`.
    ///
    /// Extents are in degrees; the file stores positive-west arcseconds.
    /// Every node gets the same latitude shift (`lat_shift_arcsec`) and a zero
    /// longitude shift, so subgrid selection is observable through the value.
    #[allow(clippy::too_many_arguments)]
    fn write_ntv2_subfile(
        bytes: &mut [u8],
        offset: usize,
        name: &str,
        parent: &str,
        west_deg: f64,
        east_deg: f64,
        south_deg: f64,
        north_deg: f64,
        res_deg: f64,
        lat_shift_arcsec: f32,
    ) -> usize {
        let nodes_x = ((east_deg - west_deg) / res_deg).round() as usize + 1;
        let nodes_y = ((north_deg - south_deg) / res_deg).round() as usize + 1;
        let gs_count = nodes_x * nodes_y;

        let header = &mut bytes[offset..offset + NTV2_HEADER_LEN];
        header[0..8].copy_from_slice(b"SUB_NAME");
        write_ntv2_label(header, 8, name);
        write_ntv2_label(header, 24, parent);
        write_ntv2_f64(header, 72, south_deg * 3600.0);
        write_ntv2_f64(header, 88, north_deg * 3600.0);
        write_ntv2_f64(header, 104, -east_deg * 3600.0);
        write_ntv2_f64(header, 120, -west_deg * 3600.0);
        write_ntv2_f64(header, 136, res_deg * 3600.0);
        write_ntv2_f64(header, 152, res_deg * 3600.0);
        write_ntv2_u32(header, 168, gs_count as u32);

        let data_start = offset + NTV2_HEADER_LEN;
        for record in 0..gs_count {
            write_ntv2_f32(
                bytes,
                data_start + record * NTV2_RECORD_LEN,
                lat_shift_arcsec,
            );
        }
        data_start + gs_count * NTV2_RECORD_LEN
    }

    /// A three-level NTv2 hierarchy with distinct constant latitude shifts:
    /// root AA (1″) ⊃ child BB (2″) ⊃ grandchild CC (3″).
    fn nested_ntv2_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; NTV2_HEADER_LEN * 4 + 3 * 25 * NTV2_RECORD_LEN];
        write_ntv2_global_header(&mut bytes[..NTV2_HEADER_LEN], 3);

        let mut offset = NTV2_HEADER_LEN;
        offset = write_ntv2_subfile(
            &mut bytes, offset, "AA", "NONE", -4.0, 0.0, 0.0, 4.0, 1.0, 1.0,
        );
        offset = write_ntv2_subfile(
            &mut bytes, offset, "BB", "AA", -3.0, -1.0, 1.0, 3.0, 0.5, 2.0,
        );
        offset = write_ntv2_subfile(
            &mut bytes, offset, "CC", "BB", -2.5, -1.5, 1.5, 2.5, 0.25, 3.0,
        );
        assert_eq!(offset, bytes.len());

        bytes
    }

    #[test]
    fn ntv2_selects_deepest_nested_subgrid() {
        let set = Ntv2GridSet::parse(&nested_ntv2_bytes()).unwrap();
        let arcsec = PI / 180.0 / 3600.0;

        let cases = [
            (-2.0, 2.0, 3.0, "inside grandchild CC"),
            (-2.8, 1.2, 2.0, "inside child BB, outside CC"),
            (-0.5, 0.5, 1.0, "inside root AA only"),
        ];
        for (lon_deg, lat_deg, expected_arcsec, label) in cases {
            let sample = set
                .sample(f64::to_radians(lon_deg), f64::to_radians(lat_deg))
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            assert!(
                (sample.lat_shift_radians - expected_arcsec * arcsec).abs() < 1e-12,
                "{label}: got {} arcsec",
                sample.lat_shift_radians / arcsec
            );
        }
    }

    #[test]
    #[ignore = "P1.2 pending: NTv2 sampling must wrap out-of-branch longitudes into the grid frame"]
    fn ntv2_wraps_out_of_branch_longitude() {
        let set = Ntv2GridSet::parse(&nested_ntv2_bytes()).unwrap();

        // 358°E is the same meridian as -2°E; GTX grids already wrap this way.
        let in_branch = set
            .sample(f64::to_radians(-2.0), f64::to_radians(2.0))
            .unwrap();
        let wrapped = set
            .sample(f64::to_radians(358.0), f64::to_radians(2.0))
            .expect("out-of-branch longitude should resolve to the same grid cell");
        assert!(
            (wrapped.lat_shift_radians - in_branch.lat_shift_radians).abs() < 1e-15
                && (wrapped.lon_shift_radians - in_branch.lon_shift_radians).abs() < 1e-15,
            "wrapped sample must match in-branch sample"
        );
    }

    fn grid_handle_parse_error(bytes: &[u8]) -> String {
        match GridHandle::from_bytes(test_grid_definition(), bytes) {
            Ok(_) => panic!("expected NTv2 parse failure"),
            Err(GridError::Parse(message)) => message,
            Err(error) => panic!("expected NTv2 parse error, got {error}"),
        }
    }

    #[cfg(feature = "geotiff")]
    #[derive(Clone)]
    struct TestTiffTag {
        tag: u16,
        field_type: u16,
        count: u32,
        value: Vec<u8>,
    }

    #[cfg(feature = "geotiff")]
    fn geotiff_parse_error(bytes: &[u8]) -> String {
        match parse_grid_data(GridFormat::GeoTiff, "test.tif", bytes) {
            Ok(_) => panic!("expected GeoTIFF parse failure"),
            Err(GridError::Parse(message)) => message,
            Err(error) => panic!("expected GeoTIFF parse error, got {error}"),
        }
    }

    #[cfg(feature = "geotiff")]
    fn minimal_geotiff_bytes(width: u32, height: u32, bands: u16, grid_type: &str) -> Vec<u8> {
        classic_tiff(vec![minimal_geotiff_tags(width, height, bands, grid_type)])
    }

    #[cfg(feature = "geotiff")]
    fn minimal_geotiff_tags(
        width: u32,
        height: u32,
        bands: u16,
        grid_type: &str,
    ) -> Vec<TestTiffTag> {
        vec![
            test_tiff_long(256, width),
            test_tiff_long(257, height),
            test_tiff_short(258, 32),
            test_tiff_short(259, 1),
            test_tiff_short(262, 1),
            test_tiff_short(277, bands),
            test_tiff_short(284, 1),
            test_tiff_short(339, 3),
            test_tiff_doubles(33550, &[1.0, 1.0, 0.0]),
            test_tiff_doubles(33922, &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            test_tiff_shorts(34735, &[1, 1, 1, 0]),
            test_tiff_ascii(42112, grid_type),
        ]
    }

    #[cfg(feature = "geotiff")]
    fn classic_tiff(mut ifds: Vec<Vec<TestTiffTag>>) -> Vec<u8> {
        for tags in &mut ifds {
            tags.sort_by_key(|tag| tag.tag);
        }

        let block_lens: Vec<usize> = ifds
            .iter()
            .map(|tags| {
                let data_len = tags.iter().fold(0usize, |len, tag| {
                    if tag.value.len() <= 4 {
                        len
                    } else {
                        len + padded_tiff_value_len(tag.value.len())
                    }
                });
                2 + tags.len() * 12 + 4 + data_len
            })
            .collect();
        let mut starts = Vec::with_capacity(block_lens.len());
        let mut next_start = 8usize;
        for block_len in &block_lens {
            starts.push(next_start);
            next_start += block_len;
        }

        let mut bytes = Vec::with_capacity(next_start);
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());

        for (ifd_index, tags) in ifds.iter().enumerate() {
            assert_eq!(bytes.len(), starts[ifd_index]);
            bytes.extend_from_slice(&(tags.len() as u16).to_le_bytes());

            let data_start = starts[ifd_index] + 2 + tags.len() * 12 + 4;
            let mut data = Vec::new();
            for tag in tags {
                bytes.extend_from_slice(&tag.tag.to_le_bytes());
                bytes.extend_from_slice(&tag.field_type.to_le_bytes());
                bytes.extend_from_slice(&tag.count.to_le_bytes());
                if tag.value.len() <= 4 {
                    let mut inline = [0u8; 4];
                    inline[..tag.value.len()].copy_from_slice(&tag.value);
                    bytes.extend_from_slice(&inline);
                } else {
                    let offset = data_start + data.len();
                    bytes.extend_from_slice(&(offset as u32).to_le_bytes());
                    data.extend_from_slice(&tag.value);
                    if data.len() % 2 != 0 {
                        data.push(0);
                    }
                }
            }

            let next_ifd = starts.get(ifd_index + 1).copied().unwrap_or(0);
            bytes.extend_from_slice(&(next_ifd as u32).to_le_bytes());
            bytes.extend_from_slice(&data);
        }

        bytes
    }

    #[cfg(feature = "geotiff")]
    fn padded_tiff_value_len(len: usize) -> usize {
        len + (len % 2)
    }

    #[cfg(feature = "geotiff")]
    fn test_tiff_ascii(tag: u16, value: &str) -> TestTiffTag {
        let mut bytes = value.as_bytes().to_vec();
        if !bytes.ends_with(&[0]) {
            bytes.push(0);
        }
        TestTiffTag {
            tag,
            field_type: 2,
            count: bytes.len() as u32,
            value: bytes,
        }
    }

    #[cfg(feature = "geotiff")]
    fn test_tiff_short(tag: u16, value: u16) -> TestTiffTag {
        test_tiff_shorts(tag, &[value])
    }

    #[cfg(feature = "geotiff")]
    fn test_tiff_shorts(tag: u16, values: &[u16]) -> TestTiffTag {
        TestTiffTag {
            tag,
            field_type: 3,
            count: values.len() as u32,
            value: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    #[cfg(feature = "geotiff")]
    fn test_tiff_long(tag: u16, value: u32) -> TestTiffTag {
        TestTiffTag {
            tag,
            field_type: 4,
            count: 1,
            value: value.to_le_bytes().to_vec(),
        }
    }

    #[cfg(feature = "geotiff")]
    fn test_tiff_doubles(tag: u16, values: &[f64]) -> TestTiffTag {
        TestTiffTag {
            tag,
            field_type: 12,
            count: values.len() as u32,
            value: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    fn test_temp_grid_root(name: &str) -> PathBuf {
        static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

        let root = std::env::temp_dir().join(format!(
            "proj-core-{name}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn ntv2_rejects_oversized_resource_length_before_reading() {
        let message = match validate_grid_resource_size(
            "oversized.gsb",
            GridFormat::Ntv2,
            MAX_NTV2_GRID_BYTES as u64 + 1,
        ) {
            Ok(()) => panic!("expected NTv2 resource size failure"),
            Err(GridError::Parse(message)) => message,
            Err(error) => panic!("expected NTv2 parse error, got {error}"),
        };

        assert!(message.contains("maximum Ntv2 grid size"), "{message}");
    }

    #[test]
    fn grid_handle_rejects_excessive_ntv2_subfile_count_before_allocation() {
        let mut bytes = vec![0u8; NTV2_HEADER_LEN];
        write_ntv2_global_header(&mut bytes, u32::MAX);

        let message = grid_handle_parse_error(&bytes);

        assert!(message.contains("subfile count"), "{message}");
    }

    #[test]
    fn ntv2_rejects_excessive_axis_count_before_cell_multiplication() {
        let mut bytes = minimal_ntv2_bytes();
        let header = &mut bytes[NTV2_HEADER_LEN..NTV2_HEADER_LEN * 2];
        write_ntv2_f64(header, 120, MAX_NTV2_CELLS_PER_SUBGRID as f64);
        write_ntv2_f64(header, 152, 1.0);

        let message = grid_handle_parse_error(&bytes);

        assert!(
            message.contains("longitude cell count exceeds limit"),
            "{message}"
        );
    }

    #[test]
    fn ntv2_rejects_excessive_subgrid_cell_count_before_allocation() {
        let mut bytes = minimal_ntv2_bytes();
        let header = &mut bytes[NTV2_HEADER_LEN..NTV2_HEADER_LEN * 2];
        write_ntv2_f64(header, 88, 4096.0);
        write_ntv2_f64(header, 120, 4096.0);
        write_ntv2_f64(header, 136, 1.0);
        write_ntv2_f64(header, 152, 1.0);
        write_ntv2_u32(header, 168, 16_785_409);

        let message = grid_handle_parse_error(&bytes);

        assert!(message.contains("exceeding limit"), "{message}");
    }

    #[test]
    fn ntv2_rejects_non_finite_shift_values() {
        let mut bytes = minimal_ntv2_bytes();
        write_ntv2_f32(&mut bytes, NTV2_HEADER_LEN * 2, f32::NAN);

        let message = grid_handle_parse_error(&bytes);

        assert!(message.contains("non-finite NTv2 shift value"), "{message}");
    }

    #[cfg(feature = "geotiff")]
    #[test]
    fn geotiff_rejects_excessive_ifd_count_before_decoding() {
        let bytes = classic_tiff(vec![Vec::new(); MAX_GEOTIFF_IFDS + 1]);

        let message = geotiff_parse_error(&bytes);

        assert!(message.contains("IFD"), "{message}");
        assert!(message.contains(&MAX_GEOTIFF_IFDS.to_string()), "{message}");
    }

    #[cfg(feature = "geotiff")]
    #[test]
    fn geotiff_rejects_excessive_axis_dimensions_before_decoding() {
        for (width, height, expected) in [
            ((MAX_GEOTIFF_CELLS_PER_IMAGE + 1) as u32, 2, "width"),
            (2, (MAX_GEOTIFF_CELLS_PER_IMAGE + 1) as u32, "height"),
        ] {
            let bytes = minimal_geotiff_bytes(width, height, 1, "TYPE=VERTICAL_OFFSET");

            let message = geotiff_parse_error(&bytes);

            assert!(message.contains(expected), "{message}");
            assert!(message.contains("exceeds limit"), "{message}");
        }
    }

    #[cfg(feature = "geotiff")]
    #[test]
    fn geotiff_rejects_excessive_cell_count_before_decoding() {
        let bytes = minimal_geotiff_bytes(4097, 4097, 1, "TYPE=VERTICAL_OFFSET");

        let message = geotiff_parse_error(&bytes);

        assert!(message.contains("cell count"), "{message}");
        assert!(message.contains("exceeds limit"), "{message}");
    }

    #[cfg(feature = "geotiff")]
    #[test]
    fn geotiff_rejects_excessive_band_count_before_decoding() {
        let bytes =
            minimal_geotiff_bytes(2, 2, (MAX_GEOTIFF_BANDS + 1) as u16, "TYPE=VERTICAL_OFFSET");

        let message = geotiff_parse_error(&bytes);

        assert!(message.contains("band count"), "{message}");
        assert!(message.contains("exceeds limit"), "{message}");
    }

    #[cfg(feature = "geotiff")]
    #[test]
    fn geotiff_rejects_horizontal_grid_with_too_few_bands_before_decoding() {
        let bytes = minimal_geotiff_bytes(2, 2, 1, "TYPE=HORIZONTAL_OFFSET");

        let message = geotiff_parse_error(&bytes);

        assert!(message.contains("needs at least 2"), "{message}");
    }

    #[cfg(feature = "geotiff")]
    #[test]
    fn geotiff_rejects_excessive_total_cell_count_before_decoding() {
        let image = minimal_geotiff_tags(4096, 4096, 2, "TYPE=HORIZONTAL_OFFSET");
        let bytes = classic_tiff(vec![image.clone(), image]);

        let message = geotiff_parse_error(&bytes);

        assert!(message.contains("total cell count"), "{message}");
        assert!(message.contains("exceeds limit"), "{message}");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn ntv2_malformed_subfile_header_fuzz_does_not_panic(
            name in proptest::collection::vec(any::<u8>(), 8),
            parent in proptest::collection::vec(any::<u8>(), 8),
            south_bits in any::<u64>(),
            north_bits in any::<u64>(),
            east_bits in any::<u64>(),
            west_bits in any::<u64>(),
            res_y_bits in any::<u64>(),
            res_x_bits in any::<u64>(),
            gs_count in any::<u32>(),
            data in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let mut bytes = vec![0u8; NTV2_HEADER_LEN * 2];
            write_ntv2_global_header(&mut bytes[..NTV2_HEADER_LEN], 1);

            let header = &mut bytes[NTV2_HEADER_LEN..NTV2_HEADER_LEN * 2];
            header[0..8].copy_from_slice(b"SUB_NAME");
            header[8..16].copy_from_slice(&name);
            header[24..32].copy_from_slice(&parent);
            write_ntv2_f64_bits(header, 72, south_bits);
            write_ntv2_f64_bits(header, 88, north_bits);
            write_ntv2_f64_bits(header, 104, east_bits);
            write_ntv2_f64_bits(header, 120, west_bits);
            write_ntv2_f64_bits(header, 136, res_y_bits);
            write_ntv2_f64_bits(header, 152, res_x_bits);
            write_ntv2_u32(header, 168, gs_count);
            bytes.extend_from_slice(&data);

            let _ = Ntv2GridSet::parse(&bytes);
        }
    }

    #[test]
    fn filesystem_provider_rejects_unsafe_resource_names() {
        let root = test_temp_grid_root("unsafe-resource");
        std::fs::write(root.join("safe.gtx"), []).unwrap();

        let provider = FilesystemGridProvider::new(vec![root.clone()]);
        let mut definition = test_grid_definition();
        definition.format = GridFormat::Gtx;
        definition.resource_names = SmallVec::from_vec(vec!["../safe.gtx".into()]);
        assert!(provider.definition(&definition).unwrap().is_none());

        definition.resource_names =
            SmallVec::from_vec(vec![root.join("safe.gtx").to_string_lossy().into_owned()]);
        assert!(provider.definition(&definition).unwrap().is_none());

        definition.resource_names = SmallVec::from_vec(vec!["safe.gtx".into()]);
        assert!(provider.definition(&definition).unwrap().is_some());
    }

    #[test]
    fn filesystem_provider_loads_grid_from_canonical_root() {
        let root = test_temp_grid_root("canonical-root");
        std::fs::write(
            root.join("safe.gtx"),
            test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
        )
        .unwrap();

        let provider = FilesystemGridProvider::new(vec![root]);
        let mut definition = test_grid_definition();
        definition.name = "safe.gtx".into();
        definition.format = GridFormat::Gtx;
        definition.resource_names = SmallVec::from_vec(vec!["safe.gtx".into()]);

        assert!(provider.definition(&definition).unwrap().is_some());
        let handle = provider.load(&definition).unwrap().unwrap();
        let sample = handle
            .sample_vertical_offset_meters(20.5f64.to_radians(), 10.5f64.to_radians())
            .unwrap();

        assert!((sample.offset_meters - 2.0).abs() < 1e-12);
    }

    #[test]
    fn filesystem_provider_reuses_located_path_between_definition_and_load() {
        let root = test_temp_grid_root("path-cache");
        std::fs::write(
            root.join("cached.gtx"),
            test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
        )
        .unwrap();

        let provider = FilesystemGridProvider::new(vec![root]);
        let mut definition = test_grid_definition();
        definition.name = "cached.gtx".into();
        definition.format = GridFormat::Gtx;
        definition.resource_names = SmallVec::from_vec(vec!["cached.gtx".into()]);

        assert!(provider.definition(&definition).unwrap().is_some());
        assert_eq!(provider.locate_searches.load(Ordering::SeqCst), 1);

        assert!(provider.load(&definition).unwrap().is_some());
        assert_eq!(provider.locate_searches.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_provider_rejects_cached_path_swapped_to_symlink() {
        use std::os::unix::fs::symlink;

        let root = test_temp_grid_root("stale-path-cache");
        let outside = test_temp_grid_root("stale-path-outside");
        let grid_path = root.join("cached.gtx");
        let outside_path = outside.join("outside.gtx");
        std::fs::write(
            &grid_path,
            test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
        )
        .unwrap();
        std::fs::write(
            &outside_path,
            test_gtx_bytes(&[
                100.0, 101.0, 102.0, 103.0, 104.0, 105.0, 106.0, 107.0, 108.0,
            ]),
        )
        .unwrap();

        let provider = FilesystemGridProvider::new(vec![root]);
        let mut definition = test_grid_definition();
        definition.name = "cached.gtx".into();
        definition.format = GridFormat::Gtx;
        definition.resource_names = SmallVec::from_vec(vec!["cached.gtx".into()]);

        assert!(provider.definition(&definition).unwrap().is_some());
        assert_eq!(provider.locate_searches.load(Ordering::SeqCst), 1);

        std::fs::remove_file(&grid_path).unwrap();
        symlink(&outside_path, &grid_path).unwrap();

        assert!(provider.load(&definition).unwrap().is_none());
        assert_eq!(provider.locate_searches.load(Ordering::SeqCst), 2);
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_grid_open_rejects_symlink_after_canonicalization() {
        use std::os::unix::fs::symlink;

        let root = test_temp_grid_root("nofollow-open");
        let outside = test_temp_grid_root("nofollow-open-outside");
        let grid_path = root.join("cached.gtx");
        let outside_path = outside.join("outside.gtx");
        std::fs::write(&grid_path, test_gtx_bytes(&[0.0; 9])).unwrap();
        std::fs::write(&outside_path, test_gtx_bytes(&[100.0; 9])).unwrap();

        let location = FilesystemGridLocation {
            root: root.canonicalize().unwrap(),
            path: grid_path.canonicalize().unwrap(),
        };
        let canonical_path = location.path.clone();

        std::fs::remove_file(&grid_path).unwrap();
        symlink(&outside_path, &grid_path).unwrap();

        let err = open_filesystem_grid_resource_file(&location, &canonical_path).unwrap_err();
        assert!(matches!(err, GridError::Unavailable(_)));
    }

    #[test]
    fn filesystem_grid_read_enforces_cap_on_bytes_read() {
        let root = test_temp_grid_root("bounded-read");
        let path = root.join("oversized.gtx");
        std::fs::write(&path, [0u8; 4]).unwrap();

        let err = read_bounded_grid_resource_bytes(&path, GridFormat::Gtx, 3).unwrap_err();

        let GridError::Parse(message) = err else {
            panic!("expected parse error");
        };
        assert!(
            message.contains("maximum Gtx grid size of 3 bytes"),
            "{message}"
        );
    }

    fn test_gtx_bytes(values: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_gtx_header(&mut bytes, 3, 3);
        for value in values {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    fn write_gtx_header(bytes: &mut Vec<u8>, height: i32, width: i32) {
        bytes.extend_from_slice(&10.0f64.to_be_bytes());
        bytes.extend_from_slice(&20.0f64.to_be_bytes());
        bytes.extend_from_slice(&1.0f64.to_be_bytes());
        bytes.extend_from_slice(&1.0f64.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
    }

    fn gtx_parse_error(bytes: &[u8]) -> String {
        match parse_grid_data(GridFormat::Gtx, "test.gtx", bytes) {
            Ok(_) => panic!("expected GTX parse failure"),
            Err(GridError::Parse(message)) => message,
            Err(error) => panic!("expected GTX parse error, got {error}"),
        }
    }

    #[test]
    fn gtx_rejects_excessive_dimensions_before_allocation() {
        let mut bytes = Vec::new();
        write_gtx_header(&mut bytes, 4097, 4097);

        let message = gtx_parse_error(&bytes);

        assert!(message.contains("GTX cell count"), "{message}");
        assert!(message.contains("exceeds limit"), "{message}");
    }

    #[test]
    fn gtx_rejects_oversized_resource_length_before_reading() {
        let message = match validate_grid_resource_size(
            "oversized.gtx",
            GridFormat::Gtx,
            MAX_GTX_GRID_BYTES as u64 + 1,
        ) {
            Ok(()) => panic!("expected GTX resource size failure"),
            Err(GridError::Parse(message)) => message,
            Err(error) => panic!("expected GTX parse error, got {error}"),
        };

        assert!(message.contains("maximum Gtx grid size"), "{message}");
    }

    #[test]
    fn gtx_truncated_data_remains_parse_error() {
        let mut bytes = Vec::new();
        write_gtx_header(&mut bytes, 3, 3);

        let message = gtx_parse_error(&bytes);

        assert!(message.contains("truncated GTX data"), "{message}");
    }

    #[test]
    fn gtx_grid_samples_bilinear_offsets() {
        let bytes = test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        let data = parse_grid_data(GridFormat::Gtx, "test.gtx", &bytes).unwrap();
        let GridData::Gtx(grid) = data else {
            panic!("expected GTX grid");
        };

        let sample = grid
            .sample(20.5f64.to_radians(), 10.5f64.to_radians())
            .unwrap();
        assert!((sample.offset_meters - 2.0).abs() < 1e-12);

        let wrapped_sample = grid
            .sample(
                (20.5 + 360.0 * 1_000_000_000_000.0f64).to_radians(),
                10.5f64.to_radians(),
            )
            .unwrap();
        assert!((wrapped_sample.offset_meters - 2.0).abs() < 1e-12);

        let lower_edge_sample = grid
            .sample(
                (20.0 - 5e-11f64).to_radians(),
                (10.0 - 5e-11f64).to_radians(),
            )
            .unwrap();
        assert!((lower_edge_sample.offset_meters - 0.0).abs() < 1e-12);
    }

    #[test]
    fn gtx_grid_rejects_outside_or_null_cells() {
        let bytes = test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, -88.8888, 5.0, 6.0, 7.0, 8.0]);
        let data = parse_grid_data(GridFormat::Gtx, "test.gtx", &bytes).unwrap();
        let GridData::Gtx(grid) = data else {
            panic!("expected GTX grid");
        };

        let null_err = grid
            .sample(20.5f64.to_radians(), 10.5f64.to_radians())
            .unwrap_err();
        assert!(matches!(null_err, GridError::OutsideCoverage(_)));

        let outside_err = grid
            .sample(30.0f64.to_radians(), 10.5f64.to_radians())
            .unwrap_err();
        assert!(matches!(outside_err, GridError::OutsideCoverage(_)));
    }

    #[test]
    fn gtx_grid_rejects_non_finite_coordinates() {
        let bytes = test_gtx_bytes(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        let data = parse_grid_data(GridFormat::Gtx, "test.gtx", &bytes).unwrap();
        let GridData::Gtx(grid) = data else {
            panic!("expected GTX grid");
        };

        for (lon, lat) in [
            (f64::INFINITY, 10.5f64.to_radians()),
            (f64::NEG_INFINITY, 10.5f64.to_radians()),
            (f64::NAN, 10.5f64.to_radians()),
            (20.5f64.to_radians(), f64::INFINITY),
            (20.5f64.to_radians(), f64::NAN),
        ] {
            let err = grid.sample(lon, lat).unwrap_err();
            assert!(matches!(err, GridError::OutsideCoverage(_)));
            let message = err.to_string();
            assert!(message.contains("non-finite"), "{message}");
        }
    }

    #[test]
    fn app_grid_provider_can_override_embedded_grid() {
        let definition_calls = Arc::new(AtomicUsize::new(0));
        let load_calls = Arc::new(AtomicUsize::new(0));
        let provider = TrackingGridProvider {
            override_definition: true,
            definition_calls: Arc::clone(&definition_calls),
            load_calls: Arc::clone(&load_calls),
        };
        let runtime = GridRuntime::new(Some(Arc::new(provider)));

        let handle = runtime
            .resolve_handle(&test_grid_definition())
            .expect("grid should resolve");

        assert_eq!(handle.definition().name, "custom override");
        assert_eq!(definition_calls.load(Ordering::SeqCst), 1);
        assert_eq!(load_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn app_grid_provider_falls_back_to_embedded_grid() {
        let definition_calls = Arc::new(AtomicUsize::new(0));
        let load_calls = Arc::new(AtomicUsize::new(0));
        let provider = TrackingGridProvider {
            override_definition: false,
            definition_calls: Arc::clone(&definition_calls),
            load_calls: Arc::clone(&load_calls),
        };
        let runtime = GridRuntime::new(Some(Arc::new(provider)));

        let handle = runtime
            .resolve_handle(&test_grid_definition())
            .expect("embedded grid should remain available");

        assert_eq!(handle.definition().name, "ntv2_0.gsb");
        assert_eq!(definition_calls.load(Ordering::SeqCst), 1);
        assert_eq!(load_calls.load(Ordering::SeqCst), 1);
    }
}

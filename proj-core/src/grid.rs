use crate::operation::{AreaOfUse, GridId, GridInterpolation, GridShiftDirection};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridFormat {
    /// NTv2 horizontal datum-shift grid (`.gsb`).
    Ntv2,
    /// NOAA/VDatum binary GTX vertical offset grid (`.gtx`).
    Gtx,
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
    roots: Vec<PathBuf>,
    data_cache: GridDataCache,
}

impl FilesystemGridProvider {
    pub fn new<I>(roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        Self {
            roots: roots.into_iter().collect(),
            data_cache: Mutex::new(HashMap::new()),
        }
    }

    fn locate(&self, grid: &GridDefinition) -> Option<PathBuf> {
        for root in &self.roots {
            let Ok(root) = root.canonicalize() else {
                continue;
            };
            for name in &grid.resource_names {
                if !is_safe_grid_resource_name(name) {
                    continue;
                }
                let candidate = root.join(name);
                let Ok(canonical_candidate) = candidate.canonicalize() else {
                    continue;
                };
                if canonical_candidate.starts_with(&root) && canonical_candidate.is_file() {
                    return Some(canonical_candidate);
                }
            }
        }
        None
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
        let Some(path) = self.locate(grid) else {
            return Ok(None);
        };

        let cache_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        let key = GridDataCacheKey::new(grid.format, cache_path.to_string_lossy());
        let data = cached_grid_data(&self.data_cache, key, || {
            let bytes = std::fs::read(&path)
                .map_err(|err| GridError::Unavailable(format!("{}: {err}", path.display())))?;
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
    match format {
        GridFormat::Ntv2 => Ok(GridData::Ntv2(Ntv2GridSet::parse(bytes)?)),
        GridFormat::Gtx => Ok(GridData::Gtx(GtxGrid::parse(bytes)?)),
        GridFormat::Unsupported => Err(GridError::UnsupportedFormat(name.into())),
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
        const HEADER_LEN: usize = 11 * 16;

        if bytes.len() < HEADER_LEN {
            return Err(GridError::Parse("NTv2 file too small".into()));
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
        let mut offset = HEADER_LEN;
        let mut grids = Vec::with_capacity(num_subfiles);
        let mut name_to_index = HashMap::new();
        let mut parent_links: Vec<Option<String>> = Vec::with_capacity(num_subfiles);

        for _ in 0..num_subfiles {
            let header = bytes
                .get(offset..offset + HEADER_LEN)
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

            if !(west < east && south < north && res_x > 0.0 && res_y > 0.0) {
                return Err(GridError::Parse(format!(
                    "invalid NTv2 georeferencing for subgrid {name}"
                )));
            }

            let width = (((east - west) / res_x).abs() + 0.5).floor() as usize + 1;
            let height = (((north - south) / res_y).abs() + 0.5).floor() as usize + 1;
            if width * height != gs_count {
                return Err(GridError::Parse(format!(
                    "NTv2 subgrid {name} cell count mismatch: expected {} got {gs_count}",
                    width * height
                )));
            }

            let data_len = gs_count
                .checked_mul(4)
                .and_then(|count| count.checked_mul(4))
                .ok_or_else(|| GridError::Parse("NTv2 data size overflow".into()))?;
            let data = bytes
                .get(offset + HEADER_LEN..offset + HEADER_LEN + data_len)
                .ok_or_else(|| {
                    GridError::Parse(format!("truncated NTv2 data for subgrid {name}"))
                })?;

            let mut lat_shift = vec![0.0f64; gs_count];
            let mut lon_shift = vec![0.0f64; gs_count];
            for y in 0..height {
                for x in 0..width {
                    let source_x = width - 1 - x;
                    let record_offset = (y * width + source_x) * 16;
                    let lat = read_f32(data, record_offset, endian)? as f64 * PI / 180.0 / 3600.0;
                    let lon =
                        -(read_f32(data, record_offset + 4, endian)? as f64) * PI / 180.0 / 3600.0;
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
            offset += HEADER_LEN + data_len;
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

        Ok((estimate_lon, estimate_lat))
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
        const HEADER_LEN: usize = 40;

        if bytes.len() < HEADER_LEN {
            return Err(GridError::Parse("GTX file too small".into()));
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
        let expected_len = HEADER_LEN
            + count
                .checked_mul(4)
                .ok_or_else(|| GridError::Parse("GTX data size overflow".into()))?;
        if bytes.len() < expected_len {
            return Err(GridError::Parse("truncated GTX data".into()));
        }

        let mut offsets_meters = Vec::with_capacity(count);
        for index in 0..count {
            let value = read_f32(bytes, HEADER_LEN + index * 4, Endian::Big)? as f64;
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
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| GridError::Parse("truncated integer".into()))?;
    Ok(match endian {
        Endian::Little => u32::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => u32::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

fn read_i32(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<i32, GridError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| GridError::Parse("truncated integer".into()))?;
    Ok(match endian {
        Endian::Little => i32::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => i32::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

fn read_f64(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<f64, GridError> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| GridError::Parse("truncated float64".into()))?;
    Ok(match endian {
        Endian::Little => f64::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => f64::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

fn read_f32(bytes: &[u8], offset: usize, endian: Endian) -> std::result::Result<f32, GridError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| GridError::Parse("truncated float32".into()))?;
    Ok(match endian {
        Endian::Little => f32::from_le_bytes(slice.try_into().expect("slice length checked")),
        Endian::Big => f32::from_be_bytes(slice.try_into().expect("slice length checked")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn filesystem_provider_rejects_unsafe_resource_names() {
        let root = std::env::temp_dir().join(format!("proj-core-grid-root-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
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

    fn test_gtx_bytes(values: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&10.0f64.to_be_bytes());
        bytes.extend_from_slice(&20.0f64.to_be_bytes());
        bytes.extend_from_slice(&1.0f64.to_be_bytes());
        bytes.extend_from_slice(&1.0f64.to_be_bytes());
        bytes.extend_from_slice(&3i32.to_be_bytes());
        bytes.extend_from_slice(&3i32.to_be_bytes());
        for value in values {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
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

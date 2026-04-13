use crate::operation::{AreaOfUse, GridId, GridInterpolation, GridShiftDirection};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridFormat {
    Ntv2,
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
    data: Arc<GridData>,
}

impl GridHandle {
    pub fn definition(&self) -> &GridDefinition {
        &self.definition
    }

    pub fn sample(
        &self,
        lon_radians: f64,
        lat_radians: f64,
    ) -> std::result::Result<GridSample, GridError> {
        match self.data.as_ref() {
            GridData::Ntv2(set) => set.sample(lon_radians, lat_radians),
        }
    }

    pub fn apply(
        &self,
        lon_radians: f64,
        lat_radians: f64,
        direction: GridShiftDirection,
    ) -> std::result::Result<(f64, f64), GridError> {
        match self.data.as_ref() {
            GridData::Ntv2(set) => set.apply(lon_radians, lat_radians, direction),
        }
    }
}

pub(crate) struct GridRuntime {
    providers: Vec<Arc<dyn GridProvider>>,
    definition_cache: Mutex<HashMap<GridId, GridDefinition>>,
    handle_cache: Mutex<HashMap<GridId, GridHandle>>,
}

impl GridRuntime {
    pub(crate) fn new(app_provider: Option<Arc<dyn GridProvider>>) -> Self {
        let mut providers: Vec<Arc<dyn GridProvider>> = vec![Arc::new(EmbeddedGridProvider)];
        if let Some(provider) = app_provider {
            providers.push(provider);
        }
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
        if let Some(cached) = self
            .definition_cache
            .lock()
            .expect("grid definition cache poisoned")
            .get(&grid.id)
            .cloned()
        {
            return Ok(cached);
        }

        for provider in &self.providers {
            if let Some(definition) = provider.definition(grid)? {
                self.definition_cache
                    .lock()
                    .expect("grid definition cache poisoned")
                    .insert(grid.id, definition.clone());
                return Ok(definition);
            }
        }

        Err(GridError::Unavailable(grid.name.clone()))
    }

    pub(crate) fn resolve_handle(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<GridHandle, GridError> {
        if let Some(cached) = self
            .handle_cache
            .lock()
            .expect("grid handle cache poisoned")
            .get(&grid.id)
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
                    .insert(grid.id, handle.clone());
                return Ok(handle);
            }
        }

        Err(GridError::Unavailable(definition.name))
    }
}

#[derive(Default)]
pub struct EmbeddedGridProvider;

impl GridProvider for EmbeddedGridProvider {
    fn definition(
        &self,
        grid: &GridDefinition,
    ) -> std::result::Result<Option<GridDefinition>, GridError> {
        if embedded_grid_bytes(&grid.resource_names).is_some() {
            return Ok(Some(grid.clone()));
        }
        Ok(None)
    }

    fn load(&self, grid: &GridDefinition) -> std::result::Result<Option<GridHandle>, GridError> {
        let Some(bytes) = embedded_grid_bytes(&grid.resource_names) else {
            return Ok(None);
        };

        let data = match grid.format {
            GridFormat::Ntv2 => GridData::Ntv2(Ntv2GridSet::parse(bytes)?),
            GridFormat::Unsupported => {
                return Err(GridError::UnsupportedFormat(grid.name.clone()));
            }
        };

        Ok(Some(GridHandle {
            definition: grid.clone(),
            data: Arc::new(data),
        }))
    }
}

pub struct FilesystemGridProvider {
    roots: Vec<PathBuf>,
}

impl FilesystemGridProvider {
    pub fn new<I>(roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        Self {
            roots: roots.into_iter().collect(),
        }
    }

    fn locate(&self, grid: &GridDefinition) -> Option<PathBuf> {
        for root in &self.roots {
            for name in &grid.resource_names {
                let candidate = root.join(name);
                if candidate.exists() {
                    return Some(candidate);
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

        let bytes = std::fs::read(&path)
            .map_err(|err| GridError::Unavailable(format!("{}: {err}", path.display())))?;
        let data = match grid.format {
            GridFormat::Ntv2 => GridData::Ntv2(Ntv2GridSet::parse(&bytes)?),
            GridFormat::Unsupported => {
                return Err(GridError::UnsupportedFormat(grid.name.clone()));
            }
        };

        Ok(Some(GridHandle {
            definition: grid.clone(),
            data: Arc::new(data),
        }))
    }
}

enum GridData {
    Ntv2(Ntv2GridSet),
}

fn embedded_grid_bytes(names: &[String]) -> Option<&'static [u8]> {
    for name in names {
        if name.eq_ignore_ascii_case("ntv2_0.gsb") {
            return Some(include_bytes!("../data/grids/ntv2_0.gsb"));
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
}

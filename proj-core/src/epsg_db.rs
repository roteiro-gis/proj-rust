//! Embedded EPSG registry with CRS, datum, operation, area-of-use, and grid tables.

use crate::crs::*;
use crate::datum::{Datum, DatumToWgs84, HelmertParams};
use crate::ellipsoid::Ellipsoid;
use crate::grid::{GridDefinition, GridFormat};
use crate::operation::{
    AreaOfUse, CoordinateOperation, CoordinateOperationId, GridId, GridInterpolation,
    GridShiftDirection, OperationAccuracy, OperationMethod, OperationStep, OperationStepDirection,
};
use smallvec::SmallVec;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

static EPSG_DATA: &[u8] = include_bytes!("../data/epsg.bin");

const MAGIC: u32 = 0x4550_5347;
const VERSION: u16 = 5;
const HEADER_SIZE: usize = 36;

const ELLIPSOID_RECORD_SIZE: usize = 20;
const DATUM_RECORD_SIZE: usize = 72;
const GEO_CRS_RECORD_BASE_SIZE: usize = 8;
const PROJ_CRS_RECORD_BASE_SIZE: usize = 80;

const DATUM_SHIFT_UNKNOWN: u8 = 0;
const DATUM_SHIFT_IDENTITY: u8 = 1;
const DATUM_SHIFT_HELMERT: u8 = 2;

const METHOD_WEB_MERCATOR: u8 = 1;
const METHOD_TRANSVERSE_MERCATOR: u8 = 2;
const METHOD_MERCATOR: u8 = 3;
const METHOD_LCC: u8 = 4;
const METHOD_ALBERS: u8 = 5;
const METHOD_POLAR_STEREO: u8 = 6;
const METHOD_EQUIDISTANT_CYL: u8 = 7;

const OP_IDENTITY: u8 = 0;
const OP_HELMERT: u8 = 1;
const OP_GRID_SHIFT: u8 = 2;
const OP_CONCATENATED: u8 = 3;

const FLAG_DEPRECATED: u8 = 1 << 0;
const FLAG_PREFERRED: u8 = 1 << 1;
const FLAG_APPROXIMATE: u8 = 1 << 2;

const GRID_FORMAT_NTV2: u8 = 1;
const GRID_INTERPOLATION_BILINEAR: u8 = 1;

#[derive(Clone)]
struct GeographicRecord {
    datum_code: u32,
    name: &'static str,
}

#[derive(Clone)]
struct ProjectedRecord {
    base_geographic_crs_epsg: u32,
    datum_code: u32,
    method: ProjectionMethod,
    linear_unit: LinearUnit,
    name: &'static str,
}

#[derive(Clone)]
struct RegistryDb {
    datums: BTreeMap<u32, Datum>,
    geographic_crs: BTreeMap<u32, GeographicRecord>,
    projected_crs: BTreeMap<u32, ProjectedRecord>,
    grids: BTreeMap<u32, GridDefinition>,
    operations: BTreeMap<u32, CoordinateOperation>,
    operation_ids_by_crs_pair: HashMap<(u32, u32), Vec<u32>>,
    operation_ids_by_datum_pair: HashMap<(u32, u32), Vec<u32>>,
}

fn db() -> &'static RegistryDb {
    static DB: OnceLock<RegistryDb> = OnceLock::new();
    DB.get_or_init(parse_db)
}

fn parse_db() -> RegistryDb {
    assert!(EPSG_DATA.len() >= HEADER_SIZE, "EPSG registry too small");
    assert_eq!(read_u32(EPSG_DATA, 0), MAGIC, "invalid EPSG registry magic");
    assert_eq!(
        read_u16(EPSG_DATA, 4),
        VERSION,
        "unsupported EPSG registry version"
    );

    let num_ellipsoids = read_u32(EPSG_DATA, 8) as usize;
    let num_datums = read_u32(EPSG_DATA, 12) as usize;
    let num_geo = read_u32(EPSG_DATA, 16) as usize;
    let num_proj = read_u32(EPSG_DATA, 20) as usize;
    let num_extents = read_u32(EPSG_DATA, 24) as usize;
    let num_grids = read_u32(EPSG_DATA, 28) as usize;
    let num_operations = read_u32(EPSG_DATA, 32) as usize;

    let mut offset = HEADER_SIZE;

    let mut ellipsoids = BTreeMap::new();
    for _ in 0..num_ellipsoids {
        let code = read_u32(EPSG_DATA, offset);
        let a = read_f64(EPSG_DATA, offset + 4);
        let inv_f = read_f64(EPSG_DATA, offset + 12);
        let ellipsoid = if inv_f == 0.0 {
            Ellipsoid::sphere(a)
        } else {
            Ellipsoid::from_a_rf(a, inv_f)
        };
        ellipsoids.insert(code, ellipsoid);
        offset += ELLIPSOID_RECORD_SIZE;
    }

    let mut datums = BTreeMap::new();
    for _ in 0..num_datums {
        let code = read_u32(EPSG_DATA, offset);
        let ellipsoid_code = read_u32(EPSG_DATA, offset + 4);
        let ellipsoid = *ellipsoids
            .get(&ellipsoid_code)
            .unwrap_or_else(|| panic!("missing ellipsoid EPSG:{ellipsoid_code}"));
        let to_wgs84 = match EPSG_DATA[offset + 8] {
            DATUM_SHIFT_UNKNOWN => DatumToWgs84::Unknown,
            DATUM_SHIFT_IDENTITY => DatumToWgs84::Identity,
            DATUM_SHIFT_HELMERT => DatumToWgs84::Helmert(HelmertParams {
                dx: read_f64(EPSG_DATA, offset + 16),
                dy: read_f64(EPSG_DATA, offset + 24),
                dz: read_f64(EPSG_DATA, offset + 32),
                rx: read_f64(EPSG_DATA, offset + 40),
                ry: read_f64(EPSG_DATA, offset + 48),
                rz: read_f64(EPSG_DATA, offset + 56),
                ds: read_f64(EPSG_DATA, offset + 64),
            }),
            other => panic!("unsupported datum shift kind: {other}"),
        };
        datums.insert(
            code,
            Datum {
                ellipsoid,
                to_wgs84,
            },
        );
        offset += DATUM_RECORD_SIZE;
    }

    let mut geographic_crs = BTreeMap::new();
    for _ in 0..num_geo {
        let code = read_u32(EPSG_DATA, offset);
        let datum_code = read_u32(EPSG_DATA, offset + 4);
        let name_len = read_u16(EPSG_DATA, offset + GEO_CRS_RECORD_BASE_SIZE) as usize;
        let name = read_static_string(EPSG_DATA, offset + GEO_CRS_RECORD_BASE_SIZE + 2, name_len);
        geographic_crs.insert(code, GeographicRecord { datum_code, name });
        offset += GEO_CRS_RECORD_BASE_SIZE + 2 + name_len;
    }

    let mut projected_crs = BTreeMap::new();
    for _ in 0..num_proj {
        let code = read_u32(EPSG_DATA, offset);
        let base_geographic_crs_epsg = read_u32(EPSG_DATA, offset + 4);
        let datum_code = read_u32(EPSG_DATA, offset + 8);
        let method_id = EPSG_DATA[offset + 12];
        let linear_unit = LinearUnit::from_meters_per_unit(read_f64(EPSG_DATA, offset + 16))
            .expect("valid linear unit in embedded registry");
        let params = [
            read_f64(EPSG_DATA, offset + 24),
            read_f64(EPSG_DATA, offset + 32),
            read_f64(EPSG_DATA, offset + 40),
            read_f64(EPSG_DATA, offset + 48),
            read_f64(EPSG_DATA, offset + 56),
            read_f64(EPSG_DATA, offset + 64),
            read_f64(EPSG_DATA, offset + 72),
        ];
        let name_len = read_u16(EPSG_DATA, offset + PROJ_CRS_RECORD_BASE_SIZE) as usize;
        let name = read_static_string(EPSG_DATA, offset + PROJ_CRS_RECORD_BASE_SIZE + 2, name_len);
        let method = decode_projection_method(method_id, params);
        projected_crs.insert(
            code,
            ProjectedRecord {
                base_geographic_crs_epsg,
                datum_code,
                method,
                linear_unit,
                name,
            },
        );
        offset += PROJ_CRS_RECORD_BASE_SIZE + 2 + name_len;
    }

    let mut extents = BTreeMap::new();
    for _ in 0..num_extents {
        let code = read_u32(EPSG_DATA, offset);
        let west = read_f64(EPSG_DATA, offset + 4);
        let south = read_f64(EPSG_DATA, offset + 12);
        let east = read_f64(EPSG_DATA, offset + 20);
        let north = read_f64(EPSG_DATA, offset + 28);
        let name_len = read_u16(EPSG_DATA, offset + 36) as usize;
        let name = read_string(EPSG_DATA, offset + 38, name_len);
        extents.insert(
            code,
            AreaOfUse {
                west,
                south,
                east,
                north,
                name,
            },
        );
        offset += 38 + name_len;
    }

    let mut grids = BTreeMap::new();
    for _ in 0..num_grids {
        let id = read_u32(EPSG_DATA, offset);
        let format = match EPSG_DATA[offset + 4] {
            GRID_FORMAT_NTV2 => GridFormat::Ntv2,
            _ => GridFormat::Unsupported,
        };
        let interpolation = match EPSG_DATA[offset + 5] {
            GRID_INTERPOLATION_BILINEAR => GridInterpolation::Bilinear,
            other => panic!("unsupported grid interpolation {other}"),
        };
        let resource_count = read_u16(EPSG_DATA, offset + 6) as usize;
        let area_code = read_u32(EPSG_DATA, offset + 8);
        let name_len = read_u16(EPSG_DATA, offset + 12) as usize;
        let mut cursor = offset + 14;
        let name = read_string(EPSG_DATA, cursor, name_len);
        cursor += name_len;
        let mut resource_names = SmallVec::<[String; 2]>::new();
        for _ in 0..resource_count {
            let len = read_u16(EPSG_DATA, cursor) as usize;
            cursor += 2;
            resource_names.push(read_string(EPSG_DATA, cursor, len));
            cursor += len;
        }
        let area_of_use = if area_code == 0 {
            None
        } else {
            extents.get(&area_code).cloned()
        };
        grids.insert(
            id,
            GridDefinition {
                id: GridId(id),
                name,
                format,
                interpolation,
                area_of_use,
                resource_names,
            },
        );
        offset = cursor;
    }

    let mut operations = BTreeMap::new();
    for _ in 0..num_operations {
        let id = read_u32(EPSG_DATA, offset);
        let method_kind = EPSG_DATA[offset + 4];
        let flags = EPSG_DATA[offset + 5];
        let area_count = read_u16(EPSG_DATA, offset + 6) as usize;
        let source_crs_epsg = read_u32(EPSG_DATA, offset + 8);
        let target_crs_epsg = read_u32(EPSG_DATA, offset + 12);
        let source_datum_epsg = read_u32(EPSG_DATA, offset + 16);
        let target_datum_epsg = read_u32(EPSG_DATA, offset + 20);
        let accuracy = read_f64(EPSG_DATA, offset + 24);
        let name_len = read_u16(EPSG_DATA, offset + 32) as usize;
        let mut cursor = offset + 34;
        let name = read_string(EPSG_DATA, cursor, name_len);
        cursor += name_len;
        let mut areas_of_use = SmallVec::<[AreaOfUse; 1]>::new();
        for _ in 0..area_count {
            let area_code = read_u32(EPSG_DATA, cursor);
            cursor += 4;
            if let Some(area) = extents.get(&area_code) {
                areas_of_use.push(area.clone());
            }
        }
        let method = match method_kind {
            OP_IDENTITY => OperationMethod::Identity,
            OP_HELMERT => {
                let params = HelmertParams {
                    dx: read_f64(EPSG_DATA, cursor),
                    dy: read_f64(EPSG_DATA, cursor + 8),
                    dz: read_f64(EPSG_DATA, cursor + 16),
                    rx: read_f64(EPSG_DATA, cursor + 24),
                    ry: read_f64(EPSG_DATA, cursor + 32),
                    rz: read_f64(EPSG_DATA, cursor + 40),
                    ds: read_f64(EPSG_DATA, cursor + 48),
                };
                cursor += 56;
                OperationMethod::Helmert { params }
            }
            OP_GRID_SHIFT => {
                let grid_id = read_u32(EPSG_DATA, cursor);
                let direction = match EPSG_DATA[cursor + 4] {
                    0 => GridShiftDirection::Forward,
                    1 => GridShiftDirection::Reverse,
                    other => panic!("unsupported grid direction {other}"),
                };
                let interpolation = match EPSG_DATA[cursor + 5] {
                    GRID_INTERPOLATION_BILINEAR => GridInterpolation::Bilinear,
                    other => panic!("unsupported grid interpolation {other}"),
                };
                cursor += 8;
                OperationMethod::GridShift {
                    grid_id: GridId(grid_id),
                    interpolation,
                    direction,
                }
            }
            OP_CONCATENATED => {
                let step_count = read_u16(EPSG_DATA, cursor) as usize;
                cursor += 2;
                let mut steps = SmallVec::<[OperationStep; 4]>::new();
                for _ in 0..step_count {
                    let op_id = read_u32(EPSG_DATA, cursor);
                    let direction = match EPSG_DATA[cursor + 4] {
                        0 => OperationStepDirection::Forward,
                        1 => OperationStepDirection::Reverse,
                        other => panic!("unsupported concatenated step direction {other}"),
                    };
                    cursor += 8;
                    steps.push(OperationStep {
                        operation_id: CoordinateOperationId(op_id),
                        direction,
                    });
                }
                OperationMethod::Concatenated { steps }
            }
            other => panic!("unsupported operation method kind {other}"),
        };

        operations.insert(
            id,
            CoordinateOperation {
                id: Some(CoordinateOperationId(id)),
                name,
                source_crs_epsg: opt_code(source_crs_epsg),
                target_crs_epsg: opt_code(target_crs_epsg),
                source_datum_epsg: opt_code(source_datum_epsg),
                target_datum_epsg: opt_code(target_datum_epsg),
                accuracy: if accuracy.is_nan() {
                    None
                } else {
                    Some(OperationAccuracy { meters: accuracy })
                },
                areas_of_use,
                deprecated: flags & FLAG_DEPRECATED != 0,
                preferred: flags & FLAG_PREFERRED != 0,
                approximate: flags & FLAG_APPROXIMATE != 0,
                method,
            },
        );

        offset = cursor;
    }

    let mut operation_ids_by_crs_pair = HashMap::new();
    let mut operation_ids_by_datum_pair = HashMap::new();
    for operation in operations.values() {
        if let (Some(source), Some(target)) = (operation.source_crs_epsg, operation.target_crs_epsg)
        {
            operation_ids_by_crs_pair
                .entry((source, target))
                .or_insert_with(Vec::new)
                .push(operation.id.expect("registry operation ids are present").0);
        }
        if let (Some(source), Some(target)) =
            (operation.source_datum_epsg, operation.target_datum_epsg)
        {
            operation_ids_by_datum_pair
                .entry((source, target))
                .or_insert_with(Vec::new)
                .push(operation.id.expect("registry operation ids are present").0);
        }
    }

    RegistryDb {
        datums,
        geographic_crs,
        projected_crs,
        grids,
        operations,
        operation_ids_by_crs_pair,
        operation_ids_by_datum_pair,
    }
}

fn decode_projection_method(method_id: u8, params: [f64; 7]) -> ProjectionMethod {
    let [p0, p1, p2, p3, p4, p5, p6] = params;
    match method_id {
        METHOD_WEB_MERCATOR => ProjectionMethod::WebMercator,
        METHOD_TRANSVERSE_MERCATOR => ProjectionMethod::TransverseMercator {
            lon0: p0,
            lat0: p1,
            k0: p2,
            false_easting: p3,
            false_northing: p4,
        },
        METHOD_MERCATOR => ProjectionMethod::Mercator {
            lon0: p0,
            lat_ts: p1,
            k0: p2,
            false_easting: p3,
            false_northing: p4,
        },
        METHOD_LCC => ProjectionMethod::LambertConformalConic {
            lon0: p0,
            lat0: p1,
            lat1: p2,
            lat2: p5,
            false_easting: p3,
            false_northing: p6,
        },
        METHOD_ALBERS => ProjectionMethod::AlbersEqualArea {
            lon0: p0,
            lat0: p1,
            lat1: p2,
            lat2: p5,
            false_easting: p3,
            false_northing: p6,
        },
        METHOD_POLAR_STEREO => ProjectionMethod::PolarStereographic {
            lon0: p0,
            lat_ts: p1,
            k0: p2,
            false_easting: p3,
            false_northing: p4,
        },
        METHOD_EQUIDISTANT_CYL => ProjectionMethod::EquidistantCylindrical {
            lon0: p0,
            lat_ts: p1,
            false_easting: p3,
            false_northing: p4,
        },
        other => panic!("unsupported projection method id {other}"),
    }
}

fn opt_code(code: u32) -> Option<u32> {
    if code == 0 {
        None
    } else {
        Some(code)
    }
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_f64(data: &[u8], offset: usize) -> f64 {
    f64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

fn read_string(data: &[u8], offset: usize, len: usize) -> String {
    String::from_utf8_lossy(&data[offset..offset + len]).into_owned()
}

fn read_static_string(data: &[u8], offset: usize, len: usize) -> &'static str {
    if len == 0 {
        ""
    } else {
        Box::leak(read_string(data, offset, len).into_boxed_str())
    }
}

pub(crate) fn lookup_datum(code: u32) -> Option<Datum> {
    db().datums.get(&code).copied()
}

pub(crate) fn lookup_geographic(code: u32) -> Option<CrsDef> {
    let record = db().geographic_crs.get(&code)?;
    let datum = db().datums.get(&record.datum_code)?;
    Some(CrsDef::Geographic(GeographicCrsDef::new(
        code,
        *datum,
        record.name,
    )))
}

pub(crate) fn lookup_projected(code: u32) -> Option<CrsDef> {
    let record = db().projected_crs.get(&code)?;
    let datum = db().datums.get(&record.datum_code)?;
    Some(CrsDef::Projected(
        ProjectedCrsDef::new_with_base_geographic_crs(
            code,
            record.base_geographic_crs_epsg,
            *datum,
            record.method,
            record.linear_unit,
            record.name,
        ),
    ))
}

pub(crate) fn lookup(code: u32) -> Option<CrsDef> {
    lookup_geographic(code).or_else(|| lookup_projected(code))
}

pub(crate) fn lookup_datum_code_for_crs(code: u32) -> Option<u32> {
    db().geographic_crs
        .get(&code)
        .map(|record| record.datum_code)
        .or_else(|| {
            db().projected_crs
                .get(&code)
                .map(|record| record.datum_code)
        })
}

pub(crate) fn lookup_operation(code: u32) -> Option<CoordinateOperation> {
    db().operations.get(&code).cloned()
}

pub(crate) fn related_operations(
    source_geo: Option<u32>,
    target_geo: Option<u32>,
) -> Vec<&'static CoordinateOperation> {
    let (Some(source_geo), Some(target_geo)) = (source_geo, target_geo) else {
        return Vec::new();
    };
    let source_datum = lookup_datum_code_for_crs(source_geo);
    let target_datum = lookup_datum_code_for_crs(target_geo);
    let mut ids = Vec::new();

    extend_index_hits(
        &mut ids,
        &db().operation_ids_by_crs_pair,
        (source_geo, target_geo),
    );
    extend_index_hits(
        &mut ids,
        &db().operation_ids_by_crs_pair,
        (target_geo, source_geo),
    );

    if let (Some(source_datum), Some(target_datum)) = (source_datum, target_datum) {
        extend_index_hits(
            &mut ids,
            &db().operation_ids_by_datum_pair,
            (source_datum, target_datum),
        );
        extend_index_hits(
            &mut ids,
            &db().operation_ids_by_datum_pair,
            (target_datum, source_datum),
        );
    }

    ids.into_iter()
        .filter_map(|id| db().operations.get(&id))
        .collect()
}

pub(crate) fn forward_operations(
    source_geo: Option<u32>,
    target_geo: Option<u32>,
) -> Vec<&'static CoordinateOperation> {
    let (Some(source_geo), Some(target_geo)) = (source_geo, target_geo) else {
        return Vec::new();
    };
    let source_datum = lookup_datum_code_for_crs(source_geo);
    let target_datum = lookup_datum_code_for_crs(target_geo);
    let mut ids = Vec::new();

    extend_index_hits(
        &mut ids,
        &db().operation_ids_by_crs_pair,
        (source_geo, target_geo),
    );
    if let (Some(source_datum), Some(target_datum)) = (source_datum, target_datum) {
        extend_index_hits(
            &mut ids,
            &db().operation_ids_by_datum_pair,
            (source_datum, target_datum),
        );
    }

    ids.into_iter()
        .filter_map(|id| db().operations.get(&id))
        .collect()
}

pub(crate) fn lookup_grid(code: u32) -> Option<GridDefinition> {
    db().grids.get(&code).cloned()
}

fn extend_index_hits(ids: &mut Vec<u32>, index: &HashMap<(u32, u32), Vec<u32>>, key: (u32, u32)) {
    if let Some(matches) = index.get(&key) {
        for id in matches {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_header_valid() {
        assert_eq!(read_u32(EPSG_DATA, 0), MAGIC);
        assert_eq!(read_u16(EPSG_DATA, 4), VERSION);
    }

    #[test]
    fn lookup_wgs84() {
        let crs = lookup(4326).expect("should find 4326");
        assert!(crs.is_geographic());
    }

    #[test]
    fn lookup_web_mercator() {
        let crs = lookup(3857).expect("should find 3857");
        assert!(crs.is_projected());
    }

    #[test]
    fn lookup_utm_18n() {
        let crs = lookup(32618).expect("should find 32618");
        assert_eq!(crs.base_geographic_crs_epsg(), Some(4326));
    }

    #[test]
    fn lookup_operation_1313() {
        let operation = lookup_operation(1313).expect("operation 1313");
        assert!(matches!(
            operation.method,
            OperationMethod::GridShift { .. }
        ));
        assert!(!operation.areas_of_use.is_empty());
    }

    #[test]
    fn lookup_grid_ntv2() {
        let operation = lookup_operation(1313).expect("operation 1313");
        let OperationMethod::GridShift { grid_id, .. } = operation.method else {
            panic!("expected grid shift");
        };
        let grid = lookup_grid(grid_id.0).expect("grid definition");
        assert_eq!(grid.format, GridFormat::Ntv2);
        assert!(grid
            .resource_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("ntv2_0.gsb")));
    }

    #[test]
    fn concatenated_grid_operation_reports_grid_usage() {
        let operation = lookup_operation(8243).expect("operation 8243");
        assert!(matches!(
            operation.method,
            OperationMethod::Concatenated { .. }
        ));
        assert!(operation.uses_grids());
        assert!(operation.metadata().uses_grids);
    }
}

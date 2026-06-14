use crate::coord::{Coord, Coord3D};
use crate::crs::{CrsDef, LinearUnit, VerticalCrsKind};
use crate::datum::{DatumGridShift, DatumGridShiftEntry, DatumToWgs84, HelmertParams};
use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::grid::{GridError, GridHandle, GridRuntime};
use crate::helmert;
use crate::operation::{
    CoordinateOperation, CoordinateOperationMetadata, GridShiftDirection, OperationMethod,
    OperationStepDirection, VerticalTransformDiagnostics,
};
use crate::projection::{make_projection, validate_lon_lat, validate_projected, Projection};
use crate::registry;
use crate::{ellipsoid, geocentric};
use smallvec::SmallVec;

#[cfg(feature = "rayon")]
pub(super) const PARALLEL_MIN_TOTAL_ITEMS: usize = 16_384;
#[cfg(feature = "rayon")]
pub(super) const PARALLEL_MIN_ITEMS_PER_THREAD: usize = 4_096;

pub(super) struct CompiledOperationPipeline {
    steps: SmallVec<[CompiledStep; 8]>,
    pub(super) source_xy_units: PipelineSourceXyUnits,
    pub(super) target_xy_units: PipelineTargetXyUnits,
}

pub(super) struct CompiledOperationFallback {
    pub(super) operation: CoordinateOperation,
    pub(super) direction: OperationStepDirection,
    pub(super) metadata: CoordinateOperationMetadata,
    pub(super) pipeline: CompiledOperationPipeline,
}

#[derive(Clone, Copy)]
pub(super) enum PipelineSourceXyUnits {
    GeographicDegrees,
    ProjectedMeters,
    ProjectedNativeToMeters(LinearUnit),
}

#[derive(Clone, Copy)]
pub(super) enum PipelineTargetXyUnits {
    GeographicDegrees,
    ProjectedMeters,
    ProjectedMetersToNative(LinearUnit),
}

impl PipelineSourceXyUnits {
    fn compile(source: &CrsDef) -> Self {
        match source.as_projected() {
            Some(projected) if projected.linear_unit_to_meter() == 1.0 => Self::ProjectedMeters,
            Some(projected) => Self::ProjectedNativeToMeters(projected.linear_unit()),
            None => Self::GeographicDegrees,
        }
    }

    fn normalize(self, coord: Coord3D) -> Result<Coord3D> {
        match self {
            Self::GeographicDegrees => {
                let lon = coord.x.to_radians();
                let lat = coord.y.to_radians();
                validate_lon_lat(lon, lat)?;
                Ok(Coord3D::new(lon, lat, coord.z))
            }
            Self::ProjectedMeters => {
                validate_projected(coord.x, coord.y)?;
                Ok(Coord3D::new(coord.x, coord.y, coord.z))
            }
            Self::ProjectedNativeToMeters(unit) => {
                validate_projected(coord.x, coord.y)?;
                let x = unit.to_meters(coord.x);
                let y = unit.to_meters(coord.y);
                validate_projected(x, y)?;
                Ok(Coord3D::new(x, y, coord.z))
            }
        }
    }
}

impl PipelineTargetXyUnits {
    fn compile(target: &CrsDef) -> Self {
        match target.as_projected() {
            Some(projected) if projected.linear_unit_to_meter() == 1.0 => Self::ProjectedMeters,
            Some(projected) => Self::ProjectedMetersToNative(projected.linear_unit()),
            None => Self::GeographicDegrees,
        }
    }

    fn denormalize(self, coord: Coord3D) -> Coord {
        match self {
            Self::GeographicDegrees => Coord::new(coord.x.to_degrees(), coord.y.to_degrees()),
            Self::ProjectedMeters => Coord::new(coord.x, coord.y),
            Self::ProjectedMetersToNative(unit) => {
                Coord::new(unit.from_meters(coord.x), unit.from_meters(coord.y))
            }
        }
    }
}

pub(super) struct PipelineExecutionOutcome {
    pub(super) coord: Coord3D,
    pub(super) vertical: VerticalTransformDiagnostics,
}

enum CompiledStep {
    ProjectionForward {
        projection: Projection,
    },
    ProjectionInverse {
        projection: Projection,
    },
    Helmert {
        params: HelmertParams,
        inverse: bool,
    },
    GridShift {
        handle: GridHandle,
        direction: GridShiftDirection,
    },
    GridShiftList {
        handles: Box<[GridHandle]>,
        allow_null: bool,
        direction: GridShiftDirection,
    },
    GeodeticToGeocentric {
        ellipsoid: Ellipsoid,
    },
    GeocentricToGeodetic {
        ellipsoid: Ellipsoid,
    },
}

pub(super) fn validate_output_len(input_len: usize, output_len: usize) -> Result<()> {
    if input_len != output_len {
        return Err(Error::OutOfRange(format!(
            "output coordinate slice length {output_len} does not match input length {input_len}"
        )));
    }
    Ok(())
}

fn execute_step(step: &CompiledStep, coord: Coord3D) -> Result<Coord3D> {
    let result = match step {
        CompiledStep::ProjectionForward { projection } => {
            let (x, y) = projection.forward(coord.x, coord.y)?;
            Coord3D::new(x, y, coord.z)
        }
        CompiledStep::ProjectionInverse { projection } => {
            let (lon, lat) = projection.inverse(coord.x, coord.y)?;
            Coord3D::new(lon, lat, coord.z)
        }
        CompiledStep::Helmert { params, inverse } => {
            let (x, y, z) = if *inverse {
                helmert::helmert_inverse(params, coord.x, coord.y, coord.z)
            } else {
                helmert::helmert_forward(params, coord.x, coord.y, coord.z)
            };
            Coord3D::new(x, y, z)
        }
        CompiledStep::GridShift { handle, direction } => {
            let (lon, lat) = handle.apply(coord.x, coord.y, *direction)?;
            Coord3D::new(lon, lat, coord.z)
        }
        CompiledStep::GridShiftList {
            handles,
            allow_null,
            direction,
        } => {
            let mut last_coverage_miss = None;
            let mut shifted = None;
            for handle in handles.iter() {
                match handle.apply(coord.x, coord.y, *direction) {
                    Ok((lon, lat)) => {
                        shifted = Some(Coord3D::new(lon, lat, coord.z));
                        break;
                    }
                    Err(GridError::OutsideCoverage(detail)) => {
                        last_coverage_miss = Some(detail);
                    }
                    Err(error) => return Err(Error::Grid(error)),
                }
            }

            if let Some(coord) = shifted {
                coord
            } else if *allow_null {
                coord
            } else {
                return Err(Error::Grid(GridError::OutsideCoverage(
                    last_coverage_miss.unwrap_or_else(|| "no datum grid covered coordinate".into()),
                )));
            }
        }
        CompiledStep::GeodeticToGeocentric { ellipsoid } => {
            let (x, y, z) =
                geocentric::geodetic_to_geocentric(ellipsoid, coord.x, coord.y, coord.z);
            Coord3D::new(x, y, z)
        }
        CompiledStep::GeocentricToGeodetic { ellipsoid } => {
            let (lon, lat, h) =
                geocentric::geocentric_to_geodetic(ellipsoid, coord.x, coord.y, coord.z);
            Coord3D::new(lon, lat, h)
        }
    };

    validate_pipeline_coord3d("pipeline step output", result)?;
    Ok(result)
}

pub(super) fn execute_pipeline_xy(
    pipeline: &CompiledOperationPipeline,
    c: Coord3D,
) -> Result<Coord> {
    let mut state = pipeline.source_xy_units.normalize(c)?;
    if pipeline.steps.is_empty() {
        let output = Coord::new(c.x, c.y);
        validate_pipeline_coord("pipeline final output", output)?;
        return Ok(output);
    }

    for step in &pipeline.steps {
        state = execute_step(step, state)?;
    }

    let output = pipeline.target_xy_units.denormalize(state);
    validate_pipeline_coord("pipeline final output", output)?;
    Ok(output)
}

fn validate_pipeline_coord(context: &str, coord: Coord) -> Result<()> {
    if coord.x.is_finite() && coord.y.is_finite() {
        return Ok(());
    }

    Err(Error::OutOfRange(format!("{context} must be finite")))
}

pub(super) fn validate_pipeline_coord3d(context: &str, coord: Coord3D) -> Result<()> {
    if coord.x.is_finite() && coord.y.is_finite() && coord.z.is_finite() {
        return Ok(());
    }

    Err(Error::OutOfRange(format!("{context} must be finite")))
}

pub(super) fn validate_vertical_ordinate(z: f64) -> Result<()> {
    if !z.is_finite() {
        return Err(Error::OutOfRange(
            "vertical input coordinate must be finite".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_transform_crs_definition(crs: &CrsDef) -> Result<()> {
    crs.datum().to_wgs84().validate()?;
    if let Some(vertical) = crs.vertical_crs() {
        if let VerticalCrsKind::EllipsoidalHeight { datum } = vertical.kind() {
            datum.to_wgs84().validate()?;
        }
    }
    Ok(())
}

pub(super) fn compile_pipeline(
    source: &CrsDef,
    target: &CrsDef,
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    grid_runtime: &GridRuntime,
) -> Result<CompiledOperationPipeline> {
    let mut steps = SmallVec::<[CompiledStep; 8]>::new();

    if let Some(projected) = source.as_projected() {
        steps.push(CompiledStep::ProjectionInverse {
            projection: make_projection(&projected.method(), projected.datum())?,
        });
    }

    if operation.id.is_none() && matches!(operation.method, OperationMethod::Identity) {
        // Synthetic identity between semantically equivalent CRS.
    } else {
        compile_operation(
            operation,
            direction,
            Some((source, target)),
            grid_runtime,
            &mut steps,
        )?;
    }

    if let Some(projected) = target.as_projected() {
        steps.push(CompiledStep::ProjectionForward {
            projection: make_projection(&projected.method(), projected.datum())?,
        });
    }

    Ok(CompiledOperationPipeline {
        steps,
        source_xy_units: PipelineSourceXyUnits::compile(source),
        target_xy_units: PipelineTargetXyUnits::compile(target),
    })
}

fn compile_operation(
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    requested_pair: Option<(&CrsDef, &CrsDef)>,
    grid_runtime: &GridRuntime,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) -> Result<()> {
    let (source_geo, target_geo) =
        resolve_operation_geographic_pair(operation, direction, requested_pair)?;
    match (&operation.method, direction) {
        (OperationMethod::Identity, _) => {}
        (OperationMethod::Helmert { params }, OperationStepDirection::Forward) => {
            params.validate()?;
            steps.push(CompiledStep::GeodeticToGeocentric {
                ellipsoid: source_geo.datum().ellipsoid(),
            });
            steps.push(CompiledStep::Helmert {
                params: *params,
                inverse: false,
            });
            steps.push(CompiledStep::GeocentricToGeodetic {
                ellipsoid: target_geo.datum().ellipsoid(),
            });
        }
        (OperationMethod::Helmert { params }, OperationStepDirection::Reverse) => {
            params.validate()?;
            steps.push(CompiledStep::GeodeticToGeocentric {
                ellipsoid: source_geo.datum().ellipsoid(),
            });
            steps.push(CompiledStep::Helmert {
                params: *params,
                inverse: true,
            });
            steps.push(CompiledStep::GeocentricToGeodetic {
                ellipsoid: target_geo.datum().ellipsoid(),
            });
        }
        (
            OperationMethod::DatumShift {
                source_to_wgs84,
                target_to_wgs84,
            },
            OperationStepDirection::Forward,
        ) => {
            compile_to_wgs84(
                source_to_wgs84,
                source_geo.datum().ellipsoid(),
                grid_runtime,
                steps,
            )?;
            compile_from_wgs84(
                target_to_wgs84,
                target_geo.datum().ellipsoid(),
                grid_runtime,
                steps,
            )?;
        }
        (
            OperationMethod::DatumShift {
                source_to_wgs84,
                target_to_wgs84,
            },
            OperationStepDirection::Reverse,
        ) => {
            compile_to_wgs84(
                target_to_wgs84,
                source_geo.datum().ellipsoid(),
                grid_runtime,
                steps,
            )?;
            compile_from_wgs84(
                source_to_wgs84,
                target_geo.datum().ellipsoid(),
                grid_runtime,
                steps,
            )?;
        }
        (
            OperationMethod::GridShift {
                grid_id,
                direction: grid_direction,
                ..
            },
            step_direction,
        ) => {
            let grid = registry::lookup_grid_definition(grid_id.0).ok_or_else(|| {
                Error::Grid(crate::grid::GridError::NotFound(format!(
                    "grid id {}",
                    grid_id.0
                )))
            })?;
            if grid.format == crate::grid::GridFormat::Unsupported {
                return Err(Error::Grid(crate::grid::GridError::UnsupportedFormat(
                    grid.name,
                )));
            }
            let handle = grid_runtime.resolve_handle(&grid)?;
            let direction = match step_direction {
                OperationStepDirection::Forward => *grid_direction,
                OperationStepDirection::Reverse => grid_direction.inverse(),
            };
            steps.push(CompiledStep::GridShift { handle, direction });
        }
        (OperationMethod::Concatenated { steps: child_steps }, OperationStepDirection::Forward) => {
            for step in child_steps {
                let child = registry::lookup_operation(step.operation_id).ok_or_else(|| {
                    Error::UnknownOperation(format!("unknown operation id {}", step.operation_id.0))
                })?;
                compile_operation(&child, step.direction, None, grid_runtime, steps)?;
            }
        }
        (OperationMethod::Concatenated { steps: child_steps }, OperationStepDirection::Reverse) => {
            for step in child_steps.iter().rev() {
                let child = registry::lookup_operation(step.operation_id).ok_or_else(|| {
                    Error::UnknownOperation(format!("unknown operation id {}", step.operation_id.0))
                })?;
                compile_operation(&child, step.direction.inverse(), None, grid_runtime, steps)?;
            }
        }
        (OperationMethod::Projection { .. }, _) | (OperationMethod::AxisUnitNormalize, _) => {
            return Err(Error::UnsupportedProjection(
                "direct projection operations are not emitted by the embedded selector".into(),
            ));
        }
    }
    Ok(())
}

fn compile_to_wgs84(
    transform: &DatumToWgs84,
    source_ellipsoid: Ellipsoid,
    grid_runtime: &GridRuntime,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) -> Result<()> {
    match transform {
        DatumToWgs84::Identity => Ok(()),
        DatumToWgs84::Helmert(params) => {
            params.validate()?;
            steps.push(CompiledStep::GeodeticToGeocentric {
                ellipsoid: source_ellipsoid,
            });
            steps.push(CompiledStep::Helmert {
                params: *params,
                inverse: false,
            });
            steps.push(CompiledStep::GeocentricToGeodetic {
                ellipsoid: ellipsoid::WGS84,
            });
            Ok(())
        }
        DatumToWgs84::GridShift(grids) => {
            compile_grid_shift_list(grids, GridShiftDirection::Forward, grid_runtime, steps)
        }
        DatumToWgs84::Unknown => Err(Error::OperationSelection(
            "datum has no known path to WGS84".into(),
        )),
    }
}

fn compile_from_wgs84(
    transform: &DatumToWgs84,
    target_ellipsoid: Ellipsoid,
    grid_runtime: &GridRuntime,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) -> Result<()> {
    match transform {
        DatumToWgs84::Identity => Ok(()),
        DatumToWgs84::Helmert(params) => {
            params.validate()?;
            steps.push(CompiledStep::GeodeticToGeocentric {
                ellipsoid: ellipsoid::WGS84,
            });
            steps.push(CompiledStep::Helmert {
                params: *params,
                inverse: true,
            });
            steps.push(CompiledStep::GeocentricToGeodetic {
                ellipsoid: target_ellipsoid,
            });
            Ok(())
        }
        DatumToWgs84::GridShift(grids) => {
            compile_grid_shift_list(grids, GridShiftDirection::Reverse, grid_runtime, steps)
        }
        DatumToWgs84::Unknown => Err(Error::OperationSelection(
            "datum has no known path from WGS84".into(),
        )),
    }
}

fn compile_grid_shift_list(
    grids: &DatumGridShift,
    direction: GridShiftDirection,
    grid_runtime: &GridRuntime,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) -> Result<()> {
    let mut handles = Vec::<GridHandle>::new();
    let mut allow_null = false;
    let mut required_grid_seen = false;

    for entry in grids.entries() {
        match entry {
            DatumGridShiftEntry::Null => {
                allow_null = true;
                break;
            }
            DatumGridShiftEntry::Grid {
                definition,
                optional,
            } => {
                if !optional {
                    required_grid_seen = true;
                }
                match grid_runtime.resolve_handle(definition) {
                    Ok(handle) => handles.push(handle),
                    Err(GridError::Unavailable(_) | GridError::NotFound(_)) if *optional => {}
                    Err(error) => return Err(Error::Grid(error)),
                }
            }
        }
    }

    if handles.is_empty() && !allow_null {
        if required_grid_seen {
            return Err(Error::Grid(GridError::Unavailable(
                "no required datum grid could be loaded".into(),
            )));
        }
        return Err(Error::Grid(GridError::Unavailable(
            "no optional datum grid could be loaded".into(),
        )));
    }

    steps.push(CompiledStep::GridShiftList {
        handles: handles.into_boxed_slice(),
        allow_null,
        direction,
    });
    Ok(())
}

fn resolve_operation_geographic_pair(
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    requested_pair: Option<(&CrsDef, &CrsDef)>,
) -> Result<(CrsDef, CrsDef)> {
    if let (Some(source_code), Some(target_code)) =
        (operation.source_crs_epsg, operation.target_crs_epsg)
    {
        let source = registry::lookup_epsg(match direction {
            OperationStepDirection::Forward => source_code,
            OperationStepDirection::Reverse => target_code,
        })
        .ok_or_else(|| {
            Error::UnknownCrs(format!("unknown EPSG code in operation {}", operation.name))
        })?;
        let target = registry::lookup_epsg(match direction {
            OperationStepDirection::Forward => target_code,
            OperationStepDirection::Reverse => source_code,
        })
        .ok_or_else(|| {
            Error::UnknownCrs(format!("unknown EPSG code in operation {}", operation.name))
        })?;
        return Ok((source, target));
    }

    if let Some((source, target)) = requested_pair {
        return Ok((source.clone(), target.clone()));
    }

    Err(Error::OperationSelection(format!(
        "operation {} is missing source/target CRS metadata",
        operation.name
    )))
}

#[cfg(feature = "rayon")]
pub(super) fn should_parallelize(len: usize) -> bool {
    if len == 0 {
        return false;
    }

    let threads = rayon::current_num_threads().max(1);
    len >= PARALLEL_MIN_TOTAL_ITEMS.max(threads.saturating_mul(PARALLEL_MIN_ITEMS_PER_THREAD))
}

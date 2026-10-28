use crate::coord::{Coord, Coord3D};
use crate::crs::{CrsDef, LinearUnit};
use crate::datum::{DatumGridShift, DatumGridShiftEntry, DatumToWgs84, HelmertParams};
use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::grid::{GridError, GridHandle, GridRuntime};
use crate::helmert;
use crate::operation::{
    CoordinateOperation, CoordinateOperationMetadata, GeocentricAffineParams, GridShiftDirection,
    OperationDomain, OperationMethod, OperationStepDirection, VerticalTransformDiagnostics,
};
use crate::projection::{make_projection, validate_lon_lat, validate_projected, Projection};
use crate::registry;
use crate::selector::SelectedOperationKind;
use crate::{ellipsoid, geocentric};
use smallvec::SmallVec;

#[cfg(feature = "rayon")]
pub(super) const PARALLEL_MIN_TOTAL_ITEMS: usize = 16_384;
#[cfg(feature = "rayon")]
pub(super) const PARALLEL_MIN_ITEMS_PER_THREAD: usize = 4_096;

#[derive(Clone)]
pub(super) struct CompiledOperationPipeline {
    steps: SmallVec<[CompiledStep; 8]>,
    pub(super) source_xy_units: PipelineSourceXyUnits,
    pub(super) target_xy_units: PipelineTargetXyUnits,
    /// True when the steps change ellipsoidal height.
    pub(super) transforms_ellipsoidal_height: bool,
}

#[derive(Clone)]
pub(super) struct CompiledOperationFallback {
    pub(super) operation: SelectedOperationKind,
    pub(super) direction: OperationStepDirection,
    pub(super) metadata: std::sync::Arc<CoordinateOperationMetadata>,
    pub(super) pipeline: CompiledOperationPipeline,
}

#[derive(Clone, Copy)]
pub(super) enum PipelineSourceXyUnits {
    GeographicDegrees,
    GeocentricMeters,
    ProjectedMeters,
    ProjectedNativeToMeters(LinearUnit),
}

#[derive(Clone, Copy)]
pub(super) enum PipelineTargetXyUnits {
    GeographicDegrees,
    GeocentricMeters,
    ProjectedMeters,
    ProjectedMetersToNative(LinearUnit),
}

impl PipelineSourceXyUnits {
    fn compile(source: &CrsDef) -> Self {
        if source.is_geocentric() {
            return Self::GeocentricMeters;
        }
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
            Self::GeocentricMeters => {
                validate_pipeline_coord3d("geocentric input coordinate", coord)?;
                Ok(coord)
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
        if target.is_geocentric() {
            return Self::GeocentricMeters;
        }
        match target.as_projected() {
            Some(projected) if projected.linear_unit_to_meter() == 1.0 => Self::ProjectedMeters,
            Some(projected) => Self::ProjectedMetersToNative(projected.linear_unit()),
            None => Self::GeographicDegrees,
        }
    }

    fn denormalize(self, coord: Coord3D) -> Coord {
        match self {
            Self::GeographicDegrees => Coord::new(coord.x.to_degrees(), coord.y.to_degrees()),
            Self::GeocentricMeters | Self::ProjectedMeters => Coord::new(coord.x, coord.y),
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

#[derive(Clone)]
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
    GeocentricAffine {
        params: GeocentricAffineParams,
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
    /// Save ellipsoidal height (C PROJ `+proj=push +v_3`).
    PushHeight,
    /// Restore the height saved by [`CompiledStep::PushHeight`].
    PopHeight,
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
        CompiledStep::GeocentricAffine { params, inverse } => {
            let (x, y, z) = if *inverse {
                params.inverse(coord.x, coord.y, coord.z)
            } else {
                params.forward(coord.x, coord.y, coord.z)
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
                geocentric::geocentric_to_geodetic(ellipsoid, coord.x, coord.y, coord.z)?;
            Coord3D::new(lon, lat, h)
        }
        CompiledStep::PushHeight | CompiledStep::PopHeight => {
            return Err(Error::InvalidDefinition(
                "height passthrough steps must be executed with a stack".into(),
            ));
        }
    };

    validate_pipeline_coord3d("pipeline step output", result)?;
    Ok(result)
}

fn execute_steps(steps: &[CompiledStep], mut coord: Coord3D) -> Result<Coord3D> {
    let mut height_stack = SmallVec::<[f64; 2]>::new();
    for step in steps {
        coord = match step {
            CompiledStep::PushHeight => {
                height_stack.push(coord.z);
                coord
            }
            CompiledStep::PopHeight => match height_stack.pop() {
                Some(z) => Coord3D::new(coord.x, coord.y, z),
                None => {
                    return Err(Error::InvalidDefinition(
                        "2D height passthrough popped with an empty stack".into(),
                    ));
                }
            },
            other => execute_step(other, coord)?,
        };
    }
    if !height_stack.is_empty() {
        return Err(Error::InvalidDefinition(
            "2D height passthrough left values on the stack".into(),
        ));
    }
    Ok(coord)
}

pub(super) fn execute_pipeline_xy(
    pipeline: &CompiledOperationPipeline,
    c: Coord3D,
) -> Result<Coord> {
    require_xy_pipeline_supported(pipeline)?;

    let state = pipeline.source_xy_units.normalize(c)?;
    if pipeline.steps.is_empty() {
        let output = Coord::new(c.x, c.y);
        validate_pipeline_coord("pipeline final output", output)?;
        return Ok(output);
    }

    let state = execute_steps(&pipeline.steps, state)?;

    let output = pipeline.target_xy_units.denormalize(state);
    validate_pipeline_coord("pipeline final output", output)?;
    Ok(output)
}

fn require_xy_pipeline_supported(pipeline: &CompiledOperationPipeline) -> Result<()> {
    if matches!(
        pipeline.source_xy_units,
        PipelineSourceXyUnits::GeocentricMeters
    ) || matches!(
        pipeline.target_xy_units,
        PipelineTargetXyUnits::GeocentricMeters
    ) {
        return Err(Error::InvalidDefinition(
            "geocentric (ECEF) transforms require convert_3d; the 2D convert API cannot represent Z"
                .into(),
        ));
    }
    Ok(())
}

/// Like [`execute_pipeline_xy`] but keeps the pipeline's `z` (metres).
pub(super) fn execute_pipeline_xyz(
    pipeline: &CompiledOperationPipeline,
    c: Coord3D,
) -> Result<Coord3D> {
    let state = pipeline.source_xy_units.normalize(c)?;
    if pipeline.steps.is_empty() {
        validate_pipeline_coord3d("pipeline final output", c)?;
        return Ok(c);
    }

    let state = execute_steps(&pipeline.steps, state)?;

    let xy = pipeline.target_xy_units.denormalize(state);
    let output = Coord3D::new(xy.x, xy.y, state.z);
    validate_pipeline_coord3d("pipeline final output", output)?;
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

pub(super) fn compile_pipeline(
    source: &CrsDef,
    target: &CrsDef,
    operation: &SelectedOperationKind,
    direction: OperationStepDirection,
    grid_runtime: &GridRuntime,
) -> Result<CompiledOperationPipeline> {
    let mut steps = SmallVec::<[CompiledStep; 8]>::new();

    if let Some(projected) = source.as_projected() {
        steps.push(CompiledStep::ProjectionInverse {
            projection: make_projection(&projected.method(), projected.datum())?,
        });
    } else if let Some(geocentric) = source.as_geocentric() {
        // Frame geocentric endpoints into geodetic radians, like projections.
        steps.push(CompiledStep::GeocentricToGeodetic {
            ellipsoid: geocentric.datum().ellipsoid(),
        });
    }

    match operation {
        SelectedOperationKind::Identity => {}
        SelectedOperationKind::Registry(operation) => {
            compile_operation(
                operation.as_ref().as_ref(),
                direction,
                Some((source, target)),
                grid_runtime,
                &mut steps,
            )?;
        }
        SelectedOperationKind::Custom(operation) => {
            compile_operation(
                operation,
                direction,
                Some((source, target)),
                grid_runtime,
                &mut steps,
            )?;
        }
    }

    if let Some(geocentric) = target.as_geocentric() {
        steps.push(CompiledStep::GeodeticToGeocentric {
            ellipsoid: geocentric.datum().ellipsoid(),
        });
    } else if let Some(projected) = target.as_projected() {
        steps.push(CompiledStep::ProjectionForward {
            projection: make_projection(&projected.method(), projected.datum())?,
        });
    }

    cancel_redundant_geocentric_framing(&mut steps);

    // Geocentric endpoints always transform height via cart framing.
    // Push/pop pairs restore height, so they do not count.
    let transforms_ellipsoidal_height = source.is_geocentric()
        || target.is_geocentric()
        || steps_transform_ellipsoidal_height(&steps);

    Ok(CompiledOperationPipeline {
        steps,
        source_xy_units: PipelineSourceXyUnits::compile(source),
        target_xy_units: PipelineTargetXyUnits::compile(target),
        transforms_ellipsoidal_height,
    })
}

fn steps_transform_ellipsoidal_height(steps: &[CompiledStep]) -> bool {
    let mut passthrough_depth = 0usize;
    for step in steps {
        match step {
            CompiledStep::PushHeight => passthrough_depth += 1,
            CompiledStep::PopHeight => {
                passthrough_depth = passthrough_depth.saturating_sub(1);
            }
            CompiledStep::Helmert { .. }
            | CompiledStep::GeocentricAffine { .. }
            | CompiledStep::GeodeticToGeocentric { .. }
            | CompiledStep::GeocentricToGeodetic { .. }
                if passthrough_depth == 0 =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn ellipsoids_match(a: Ellipsoid, b: Ellipsoid) -> bool {
    (a.semi_major_axis() - b.semi_major_axis()).abs() < 1e-6
        && (a.flattening() - b.flattening()).abs() < 1e-12
}

fn geocentric_framing_cancels(left: &CompiledStep, right: &CompiledStep) -> bool {
    match (left, right) {
        (
            CompiledStep::GeodeticToGeocentric { ellipsoid: a },
            CompiledStep::GeocentricToGeodetic { ellipsoid: b },
        )
        | (
            CompiledStep::GeocentricToGeodetic { ellipsoid: a },
            CompiledStep::GeodeticToGeocentric { ellipsoid: b },
        ) => ellipsoids_match(*a, *b),
        _ => false,
    }
}

/// Drop adjacent geodetic to ECEF pairs on the same ellipsoid.
///
/// Helmert sandwiches already enter and leave geodetic space, so extra
/// geocentric framing next to them is redundant.
fn cancel_redundant_geocentric_framing(steps: &mut SmallVec<[CompiledStep; 8]>) {
    loop {
        let mut removed = false;
        let mut i = 0;
        while i + 1 < steps.len() {
            if geocentric_framing_cancels(&steps[i], &steps[i + 1]) {
                steps.remove(i + 1);
                steps.remove(i);
                removed = true;
            } else {
                i += 1;
            }
        }
        if !removed {
            break;
        }
    }
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
    let preserve_height = preserve_ellipsoidal_height(operation.domain, requested_pair);
    match (&operation.method, direction) {
        (OperationMethod::Identity, _) => {}
        (OperationMethod::Helmert { params }, OperationStepDirection::Forward) => {
            params.validate()?;
            compile_geocentric_sandwich(
                source_geo.datum().ellipsoid(),
                target_geo.datum().ellipsoid(),
                preserve_height,
                CompiledStep::Helmert {
                    params: *params,
                    inverse: false,
                },
                steps,
            );
        }
        (OperationMethod::Helmert { params }, OperationStepDirection::Reverse) => {
            params.validate()?;
            compile_geocentric_sandwich(
                source_geo.datum().ellipsoid(),
                target_geo.datum().ellipsoid(),
                preserve_height,
                CompiledStep::Helmert {
                    params: *params,
                    inverse: true,
                },
                steps,
            );
        }
        (OperationMethod::GeocentricAffine { params }, direction) => {
            params.validate()?;
            compile_geocentric_sandwich(
                source_geo.datum().ellipsoid(),
                target_geo.datum().ellipsoid(),
                preserve_height,
                CompiledStep::GeocentricAffine {
                    params: *params,
                    inverse: matches!(direction, OperationStepDirection::Reverse),
                },
                steps,
            );
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
                preserve_height,
                grid_runtime,
                steps,
            )?;
            compile_from_wgs84(
                target_to_wgs84,
                target_geo.datum().ellipsoid(),
                preserve_height,
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
                preserve_height,
                grid_runtime,
                steps,
            )?;
            compile_from_wgs84(
                source_to_wgs84,
                target_geo.datum().ellipsoid(),
                preserve_height,
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
                compile_operation(&child, step.direction, requested_pair, grid_runtime, steps)?;
            }
        }
        (OperationMethod::Concatenated { steps: child_steps }, OperationStepDirection::Reverse) => {
            for step in child_steps.iter().rev() {
                let child = registry::lookup_operation(step.operation_id).ok_or_else(|| {
                    Error::UnknownOperation(format!("unknown operation id {}", step.operation_id.0))
                })?;
                compile_operation(
                    &child,
                    step.direction.inverse(),
                    requested_pair,
                    grid_runtime,
                    steps,
                )?;
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

/// Preserve height when the operation is horizontal-only and at least one
/// requested endpoint has no ellipsoidal height: 2D, or a gravity-related
/// compound whose height the vertical transform owns.
fn preserve_ellipsoidal_height(
    domain: OperationDomain,
    requested_pair: Option<(&CrsDef, &CrsDef)>,
) -> bool {
    domain == OperationDomain::HorizontalOnly
        && requested_pair.is_some_and(|(source, target)| {
            source.operation_domain() == OperationDomain::HorizontalOnly
                || target.operation_domain() == OperationDomain::HorizontalOnly
        })
}

/// True for a zero-parameter Helmert that C PROJ compiles to `+proj=noop`:
/// same ellipsoid on both sides, or GRS80 and WGS 84 (e.g. EPSG:1149
/// ETRS89 to WGS 84). Other ellipsoid pairs keep the geocentric round trip.
fn is_null_helmert(step: &CompiledStep, source: Ellipsoid, target: Ellipsoid) -> bool {
    let grs80_or_wgs84 = |e: Ellipsoid| {
        ellipsoids_match(e, ellipsoid::GRS80) || ellipsoids_match(e, ellipsoid::WGS84)
    };
    matches!(step, CompiledStep::Helmert { params, .. } if *params == HelmertParams::identity())
        && (ellipsoids_match(source, target) || (grs80_or_wgs84(source) && grs80_or_wgs84(target)))
}

fn compile_geocentric_sandwich(
    source_ellipsoid: Ellipsoid,
    target_ellipsoid: Ellipsoid,
    preserve_height: bool,
    step: CompiledStep,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) {
    if is_null_helmert(&step, source_ellipsoid, target_ellipsoid) {
        return;
    }
    if preserve_height {
        steps.push(CompiledStep::PushHeight);
    }
    steps.push(CompiledStep::GeodeticToGeocentric {
        ellipsoid: source_ellipsoid,
    });
    steps.push(step);
    steps.push(CompiledStep::GeocentricToGeodetic {
        ellipsoid: target_ellipsoid,
    });
    if preserve_height {
        steps.push(CompiledStep::PopHeight);
    }
}

fn compile_to_wgs84(
    transform: &DatumToWgs84,
    source_ellipsoid: Ellipsoid,
    preserve_height: bool,
    grid_runtime: &GridRuntime,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) -> Result<()> {
    match transform {
        DatumToWgs84::Identity => Ok(()),
        DatumToWgs84::Helmert(params) => {
            params.validate()?;
            compile_geocentric_sandwich(
                source_ellipsoid,
                ellipsoid::WGS84,
                preserve_height,
                CompiledStep::Helmert {
                    params: *params,
                    inverse: false,
                },
                steps,
            );
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
    preserve_height: bool,
    grid_runtime: &GridRuntime,
    steps: &mut SmallVec<[CompiledStep; 8]>,
) -> Result<()> {
    match transform {
        DatumToWgs84::Identity => Ok(()),
        DatumToWgs84::Helmert(params) => {
            params.validate()?;
            compile_geocentric_sandwich(
                ellipsoid::WGS84,
                target_ellipsoid,
                preserve_height,
                CompiledStep::Helmert {
                    params: *params,
                    inverse: true,
                },
                steps,
            );
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

#[cfg(test)]
mod framing_tests {
    use super::*;
    use crate::ellipsoid;

    #[test]
    fn cancel_adjacent_same_ellipsoid_cart_pair() {
        let mut steps = SmallVec::<[CompiledStep; 8]>::new();
        steps.push(CompiledStep::GeocentricToGeodetic {
            ellipsoid: ellipsoid::WGS84,
        });
        steps.push(CompiledStep::GeodeticToGeocentric {
            ellipsoid: ellipsoid::WGS84,
        });
        cancel_redundant_geocentric_framing(&mut steps);
        assert!(steps.is_empty());
    }

    #[test]
    fn keep_cart_pair_on_different_ellipsoids() {
        let mut steps = SmallVec::<[CompiledStep; 8]>::new();
        steps.push(CompiledStep::GeodeticToGeocentric {
            ellipsoid: ellipsoid::CLARKE1866,
        });
        steps.push(CompiledStep::GeocentricToGeodetic {
            ellipsoid: ellipsoid::WGS84,
        });
        cancel_redundant_geocentric_framing(&mut steps);
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn push_pop_restores_height_and_does_not_count_as_transforming_it() {
        let mut steps = SmallVec::<[CompiledStep; 8]>::new();
        steps.push(CompiledStep::PushHeight);
        steps.push(CompiledStep::GeodeticToGeocentric {
            ellipsoid: ellipsoid::WGS84,
        });
        steps.push(CompiledStep::GeocentricToGeodetic {
            ellipsoid: ellipsoid::WGS84,
        });
        steps.push(CompiledStep::PopHeight);
        assert!(!steps_transform_ellipsoidal_height(&steps));

        let input = Coord3D::new(0.1, 0.9, 43.0);
        let output = execute_steps(&steps, input).unwrap();
        assert!((output.z - 43.0).abs() < 1e-12);
    }

    #[test]
    fn push_pop_only_when_a_requested_endpoint_has_no_ellipsoidal_height() {
        let geographic = CrsDef::Geographic(crate::crs::GeographicCrsDef::new(
            4326,
            crate::datum::WGS84,
            "WGS 84",
        ));
        let geocentric = CrsDef::Geocentric(crate::crs::GeocentricCrsDef::new(
            4978,
            4326,
            crate::datum::WGS84,
            "WGS 84",
        ));
        let geographic_3d = registry::lookup_epsg(4979).unwrap();
        let gravity_compound = registry::lookup_epsg(7415).unwrap();
        assert!(preserve_ellipsoidal_height(
            OperationDomain::HorizontalOnly,
            Some((&geographic, &geocentric)),
        ));
        // A geoid grid owns the height of a gravity-related compound.
        assert!(preserve_ellipsoidal_height(
            OperationDomain::HorizontalOnly,
            Some((&geographic_3d, &gravity_compound)),
        ));
        assert!(!preserve_ellipsoidal_height(
            OperationDomain::HorizontalOnly,
            Some((&geocentric, &geocentric)),
        ));
        assert!(!preserve_ellipsoidal_height(
            OperationDomain::HorizontalOnly,
            Some((&geographic_3d, &geographic_3d)),
        ));
        assert!(!preserve_ellipsoidal_height(
            OperationDomain::IncludesHeight,
            Some((&geographic, &geographic)),
        ));
        assert!(!preserve_ellipsoidal_height(
            OperationDomain::HorizontalOnly,
            None,
        ));
    }

    fn sandwich(source: Ellipsoid, target: Ellipsoid, params: HelmertParams) -> usize {
        let mut steps = SmallVec::<[CompiledStep; 8]>::new();
        compile_geocentric_sandwich(
            source,
            target,
            false,
            CompiledStep::Helmert {
                params,
                inverse: true,
            },
            &mut steps,
        );
        steps.len()
    }

    #[test]
    fn null_helmert_between_grs80_and_wgs84_compiles_to_noop() {
        let null = HelmertParams::identity();
        assert_eq!(sandwich(ellipsoid::GRS80, ellipsoid::WGS84, null), 0);
        assert_eq!(sandwich(ellipsoid::WGS84, ellipsoid::GRS80, null), 0);
        assert_eq!(
            sandwich(ellipsoid::BESSEL1841, ellipsoid::BESSEL1841, null),
            0
        );
    }

    #[test]
    fn null_helmert_keeps_cart_framing_across_distinct_ellipsoids() {
        let null = HelmertParams::identity();
        assert_eq!(sandwich(ellipsoid::BESSEL1841, ellipsoid::WGS84, null), 3);
        let shift = HelmertParams::translation(1.0, 0.0, 0.0).unwrap();
        assert_eq!(sandwich(ellipsoid::GRS80, ellipsoid::WGS84, shift), 3);
    }

    #[test]
    fn unwrapped_cart_counts_as_transforming_height() {
        let mut steps = SmallVec::<[CompiledStep; 8]>::new();
        steps.push(CompiledStep::GeodeticToGeocentric {
            ellipsoid: ellipsoid::WGS84,
        });
        assert!(steps_transform_ellipsoidal_height(&steps));
    }
}

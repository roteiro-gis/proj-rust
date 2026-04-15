use crate::coord::{Bounds, Coord, Coord3D, Transformable, Transformable3D};
use crate::crs::CrsDef;
use crate::datum::HelmertParams;
use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::geocentric;
use crate::grid::{GridHandle, GridRuntime};
use crate::helmert;
use crate::operation::{
    CoordinateOperation, CoordinateOperationId, CoordinateOperationMetadata, GridShiftDirection,
    OperationMethod, OperationSelectionDiagnostics, OperationStepDirection, SelectionOptions,
    SelectionPolicy, SkippedOperation, SkippedOperationReason,
};
use crate::projection::{make_projection, Projection};
use crate::registry;
use crate::selector;
use smallvec::SmallVec;

#[cfg(feature = "rayon")]
const PARALLEL_MIN_TOTAL_ITEMS: usize = 16_384;
#[cfg(feature = "rayon")]
const PARALLEL_MIN_ITEMS_PER_THREAD: usize = 4_096;
#[cfg(feature = "rayon")]
const PARALLEL_CHUNKS_PER_THREAD: usize = 4;
#[cfg(feature = "rayon")]
const PARALLEL_MIN_CHUNK_SIZE: usize = 1_024;

/// A reusable coordinate transformation between two CRS.
pub struct Transform {
    source: CrsDef,
    target: CrsDef,
    selected_operation_definition: CoordinateOperation,
    selected_direction: OperationStepDirection,
    selected_operation: CoordinateOperationMetadata,
    diagnostics: OperationSelectionDiagnostics,
    selection_options: SelectionOptions,
    pipeline: CompiledOperationPipeline,
}

struct CompiledOperationPipeline {
    steps: SmallVec<[CompiledStep; 8]>,
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
    GeodeticToGeocentric {
        ellipsoid: Ellipsoid,
    },
    GeocentricToGeodetic {
        ellipsoid: Ellipsoid,
    },
}

impl Transform {
    /// Create a transform from authority code strings (e.g., `"EPSG:4326"`).
    pub fn new(from_crs: &str, to_crs: &str) -> Result<Self> {
        Self::with_selection_options(from_crs, to_crs, SelectionOptions::default())
    }

    /// Create a transform with explicit selection options.
    pub fn with_selection_options(
        from_crs: &str,
        to_crs: &str,
        options: SelectionOptions,
    ) -> Result<Self> {
        let source = registry::lookup_authority_code(from_crs)?;
        let target = registry::lookup_authority_code(to_crs)?;
        Self::from_crs_defs_with_selection_options(&source, &target, options)
    }

    /// Create a transform from an explicit registry operation id.
    pub fn from_operation(
        operation_id: CoordinateOperationId,
        from_crs: &str,
        to_crs: &str,
    ) -> Result<Self> {
        Self::with_selection_options(
            from_crs,
            to_crs,
            SelectionOptions {
                policy: SelectionPolicy::Operation(operation_id),
                ..SelectionOptions::default()
            },
        )
    }

    /// Create a transform from EPSG codes directly.
    pub fn from_epsg(from: u32, to: u32) -> Result<Self> {
        let source = registry::lookup_epsg(from)
            .ok_or_else(|| Error::UnknownCrs(format!("unknown EPSG code: {from}")))?;
        let target = registry::lookup_epsg(to)
            .ok_or_else(|| Error::UnknownCrs(format!("unknown EPSG code: {to}")))?;
        Self::from_crs_defs(&source, &target)
    }

    /// Create a transform from explicit CRS definitions.
    pub fn from_crs_defs(from: &CrsDef, to: &CrsDef) -> Result<Self> {
        Self::from_crs_defs_with_selection_options(from, to, SelectionOptions::default())
    }

    fn from_crs_defs_with_selection_options(
        from: &CrsDef,
        to: &CrsDef,
        options: SelectionOptions,
    ) -> Result<Self> {
        let grid_runtime = GridRuntime::new(options.grid_provider.clone());
        let candidate_set = selector::rank_operation_candidates(from, to, &options)?;
        if candidate_set.ranked.is_empty() {
            return Err(match options.policy {
                SelectionPolicy::Operation(id) => match registry::lookup_operation(id) {
                    Some(_) => Error::OperationSelection(format!(
                        "operation id {} is not compatible with source EPSG:{} target EPSG:{}",
                        id.0,
                        from.epsg(),
                        to.epsg()
                    )),
                    None => Error::UnknownOperation(format!("unknown operation id {}", id.0)),
                },
                _ => Error::OperationSelection(format!(
                    "no compatible operation found for source EPSG:{} target EPSG:{}",
                    from.epsg(),
                    to.epsg()
                )),
            });
        }

        let mut skipped_operations = candidate_set.skipped;
        let mut missing_required_grid = None;
        for (index, candidate) in candidate_set.ranked.iter().enumerate() {
            match compile_pipeline(
                from,
                to,
                candidate.operation.as_ref(),
                candidate.direction,
                &grid_runtime,
            ) {
                Ok(pipeline) => {
                    let metadata = selected_metadata(
                        candidate.operation.as_ref(),
                        candidate.direction,
                        candidate.matched_area_of_use.clone(),
                    );
                    let selected_reasons =
                        selected_reasons_for(candidate, &candidate_set.ranked[index + 1..]);
                    skipped_operations.extend(candidate_set.ranked[index + 1..].iter().map(
                        |other| {
                            skipped_for_unselected_candidate(other, !candidate.operation.deprecated)
                        },
                    ));
                    let diagnostics = OperationSelectionDiagnostics {
                        selected_operation: metadata.clone(),
                        selected_match_kind: candidate.match_kind,
                        selected_reasons,
                        skipped_operations,
                        approximate: candidate.operation.approximate,
                        missing_required_grid,
                    };
                    return Ok(Self {
                        source: *from,
                        target: *to,
                        selected_operation_definition: candidate.operation.clone().into_owned(),
                        selected_direction: candidate.direction,
                        selected_operation: metadata,
                        diagnostics,
                        selection_options: options,
                        pipeline,
                    });
                }
                Err(Error::Grid(error)) => {
                    if missing_required_grid.is_none() {
                        missing_required_grid = Some(error.to_string());
                    }
                    skipped_operations.push(SkippedOperation {
                        metadata: selected_metadata(
                            candidate.operation.as_ref(),
                            candidate.direction,
                            candidate.matched_area_of_use.clone(),
                        ),
                        reason: match error {
                            crate::grid::GridError::UnsupportedFormat(_) => {
                                SkippedOperationReason::UnsupportedGridFormat
                            }
                            _ => SkippedOperationReason::MissingGrid,
                        },
                        detail: error.to_string(),
                    });
                }
                Err(error) => {
                    skipped_operations.push(SkippedOperation {
                        metadata: selected_metadata(
                            candidate.operation.as_ref(),
                            candidate.direction,
                            candidate.matched_area_of_use.clone(),
                        ),
                        reason: SkippedOperationReason::LessPreferred,
                        detail: error.to_string(),
                    });
                }
            }
        }

        if let Some(message) = missing_required_grid {
            return Err(Error::OperationSelection(format!(
                "better operations were skipped because required grids were unavailable: {message}"
            )));
        }

        Err(Error::OperationSelection(format!(
            "unable to compile an operation for source EPSG:{} target EPSG:{}",
            from.epsg(),
            to.epsg()
        )))
    }

    /// Transform a single coordinate.
    pub fn convert<T: Transformable>(&self, coord: T) -> Result<T> {
        let c = coord.into_coord();
        let result = self.convert_coord(c)?;
        Ok(T::from_coord(result))
    }

    /// Transform a single 3D coordinate.
    pub fn convert_3d<T: Transformable3D>(&self, coord: T) -> Result<T> {
        let c = coord.into_coord3d();
        let result = self.convert_coord3d(c)?;
        Ok(T::from_coord3d(result))
    }

    /// Return the source CRS definition for this transform.
    pub fn source_crs(&self) -> &CrsDef {
        &self.source
    }

    /// Return the target CRS definition for this transform.
    pub fn target_crs(&self) -> &CrsDef {
        &self.target
    }

    /// Return metadata for the selected coordinate operation.
    pub fn selected_operation(&self) -> &CoordinateOperationMetadata {
        &self.selected_operation
    }

    /// Return selection diagnostics for this transform.
    pub fn selection_diagnostics(&self) -> &OperationSelectionDiagnostics {
        &self.diagnostics
    }

    /// Build the inverse transform by swapping the source and target CRS.
    pub fn inverse(&self) -> Result<Self> {
        let grid_runtime = GridRuntime::new(self.selection_options.grid_provider.clone());
        let selected_direction = self.selected_direction.inverse();
        let pipeline = compile_pipeline(
            &self.target,
            &self.source,
            &self.selected_operation_definition,
            selected_direction,
            &grid_runtime,
        )?;
        let selected_operation = selected_metadata(
            &self.selected_operation_definition,
            selected_direction,
            self.selected_operation.area_of_use.clone(),
        );
        let diagnostics = OperationSelectionDiagnostics {
            selected_operation: selected_operation.clone(),
            selected_match_kind: self.diagnostics.selected_match_kind,
            selected_reasons: self.diagnostics.selected_reasons.clone(),
            skipped_operations: Vec::new(),
            approximate: self.diagnostics.approximate,
            missing_required_grid: self.diagnostics.missing_required_grid.clone(),
        };
        Ok(Self {
            source: self.target,
            target: self.source,
            selected_operation_definition: self.selected_operation_definition.clone(),
            selected_direction,
            selected_operation,
            diagnostics,
            selection_options: self.selection_options.inverse(),
            pipeline,
        })
    }

    /// Reproject a 2D bounding box by sampling its perimeter.
    pub fn transform_bounds(&self, bounds: Bounds, densify_points: usize) -> Result<Bounds> {
        if !bounds.is_valid() {
            return Err(Error::OutOfRange(
                "bounds must be finite and satisfy min <= max".into(),
            ));
        }

        let segments = densify_points
            .checked_add(1)
            .ok_or_else(|| Error::OutOfRange("densify point count is too large".into()))?;

        let mut transformed: Option<Bounds> = None;
        for i in 0..=segments {
            let t = i as f64 / segments as f64;
            let x = bounds.min_x + bounds.width() * t;
            let y = bounds.min_y + bounds.height() * t;

            for sample in [
                Coord::new(x, bounds.min_y),
                Coord::new(x, bounds.max_y),
                Coord::new(bounds.min_x, y),
                Coord::new(bounds.max_x, y),
            ] {
                let coord = self.convert_coord(sample)?;
                if let Some(accum) = &mut transformed {
                    accum.expand_to_include(coord);
                } else {
                    transformed = Some(Bounds::new(coord.x, coord.y, coord.x, coord.y));
                }
            }
        }

        transformed.ok_or_else(|| Error::OutOfRange("failed to sample bounds".into()))
    }

    fn convert_coord(&self, c: Coord) -> Result<Coord> {
        let result = self.convert_coord3d(Coord3D::new(c.x, c.y, 0.0))?;
        Ok(Coord::new(result.x, result.y))
    }

    fn convert_coord3d(&self, c: Coord3D) -> Result<Coord3D> {
        if self.pipeline.steps.is_empty() {
            return Ok(c);
        }

        let preserved_z = c.z;
        let mut state = if self.source.is_projected() {
            let (x_m, y_m) = self.source_projected_native_to_meters(c.x, c.y);
            Coord3D::new(x_m, y_m, 0.0)
        } else {
            Coord3D::new(c.x.to_radians(), c.y.to_radians(), 0.0)
        };

        for step in &self.pipeline.steps {
            state = execute_step(step, state)?;
        }

        let (x, y) = if self.target.is_projected() {
            self.projected_meters_to_target_native(state.x, state.y)
        } else {
            (state.x.to_degrees(), state.y.to_degrees())
        };

        Ok(Coord3D::new(x, y, preserved_z))
    }

    fn source_projected_native_to_meters(&self, x: f64, y: f64) -> (f64, f64) {
        match self.source {
            CrsDef::Projected(projected) => (
                projected.linear_unit().to_meters(x),
                projected.linear_unit().to_meters(y),
            ),
            CrsDef::Geographic(_) => (x, y),
        }
    }

    fn projected_meters_to_target_native(&self, x: f64, y: f64) -> (f64, f64) {
        match self.target {
            CrsDef::Projected(projected) => (
                projected.linear_unit().from_meters(x),
                projected.linear_unit().from_meters(y),
            ),
            CrsDef::Geographic(_) => (x, y),
        }
    }

    /// Batch transform (sequential).
    pub fn convert_batch<T: Transformable + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert(c.clone())).collect()
    }

    /// Batch transform of 3D coordinates (sequential).
    pub fn convert_batch_3d<T: Transformable3D + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert_3d(c.clone())).collect()
    }

    /// Batch transform with Rayon parallelism.
    #[cfg(feature = "rayon")]
    pub fn convert_batch_parallel<T: Transformable + Send + Sync + Clone>(
        &self,
        coords: &[T],
    ) -> Result<Vec<T>> {
        self.convert_batch_parallel_adaptive(coords, |this, chunk| this.convert_batch(chunk))
    }

    /// Batch transform of 3D coordinates with adaptive Rayon parallelism.
    #[cfg(feature = "rayon")]
    pub fn convert_batch_parallel_3d<T: Transformable3D + Send + Sync + Clone>(
        &self,
        coords: &[T],
    ) -> Result<Vec<T>> {
        self.convert_batch_parallel_adaptive(coords, |this, chunk| this.convert_batch_3d(chunk))
    }

    #[cfg(feature = "rayon")]
    fn convert_batch_parallel_adaptive<T, F>(&self, coords: &[T], convert: F) -> Result<Vec<T>>
    where
        T: Send + Sync + Clone,
        F: Fn(&Self, &[T]) -> Result<Vec<T>> + Sync,
    {
        if !should_parallelize(coords.len()) {
            return convert(self, coords);
        }

        use rayon::prelude::*;

        let chunk_size = parallel_chunk_size(coords.len());
        let chunk_results: Vec<Result<Vec<T>>> = coords
            .par_chunks(chunk_size)
            .map(|chunk| convert(self, chunk))
            .collect();

        let mut results = Vec::with_capacity(coords.len());
        for chunk in chunk_results {
            results.extend(chunk?);
        }
        Ok(results)
    }
}

fn selected_metadata(
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    matched_area_of_use: Option<crate::operation::AreaOfUse>,
) -> CoordinateOperationMetadata {
    let mut metadata = operation.metadata_for_direction(direction);
    metadata.area_of_use = matched_area_of_use.or_else(|| operation.areas_of_use.first().cloned());
    metadata
}

fn selected_reasons_for(
    selected: &selector::RankedOperationCandidate,
    alternatives: &[selector::RankedOperationCandidate],
) -> SmallVec<[crate::operation::SelectionReason; 4]> {
    let mut reasons = selected.reasons.clone();
    if selected_accuracy_preferred(selected, alternatives)
        && !reasons.contains(&crate::operation::SelectionReason::AccuracyPreferred)
    {
        reasons.push(crate::operation::SelectionReason::AccuracyPreferred);
    }
    reasons
}

fn selected_accuracy_preferred(
    selected: &selector::RankedOperationCandidate,
    alternatives: &[selector::RankedOperationCandidate],
) -> bool {
    let Some(selected_accuracy) = selected.operation.accuracy.map(|value| value.meters) else {
        return false;
    };

    alternatives.iter().any(|alternative| {
        same_pre_accuracy_priority(selected, alternative)
            && alternative
                .operation
                .accuracy
                .map(|value| selected_accuracy < value.meters)
                .unwrap_or(false)
    })
}

fn same_pre_accuracy_priority(
    left: &selector::RankedOperationCandidate,
    right: &selector::RankedOperationCandidate,
) -> bool {
    match_kind_priority(left.match_kind) == match_kind_priority(right.match_kind)
        && left.matched_area_of_use.is_some() == right.matched_area_of_use.is_some()
}

fn match_kind_priority(kind: crate::operation::OperationMatchKind) -> u8 {
    match kind {
        crate::operation::OperationMatchKind::Explicit => 4,
        crate::operation::OperationMatchKind::ExactSourceTarget => 3,
        crate::operation::OperationMatchKind::DerivedGeographic => 2,
        crate::operation::OperationMatchKind::DatumCompatible => 1,
        crate::operation::OperationMatchKind::ApproximateFallback => 0,
    }
}

fn skipped_for_unselected_candidate(
    candidate: &selector::RankedOperationCandidate,
    prefer_non_deprecated: bool,
) -> SkippedOperation {
    let reason = if prefer_non_deprecated && candidate.operation.deprecated {
        SkippedOperationReason::Deprecated
    } else {
        SkippedOperationReason::LessPreferred
    };
    let detail = match reason {
        SkippedOperationReason::Deprecated => {
            "not selected because a non-deprecated higher-ranked operation compiled successfully"
                .into()
        }
        _ => "not selected because a higher-ranked operation compiled successfully".into(),
    };
    SkippedOperation {
        metadata: selected_metadata(
            candidate.operation.as_ref(),
            candidate.direction,
            candidate.matched_area_of_use.clone(),
        ),
        reason,
        detail,
    }
}

fn execute_step(step: &CompiledStep, coord: Coord3D) -> Result<Coord3D> {
    match step {
        CompiledStep::ProjectionForward { projection } => {
            let (x, y) = projection.forward(coord.x, coord.y)?;
            Ok(Coord3D::new(x, y, coord.z))
        }
        CompiledStep::ProjectionInverse { projection } => {
            let (lon, lat) = projection.inverse(coord.x, coord.y)?;
            Ok(Coord3D::new(lon, lat, coord.z))
        }
        CompiledStep::Helmert { params, inverse } => {
            let (x, y, z) = if *inverse {
                helmert::helmert_inverse(params, coord.x, coord.y, coord.z)
            } else {
                helmert::helmert_forward(params, coord.x, coord.y, coord.z)
            };
            Ok(Coord3D::new(x, y, z))
        }
        CompiledStep::GridShift { handle, direction } => {
            let (lon, lat) = handle.apply(coord.x, coord.y, *direction)?;
            Ok(Coord3D::new(lon, lat, coord.z))
        }
        CompiledStep::GeodeticToGeocentric { ellipsoid } => {
            let (x, y, z) =
                geocentric::geodetic_to_geocentric(ellipsoid, coord.x, coord.y, coord.z);
            Ok(Coord3D::new(x, y, z))
        }
        CompiledStep::GeocentricToGeodetic { ellipsoid } => {
            let (lon, lat, h) =
                geocentric::geocentric_to_geodetic(ellipsoid, coord.x, coord.y, coord.z);
            Ok(Coord3D::new(lon, lat, h))
        }
    }
}

fn compile_pipeline(
    source: &CrsDef,
    target: &CrsDef,
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    grid_runtime: &GridRuntime,
) -> Result<CompiledOperationPipeline> {
    let mut steps = SmallVec::<[CompiledStep; 8]>::new();

    if let CrsDef::Projected(projected) = source {
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

    if let CrsDef::Projected(projected) = target {
        steps.push(CompiledStep::ProjectionForward {
            projection: make_projection(&projected.method(), projected.datum())?,
        });
    }

    Ok(CompiledOperationPipeline { steps })
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
            steps.push(CompiledStep::GeodeticToGeocentric {
                ellipsoid: source_geo.datum().ellipsoid,
            });
            steps.push(CompiledStep::Helmert {
                params: *params,
                inverse: false,
            });
            steps.push(CompiledStep::GeocentricToGeodetic {
                ellipsoid: target_geo.datum().ellipsoid,
            });
        }
        (OperationMethod::Helmert { params }, OperationStepDirection::Reverse) => {
            steps.push(CompiledStep::GeodeticToGeocentric {
                ellipsoid: source_geo.datum().ellipsoid,
            });
            steps.push(CompiledStep::Helmert {
                params: *params,
                inverse: true,
            });
            steps.push(CompiledStep::GeocentricToGeodetic {
                ellipsoid: target_geo.datum().ellipsoid,
            });
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
        return Ok(match direction {
            OperationStepDirection::Forward => (*source, *target),
            OperationStepDirection::Reverse => (*target, *source),
        });
    }

    Err(Error::OperationSelection(format!(
        "operation {} is missing source/target CRS metadata",
        operation.name
    )))
}

#[cfg(feature = "rayon")]
fn should_parallelize(len: usize) -> bool {
    if len == 0 {
        return false;
    }

    let threads = rayon::current_num_threads().max(1);
    len >= PARALLEL_MIN_TOTAL_ITEMS.max(threads.saturating_mul(PARALLEL_MIN_ITEMS_PER_THREAD))
}

#[cfg(feature = "rayon")]
fn parallel_chunk_size(len: usize) -> usize {
    let threads = rayon::current_num_threads().max(1);
    let target_chunks = threads.saturating_mul(PARALLEL_CHUNKS_PER_THREAD).max(1);
    let chunk_size = len.div_ceil(target_chunks);
    chunk_size.max(PARALLEL_MIN_CHUNK_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crs::{CrsDef, GeographicCrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod};
    use crate::datum::{self, DatumToWgs84};
    use crate::operation::{
        AreaOfInterest, OperationMatchKind, SelectionPolicy, SelectionReason,
        SkippedOperationReason,
    };

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

    #[test]
    fn identity_same_crs() {
        let t = Transform::new("EPSG:4326", "EPSG:4326").unwrap();
        let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
        assert_eq!(x, -74.006);
        assert_eq!(y, 40.7128);
    }

    #[test]
    fn wgs84_to_web_mercator() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0, "x = {x}");
        assert!((y - 4970072.0).abs() < 100.0, "y = {y}");
    }

    #[test]
    fn web_mercator_to_wgs84() {
        let t = Transform::new("EPSG:3857", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-8238310.0, 4970072.0)).unwrap();
        assert!((lon - (-74.006)).abs() < 0.001, "lon = {lon}");
        assert!((lat - 40.7128).abs() < 0.001, "lat = {lat}");
    }

    #[test]
    fn roundtrip_4326_3857() {
        let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let inv = fwd.inverse().unwrap();

        let original = (-74.0445, 40.6892);
        let projected = fwd.convert(original).unwrap();
        let back = inv.convert(projected).unwrap();

        assert!((back.0 - original.0).abs() < 1e-8);
        assert!((back.1 - original.1).abs() < 1e-8);
    }

    #[test]
    fn wgs84_to_utm_18n() {
        let t = Transform::new("EPSG:4326", "EPSG:32618").unwrap();
        let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - 583960.0).abs() < 1.0, "easting = {x}");
        assert!(y > 4_500_000.0 && y < 4_510_000.0, "northing = {y}");
    }

    #[test]
    fn equivalent_meter_and_foot_state_plane_crs_match_after_unit_conversion() {
        let coord = (-80.8431, 35.2271);
        let meter_tx = Transform::new("EPSG:4326", "EPSG:32119").unwrap();
        let foot_tx = Transform::new("EPSG:4326", "EPSG:2264").unwrap();

        let (mx, my) = meter_tx.convert(coord).unwrap();
        let (fx, fy) = foot_tx.convert(coord).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02);
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02);
    }

    #[test]
    fn inverse_transform_accepts_native_projected_units_for_foot_crs() {
        let coord = (-80.8431, 35.2271);
        let forward = Transform::new("EPSG:4326", "EPSG:2264").unwrap();
        let inverse = Transform::new("EPSG:2264", "EPSG:4326").unwrap();

        let projected = forward.convert(coord).unwrap();
        let roundtrip = inverse.convert(projected).unwrap();

        assert!((roundtrip.0 - coord.0).abs() < 1e-8);
        assert!((roundtrip.1 - coord.1).abs() < 1e-8);
    }

    #[test]
    fn utm_to_web_mercator() {
        let t = Transform::new("EPSG:32618", "EPSG:3857").unwrap();
        let (x, _y) = t.convert((583960.0, 4507523.0)).unwrap();
        assert!((x - (-8238310.0)).abs() < 200.0, "x = {x}");
    }

    #[test]
    fn wgs84_to_polar_stereo_3413() {
        let t = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
        let (x, y) = t.convert((-45.0, 90.0)).unwrap();
        assert!(x.abs() < 1.0, "x = {x}");
        assert!(y.abs() < 1.0, "y = {y}");
    }

    #[test]
    fn roundtrip_4326_3413() {
        let fwd = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
        let inv = fwd.inverse().unwrap();

        let original = (-45.0, 75.0);
        let projected = fwd.convert(original).unwrap();
        let back = inv.convert(projected).unwrap();

        assert!((back.0 - original.0).abs() < 1e-6);
        assert!((back.1 - original.1).abs() < 1e-6);
    }

    #[test]
    fn geographic_to_geographic_same_datum_is_identity() {
        let t = Transform::new("EPSG:4269", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-74.006, 40.7128)).unwrap();
        assert_eq!(lon, -74.006);
        assert_eq!(lat, 40.7128);
        assert_eq!(t.selected_operation().name, "Identity");
    }

    #[test]
    fn unknown_crs_error() {
        let result = Transform::new("EPSG:99999", "EPSG:4326");
        assert!(result.is_err());
    }

    #[test]
    fn cross_datum_nad27_to_wgs84() {
        let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-90.0, 45.0)).unwrap();
        assert!((lon - (-90.0)).abs() < 0.01, "lon = {lon}");
        assert!((lat - 45.0).abs() < 0.01, "lat = {lat}");
        assert!(!t.selected_operation().approximate);
    }

    #[test]
    fn explicit_grid_operation_compiles() {
        let t = Transform::from_operation(CoordinateOperationId(1693), "EPSG:4267", "EPSG:4326")
            .unwrap();
        assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1693)));
    }

    #[test]
    fn explicit_operation_rejects_incompatible_crs_pair() {
        let err = match Transform::from_operation(
            CoordinateOperationId(1693),
            "EPSG:4326",
            "EPSG:3857",
        ) {
            Ok(_) => panic!("incompatible operation should be rejected"),
            Err(err) => err,
        };
        assert!(matches!(err, Error::OperationSelection(_)));
        assert!(err.to_string().contains("not compatible"));
    }

    #[test]
    fn explicit_selection_options_choose_grid_operation() {
        let t = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                    -80.5041667,
                    44.5458333,
                ))),
                ..SelectionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1313)));
        assert!(!t.selection_diagnostics().approximate);
    }

    #[test]
    fn source_crs_area_of_interest_is_normalized_before_selection() {
        let to_projected = Transform::new("EPSG:4267", "EPSG:26717").unwrap();
        let projected = to_projected.convert((-80.5041667, 44.5458333)).unwrap();

        let t = Transform::with_selection_options(
            "EPSG:26717",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::source_crs_point(Coord::new(
                    projected.0,
                    projected.1,
                ))),
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1313)));
        assert_eq!(
            t.selection_diagnostics().selected_match_kind,
            OperationMatchKind::DerivedGeographic
        );
    }

    #[test]
    fn selection_diagnostics_capture_accuracy_preference() {
        let t = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                    -80.5041667,
                    44.5458333,
                ))),
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        assert!(t
            .selection_diagnostics()
            .selected_reasons
            .contains(&SelectionReason::AccuracyPreferred));
    }

    #[test]
    fn selection_diagnostics_capture_policy_filtered_candidates() {
        let t = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4326",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                    -80.5041667,
                    44.5458333,
                ))),
                policy: SelectionPolicy::RequireGrids,
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        assert!(t
            .selection_diagnostics()
            .skipped_operations
            .iter()
            .any(|skipped| { matches!(skipped.reason, SkippedOperationReason::PolicyFiltered) }));
    }

    #[test]
    fn selection_diagnostics_capture_area_mismatch_candidates() {
        let t = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                    -80.5041667,
                    44.5458333,
                ))),
                policy: SelectionPolicy::RequireExactAreaMatch,
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        assert!(t
            .selection_diagnostics()
            .skipped_operations
            .iter()
            .any(|skipped| {
                matches!(skipped.reason, SkippedOperationReason::AreaOfUseMismatch)
            }));
    }

    #[test]
    fn cross_datum_roundtrip_nad27() {
        let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let inv = fwd.inverse().unwrap();
        let original = (-90.0, 45.0);
        let shifted = fwd.convert(original).unwrap();
        let back = inv.convert(shifted).unwrap();
        assert!((back.0 - original.0).abs() < 1e-6);
        assert!((back.1 - original.1).abs() < 1e-6);
    }

    #[test]
    fn inverse_preserves_explicit_operation_selection() {
        let fwd = Transform::from_operation(CoordinateOperationId(1693), "EPSG:4267", "EPSG:4326")
            .unwrap();
        let inv = fwd.inverse().unwrap();

        assert_eq!(
            fwd.selected_operation().id,
            Some(CoordinateOperationId(1693))
        );
        assert_eq!(
            inv.selected_operation().id,
            Some(CoordinateOperationId(1693))
        );
    }

    #[test]
    fn inverse_reorients_selected_metadata_and_diagnostics() {
        let fwd = Transform::from_operation(CoordinateOperationId(1693), "EPSG:4267", "EPSG:4326")
            .unwrap();
        let inv = fwd.inverse().unwrap();

        assert_eq!(inv.source_crs().epsg(), 4326);
        assert_eq!(inv.target_crs().epsg(), 4267);
        assert_eq!(inv.selected_operation().source_crs_epsg, Some(4326));
        assert_eq!(inv.selected_operation().target_crs_epsg, Some(4267));
        assert_eq!(
            inv.selection_diagnostics()
                .selected_operation
                .source_crs_epsg,
            Some(4326)
        );
        assert_eq!(
            inv.selection_diagnostics()
                .selected_operation
                .target_crs_epsg,
            Some(4267)
        );
    }

    #[test]
    fn cross_datum_osgb36_to_wgs84() {
        let t = Transform::new("EPSG:4277", "EPSG:4326").unwrap();
        let (lon, lat) = t.convert((-0.1278, 51.5074)).unwrap();
        assert!((lon - (-0.1278)).abs() < 0.01, "lon = {lon}");
        assert!((lat - 51.5074).abs() < 0.01, "lat = {lat}");
    }

    #[test]
    fn wgs84_to_web_mercator_3d_preserves_height() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((y - 4970072.0).abs() < 100.0);
        assert!((z - 123.45).abs() < 1e-12);
    }

    #[test]
    fn cross_datum_roundtrip_nad27_3d() {
        let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let inv = fwd.inverse().unwrap();
        let original = (-90.0, 45.0, 250.0);
        let shifted = fwd.convert_3d(original).unwrap();
        let back = inv.convert_3d(shifted).unwrap();
        assert!((back.0 - original.0).abs() < 1e-6);
        assert!((back.1 - original.1).abs() < 1e-6);
        assert!((back.2 - original.2).abs() < 1e-12);
    }

    #[test]
    fn identical_custom_projected_crs_is_identity() {
        let from = CrsDef::Projected(ProjectedCrsDef::new(
            0,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "Custom Web Mercator A",
        ));
        let to = CrsDef::Projected(ProjectedCrsDef::new(
            0,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "Custom Web Mercator B",
        ));

        let t = Transform::from_crs_defs(&from, &to).unwrap();
        assert_eq!(t.selected_operation().name, "Identity");
    }

    #[test]
    fn unknown_custom_datums_do_not_collapse_to_identity() {
        let unknown = datum::Datum {
            ellipsoid: datum::WGS84.ellipsoid,
            to_wgs84: DatumToWgs84::Unknown,
        };
        let from = CrsDef::Projected(ProjectedCrsDef::new(
            0,
            unknown,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "Unknown A",
        ));
        let to = CrsDef::Projected(ProjectedCrsDef::new(
            0,
            unknown,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "Unknown B",
        ));

        let err = match Transform::from_crs_defs(&from, &to) {
            Ok(_) => panic!("unknown custom datums should not build a transform"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("no compatible operation found")
                || err
                    .to_string()
                    .contains("legacy Helmert fallback unavailable")
        );
    }

    #[test]
    fn approximate_fallback_is_modeled_as_helmert_operation() {
        let from = CrsDef::Geographic(GeographicCrsDef::new(0, datum::NAD27, "Custom NAD27"));
        let to = CrsDef::Geographic(GeographicCrsDef::new(0, datum::OSGB36, "Custom OSGB36"));

        let t = Transform::from_crs_defs(&from, &to).unwrap();

        assert!(t.selected_operation().approximate);
        assert!(matches!(
            t.selected_operation_definition.method,
            OperationMethod::Helmert { .. }
        ));
    }

    #[test]
    fn inverse_exposes_swapped_crs() {
        let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let inv = fwd.inverse().unwrap();

        assert_eq!(fwd.source_crs().epsg(), 4326);
        assert_eq!(fwd.target_crs().epsg(), 3857);
        assert_eq!(inv.source_crs().epsg(), 3857);
        assert_eq!(inv.target_crs().epsg(), 4326);
    }

    #[test]
    fn transform_bounds_web_mercator() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let bounds = Bounds::new(-74.3, 40.45, -73.65, 40.95);

        let result = t.transform_bounds(bounds, 8).unwrap();

        assert!(result.min_x < -8_200_000.0);
        assert!(result.max_x < -8_100_000.0);
        assert!(result.min_y > 4_900_000.0);
        assert!(result.max_y > result.min_y);
    }

    #[test]
    fn transform_bounds_rejects_invalid_input() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let err = t
            .transform_bounds(Bounds::new(10.0, 5.0, -10.0, 20.0), 0)
            .unwrap_err();

        assert!(matches!(err, Error::OutOfRange(_)));
    }

    #[test]
    fn batch_transform() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64)> = (0..10)
            .map(|i| (-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
            .collect();

        let results = t.convert_batch(&coords).unwrap();
        assert_eq!(results.len(), 10);
        for (x, _y) in &results {
            assert!(*x < 0.0);
        }
    }

    #[test]
    fn batch_transform_3d() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64, f64)> = (0..10)
            .map(|i| (-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1, i as f64))
            .collect();

        let results = t.convert_batch_3d(&coords).unwrap();
        assert_eq!(results.len(), 10);
        for (index, (x, _y, z)) in results.iter().enumerate() {
            assert!(*x < 0.0);
            assert!((*z - index as f64).abs() < 1e-12);
        }
    }

    #[test]
    fn coord_type() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let c = Coord::new(-74.006, 40.7128);
        let result = t.convert(c).unwrap();
        assert!((result.x - (-8238310.0)).abs() < 100.0);
    }

    #[cfg(feature = "geo-types")]
    #[test]
    fn geo_types_coord() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let c = geo_types::Coord {
            x: -74.006,
            y: 40.7128,
        };
        let result: geo_types::Coord<f64> = t.convert(c).unwrap();
        assert!((result.x - (-8238310.0)).abs() < 100.0);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batch_transform() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64)> = (0..100)
            .map(|i| (-74.0 + i as f64 * 0.01, 40.0 + i as f64 * 0.01))
            .collect();

        let results = t.convert_batch_parallel(&coords).unwrap();
        assert_eq!(results.len(), 100);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batch_transform_matches_sequential_on_large_input() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let len = rayon::current_num_threads() * PARALLEL_MIN_ITEMS_PER_THREAD;
        let coords: Vec<(f64, f64)> = (0..len)
            .map(|i| (-179.0 + i as f64 * 0.0001, -80.0 + i as f64 * 0.00005))
            .collect();

        let sequential = t.convert_batch(&coords).unwrap();
        let parallel = t.convert_batch_parallel(&coords).unwrap();

        assert_eq!(parallel, sequential);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batch_transform_3d() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let coords: Vec<(f64, f64, f64)> = (0..100)
            .map(|i| (-74.0 + i as f64 * 0.01, 40.0 + i as f64 * 0.01, i as f64))
            .collect();

        let results = t.convert_batch_parallel_3d(&coords).unwrap();
        assert_eq!(results.len(), 100);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn parallel_batch_transform_3d_matches_sequential_on_large_input() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let len = rayon::current_num_threads() * PARALLEL_MIN_ITEMS_PER_THREAD;
        let coords: Vec<(f64, f64, f64)> = (0..len)
            .map(|i| {
                (
                    -179.0 + i as f64 * 0.0001,
                    -80.0 + i as f64 * 0.00005,
                    i as f64,
                )
            })
            .collect();

        let sequential = t.convert_batch_3d(&coords).unwrap();
        let parallel = t.convert_batch_parallel_3d(&coords).unwrap();

        assert_eq!(parallel, sequential);
    }
}

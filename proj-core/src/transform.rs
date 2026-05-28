use crate::coord::{Bounds, Coord, Coord3D, Transformable, Transformable3D};
use crate::crs::CrsDef;
use crate::error::{Error, Result};
use crate::grid::{GridError, GridRuntime};
use crate::operation::{
    CoordinateOperation, CoordinateOperationId, CoordinateOperationMetadata, GridCoverageMiss,
    OperationSelectionDiagnostics, OperationStepDirection, SelectionOptions, TransformOutcome,
    VerticalTransformAction, VerticalTransformDiagnostics,
};
use crate::registry;

#[cfg(feature = "geo-types")]
mod geo_adapters;
mod pipeline;
mod selection;
#[cfg(test)]
mod tests;
mod vertical;

use pipeline::{
    compile_pipeline, execute_pipeline_xy, validate_output_len, validate_pipeline_coord3d,
    validate_transform_crs_definition, validate_vertical_ordinate, CompiledOperationFallback,
    CompiledOperationPipeline, PipelineExecutionOutcome,
};
use selection::{
    compile_selected_pipelines, grid_coverage_miss_detail, is_grid_coverage_miss, selected_metadata,
};
use vertical::{compile_vertical_transform, vertical_diagnostics, VerticalTransform};

#[cfg(feature = "rayon")]
use pipeline::should_parallelize;

#[cfg(all(test, feature = "rayon"))]
use pipeline::PARALLEL_MIN_ITEMS_PER_THREAD;
#[cfg(test)]
use pipeline::{PipelineSourceXyUnits, PipelineTargetXyUnits};

/// A reusable coordinate transformation between two CRS.
pub struct Transform {
    source: CrsDef,
    target: CrsDef,
    selected_operation_definition: CoordinateOperation,
    selected_direction: OperationStepDirection,
    selected_operation: CoordinateOperationMetadata,
    diagnostics: OperationSelectionDiagnostics,
    vertical_transform: VerticalTransform,
    selection_options: SelectionOptions,
    pipeline: CompiledOperationPipeline,
    fallback_pipelines: Vec<CompiledOperationFallback>,
}

/// Trait for `geo-types` geometries that can be transformed as whole values.
///
/// Implementations transform coordinates in storage order and return the first
/// coordinate error without producing a partially transformed geometry.
#[cfg(feature = "geo-types")]
pub trait TransformableGeometry: Sized {
    fn transform_geometry(self, transform: &Transform) -> Result<Self>;
}

fn validate_wrapped_geographic_transform_bounds(bounds: Bounds) -> Result<()> {
    if !bounds.min_x.is_finite()
        || !bounds.min_y.is_finite()
        || !bounds.max_x.is_finite()
        || !bounds.max_y.is_finite()
        || bounds.min_x <= bounds.max_x
        || bounds.min_y > bounds.max_y
    {
        return Err(Error::OutOfRange(
            "wrapped geographic bounds must be finite and satisfy west > east and south <= north"
                .into(),
        ));
    }

    for point in [
        Coord::new(bounds.min_x, bounds.min_y),
        Coord::new(bounds.min_x, bounds.max_y),
        Coord::new(bounds.max_x, bounds.min_y),
        Coord::new(bounds.max_x, bounds.max_y),
    ] {
        if !(-180.0..=180.0).contains(&point.x) {
            return Err(Error::OutOfRange(format!(
                "wrapped geographic bounds longitude {:.8} degrees is outside [-180, 180]",
                point.x
            )));
        }
        if !(-90.0..=90.0).contains(&point.y) {
            return Err(Error::OutOfRange(format!(
                "wrapped geographic bounds latitude {:.8} degrees is outside [-90, 90]",
                point.y
            )));
        }
    }

    Ok(())
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
            SelectionOptions::new().with_operation(operation_id),
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

    /// Create a horizontal-only transform from explicit CRS definitions.
    ///
    /// Compound CRS inputs are reduced to their horizontal component before
    /// operation selection. This is intended for XY-only workflows where
    /// vertical transformation is deliberately out of scope.
    pub fn from_horizontal_components(from: &CrsDef, to: &CrsDef) -> Result<Self> {
        Self::from_horizontal_components_with_selection_options(
            from,
            to,
            SelectionOptions::default(),
        )
    }

    /// Create a horizontal-only transform from explicit CRS definitions with
    /// operation-selection options.
    pub fn from_horizontal_components_with_selection_options(
        from: &CrsDef,
        to: &CrsDef,
        options: SelectionOptions,
    ) -> Result<Self> {
        let source = from.horizontal_crs().ok_or_else(|| {
            Error::InvalidDefinition("source CRS does not contain a horizontal component".into())
        })?;
        let target = to.horizontal_crs().ok_or_else(|| {
            Error::InvalidDefinition("target CRS does not contain a horizontal component".into())
        })?;
        Self::from_crs_defs_with_selection_options(&source, &target, options)
    }

    /// Create a transform from explicit CRS definitions with operation-selection options.
    ///
    /// Use this when a custom CRS references grid resources and the transform
    /// needs an application-supplied [`crate::grid::GridProvider`].
    pub fn from_crs_defs_with_selection_options(
        from: &CrsDef,
        to: &CrsDef,
        options: SelectionOptions,
    ) -> Result<Self> {
        validate_transform_crs_definition(from)?;
        validate_transform_crs_definition(to)?;

        let grid_runtime = GridRuntime::new(options.grid_provider.clone());
        let vertical_transform = compile_vertical_transform(from, to, &options, &grid_runtime)?;
        let selected = compile_selected_pipelines(from, to, &options, &grid_runtime)?;
        Ok(Self {
            source: from.clone(),
            target: to.clone(),
            selected_operation_definition: selected.operation,
            selected_direction: selected.direction,
            selected_operation: selected.metadata,
            diagnostics: selected.diagnostics,
            vertical_transform,
            selection_options: options,
            pipeline: selected.pipeline,
            fallback_pipelines: selected.fallback_pipelines,
        })
    }

    /// Transform a single coordinate.
    pub fn convert<T: Transformable>(&self, coord: T) -> Result<T> {
        let c = coord.into_coord();
        let result = self.convert_coord(c)?;
        Ok(T::from_coord(result))
    }

    /// Transform a whole `geo-types` geometry.
    ///
    /// This method is available only with the `geo-types` feature. It
    /// transforms coordinates in geometry storage order and returns the first
    /// coordinate error without producing a partial result.
    #[cfg(feature = "geo-types")]
    pub fn convert_geometry<T: TransformableGeometry>(&self, geometry: T) -> Result<T> {
        geometry.transform_geometry(self)
    }

    /// Transform a single 3D coordinate.
    pub fn convert_3d<T: Transformable3D>(&self, coord: T) -> Result<T> {
        let c = coord.into_coord3d();
        let result = self.convert_coord3d(c)?;
        Ok(T::from_coord3d(result))
    }

    /// Transform a single coordinate and report the operation actually used.
    ///
    /// This 2D API is XY-only: it does not apply or sample configured vertical
    /// transforms.
    ///
    /// When the selected grid-backed operation misses grid coverage, this
    /// reports the coverage misses and the lower-ranked fallback operation that
    /// produced the result.
    pub fn convert_with_diagnostics<T: Transformable>(
        &self,
        coord: T,
    ) -> Result<TransformOutcome<T>> {
        let c = coord.into_coord();
        let outcome = self.convert_coord_with_diagnostics(c)?;
        Ok(TransformOutcome {
            coord: T::from_coord(outcome.coord),
            operation: outcome.operation,
            vertical: outcome.vertical,
            grid_coverage_misses: outcome.grid_coverage_misses,
        })
    }

    /// Transform a single 3D coordinate and report the operation actually used.
    ///
    /// When the selected grid-backed operation misses grid coverage, this
    /// reports the coverage misses and the lower-ranked fallback operation that
    /// produced the result.
    pub fn convert_3d_with_diagnostics<T: Transformable3D>(
        &self,
        coord: T,
    ) -> Result<TransformOutcome<T>> {
        let c = coord.into_coord3d();
        let outcome = self.convert_coord3d_with_diagnostics(c)?;
        Ok(TransformOutcome {
            coord: T::from_coord3d(outcome.coord),
            operation: outcome.operation,
            vertical: outcome.vertical,
            grid_coverage_misses: outcome.grid_coverage_misses,
        })
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

    /// Return diagnostics for the vertical component of this transform.
    pub fn vertical_diagnostics(&self) -> &VerticalTransformDiagnostics {
        self.vertical_transform.diagnostics()
    }

    /// Build the inverse transform by swapping the source and target CRS.
    pub fn inverse(&self) -> Result<Self> {
        let grid_runtime = GridRuntime::new(self.selection_options.grid_provider.clone());
        let inverse_options = self.selection_options.inverse();
        let vertical_transform = compile_vertical_transform(
            &self.target,
            &self.source,
            &inverse_options,
            &grid_runtime,
        )?;
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
        let mut fallback_pipelines = Vec::with_capacity(self.fallback_pipelines.len());
        for fallback in &self.fallback_pipelines {
            let direction = fallback.direction.inverse();
            let pipeline = compile_pipeline(
                &self.target,
                &self.source,
                &fallback.operation,
                direction,
                &grid_runtime,
            )?;
            let metadata = selected_metadata(
                &fallback.operation,
                direction,
                fallback.metadata.area_of_use.clone(),
            );
            fallback_pipelines.push(CompiledOperationFallback {
                operation: fallback.operation.clone(),
                direction,
                metadata,
                pipeline,
            });
        }
        let diagnostics = OperationSelectionDiagnostics {
            selected_operation: selected_operation.clone(),
            selected_match_kind: self.diagnostics.selected_match_kind,
            selected_reasons: self.diagnostics.selected_reasons.clone(),
            fallback_operations: fallback_pipelines
                .iter()
                .map(|fallback| fallback.metadata.clone())
                .collect(),
            skipped_operations: Vec::new(),
            approximate: self.diagnostics.approximate,
            missing_required_grid: self.diagnostics.missing_required_grid.clone(),
        };
        Ok(Self {
            source: self.target.clone(),
            target: self.source.clone(),
            selected_operation_definition: self.selected_operation_definition.clone(),
            selected_direction,
            selected_operation,
            diagnostics,
            vertical_transform,
            selection_options: inverse_options,
            pipeline,
            fallback_pipelines,
        })
    }

    /// Reproject a 2D bounding box by sampling its perimeter.
    pub fn transform_bounds(&self, bounds: Bounds, densify_points: usize) -> Result<Bounds> {
        if !bounds.is_valid() {
            return Err(Error::OutOfRange(
                "bounds must be finite and satisfy min <= max".into(),
            ));
        }

        self.transform_valid_bounds(bounds, densify_points)
    }

    /// Reproject a geographic bounding box that crosses the antimeridian.
    ///
    /// `bounds` is interpreted as west/south/east/north in source geographic
    /// degrees and must satisfy `west > east`. Projected and normal
    /// non-wrapped bounds should use [`Self::transform_bounds`].
    pub fn transform_geographic_wrapped_bounds(
        &self,
        bounds: Bounds,
        densify_points: usize,
    ) -> Result<Bounds> {
        if !self.source.is_geographic() {
            return Err(Error::InvalidDefinition(
                "wrapped geographic bounds require a geographic source CRS".into(),
            ));
        }
        validate_wrapped_geographic_transform_bounds(bounds)?;

        let west_segment = Bounds::new(bounds.min_x, bounds.min_y, 180.0, bounds.max_y);
        let east_segment = Bounds::new(-180.0, bounds.min_y, bounds.max_x, bounds.max_y);
        let mut transformed = self.transform_valid_bounds(west_segment, densify_points)?;
        let east_transformed = self.transform_valid_bounds(east_segment, densify_points)?;
        transformed.expand_to_include(Coord::new(east_transformed.min_x, east_transformed.min_y));
        transformed.expand_to_include(Coord::new(east_transformed.max_x, east_transformed.max_y));
        Ok(transformed)
    }

    fn transform_valid_bounds(&self, bounds: Bounds, densify_points: usize) -> Result<Bounds> {
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
        match execute_pipeline_xy(&self.pipeline, Coord3D::new(c.x, c.y, 0.0)) {
            Ok(coord) => return Ok(coord),
            Err(error) => {
                if !is_grid_coverage_miss(&error) {
                    return Err(error);
                }
            }
        }

        for fallback in &self.fallback_pipelines {
            match execute_pipeline_xy(&fallback.pipeline, Coord3D::new(c.x, c.y, 0.0)) {
                Ok(coord) => return Ok(coord),
                Err(error) => {
                    if !is_grid_coverage_miss(&error) {
                        return Err(error);
                    }
                }
            }
        }

        Err(Error::Grid(GridError::OutsideCoverage(
            "grid coverage miss".into(),
        )))
    }

    fn convert_coord3d(&self, c: Coord3D) -> Result<Coord3D> {
        match self.execute_pipeline_coord3d(&self.pipeline, c) {
            Ok(coord) => return Ok(coord),
            Err(error) => {
                if !is_grid_coverage_miss(&error) {
                    return Err(error);
                }
            }
        }

        for fallback in &self.fallback_pipelines {
            match self.execute_pipeline_coord3d(&fallback.pipeline, c) {
                Ok(coord) => return Ok(coord),
                Err(error) => {
                    if !is_grid_coverage_miss(&error) {
                        return Err(error);
                    }
                }
            }
        }

        Err(Error::Grid(GridError::OutsideCoverage(
            "grid coverage miss".into(),
        )))
    }

    fn convert_coord_with_diagnostics(&self, c: Coord) -> Result<TransformOutcome<Coord>> {
        let mut grid_coverage_misses = Vec::new();
        let c = Coord3D::new(c.x, c.y, 0.0);

        match execute_pipeline_xy(&self.pipeline, c) {
            Ok(coord) => {
                return Ok(TransformOutcome {
                    coord,
                    operation: self.selected_operation.clone(),
                    vertical: vertical_diagnostics(VerticalTransformAction::None, None, None, None),
                    grid_coverage_misses,
                });
            }
            Err(error) => {
                if let Some(detail) = grid_coverage_miss_detail(&error) {
                    grid_coverage_misses.push(GridCoverageMiss {
                        operation: self.selected_operation.clone(),
                        detail,
                    });
                } else {
                    return Err(error);
                }
            }
        }

        for fallback in &self.fallback_pipelines {
            match execute_pipeline_xy(&fallback.pipeline, c) {
                Ok(coord) => {
                    return Ok(TransformOutcome {
                        coord,
                        operation: fallback.metadata.clone(),
                        vertical: vertical_diagnostics(
                            VerticalTransformAction::None,
                            None,
                            None,
                            None,
                        ),
                        grid_coverage_misses,
                    });
                }
                Err(error) => {
                    if let Some(detail) = grid_coverage_miss_detail(&error) {
                        grid_coverage_misses.push(GridCoverageMiss {
                            operation: fallback.metadata.clone(),
                            detail,
                        });
                    } else {
                        return Err(error);
                    }
                }
            }
        }

        Err(Error::Grid(GridError::OutsideCoverage(
            grid_coverage_misses
                .last()
                .map(|miss| miss.detail.clone())
                .unwrap_or_else(|| "grid coverage miss".into()),
        )))
    }

    fn convert_coord3d_with_diagnostics(&self, c: Coord3D) -> Result<TransformOutcome<Coord3D>> {
        let mut grid_coverage_misses = Vec::new();
        match self.execute_pipeline(&self.pipeline, c) {
            Ok(outcome) => {
                return Ok(TransformOutcome {
                    coord: outcome.coord,
                    operation: self.selected_operation.clone(),
                    vertical: outcome.vertical,
                    grid_coverage_misses,
                });
            }
            Err(error) => {
                if let Some(detail) = grid_coverage_miss_detail(&error) {
                    grid_coverage_misses.push(GridCoverageMiss {
                        operation: self.selected_operation.clone(),
                        detail,
                    });
                } else {
                    return Err(error);
                }
            }
        }

        for fallback in &self.fallback_pipelines {
            match self.execute_pipeline(&fallback.pipeline, c) {
                Ok(outcome) => {
                    return Ok(TransformOutcome {
                        coord: outcome.coord,
                        operation: fallback.metadata.clone(),
                        vertical: outcome.vertical,
                        grid_coverage_misses,
                    });
                }
                Err(error) => {
                    if let Some(detail) = grid_coverage_miss_detail(&error) {
                        grid_coverage_misses.push(GridCoverageMiss {
                            operation: fallback.metadata.clone(),
                            detail,
                        });
                    } else {
                        return Err(error);
                    }
                }
            }
        }

        Err(Error::Grid(GridError::OutsideCoverage(
            grid_coverage_misses
                .last()
                .map(|miss| miss.detail.clone())
                .unwrap_or_else(|| "grid coverage miss".into()),
        )))
    }

    fn execute_pipeline(
        &self,
        pipeline: &CompiledOperationPipeline,
        c: Coord3D,
    ) -> Result<PipelineExecutionOutcome> {
        validate_vertical_ordinate(c.z)?;
        let xy = execute_pipeline_xy(pipeline, c)?;
        let vertical = self.vertical_transform.apply(c)?;
        let coord = Coord3D::new(xy.x, xy.y, vertical.z);
        validate_pipeline_coord3d("pipeline final output", coord)?;
        Ok(PipelineExecutionOutcome {
            coord,
            vertical: vertical.diagnostics,
        })
    }

    fn execute_pipeline_coord3d(
        &self,
        pipeline: &CompiledOperationPipeline,
        c: Coord3D,
    ) -> Result<Coord3D> {
        validate_vertical_ordinate(c.z)?;
        let xy = execute_pipeline_xy(pipeline, c)?;
        let z = self.vertical_transform.apply_z(c)?;
        let coord = Coord3D::new(xy.x, xy.y, z);
        validate_pipeline_coord3d("pipeline final output", coord)?;
        Ok(coord)
    }

    /// Batch transform (sequential).
    pub fn convert_batch<T: Transformable + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert(c.clone())).collect()
    }

    /// Batch transform of 3D coordinates (sequential).
    pub fn convert_batch_3d<T: Transformable3D + Clone>(&self, coords: &[T]) -> Result<Vec<T>> {
        coords.iter().map(|c| self.convert_3d(c.clone())).collect()
    }

    /// Transform 2D coordinates in place without allocating.
    ///
    /// Coordinates before a failing coordinate are left converted; the failing
    /// coordinate and subsequent coordinates are left unchanged.
    pub fn convert_coords_in_place(&self, coords: &mut [Coord]) -> Result<()> {
        for coord in coords {
            *coord = self.convert_coord(*coord)?;
        }
        Ok(())
    }

    /// Transform 3D coordinates in place without allocating.
    ///
    /// Coordinates before a failing coordinate are left converted; the failing
    /// coordinate and subsequent coordinates are left unchanged.
    pub fn convert_coords_3d_in_place(&self, coords: &mut [Coord3D]) -> Result<()> {
        for coord in coords {
            *coord = self.convert_coord3d(*coord)?;
        }
        Ok(())
    }

    /// Transform 2D coordinates from `input` into an existing `output` slice.
    ///
    /// `output` must have exactly the same length as `input`. This API performs
    /// no allocation and does not require cloning input coordinates.
    pub fn convert_coords_into(&self, input: &[Coord], output: &mut [Coord]) -> Result<()> {
        validate_output_len(input.len(), output.len())?;
        for (source, target) in input.iter().zip(output.iter_mut()) {
            *target = self.convert_coord(*source)?;
        }
        Ok(())
    }

    /// Transform 3D coordinates from `input` into an existing `output` slice.
    ///
    /// `output` must have exactly the same length as `input`. This API performs
    /// no allocation and does not require cloning input coordinates.
    pub fn convert_coords_3d_into(&self, input: &[Coord3D], output: &mut [Coord3D]) -> Result<()> {
        validate_output_len(input.len(), output.len())?;
        for (source, target) in input.iter().zip(output.iter_mut()) {
            *target = self.convert_coord3d(*source)?;
        }
        Ok(())
    }

    /// Batch transform with Rayon parallelism.
    #[cfg(feature = "rayon")]
    pub fn convert_batch_parallel<T: Transformable + Send + Sync + Clone>(
        &self,
        coords: &[T],
    ) -> Result<Vec<T>> {
        if !should_parallelize(coords.len()) {
            return self.convert_batch(coords);
        }

        use rayon::prelude::*;

        coords
            .par_iter()
            .map(|coord| self.convert(coord.clone()))
            .collect()
    }

    /// Batch transform of 3D coordinates with adaptive Rayon parallelism.
    #[cfg(feature = "rayon")]
    pub fn convert_batch_parallel_3d<T: Transformable3D + Send + Sync + Clone>(
        &self,
        coords: &[T],
    ) -> Result<Vec<T>> {
        if !should_parallelize(coords.len()) {
            return self.convert_batch_3d(coords);
        }

        use rayon::prelude::*;

        coords
            .par_iter()
            .map(|coord| self.convert_3d(coord.clone()))
            .collect()
    }
}

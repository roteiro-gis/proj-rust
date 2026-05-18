use crate::coord::{Bounds, Coord, Coord3D, Transformable, Transformable3D};
use crate::crs::{CrsDef, LinearUnit, VerticalCrsDef};
use crate::datum::{DatumGridShift, DatumGridShiftEntry, DatumToWgs84, HelmertParams};
use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::grid::{GridError, GridHandle, GridRuntime};
use crate::helmert;
use crate::operation::{
    CoordinateOperation, CoordinateOperationId, CoordinateOperationMetadata, GridCoverageMiss,
    GridShiftDirection, OperationMethod, OperationSelectionDiagnostics, OperationStepDirection,
    SelectionOptions, SelectionPolicy, SkippedOperation, SkippedOperationReason, TransformOutcome,
    VerticalGridOffsetConvention, VerticalGridOperation, VerticalGridProvenance,
    VerticalTransformAction, VerticalTransformDiagnostics,
};
use crate::projection::{make_projection, Projection};
use crate::registry;
use crate::selector;
use crate::{ellipsoid, geocentric};
use smallvec::SmallVec;
use std::sync::Arc;

#[cfg(feature = "rayon")]
const PARALLEL_MIN_TOTAL_ITEMS: usize = 16_384;
#[cfg(feature = "rayon")]
const PARALLEL_MIN_ITEMS_PER_THREAD: usize = 4_096;

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

struct CompiledOperationPipeline {
    steps: SmallVec<[CompiledStep; 8]>,
    source_xy_units: PipelineSourceXyUnits,
    target_xy_units: PipelineTargetXyUnits,
}

struct CompiledOperationFallback {
    operation: CoordinateOperation,
    direction: OperationStepDirection,
    metadata: CoordinateOperationMetadata,
    pipeline: CompiledOperationPipeline,
}

#[derive(Clone, Copy)]
enum PipelineSourceXyUnits {
    GeographicDegrees,
    ProjectedMeters,
    ProjectedNativeToMeters(LinearUnit),
}

#[derive(Clone, Copy)]
enum PipelineTargetXyUnits {
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

    fn normalize(self, coord: Coord3D) -> Coord3D {
        match self {
            Self::GeographicDegrees => {
                Coord3D::new(coord.x.to_radians(), coord.y.to_radians(), 0.0)
            }
            Self::ProjectedMeters => Coord3D::new(coord.x, coord.y, 0.0),
            Self::ProjectedNativeToMeters(unit) => {
                Coord3D::new(unit.to_meters(coord.x), unit.to_meters(coord.y), 0.0)
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

struct PipelineExecutionOutcome {
    coord: Coord3D,
    vertical: VerticalTransformDiagnostics,
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

enum VerticalTransform {
    None {
        diagnostics: VerticalTransformDiagnostics,
    },
    Preserve {
        diagnostics: VerticalTransformDiagnostics,
    },
    UnitConvert {
        source_unit: LinearUnit,
        target_unit: LinearUnit,
        diagnostics: VerticalTransformDiagnostics,
    },
    GridShiftList {
        shifts: Box<[CompiledVerticalGridShift]>,
        diagnostics: VerticalTransformDiagnostics,
    },
}

struct CompiledVerticalGridShift {
    handle: GridHandle,
    direction: VerticalGridShiftDirection,
    source_unit: LinearUnit,
    target_unit: LinearUnit,
    sample_horizontal: VerticalSampleHorizontal,
    diagnostics: VerticalTransformDiagnostics,
}

#[derive(Clone, Copy)]
enum VerticalGridShiftDirection {
    EllipsoidToGravity,
    GravityToEllipsoid,
}

enum VerticalSampleHorizontal {
    Geographic,
    Projected {
        projection: Arc<Projection>,
        linear_unit: LinearUnit,
    },
    Transform(Box<Transform>),
}

impl VerticalSampleHorizontal {
    fn compile(
        source: &CrsDef,
        grid_horizontal_crs_epsg: Option<u32>,
        options: &SelectionOptions,
    ) -> Result<Self> {
        if let Some(epsg) = grid_horizontal_crs_epsg {
            return Self::compile_for_grid_crs(source, epsg, options);
        }

        if let Some(projected) = source.as_projected() {
            return Ok(Self::Projected {
                projection: Arc::new(make_projection(&projected.method(), projected.datum())?),
                linear_unit: projected.linear_unit(),
            });
        }
        if source.as_geographic().is_some() {
            return Ok(Self::Geographic);
        }
        Err(Error::InvalidDefinition(
            "vertical grid transforms require a horizontal CRS component".into(),
        ))
    }

    fn compile_for_grid_crs(
        source: &CrsDef,
        grid_horizontal_crs_epsg: u32,
        options: &SelectionOptions,
    ) -> Result<Self> {
        let source_base = source.base_geographic_crs_epsg();
        if source_base == Some(grid_horizontal_crs_epsg) {
            return Self::compile(source, None, options);
        }

        let grid_crs = registry::lookup_epsg(grid_horizontal_crs_epsg).ok_or_else(|| {
            Error::UnknownCrs(format!(
                "unknown vertical grid horizontal CRS EPSG:{grid_horizontal_crs_epsg}"
            ))
        })?;
        if !grid_crs.is_geographic() {
            return Err(Error::OperationSelection(format!(
                "vertical grid horizontal CRS EPSG:{grid_horizontal_crs_epsg} is not a supported geographic sampling CRS"
            )));
        }

        let mut horizontal_options = options.clone();
        horizontal_options.vertical_grid_operations.clear();
        let transform = Transform::from_horizontal_components_with_selection_options(
            source,
            &grid_crs,
            horizontal_options,
        )?;
        Ok(Self::Transform(Box::new(transform)))
    }

    fn lon_lat_radians(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        match self {
            Self::Geographic => Ok((x.to_radians(), y.to_radians())),
            Self::Projected {
                projection,
                linear_unit,
            } => projection.inverse(linear_unit.to_meters(x), linear_unit.to_meters(y)),
            Self::Transform(transform) => {
                let grid_coord = transform.convert((x, y))?;
                Ok((grid_coord.0.to_radians(), grid_coord.1.to_radians()))
            }
        }
    }
}

struct VerticalApplyOutcome {
    z: f64,
    diagnostics: VerticalTransformDiagnostics,
}

impl VerticalTransform {
    fn apply_z(&self, coord: Coord3D) -> Result<f64> {
        match self {
            Self::None { .. } | Self::Preserve { .. } => Ok(coord.z),
            Self::UnitConvert {
                source_unit,
                target_unit,
                ..
            } => Ok(target_unit.from_meters(source_unit.to_meters(coord.z))),
            Self::GridShiftList { shifts, .. } => {
                let mut last_coverage_error = None;
                for shift in shifts {
                    match apply_vertical_grid_shift(shift, coord) {
                        Ok(z) => return Ok(z),
                        Err(Error::Grid(crate::grid::GridError::OutsideCoverage(detail))) => {
                            last_coverage_error = Some(detail);
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(Error::Grid(crate::grid::GridError::OutsideCoverage(
                    last_coverage_error.unwrap_or_else(|| "vertical grid coverage miss".into()),
                )))
            }
        }
    }

    fn apply(&self, coord: Coord3D) -> Result<VerticalApplyOutcome> {
        match self {
            Self::None { diagnostics } | Self::Preserve { diagnostics } => {
                Ok(VerticalApplyOutcome {
                    z: coord.z,
                    diagnostics: diagnostics.clone(),
                })
            }
            Self::UnitConvert {
                source_unit,
                target_unit,
                diagnostics,
            } => Ok(VerticalApplyOutcome {
                z: target_unit.from_meters(source_unit.to_meters(coord.z)),
                diagnostics: diagnostics.clone(),
            }),
            Self::GridShiftList { shifts, .. } => {
                let mut last_coverage_error = None;
                for shift in shifts {
                    match apply_vertical_grid_shift(shift, coord) {
                        Ok(z) => {
                            return Ok(VerticalApplyOutcome {
                                z,
                                diagnostics: shift.diagnostics.clone(),
                            });
                        }
                        Err(Error::Grid(crate::grid::GridError::OutsideCoverage(detail))) => {
                            last_coverage_error = Some(detail);
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(Error::Grid(crate::grid::GridError::OutsideCoverage(
                    last_coverage_error.unwrap_or_else(|| "vertical grid coverage miss".into()),
                )))
            }
        }
    }

    fn diagnostics(&self) -> &VerticalTransformDiagnostics {
        match self {
            Self::None { diagnostics }
            | Self::Preserve { diagnostics }
            | Self::UnitConvert { diagnostics, .. }
            | Self::GridShiftList { diagnostics, .. } => diagnostics,
        }
    }
}

fn apply_vertical_grid_shift(shift: &CompiledVerticalGridShift, coord: Coord3D) -> Result<f64> {
    let (lon, lat) = shift.sample_horizontal.lon_lat_radians(coord.x, coord.y)?;
    let offset_meters = shift
        .handle
        .sample_vertical_offset_meters(lon, lat)?
        .offset_meters;
    let source_meters = shift.source_unit.to_meters(coord.z);
    let target_meters = match shift.direction {
        VerticalGridShiftDirection::EllipsoidToGravity => source_meters - offset_meters,
        VerticalGridShiftDirection::GravityToEllipsoid => source_meters + offset_meters,
    };
    Ok(shift.target_unit.from_meters(target_meters))
}

fn no_ranked_operation_error(
    from: &CrsDef,
    to: &CrsDef,
    options: &SelectionOptions,
    skipped_operations: &[SkippedOperation],
) -> Error {
    let approximate_fallback_disabled = skipped_operations
        .iter()
        .any(|skipped| skipped.detail == selector::APPROXIMATE_HELMERT_FALLBACK_DISABLED_DETAIL);

    match options.policy {
        SelectionPolicy::Operation(id) => match registry::lookup_operation(id) {
            Some(_) => Error::OperationSelection(format!(
                "operation id {} is not compatible with source EPSG:{} target EPSG:{}",
                id.0,
                from.epsg(),
                to.epsg()
            )),
            None => Error::UnknownOperation(format!("unknown operation id {}", id.0)),
        },
        _ if approximate_fallback_disabled => Error::OperationSelection(format!(
            "no non-approximate compatible operation found for source EPSG:{} target EPSG:{}; {}",
            from.epsg(),
            to.epsg(),
            selector::APPROXIMATE_HELMERT_FALLBACK_DISABLED_DETAIL
        )),
        _ => Error::OperationSelection(format!(
            "no compatible operation found for source EPSG:{} target EPSG:{}",
            from.epsg(),
            to.epsg()
        )),
    }
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
        let grid_runtime = GridRuntime::new(options.grid_provider.clone());
        let vertical_transform = compile_vertical_transform(from, to, &options, &grid_runtime)?;
        let candidate_set = selector::rank_operation_candidates(from, to, &options)?;
        if candidate_set.ranked.is_empty() {
            return Err(no_ranked_operation_error(
                from,
                to,
                &options,
                &candidate_set.skipped,
            ));
        }

        let mut skipped_operations = candidate_set.skipped;
        let mut missing_required_grid = None;
        let mut selected: Option<(
            usize,
            &selector::RankedOperationCandidate,
            CoordinateOperationMetadata,
            CompiledOperationPipeline,
        )> = None;
        let mut fallback_pipelines = Vec::new();

        for (index, candidate) in candidate_set.ranked.iter().enumerate() {
            if let Some((_, selected_candidate, ..)) = &selected {
                if !selected_candidate.operation.uses_grids() {
                    skipped_operations.push(skipped_for_unselected_candidate(
                        candidate,
                        !selected_candidate.operation.deprecated,
                    ));
                    continue;
                }
            }

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
                    if let Some((_, selected_candidate, ..)) = &selected {
                        skipped_operations.push(skipped_for_unselected_candidate(
                            candidate,
                            !selected_candidate.operation.deprecated,
                        ));
                        fallback_pipelines.push(CompiledOperationFallback {
                            operation: candidate.operation.clone().into_owned(),
                            direction: candidate.direction,
                            metadata,
                            pipeline,
                        });
                    } else {
                        selected = Some((index, candidate, metadata, pipeline));
                    }
                }
                Err(Error::Grid(error)) => {
                    if selected.is_none() && missing_required_grid.is_none() {
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

        if let Some((index, candidate, metadata, pipeline)) = selected {
            let selected_reasons =
                selected_reasons_for(candidate, &candidate_set.ranked[index + 1..]);
            let diagnostics = OperationSelectionDiagnostics {
                selected_operation: metadata.clone(),
                selected_match_kind: candidate.match_kind,
                selected_reasons,
                fallback_operations: fallback_pipelines
                    .iter()
                    .map(|fallback| fallback.metadata.clone())
                    .collect(),
                skipped_operations,
                approximate: candidate.operation.approximate,
                missing_required_grid,
            };
            return Ok(Self {
                source: from.clone(),
                target: to.clone(),
                selected_operation_definition: candidate.operation.clone().into_owned(),
                selected_direction: candidate.direction,
                selected_operation: metadata,
                diagnostics,
                vertical_transform,
                selection_options: options,
                pipeline,
                fallback_pipelines,
            });
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
        let xy = execute_pipeline_xy(pipeline, c)?;
        let vertical = self.vertical_transform.apply(c)?;
        Ok(PipelineExecutionOutcome {
            coord: Coord3D::new(xy.x, xy.y, vertical.z),
            vertical: vertical.diagnostics,
        })
    }

    fn execute_pipeline_coord3d(
        &self,
        pipeline: &CompiledOperationPipeline,
        c: Coord3D,
    ) -> Result<Coord3D> {
        let xy = execute_pipeline_xy(pipeline, c)?;
        let z = self.vertical_transform.apply_z(c)?;
        Ok(Coord3D::new(xy.x, xy.y, z))
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

fn selected_metadata(
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    matched_area_of_use: Option<crate::operation::AreaOfUse>,
) -> CoordinateOperationMetadata {
    let mut metadata = operation.metadata_for_direction(direction);
    metadata.area_of_use = matched_area_of_use.or_else(|| operation.areas_of_use.first().cloned());
    metadata
}

fn is_grid_coverage_miss(error: &Error) -> bool {
    matches!(error, Error::Grid(GridError::OutsideCoverage(_)))
}

fn grid_coverage_miss_detail(error: &Error) -> Option<String> {
    match error {
        Error::Grid(GridError::OutsideCoverage(detail)) => Some(detail.clone()),
        _ => None,
    }
}

fn validate_output_len(input_len: usize, output_len: usize) -> Result<()> {
    if input_len != output_len {
        return Err(Error::OutOfRange(format!(
            "output coordinate slice length {output_len} does not match input length {input_len}"
        )));
    }
    Ok(())
}

fn compile_vertical_transform(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
    grid_runtime: &GridRuntime,
) -> Result<VerticalTransform> {
    match (source.vertical_crs(), target.vertical_crs()) {
        (None, None) => Ok(VerticalTransform::None {
            diagnostics: vertical_diagnostics(
                VerticalTransformAction::None,
                None,
                None,
                None,
            ),
        }),
        (Some(source_vertical), Some(target_vertical))
            if source_vertical.same_vertical_reference(target_vertical) =>
        {
            if unit_factors_match(
                source_vertical.linear_unit_to_meter(),
                target_vertical.linear_unit_to_meter(),
            ) {
                Ok(VerticalTransform::Preserve {
                    diagnostics: vertical_diagnostics(
                        VerticalTransformAction::Preserved,
                        Some("Vertical ordinate preservation".into()),
                        Some(source_vertical),
                        Some(target_vertical),
                    ),
                })
            } else {
                Ok(VerticalTransform::UnitConvert {
                    source_unit: source_vertical.linear_unit(),
                    target_unit: target_vertical.linear_unit(),
                    diagnostics: vertical_diagnostics(
                        VerticalTransformAction::UnitConverted,
                        Some("Vertical unit conversion".into()),
                        Some(source_vertical),
                        Some(target_vertical),
                    ),
                })
            }
        }
        (Some(source_vertical), Some(target_vertical))
            if is_ellipsoidal_gravity_pair(source_vertical, target_vertical) =>
        {
            let shifts = compile_vertical_grid_shifts(
                source,
                target,
                source_vertical,
                target_vertical,
                options,
                grid_runtime,
            )?;
            if !shifts.is_empty() {
                let diagnostics = shifts[0].diagnostics.clone();
                return Ok(VerticalTransform::GridShiftList {
                    shifts: shifts.into_boxed_slice(),
                    diagnostics,
                });
            }

            Err(Error::OperationSelection(format!(
                "vertical CRS transformations between ellipsoidal and gravity-related heights require a supported geoid grid operation; no supported vertical grid operation is available for source {} target {}",
                vertical_label(source_vertical),
                vertical_label(target_vertical)
            )))
        }
        (Some(source_vertical), Some(target_vertical)) => Err(Error::OperationSelection(format!(
            "vertical CRS transformations between different vertical reference frames require a supported vertical operation/grid; no supported operation is available for source {} target {}",
            vertical_label(source_vertical),
            vertical_label(target_vertical)
        ))),
        (Some(_), None) | (None, Some(_)) => Err(Error::OperationSelection(
            "cannot transform between an explicit vertical CRS and a horizontal-only CRS; vertical CRS transformations are not supported".into(),
        )),
    }
}

fn compile_vertical_grid_shifts(
    source_crs: &CrsDef,
    target_crs: &CrsDef,
    source_vertical: &VerticalCrsDef,
    target_vertical: &VerticalCrsDef,
    options: &SelectionOptions,
    grid_runtime: &GridRuntime,
) -> Result<Vec<CompiledVerticalGridShift>> {
    let area = selector::resolve_area_of_interest(source_crs, target_crs, options)?;
    let direction = vertical_grid_shift_direction(source_vertical, target_vertical)?;
    let mut candidates =
        matching_vertical_grid_operations(source_vertical, target_vertical, options, area.as_ref());

    if matches!(options.policy, SelectionPolicy::RequireExactAreaMatch) && area.is_some() {
        candidates.retain(|candidate| candidate.area_of_use_match == Some(true));
    }
    sort_vertical_grid_candidates(&mut candidates);

    let mut first_error = None;
    let mut shifts = Vec::new();
    for candidate in candidates {
        match compile_vertical_grid_shift(
            candidate,
            source_crs,
            source_vertical,
            target_vertical,
            direction,
            options,
            grid_runtime,
        ) {
            Ok(shift) => shifts.push(shift),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    if shifts.is_empty() {
        if let Some(error) = first_error {
            return Err(error);
        }
    }

    Ok(shifts)
}

struct VerticalGridCandidate<'a> {
    operation: &'a VerticalGridOperation,
    area_of_use_match: Option<bool>,
    grid_area_of_use_match: Option<bool>,
}

fn matching_vertical_grid_operations<'a>(
    source: &VerticalCrsDef,
    target: &VerticalCrsDef,
    options: &'a SelectionOptions,
    area: Option<&selector::ResolvedAreaOfInterest>,
) -> Vec<VerticalGridCandidate<'a>> {
    options
        .vertical_grid_operations
        .iter()
        .filter(|operation| vertical_grid_operation_matches(operation, source, target))
        .map(|operation| VerticalGridCandidate {
            operation,
            area_of_use_match: vertical_operation_area_match(operation, area),
            grid_area_of_use_match: area_of_use_match(operation.grid.area_of_use.as_ref(), area),
        })
        .collect()
}

fn sort_vertical_grid_candidates(candidates: &mut [VerticalGridCandidate<'_>]) {
    candidates.sort_by(|left, right| {
        right
            .area_of_use_match
            .unwrap_or(false)
            .cmp(&left.area_of_use_match.unwrap_or(false))
            .then_with(|| {
                let left_accuracy = left
                    .operation
                    .accuracy
                    .map(|accuracy| accuracy.meters)
                    .unwrap_or(f64::MAX);
                let right_accuracy = right
                    .operation
                    .accuracy
                    .map(|accuracy| accuracy.meters)
                    .unwrap_or(f64::MAX);
                left_accuracy
                    .partial_cmp(&right_accuracy)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
}

fn compile_vertical_grid_shift(
    candidate: VerticalGridCandidate<'_>,
    source_crs: &CrsDef,
    source_vertical: &VerticalCrsDef,
    target_vertical: &VerticalCrsDef,
    direction: VerticalGridShiftDirection,
    options: &SelectionOptions,
    grid_runtime: &GridRuntime,
) -> Result<CompiledVerticalGridShift> {
    let operation = candidate.operation;
    match operation.offset_convention {
        VerticalGridOffsetConvention::GeoidHeightMeters => {}
    }
    let handle = grid_runtime.resolve_handle(&operation.grid)?;
    Ok(CompiledVerticalGridShift {
        direction,
        source_unit: source_vertical.linear_unit(),
        target_unit: target_vertical.linear_unit(),
        sample_horizontal: VerticalSampleHorizontal::compile(
            source_crs,
            operation.grid_horizontal_crs_epsg,
            options,
        )?,
        diagnostics: vertical_grid_diagnostics(
            operation,
            &handle,
            candidate.area_of_use_match,
            candidate.grid_area_of_use_match,
            source_vertical,
            target_vertical,
        ),
        handle,
    })
}

fn vertical_grid_operation_matches(
    operation: &VerticalGridOperation,
    source: &VerticalCrsDef,
    target: &VerticalCrsDef,
) -> bool {
    vertical_crs_filter_matches(operation.source_vertical_crs_epsg, source)
        && vertical_crs_filter_matches(operation.target_vertical_crs_epsg, target)
        && vertical_datum_filter_matches(operation.source_vertical_datum_epsg, source)
        && vertical_datum_filter_matches(operation.target_vertical_datum_epsg, target)
}

fn vertical_operation_area_match(
    operation: &VerticalGridOperation,
    area: Option<&selector::ResolvedAreaOfInterest>,
) -> Option<bool> {
    let area_of_use = operation
        .area_of_use
        .as_ref()
        .or(operation.grid.area_of_use.as_ref());
    area_of_use_match(area_of_use, area)
}

fn area_of_use_match(
    area_of_use: Option<&crate::operation::AreaOfUse>,
    area: Option<&selector::ResolvedAreaOfInterest>,
) -> Option<bool> {
    let area = area?;
    let area_of_use = area_of_use?;
    Some(
        area.point
            .map(|value| area_of_use.contains_point(value))
            .unwrap_or(false)
            || area
                .bounds
                .map(|value| area_of_use.contains_bounds(value))
                .unwrap_or(false),
    )
}

fn vertical_crs_filter_matches(filter: Option<u32>, vertical: &VerticalCrsDef) -> bool {
    filter.is_none_or(|epsg| nonzero_vertical_epsg(vertical) == Some(epsg))
}

fn vertical_datum_filter_matches(filter: Option<u32>, vertical: &VerticalCrsDef) -> bool {
    filter.is_none_or(|epsg| vertical.vertical_datum_epsg() == Some(epsg))
}

fn vertical_grid_shift_direction(
    source: &VerticalCrsDef,
    target: &VerticalCrsDef,
) -> Result<VerticalGridShiftDirection> {
    if source.kind().is_ellipsoidal_height() && target.kind().is_gravity_related_height() {
        return Ok(VerticalGridShiftDirection::EllipsoidToGravity);
    }
    if source.kind().is_gravity_related_height() && target.kind().is_ellipsoidal_height() {
        return Ok(VerticalGridShiftDirection::GravityToEllipsoid);
    }
    Err(Error::OperationSelection(
        "vertical grid operations currently support ellipsoidal height to/from gravity-related height".into(),
    ))
}

fn vertical_grid_diagnostics(
    operation: &VerticalGridOperation,
    handle: &GridHandle,
    area_of_use_match: Option<bool>,
    grid_area_of_use_match: Option<bool>,
    source: &VerticalCrsDef,
    target: &VerticalCrsDef,
) -> VerticalTransformDiagnostics {
    let mut diagnostics = vertical_diagnostics(
        VerticalTransformAction::Transformed,
        Some(operation.name.clone()),
        Some(source),
        Some(target),
    );
    diagnostics.accuracy = operation.accuracy;
    diagnostics.area_of_use = operation
        .area_of_use
        .clone()
        .or_else(|| handle.definition().area_of_use.clone());
    diagnostics.area_of_use_match = area_of_use_match;
    diagnostics.grids.push(VerticalGridProvenance {
        name: handle.definition().name.clone(),
        checksum: Some(handle.checksum().to_string()),
        accuracy: operation.accuracy,
        area_of_use: handle.definition().area_of_use.clone(),
        area_of_use_match: grid_area_of_use_match,
    });
    diagnostics
}

fn vertical_diagnostics(
    action: VerticalTransformAction,
    operation_name: Option<String>,
    source: Option<&VerticalCrsDef>,
    target: Option<&VerticalCrsDef>,
) -> VerticalTransformDiagnostics {
    VerticalTransformDiagnostics {
        action,
        operation_name,
        source_vertical_crs_epsg: source.and_then(nonzero_vertical_epsg),
        target_vertical_crs_epsg: target.and_then(nonzero_vertical_epsg),
        source_vertical_datum_epsg: source.and_then(VerticalCrsDef::vertical_datum_epsg),
        target_vertical_datum_epsg: target.and_then(VerticalCrsDef::vertical_datum_epsg),
        source_unit_to_meter: source.map(VerticalCrsDef::linear_unit_to_meter),
        target_unit_to_meter: target.map(VerticalCrsDef::linear_unit_to_meter),
        accuracy: None,
        area_of_use: None,
        area_of_use_match: None,
        grids: Vec::new(),
    }
}

fn nonzero_vertical_epsg(vertical: &VerticalCrsDef) -> Option<u32> {
    match vertical.epsg() {
        0 => None,
        epsg => Some(epsg),
    }
}

fn unit_factors_match(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}

fn is_ellipsoidal_gravity_pair(source: &VerticalCrsDef, target: &VerticalCrsDef) -> bool {
    (source.kind().is_ellipsoidal_height() && target.kind().is_gravity_related_height())
        || (source.kind().is_gravity_related_height() && target.kind().is_ellipsoidal_height())
}

fn vertical_label(vertical: &VerticalCrsDef) -> String {
    match nonzero_vertical_epsg(vertical) {
        Some(epsg) => format!("EPSG:{epsg}"),
        None if vertical.name().is_empty() => "unnamed vertical CRS".into(),
        None => vertical.name().into(),
    }
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
        CompiledStep::GridShiftList {
            handles,
            allow_null,
            direction,
        } => {
            let mut last_coverage_miss = None;
            for handle in handles.iter() {
                match handle.apply(coord.x, coord.y, *direction) {
                    Ok((lon, lat)) => return Ok(Coord3D::new(lon, lat, coord.z)),
                    Err(GridError::OutsideCoverage(detail)) => {
                        last_coverage_miss = Some(detail);
                    }
                    Err(error) => return Err(Error::Grid(error)),
                }
            }

            if *allow_null {
                return Ok(coord);
            }

            Err(Error::Grid(GridError::OutsideCoverage(
                last_coverage_miss.unwrap_or_else(|| "no datum grid covered coordinate".into()),
            )))
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

fn execute_pipeline_xy(pipeline: &CompiledOperationPipeline, c: Coord3D) -> Result<Coord> {
    if pipeline.steps.is_empty() {
        return Ok(Coord::new(c.x, c.y));
    }
    let mut state = pipeline.source_xy_units.normalize(c);

    for step in &pipeline.steps {
        state = execute_step(step, state)?;
    }

    Ok(pipeline.target_xy_units.denormalize(state))
}

fn compile_pipeline(
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
            OperationMethod::DatumShift {
                source_to_wgs84,
                target_to_wgs84,
            },
            OperationStepDirection::Forward,
        ) => {
            compile_to_wgs84(
                source_to_wgs84,
                source_geo.datum().ellipsoid,
                grid_runtime,
                steps,
            )?;
            compile_from_wgs84(
                target_to_wgs84,
                target_geo.datum().ellipsoid,
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
                source_geo.datum().ellipsoid,
                grid_runtime,
                steps,
            )?;
            compile_from_wgs84(
                source_to_wgs84,
                target_geo.datum().ellipsoid,
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
fn should_parallelize(len: usize) -> bool {
    if len == 0 {
        return false;
    }

    let threads = rayon::current_num_threads().max(1);
    len >= PARALLEL_MIN_TOTAL_ITEMS.max(threads.saturating_mul(PARALLEL_MIN_ITEMS_PER_THREAD))
}

#[cfg(feature = "geo-types")]
fn transform_geo_coord(
    transform: &Transform,
    coord: geo_types::Coord<f64>,
) -> Result<geo_types::Coord<f64>> {
    transform.convert(coord)
}

#[cfg(feature = "geo-types")]
fn transform_geo_coords(
    transform: &Transform,
    coords: Vec<geo_types::Coord<f64>>,
) -> Result<Vec<geo_types::Coord<f64>>> {
    coords
        .into_iter()
        .map(|coord| transform_geo_coord(transform, coord))
        .collect()
}

#[cfg(feature = "geo-types")]
fn transform_geo_rect(
    transform: &Transform,
    rect: geo_types::Rect<f64>,
) -> Result<geo_types::Rect<f64>> {
    let min = rect.min();
    let max = rect.max();
    let corners = [
        geo_types::Coord { x: min.x, y: min.y },
        geo_types::Coord { x: max.x, y: min.y },
        geo_types::Coord { x: max.x, y: max.y },
        geo_types::Coord { x: min.x, y: max.y },
    ];

    let mut transformed = corners
        .into_iter()
        .map(|coord| transform_geo_coord(transform, coord));
    let first = transformed.next().expect("rect has four corners")?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x;
    let mut max_y = first.y;
    for coord in transformed {
        let coord = coord?;
        min_x = min_x.min(coord.x);
        min_y = min_y.min(coord.y);
        max_x = max_x.max(coord.x);
        max_y = max_y.max(coord.y);
    }

    Ok(geo_types::Rect::new(
        geo_types::Coord { x: min_x, y: min_y },
        geo_types::Coord { x: max_x, y: max_y },
    ))
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Coord<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        transform_geo_coord(transform, self)
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Point<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::Point::from(transform_geo_coord(
            transform, self.0,
        )?))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Line<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::Line::new(
            transform_geo_coord(transform, self.start)?,
            transform_geo_coord(transform, self.end)?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::LineString<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::LineString::new(transform_geo_coords(
            transform,
            self.into_inner(),
        )?))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Polygon<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        let (exterior, interiors) = self.into_inner();
        let exterior = exterior.transform_geometry(transform)?;
        let interiors = interiors
            .into_iter()
            .map(|ring| ring.transform_geometry(transform))
            .collect::<Result<Vec<_>>>()?;
        Ok(geo_types::Polygon::new(exterior, interiors))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::MultiPoint<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::MultiPoint(
            self.0
                .into_iter()
                .map(|point| point.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::MultiLineString<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::MultiLineString(
            self.0
                .into_iter()
                .map(|line| line.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::MultiPolygon<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::MultiPolygon(
            self.0
                .into_iter()
                .map(|polygon| polygon.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::GeometryCollection<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::GeometryCollection(
            self.0
                .into_iter()
                .map(|geometry| geometry.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Rect<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        transform_geo_rect(transform, self)
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Triangle<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        let [v1, v2, v3] = self.to_array();
        Ok(geo_types::Triangle(
            transform_geo_coord(transform, v1)?,
            transform_geo_coord(transform, v2)?,
            transform_geo_coord(transform, v3)?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Geometry<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(match self {
            geo_types::Geometry::Point(geometry) => {
                geo_types::Geometry::Point(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Line(geometry) => {
                geo_types::Geometry::Line(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::LineString(geometry) => {
                geo_types::Geometry::LineString(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Polygon(geometry) => {
                geo_types::Geometry::Polygon(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::MultiPoint(geometry) => {
                geo_types::Geometry::MultiPoint(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::MultiLineString(geometry) => {
                geo_types::Geometry::MultiLineString(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::MultiPolygon(geometry) => {
                geo_types::Geometry::MultiPolygon(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::GeometryCollection(geometry) => {
                geo_types::Geometry::GeometryCollection(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Rect(geometry) => {
                geo_types::Geometry::Rect(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Triangle(geometry) => {
                geo_types::Geometry::Triangle(geometry.transform_geometry(transform)?)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crs::{
        CompoundCrsDef, CrsDef, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
        ProjectionMethod, VerticalCrsDef,
    };
    use crate::datum::{self, DatumToWgs84};
    use crate::grid::{FilesystemGridProvider, GridDefinition, GridFormat};
    use crate::operation::{
        AreaOfInterest, GridId, GridInterpolation, OperationMatchKind, SelectionPolicy,
        SelectionReason, SkippedOperationReason, VerticalGridOffsetConvention,
        VerticalGridOperation, VerticalTransformAction,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;
    static TEMP_GRID_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn expect_transform_error(result: Result<Transform>) -> Error {
        match result {
            Ok(_) => panic!("expected transform construction to fail"),
            Err(err) => err,
        }
    }

    fn write_test_gtx(values: &[f32]) -> PathBuf {
        write_test_gtx_files(&[("test.gtx", -75.0, 40.0, values)])
    }

    fn write_test_gtx_files(files: &[(&str, f64, f64, &[f32])]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "proj-core-vertical-grid-{}-{}",
            std::process::id(),
            TEMP_GRID_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, west, south, values) in files {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&(*south).to_be_bytes());
            bytes.extend_from_slice(&(*west).to_be_bytes());
            bytes.extend_from_slice(&1.0f64.to_be_bytes());
            bytes.extend_from_slice(&1.0f64.to_be_bytes());
            bytes.extend_from_slice(&2i32.to_be_bytes());
            bytes.extend_from_slice(&2i32.to_be_bytes());
            for value in *values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        dir
    }

    fn test_vertical_grid_operation() -> VerticalGridOperation {
        test_vertical_grid_operation_named("Test geoid height to NAVD88", "test.gtx")
    }

    fn test_vertical_grid_operation_named(
        name: &str,
        resource_name: &str,
    ) -> VerticalGridOperation {
        VerticalGridOperation {
            name: name.into(),
            grid: GridDefinition {
                id: GridId(900_001),
                name: resource_name.into(),
                format: GridFormat::Gtx,
                interpolation: GridInterpolation::Bilinear,
                area_of_use: Some(crate::operation::AreaOfUse {
                    west: -75.0,
                    south: 40.0,
                    east: -74.0,
                    north: 41.0,
                    name: "test grid".into(),
                }),
                resource_names: SmallVec::from_vec(vec![resource_name.into()]),
            },
            grid_horizontal_crs_epsg: Some(4326),
            source_vertical_crs_epsg: None,
            target_vertical_crs_epsg: Some(5703),
            source_vertical_datum_epsg: None,
            target_vertical_datum_epsg: Some(5103),
            accuracy: Some(crate::operation::OperationAccuracy { meters: 0.01 }),
            area_of_use: None,
            offset_convention: VerticalGridOffsetConvention::GeoidHeightMeters,
        }
    }

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
    fn pipeline_compiles_xy_unit_modes() {
        let web_mercator = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        assert!(matches!(
            web_mercator.pipeline.source_xy_units,
            PipelineSourceXyUnits::GeographicDegrees
        ));
        assert!(matches!(
            web_mercator.pipeline.target_xy_units,
            PipelineTargetXyUnits::ProjectedMeters
        ));

        let foot_crs = Transform::new("EPSG:2264", "EPSG:4326").unwrap();
        assert!(matches!(
            foot_crs.pipeline.source_xy_units,
            PipelineSourceXyUnits::ProjectedNativeToMeters(unit)
                if (unit.meters_per_unit() - US_FOOT_TO_METER).abs() < 1e-15
        ));
        assert!(matches!(
            foot_crs.pipeline.target_xy_units,
            PipelineTargetXyUnits::GeographicDegrees
        ));
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
    fn area_of_interest_rejects_invalid_geographic_bounds() {
        for bounds in [
            Bounds::new(10.0, 5.0, -10.0, 20.0),
            Bounds::new(f64::NAN, 5.0, 10.0, 20.0),
            Bounds::new(-181.0, 5.0, -170.0, 20.0),
            Bounds::new(-80.0, -91.0, -70.0, -80.0),
        ] {
            let err = expect_transform_error(Transform::with_selection_options(
                "EPSG:4267",
                "EPSG:4269",
                SelectionOptions {
                    area_of_interest: Some(AreaOfInterest::geographic_bounds(bounds)),
                    ..SelectionOptions::default()
                },
            ));

            assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
        }
    }

    #[test]
    fn area_of_interest_validates_geographic_source_and_target_bounds() {
        for area_of_interest in [
            AreaOfInterest::source_crs_bounds(Bounds::new(-181.0, 40.0, -170.0, 45.0)),
            AreaOfInterest::target_crs_bounds(Bounds::new(-80.0, 40.0, -70.0, 91.0)),
        ] {
            let err = expect_transform_error(Transform::with_selection_options(
                "EPSG:4267",
                "EPSG:4269",
                SelectionOptions {
                    area_of_interest: Some(area_of_interest),
                    ..SelectionOptions::default()
                },
            ));

            assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
        }
    }

    #[test]
    fn area_of_interest_rejects_invalid_projected_bounds_before_sampling() {
        let err = expect_transform_error(Transform::with_selection_options(
            "EPSG:3857",
            "EPSG:4326",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::source_crs_bounds(Bounds::new(
                    10.0, 0.0, -10.0, 10.0,
                ))),
                ..SelectionOptions::default()
            },
        ));

        assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
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
    fn grid_coverage_miss_does_not_use_approximate_fallback_by_default() {
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
        assert!(t.selection_diagnostics().fallback_operations.is_empty());

        let err = t.convert((0.0, 0.0)).unwrap_err();

        assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
        assert!(t
            .selection_diagnostics()
            .skipped_operations
            .iter()
            .any(|skipped| skipped.metadata.approximate
                && matches!(skipped.reason, SkippedOperationReason::PolicyFiltered)
                && skipped
                    .detail
                    .contains("allow_approximate_helmert_fallback")));
    }

    #[test]
    fn grid_coverage_miss_falls_back_when_approximate_fallback_allowed() {
        let t = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                    -80.5041667,
                    44.5458333,
                ))),
                policy: SelectionPolicy::AllowApproximateHelmertFallback,
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1313)));
        assert!(!t.selection_diagnostics().fallback_operations.is_empty());

        let outcome = t.convert_with_diagnostics((0.0, 0.0)).unwrap();
        let plain = t.convert((0.0, 0.0)).unwrap();

        assert_eq!(plain, outcome.coord);
        assert_ne!(outcome.operation.id, Some(CoordinateOperationId(1313)));
        assert!(outcome.operation.approximate);
        assert!(outcome
            .grid_coverage_misses
            .iter()
            .any(|miss| miss.operation.id == Some(CoordinateOperationId(1313))));
    }

    #[test]
    fn inverse_grid_coverage_miss_preserves_fallback_operations() {
        let fwd = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                    -80.5041667,
                    44.5458333,
                ))),
                policy: SelectionPolicy::AllowApproximateHelmertFallback,
                ..SelectionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            fwd.selected_operation().id,
            Some(CoordinateOperationId(1313))
        );
        assert!(!fwd.selection_diagnostics().fallback_operations.is_empty());

        let inv = fwd.inverse().unwrap();
        assert_eq!(
            inv.selected_operation().id,
            Some(CoordinateOperationId(1313))
        );
        assert_eq!(
            inv.selected_operation().direction,
            OperationStepDirection::Reverse
        );
        assert!(!inv.selection_diagnostics().fallback_operations.is_empty());

        let outcome = inv.convert_with_diagnostics((0.0, 0.0)).unwrap();
        let plain = inv.convert((0.0, 0.0)).unwrap();

        assert_eq!(plain, outcome.coord);
        assert_ne!(outcome.operation.id, Some(CoordinateOperationId(1313)));
        assert_eq!(outcome.operation.source_crs_epsg, Some(4269));
        assert_eq!(outcome.operation.target_crs_epsg, Some(4267));
        assert!(inv
            .selection_diagnostics()
            .fallback_operations
            .iter()
            .any(|operation| operation.id == outcome.operation.id
                && operation.direction == outcome.operation.direction));
        assert!(outcome.grid_coverage_misses.iter().any(|miss| {
            miss.operation.id == Some(CoordinateOperationId(1313))
                && miss.operation.direction == OperationStepDirection::Reverse
        }));
    }

    #[test]
    fn grid_coverage_miss_does_not_use_non_grid_fallback_when_grids_are_required() {
        let t = Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
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
            .fallback_operations
            .iter()
            .all(|operation| operation.uses_grids));

        let err = t.convert((0.0, 0.0)).unwrap_err();

        assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
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
        assert_eq!(
            inv.selected_operation().direction,
            OperationStepDirection::Reverse
        );
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
    fn equal_compound_vertical_crs_preserves_height() {
        let source = registry::lookup_epsg(4979).unwrap();
        let target_horizontal = ProjectedCrsDef::new_with_base_geographic_crs(
            3857,
            4326,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "WGS 84 / Pseudo-Mercator",
        );
        let target_vertical = VerticalCrsDef::ellipsoidal_height(
            0,
            datum::WGS84,
            LinearUnit::metre(),
            "WGS 84 ellipsoidal height",
        );
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            HorizontalCrsDef::Projected(target_horizontal),
            target_vertical,
            "WGS 84 / Pseudo-Mercator + ellipsoidal height",
        )));

        let t = Transform::from_crs_defs(&source, &target).unwrap();
        let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((y - 4970072.0).abs() < 100.0);
        assert!((z - 123.45).abs() < 1e-12);
        assert_eq!(
            t.vertical_diagnostics().action,
            VerticalTransformAction::Preserved
        );
    }

    #[test]
    fn horizontal_components_allow_compound_xy_preview() {
        let source = registry::lookup_epsg(4979).unwrap();
        let target = registry::lookup_epsg(3857).unwrap();
        let err = expect_transform_error(Transform::from_crs_defs(&source, &target));
        assert!(err.to_string().contains("explicit vertical CRS"));

        let t = Transform::from_horizontal_components(&source, &target).unwrap();
        assert!(t.source_crs().vertical_crs().is_none());
        assert!(t.target_crs().vertical_crs().is_none());
        assert_eq!(
            t.vertical_diagnostics().action,
            VerticalTransformAction::None
        );
        let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((y - 4970072.0).abs() < 100.0);
        assert!((z - 123.45).abs() < 1e-12);
    }

    #[test]
    fn same_vertical_reference_converts_z_units() {
        let horizontal =
            HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
        let source_vertical =
            VerticalCrsDef::gravity_related_height(5703, 5103, LinearUnit::metre(), "NAVD88")
                .unwrap();
        let target_vertical =
            VerticalCrsDef::gravity_related_height(0, 5103, LinearUnit::foot(), "NAVD88 foot")
                .unwrap();
        let source = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal.clone(),
            source_vertical,
            "WGS 84 + NAVD88 height",
        )));
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal,
            target_vertical,
            "WGS 84 + NAVD88 height (ft)",
        )));

        let t = Transform::from_crs_defs(&source, &target).unwrap();
        let plain = t.convert_3d((-74.006, 40.7128, 1.0)).unwrap();
        assert!((plain.2 - 3.280839895013123).abs() < 1e-12);

        let outcome = t
            .convert_3d_with_diagnostics((-74.006, 40.7128, 1.0))
            .unwrap();
        assert!((outcome.coord.2 - 3.280839895013123).abs() < 1e-12);
        assert_eq!(
            outcome.vertical.action,
            VerticalTransformAction::UnitConverted
        );
        assert_eq!(outcome.vertical.source_vertical_crs_epsg, Some(5703));
        assert_eq!(outcome.vertical.source_vertical_datum_epsg, Some(5103));
        assert_eq!(outcome.vertical.target_vertical_datum_epsg, Some(5103));
    }

    #[test]
    fn vertical_geoid_grid_transforms_ellipsoidal_to_gravity_height() {
        let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
        let source = registry::lookup_epsg(4979).unwrap();
        let target_horizontal = ProjectedCrsDef::new_with_base_geographic_crs(
            3857,
            4326,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "WGS 84 / Pseudo-Mercator",
        );
        let target_vertical = registry::lookup_vertical_epsg(5703).unwrap();
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            HorizontalCrsDef::Projected(target_horizontal),
            target_vertical,
            "WGS 84 / Pseudo-Mercator + NAVD88 height",
        )));

        let t = Transform::from_crs_defs_with_selection_options(
            &source,
            &target,
            SelectionOptions {
                grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
                vertical_grid_operations: vec![test_vertical_grid_operation()],
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        let plain = t.convert_3d((-74.5, 40.5, 100.0)).unwrap();
        assert!((plain.2 - 130.0).abs() < 1e-9);

        let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
        assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
        assert_eq!(
            outcome.vertical.action,
            VerticalTransformAction::Transformed
        );
        assert_eq!(outcome.vertical.target_vertical_crs_epsg, Some(5703));
        assert_eq!(outcome.vertical.grids[0].name, "test.gtx");
        assert!(outcome.vertical.grids[0]
            .checksum
            .as_ref()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(outcome.vertical.area_of_use_match, None);

        let inverse = t.inverse().unwrap();
        let roundtrip = inverse.convert_3d(outcome.coord).unwrap();
        assert!((roundtrip.0 - -74.5).abs() < 1e-6);
        assert!((roundtrip.1 - 40.5).abs() < 1e-6);
        assert!((roundtrip.2 - 100.0).abs() < 1e-9);
    }

    #[test]
    fn vertical_grid_selection_prefers_area_of_use_match() {
        let grid_root = write_test_gtx_files(&[
            ("outside.gtx", -75.0, 40.0, &[-10.0, -10.0, -10.0, -10.0]),
            ("inside.gtx", -75.0, 40.0, &[-30.0, -30.0, -30.0, -30.0]),
        ]);
        let source = registry::lookup_epsg(4979).unwrap();
        let horizontal =
            HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal,
            registry::lookup_vertical_epsg(5703).unwrap(),
            "WGS 84 + NAVD88 height",
        )));

        let mut outside =
            test_vertical_grid_operation_named("outside geoid operation", "outside.gtx");
        outside.grid.area_of_use = Some(crate::operation::AreaOfUse {
            west: 0.0,
            south: 0.0,
            east: 1.0,
            north: 1.0,
            name: "outside".into(),
        });
        let inside = test_vertical_grid_operation_named("inside geoid operation", "inside.gtx");

        let t = Transform::from_crs_defs_with_selection_options(
            &source,
            &target,
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(-74.5, 40.5))),
                grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
                vertical_grid_operations: vec![outside, inside],
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
        assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
        assert_eq!(
            outcome.vertical.operation_name.as_deref(),
            Some("inside geoid operation")
        );
        assert_eq!(outcome.vertical.area_of_use_match, Some(true));
        assert_eq!(outcome.vertical.grids[0].area_of_use_match, Some(true));
    }

    #[test]
    fn vertical_grid_runtime_falls_back_after_coverage_miss() {
        let grid_root = write_test_gtx_files(&[
            ("outside.gtx", 10.0, 10.0, &[-10.0, -10.0, -10.0, -10.0]),
            ("inside.gtx", -75.0, 40.0, &[-30.0, -30.0, -30.0, -30.0]),
        ]);
        let source = registry::lookup_epsg(4979).unwrap();
        let horizontal =
            HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal,
            registry::lookup_vertical_epsg(5703).unwrap(),
            "WGS 84 + NAVD88 height",
        )));

        let outside = test_vertical_grid_operation_named("outside geoid operation", "outside.gtx");
        let inside = test_vertical_grid_operation_named("inside geoid operation", "inside.gtx");
        let t = Transform::from_crs_defs_with_selection_options(
            &source,
            &target,
            SelectionOptions {
                grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
                vertical_grid_operations: vec![outside, inside],
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
        assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
        assert_eq!(
            outcome.vertical.operation_name.as_deref(),
            Some("inside geoid operation")
        );
    }

    #[test]
    fn vertical_grid_rejects_unsupported_sampling_crs() {
        let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
        let source = registry::lookup_epsg(4979).unwrap();
        let horizontal =
            HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal,
            registry::lookup_vertical_epsg(5703).unwrap(),
            "WGS 84 + NAVD88 height",
        )));
        let mut operation = test_vertical_grid_operation();
        operation.grid_horizontal_crs_epsg = Some(3857);

        let err = expect_transform_error(Transform::from_crs_defs_with_selection_options(
            &source,
            &target,
            SelectionOptions {
                grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
                vertical_grid_operations: vec![operation],
                ..SelectionOptions::default()
            },
        ));

        assert!(err
            .to_string()
            .contains("not a supported geographic sampling CRS"));
    }

    #[test]
    fn vertical_geoid_grid_rejects_outside_coverage() {
        let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
        let horizontal =
            HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal,
            registry::lookup_vertical_epsg(5703).unwrap(),
            "WGS 84 + NAVD88 height",
        )));
        let source = registry::lookup_epsg(4979).unwrap();
        let t = Transform::from_crs_defs_with_selection_options(
            &source,
            &target,
            SelectionOptions {
                grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
                vertical_grid_operations: vec![test_vertical_grid_operation()],
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        let err = t.convert_3d((-80.0, 40.5, 100.0)).unwrap_err();
        assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
    }

    #[test]
    fn two_dimensional_convert_does_not_sample_vertical_grids() {
        let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
        let source = registry::lookup_epsg(4979).unwrap();
        let target_horizontal = ProjectedCrsDef::new_with_base_geographic_crs(
            3857,
            4326,
            datum::WGS84,
            ProjectionMethod::WebMercator,
            LinearUnit::metre(),
            "WGS 84 / Pseudo-Mercator",
        );
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            HorizontalCrsDef::Projected(target_horizontal),
            registry::lookup_vertical_epsg(5703).unwrap(),
            "WGS 84 / Pseudo-Mercator + NAVD88 height",
        )));
        let t = Transform::from_crs_defs_with_selection_options(
            &source,
            &target,
            SelectionOptions {
                grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
                vertical_grid_operations: vec![test_vertical_grid_operation()],
                ..SelectionOptions::default()
            },
        )
        .unwrap();

        let (x, y) = t.convert((-80.0, 40.5)).unwrap();
        assert!(x < -8_900_000.0 && x > -8_910_000.0, "x = {x}");
        assert!(y > 4_930_000.0 && y < 4_940_000.0, "y = {y}");

        let outcome = t.convert_with_diagnostics((-80.0, 40.5)).unwrap();
        assert!((outcome.coord.0 - x).abs() < 1e-9);
        assert!((outcome.coord.1 - y).abs() < 1e-9);
        assert_eq!(outcome.vertical.action, VerticalTransformAction::None);
        assert!(outcome.grid_coverage_misses.is_empty());

        let err = t.convert_3d((-80.0, 40.5, 100.0)).unwrap_err();
        assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
    }

    #[test]
    fn explicit_vertical_to_horizontal_only_transform_is_rejected() {
        let source = registry::lookup_epsg(4979).unwrap();
        let target = registry::lookup_epsg(4326).unwrap();
        let err = expect_transform_error(Transform::from_crs_defs(&source, &target));
        assert!(err.to_string().contains("explicit vertical CRS"));
    }

    #[test]
    fn mismatched_compound_vertical_crs_is_rejected() {
        let source = registry::lookup_epsg(4979).unwrap();
        let target_horizontal = GeographicCrsDef::new(4326, datum::WGS84, "WGS 84");
        let target_vertical =
            VerticalCrsDef::gravity_related_height(5703, 5103, LinearUnit::metre(), "NAVD88")
                .unwrap();
        let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            HorizontalCrsDef::Geographic(target_horizontal),
            target_vertical,
            "WGS 84 + NAVD88 height",
        )));

        let err = expect_transform_error(Transform::from_crs_defs(&source, &target));
        assert!(err.to_string().contains("vertical CRS transformations"));
        assert!(err.to_string().contains("geoid grid"));
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
            unknown.clone(),
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
    fn approximate_helmert_fallback_requires_explicit_opt_in() {
        let from = CrsDef::Geographic(GeographicCrsDef::new(0, datum::NAD27, "Custom NAD27"));
        let to = CrsDef::Geographic(GeographicCrsDef::new(0, datum::OSGB36, "Custom OSGB36"));

        let err = expect_transform_error(Transform::from_crs_defs(&from, &to));
        let message = err.to_string();
        assert!(message.contains("no non-approximate compatible operation found"));
        assert!(message.contains("approximate Helmert fallback is available but disabled"));
        assert!(message.contains("SelectionOptions::allow_approximate_helmert_fallback()"));

        let t = Transform::from_crs_defs_with_selection_options(
            &from,
            &to,
            SelectionOptions::new().allow_approximate_helmert_fallback(),
        )
        .unwrap();

        assert!(t.selected_operation().approximate);
        assert!(t.selection_diagnostics().approximate);
        assert_eq!(
            t.selection_diagnostics().selected_match_kind,
            OperationMatchKind::ApproximateFallback
        );
        assert!(matches!(
            t.selected_operation_definition.method,
            OperationMethod::Helmert { .. }
        ));
    }

    #[test]
    fn selection_diagnostics_report_disabled_approximate_fallback() {
        let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();

        assert!(!t.selection_diagnostics().approximate);
        assert!(t
            .selection_diagnostics()
            .skipped_operations
            .iter()
            .any(|skipped| {
                matches!(skipped.reason, SkippedOperationReason::PolicyFiltered)
                    && skipped.metadata.approximate
                    && skipped
                        .detail
                        .contains("allow_approximate_helmert_fallback")
            }));
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
    fn batch_transform_coords_in_place_matches_vec_batch() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let input: Vec<Coord> = (0..10)
            .map(|i| Coord::new(-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
            .collect();
        let expected = t.convert_batch(&input).unwrap();
        let mut actual = input.clone();

        t.convert_coords_in_place(&mut actual).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn batch_transform_coords_into_reuses_output_slice() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let input: Vec<Coord> = (0..10)
            .map(|i| Coord::new(-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
            .collect();
        let expected = t.convert_batch(&input).unwrap();
        let mut actual = vec![Coord::new(0.0, 0.0); input.len()];

        t.convert_coords_into(&input, &mut actual).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn batch_transform_coords_into_rejects_mismatched_output_len() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let input = [Coord::new(-74.0, 40.0), Coord::new(-73.0, 41.0)];
        let mut output = [Coord::new(0.0, 0.0)];

        let err = t.convert_coords_into(&input, &mut output).unwrap_err();

        assert!(matches!(err, Error::OutOfRange(_)));
    }

    #[test]
    fn batch_transform_coords_3d_in_place_and_into_preserve_height() {
        let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let input: Vec<Coord3D> = (0..10)
            .map(|i| Coord3D::new(-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1, i as f64))
            .collect();
        let expected = t.convert_batch_3d(&input).unwrap();

        let mut in_place = input.clone();
        t.convert_coords_3d_in_place(&mut in_place).unwrap();
        assert_eq!(in_place, expected);

        let mut output = vec![Coord3D::new(0.0, 0.0, 0.0); input.len()];
        t.convert_coords_3d_into(&input, &mut output).unwrap();
        assert_eq!(output, expected);
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

use crate::coord::{Bounds, Coord};
use crate::crs::{LinearUnit, ProjectionMethod};
use crate::datum::{DatumToWgs84, HelmertParams};
use smallvec::SmallVec;
use std::collections::HashSet;
use std::sync::Arc;

const DEFAULT_AREA_BOUNDS_DENSIFY_POINTS: usize = 21;

/// Stable identifier for a registry-backed coordinate operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CoordinateOperationId(pub u32);

/// Stable identifier for a grid resource referenced by an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridId(pub u32);

/// Ranked area-of-use metadata for an operation or grid.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaOfUse {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
    pub name: String,
}

impl AreaOfUse {
    pub fn contains_point(&self, point: Coord) -> bool {
        longitude_range_contains_point(self.west, self.east, point.x)
            && point.y >= self.south
            && point.y <= self.north
    }

    pub fn contains_bounds(&self, bounds: Bounds) -> bool {
        longitude_range_contains_range(self.west, self.east, bounds.min_x, bounds.max_x)
            && bounds.min_y >= self.south
            && bounds.max_y <= self.north
    }
}

fn longitude_range_contains_point(west: f64, east: f64, longitude: f64) -> bool {
    longitude_delta(west, longitude) <= longitude_span(west, east)
}

fn longitude_range_contains_range(
    outer_west: f64,
    outer_east: f64,
    inner_west: f64,
    inner_east: f64,
) -> bool {
    let outer_span = longitude_span(outer_west, outer_east);
    if outer_span >= 360.0 {
        return true;
    }
    let inner_start = longitude_delta(outer_west, inner_west);
    let inner_span = longitude_span(inner_west, inner_east);
    inner_start + inner_span <= outer_span
}

fn longitude_span(west: f64, east: f64) -> f64 {
    if east >= west {
        east - west
    } else {
        east + 360.0 - west
    }
}

fn longitude_delta(west: f64, east: f64) -> f64 {
    (east - west).rem_euclid(360.0)
}

/// Nominal operation accuracy in meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OperationAccuracy {
    pub meters: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStepDirection {
    Forward,
    Reverse,
}

impl OperationStepDirection {
    pub fn inverse(self) -> Self {
        match self {
            Self::Forward => Self::Reverse,
            Self::Reverse => Self::Forward,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaOfInterestCrs {
    /// Geographic degrees with conventional west <= east bounds.
    GeographicDegrees,
    /// Geographic degrees with bounds crossing the antimeridian, represented
    /// by west > east.
    GeographicDegreesWrapped,
    SourceCrs,
    TargetCrs,
}

impl AreaOfInterestCrs {
    pub fn inverse(self) -> Self {
        match self {
            Self::GeographicDegrees => Self::GeographicDegrees,
            Self::GeographicDegreesWrapped => Self::GeographicDegreesWrapped,
            Self::SourceCrs => Self::TargetCrs,
            Self::TargetCrs => Self::SourceCrs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AreaOfInterest {
    pub crs: AreaOfInterestCrs,
    pub point: Option<Coord>,
    pub bounds: Option<Bounds>,
}

impl AreaOfInterest {
    pub fn geographic_point(point: Coord) -> Self {
        Self {
            crs: AreaOfInterestCrs::GeographicDegrees,
            point: Some(point),
            bounds: None,
        }
    }

    pub fn geographic_bounds(bounds: Bounds) -> Self {
        Self {
            crs: AreaOfInterestCrs::GeographicDegrees,
            point: None,
            bounds: Some(bounds),
        }
    }

    /// Construct a geographic area of interest that crosses the antimeridian.
    ///
    /// The bounds are interpreted as west/south/east/north in degrees and must
    /// satisfy `west > east`; use [`Self::geographic_bounds`] for normal
    /// non-wrapped geographic bounds.
    pub fn geographic_wrapped_bounds(bounds: Bounds) -> Self {
        Self {
            crs: AreaOfInterestCrs::GeographicDegreesWrapped,
            point: None,
            bounds: Some(bounds),
        }
    }

    pub fn source_crs_point(point: Coord) -> Self {
        Self {
            crs: AreaOfInterestCrs::SourceCrs,
            point: Some(point),
            bounds: None,
        }
    }

    pub fn source_crs_bounds(bounds: Bounds) -> Self {
        Self {
            crs: AreaOfInterestCrs::SourceCrs,
            point: None,
            bounds: Some(bounds),
        }
    }

    pub fn target_crs_point(point: Coord) -> Self {
        Self {
            crs: AreaOfInterestCrs::TargetCrs,
            point: Some(point),
            bounds: None,
        }
    }

    pub fn target_crs_bounds(bounds: Bounds) -> Self {
        Self {
            crs: AreaOfInterestCrs::TargetCrs,
            point: None,
            bounds: Some(bounds),
        }
    }

    pub fn inverse(self) -> Self {
        Self {
            crs: self.crs.inverse(),
            point: self.point,
            bounds: self.bounds,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridInterpolation {
    Bilinear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridShiftDirection {
    Forward,
    Reverse,
}

impl GridShiftDirection {
    pub fn inverse(self) -> Self {
        match self {
            Self::Forward => Self::Reverse,
            Self::Reverse => Self::Forward,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OperationStep {
    pub operation_id: CoordinateOperationId,
    pub direction: OperationStepDirection,
}

/// Enum-backed operation method model used by selection and compilation.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationMethod {
    Identity,
    Helmert {
        params: HelmertParams,
    },
    GridShift {
        grid_id: GridId,
        interpolation: GridInterpolation,
        direction: GridShiftDirection,
    },
    DatumShift {
        source_to_wgs84: DatumToWgs84,
        target_to_wgs84: DatumToWgs84,
    },
    Projection {
        forward: bool,
        method: ProjectionMethod,
        linear_unit: LinearUnit,
    },
    AxisUnitNormalize,
    Concatenated {
        steps: SmallVec<[OperationStep; 4]>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationMatchKind {
    Custom,
    ExactSourceTarget,
    DerivedGeographic,
    DatumCompatible,
    Explicit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoordinateOperation {
    pub id: Option<CoordinateOperationId>,
    pub name: String,
    pub source_crs_epsg: Option<u32>,
    pub target_crs_epsg: Option<u32>,
    pub source_datum_epsg: Option<u32>,
    pub target_datum_epsg: Option<u32>,
    pub accuracy: Option<OperationAccuracy>,
    pub areas_of_use: SmallVec<[AreaOfUse; 1]>,
    pub deprecated: bool,
    pub preferred: bool,
    pub approximate: bool,
    pub method: OperationMethod,
}

impl CoordinateOperation {
    pub fn metadata(&self) -> CoordinateOperationMetadata {
        CoordinateOperationMetadata {
            id: self.id,
            name: self.name.clone(),
            direction: OperationStepDirection::Forward,
            source_crs_epsg: self.source_crs_epsg,
            target_crs_epsg: self.target_crs_epsg,
            source_datum_epsg: self.source_datum_epsg,
            target_datum_epsg: self.target_datum_epsg,
            accuracy: self.accuracy,
            area_of_use: self.areas_of_use.first().cloned(),
            deprecated: self.deprecated,
            preferred: self.preferred,
            approximate: self.approximate,
            uses_grids: self.uses_grids(),
        }
    }

    pub fn metadata_for_direction(
        &self,
        direction: OperationStepDirection,
    ) -> CoordinateOperationMetadata {
        let mut metadata = self.metadata();
        metadata.direction = direction;
        if matches!(direction, OperationStepDirection::Reverse) {
            std::mem::swap(&mut metadata.source_crs_epsg, &mut metadata.target_crs_epsg);
            std::mem::swap(
                &mut metadata.source_datum_epsg,
                &mut metadata.target_datum_epsg,
            );
        }
        metadata
    }

    pub fn uses_grids(&self) -> bool {
        let mut visited = HashSet::new();
        self.uses_grids_with_visited(&mut visited)
    }

    fn uses_grids_with_visited(&self, visited: &mut HashSet<CoordinateOperationId>) -> bool {
        match &self.method {
            OperationMethod::GridShift { .. } => true,
            OperationMethod::DatumShift {
                source_to_wgs84,
                target_to_wgs84,
            } => source_to_wgs84.uses_grid_shift() || target_to_wgs84.uses_grid_shift(),
            OperationMethod::Concatenated { steps } => steps.iter().any(|step| {
                if !visited.insert(step.operation_id) {
                    return false;
                }
                let uses_grids = crate::registry::lookup_operation(step.operation_id)
                    .map(|operation| operation.uses_grids_with_visited(visited))
                    .unwrap_or(false);
                visited.remove(&step.operation_id);
                uses_grids
            }),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoordinateOperationMetadata {
    pub id: Option<CoordinateOperationId>,
    pub name: String,
    pub direction: OperationStepDirection,
    pub source_crs_epsg: Option<u32>,
    pub target_crs_epsg: Option<u32>,
    pub source_datum_epsg: Option<u32>,
    pub target_datum_epsg: Option<u32>,
    pub accuracy: Option<OperationAccuracy>,
    pub area_of_use: Option<AreaOfUse>,
    pub deprecated: bool,
    pub preferred: bool,
    pub approximate: bool,
    pub uses_grids: bool,
}

#[derive(Debug, Clone)]
pub enum SelectionPolicy {
    /// Select the best supported registry/generated-registry operation,
    /// explicit custom operation, or internal identity behavior.
    BestAvailable,
    /// Require a grid-backed datum operation whenever a datum shift is needed.
    RequireGrids,
    /// Require selected registry operations to match the configured area of interest.
    RequireExactAreaMatch,
    /// Select one explicit registry operation by id.
    Operation(CoordinateOperationId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalGridOffsetConvention {
    /// Grid values are geoid heights in meters (`N`), applied as
    /// gravity height `H = h - N` and ellipsoidal height `h = H + N`.
    GeoidHeightMeters,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerticalGridOperation {
    /// Human-readable operation name used in diagnostics.
    pub name: String,
    /// Grid resource definition resolved through the configured grid provider.
    pub grid: crate::grid::GridDefinition,
    /// Horizontal CRS EPSG code in which the grid is sampled, when known.
    pub grid_horizontal_crs_epsg: Option<u32>,
    /// Optional source vertical CRS EPSG filter.
    pub source_vertical_crs_epsg: Option<u32>,
    /// Optional target vertical CRS EPSG filter.
    pub target_vertical_crs_epsg: Option<u32>,
    /// Optional source gravity-related vertical datum EPSG filter.
    pub source_vertical_datum_epsg: Option<u32>,
    /// Optional target gravity-related vertical datum EPSG filter.
    pub target_vertical_datum_epsg: Option<u32>,
    /// Expected operation accuracy in meters, when known.
    pub accuracy: Option<OperationAccuracy>,
    /// Operation area of use, when distinct from the grid's area.
    pub area_of_use: Option<AreaOfUse>,
    pub offset_convention: VerticalGridOffsetConvention,
}

impl VerticalGridOperation {
    pub fn inverse(&self) -> Self {
        let mut inverse = self.clone();
        std::mem::swap(
            &mut inverse.source_vertical_crs_epsg,
            &mut inverse.target_vertical_crs_epsg,
        );
        std::mem::swap(
            &mut inverse.source_vertical_datum_epsg,
            &mut inverse.target_vertical_datum_epsg,
        );
        inverse
    }
}

#[derive(Clone)]
pub struct SelectionOptions {
    pub area_of_interest: Option<AreaOfInterest>,
    /// Intermediate points sampled per edge when source/target CRS AOI bounds
    /// are normalized to geographic degrees for operation selection.
    ///
    /// Values above [`crate::MAX_BOUNDS_DENSIFY_POINTS`] are rejected during
    /// transform construction.
    pub area_bounds_densify_points: usize,
    pub policy: SelectionPolicy,
    pub grid_provider: Option<Arc<dyn crate::grid::GridProvider>>,
    pub coordinate_operations: Vec<CoordinateOperation>,
    pub vertical_grid_operations: Vec<VerticalGridOperation>,
}

impl Default for SelectionOptions {
    fn default() -> Self {
        Self {
            area_of_interest: None,
            area_bounds_densify_points: DEFAULT_AREA_BOUNDS_DENSIFY_POINTS,
            policy: SelectionPolicy::BestAvailable,
            grid_provider: None,
            coordinate_operations: Vec::new(),
            vertical_grid_operations: Vec::new(),
        }
    }
}

impl SelectionOptions {
    /// Create default selection options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the area of interest used for operation ranking and filtering.
    pub fn with_area_of_interest(mut self, area_of_interest: AreaOfInterest) -> Self {
        self.area_of_interest = Some(area_of_interest);
        self
    }

    /// Set how many intermediate points are sampled on each AOI bounds edge
    /// when source/target CRS bounds are converted to geographic degrees.
    ///
    /// Values above [`crate::MAX_BOUNDS_DENSIFY_POINTS`] are rejected during
    /// transform construction.
    pub fn with_area_bounds_densify_points(mut self, densify_points: usize) -> Self {
        self.area_bounds_densify_points = densify_points;
        self
    }

    /// Set the operation selection policy.
    pub fn with_policy(mut self, policy: SelectionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Select the best supported registry/generated-registry operation,
    /// explicit custom operation, or internal identity behavior.
    ///
    /// This is the default policy.
    pub fn best_available(self) -> Self {
        self.with_policy(SelectionPolicy::BestAvailable)
    }

    /// Require a grid-backed datum operation when a datum operation is needed.
    pub fn require_grids(self) -> Self {
        self.with_policy(SelectionPolicy::RequireGrids)
    }

    /// Require selected operations to match the configured area of interest.
    pub fn require_exact_area_match(self) -> Self {
        self.with_policy(SelectionPolicy::RequireExactAreaMatch)
    }

    /// Select a specific registry operation by id.
    pub fn with_operation(self, operation_id: CoordinateOperationId) -> Self {
        self.with_policy(SelectionPolicy::Operation(operation_id))
    }

    /// Set the grid provider used to resolve grid-backed horizontal and vertical operations.
    pub fn with_grid_provider(mut self, provider: Arc<dyn crate::grid::GridProvider>) -> Self {
        self.grid_provider = Some(provider);
        self
    }

    /// Add one explicit horizontal coordinate operation candidate.
    pub fn with_coordinate_operation(mut self, operation: CoordinateOperation) -> Self {
        self.coordinate_operations.push(operation);
        self
    }

    /// Add explicit horizontal coordinate operation candidates.
    pub fn with_coordinate_operations(
        mut self,
        operations: impl IntoIterator<Item = CoordinateOperation>,
    ) -> Self {
        self.coordinate_operations.extend(operations);
        self
    }

    /// Add one explicit vertical grid operation candidate.
    pub fn with_vertical_grid_operation(mut self, operation: VerticalGridOperation) -> Self {
        self.vertical_grid_operations.push(operation);
        self
    }

    /// Add explicit vertical grid operation candidates.
    pub fn with_vertical_grid_operations(
        mut self,
        operations: impl IntoIterator<Item = VerticalGridOperation>,
    ) -> Self {
        self.vertical_grid_operations.extend(operations);
        self
    }

    pub fn inverse(&self) -> Self {
        Self {
            area_of_interest: self.area_of_interest.map(AreaOfInterest::inverse),
            area_bounds_densify_points: self.area_bounds_densify_points,
            policy: self.policy.clone(),
            grid_provider: self.grid_provider.clone(),
            coordinate_operations: self.coordinate_operations.clone(),
            vertical_grid_operations: self
                .vertical_grid_operations
                .iter()
                .map(VerticalGridOperation::inverse)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    CustomOperation,
    ExplicitOperation,
    ExactSourceTarget,
    AreaOfUseMatch,
    AccuracyPreferred,
    NonDeprecated,
    PreferredOperation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkippedOperationReason {
    AreaOfUseMismatch,
    MissingGrid,
    UnsupportedGridFormat,
    PolicyFiltered,
    LessPreferred,
    Deprecated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkippedOperation {
    pub metadata: CoordinateOperationMetadata,
    pub reason: SkippedOperationReason,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalTransformAction {
    /// No explicit vertical CRS participates in the transform.
    None,
    /// `z` is preserved because the vertical CRS semantics and units match.
    Preserved,
    /// `z` is converted between units of the same vertical reference frame.
    UnitConverted,
    /// `z` is transformed by an explicit vertical operation.
    Transformed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerticalGridProvenance {
    pub name: String,
    /// Content checksum of the resolved grid resource, formatted as `sha256:<hex>`.
    pub checksum: Option<String>,
    pub accuracy: Option<OperationAccuracy>,
    pub area_of_use: Option<AreaOfUse>,
    pub area_of_use_match: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerticalTransformDiagnostics {
    pub action: VerticalTransformAction,
    pub operation_name: Option<String>,
    pub source_vertical_crs_epsg: Option<u32>,
    pub target_vertical_crs_epsg: Option<u32>,
    pub source_vertical_datum_epsg: Option<u32>,
    pub target_vertical_datum_epsg: Option<u32>,
    pub source_unit_to_meter: Option<f64>,
    pub target_unit_to_meter: Option<f64>,
    pub accuracy: Option<OperationAccuracy>,
    pub area_of_use: Option<AreaOfUse>,
    pub area_of_use_match: Option<bool>,
    pub grids: Vec<VerticalGridProvenance>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OperationSelectionDiagnostics {
    pub selected_operation: CoordinateOperationMetadata,
    pub selected_match_kind: OperationMatchKind,
    pub selected_reasons: SmallVec<[SelectionReason; 4]>,
    pub fallback_operations: Vec<CoordinateOperationMetadata>,
    pub skipped_operations: Vec<SkippedOperation>,
    pub approximate: bool,
    pub missing_required_grid: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GridCoverageMiss {
    pub operation: CoordinateOperationMetadata,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformOutcome<T> {
    pub coord: T,
    pub operation: CoordinateOperationMetadata,
    pub vertical: VerticalTransformDiagnostics,
    pub grid_coverage_misses: Vec<GridCoverageMiss>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{EmbeddedGridProvider, GridDefinition, GridFormat};

    fn vertical_grid_operation(
        name: &str,
        source_vertical_crs_epsg: Option<u32>,
        target_vertical_crs_epsg: Option<u32>,
    ) -> VerticalGridOperation {
        VerticalGridOperation {
            name: name.into(),
            grid: GridDefinition {
                id: GridId(1),
                name: format!("{name}.gtx"),
                format: GridFormat::Gtx,
                interpolation: GridInterpolation::Bilinear,
                area_of_use: None,
                resource_names: smallvec::SmallVec::from_vec(vec![format!("{name}.gtx")]),
            },
            grid_horizontal_crs_epsg: Some(4326),
            source_vertical_crs_epsg,
            target_vertical_crs_epsg,
            source_vertical_datum_epsg: Some(1),
            target_vertical_datum_epsg: Some(2),
            accuracy: Some(OperationAccuracy { meters: 0.1 }),
            area_of_use: None,
            offset_convention: VerticalGridOffsetConvention::GeoidHeightMeters,
        }
    }

    fn coordinate_operation(name: &str) -> CoordinateOperation {
        CoordinateOperation {
            id: None,
            name: name.into(),
            source_crs_epsg: None,
            target_crs_epsg: None,
            source_datum_epsg: None,
            target_datum_epsg: None,
            accuracy: Some(OperationAccuracy { meters: 0.0 }),
            areas_of_use: smallvec::SmallVec::new(),
            deprecated: false,
            preferred: true,
            approximate: false,
            method: OperationMethod::Identity,
        }
    }

    #[test]
    fn selection_options_builders_chain_advanced_options() {
        let area = AreaOfInterest::geographic_point(Coord::new(-74.0, 40.0));
        let provider: Arc<dyn crate::grid::GridProvider> = Arc::new(EmbeddedGridProvider);
        let first_operation = coordinate_operation("first operation");
        let second_operation = coordinate_operation("second operation");
        let first = vertical_grid_operation("first", Some(4979), Some(5703));
        let second = vertical_grid_operation("second", Some(4979), Some(5703));

        let options = SelectionOptions::new()
            .with_area_of_interest(area)
            .with_area_bounds_densify_points(32)
            .require_grids()
            .with_grid_provider(provider.clone())
            .with_coordinate_operation(first_operation.clone())
            .with_coordinate_operations([second_operation.clone()])
            .with_vertical_grid_operation(first.clone())
            .with_vertical_grid_operations([second.clone()]);

        assert_eq!(options.area_of_interest, Some(area));
        assert_eq!(options.area_bounds_densify_points, 32);
        assert!(matches!(options.policy, SelectionPolicy::RequireGrids));
        assert!(Arc::ptr_eq(
            options.grid_provider.as_ref().unwrap(),
            &provider
        ));
        assert_eq!(
            options.coordinate_operations,
            vec![first_operation, second_operation]
        );
        assert_eq!(options.vertical_grid_operations, vec![first, second]);
    }

    #[test]
    fn geographic_wrapped_bounds_constructor_marks_antimeridian_aoi() {
        let bounds = Bounds::new(170.0, -20.0, -170.0, -10.0);
        let area = AreaOfInterest::geographic_wrapped_bounds(bounds);

        assert_eq!(area.crs, AreaOfInterestCrs::GeographicDegreesWrapped);
        assert_eq!(area.bounds, Some(bounds));
        assert_eq!(area.point, None);
        assert_eq!(area.inverse(), area);
    }

    #[test]
    fn area_of_use_contains_antimeridian_points_and_bounds() {
        let area = AreaOfUse {
            west: 160.0,
            south: -25.0,
            east: -160.0,
            north: -5.0,
            name: "Pacific antimeridian test area".into(),
        };

        assert!(area.contains_point(Coord::new(170.0, -15.0)));
        assert!(area.contains_point(Coord::new(-170.0, -15.0)));
        assert!(!area.contains_point(Coord::new(0.0, -15.0)));
        assert!(area.contains_bounds(Bounds::new(170.0, -20.0, -170.0, -10.0)));
        assert!(!area.contains_bounds(Bounds::new(150.0, -20.0, -170.0, -10.0)));

        let world = AreaOfUse {
            west: -180.0,
            south: -90.0,
            east: 180.0,
            north: 90.0,
            name: "World".into(),
        };
        assert!(world.contains_bounds(Bounds::new(170.0, -20.0, -170.0, -10.0)));
    }

    #[test]
    fn selection_options_policy_builders_cover_all_modes() {
        assert!(matches!(
            SelectionOptions::new().best_available().policy,
            SelectionPolicy::BestAvailable
        ));
        assert!(matches!(
            SelectionOptions::new().require_exact_area_match().policy,
            SelectionPolicy::RequireExactAreaMatch
        ));
        assert!(matches!(
            SelectionOptions::new()
                .with_operation(CoordinateOperationId(1234))
                .policy,
            SelectionPolicy::Operation(CoordinateOperationId(1234))
        ));
    }

    #[test]
    fn selection_options_inverse_preserves_builder_values() {
        let options = SelectionOptions::new()
            .with_area_of_interest(AreaOfInterest::source_crs_point(Coord::new(1.0, 2.0)))
            .with_area_bounds_densify_points(32)
            .with_policy(SelectionPolicy::RequireExactAreaMatch)
            .with_coordinate_operation(coordinate_operation("operation"))
            .with_vertical_grid_operation(vertical_grid_operation("grid", Some(4979), Some(5703)));

        let inverse = options.inverse();

        assert!(matches!(
            inverse.area_of_interest,
            Some(AreaOfInterest {
                crs: AreaOfInterestCrs::TargetCrs,
                point: Some(Coord { x: 1.0, y: 2.0 }),
                bounds: None,
            })
        ));
        assert!(matches!(
            inverse.policy,
            SelectionPolicy::RequireExactAreaMatch
        ));
        assert_eq!(inverse.area_bounds_densify_points, 32);
        assert_eq!(inverse.coordinate_operations, options.coordinate_operations);
        assert_eq!(
            inverse.vertical_grid_operations[0].source_vertical_crs_epsg,
            Some(5703)
        );
        assert_eq!(
            inverse.vertical_grid_operations[0].target_vertical_crs_epsg,
            Some(4979)
        );
    }
}

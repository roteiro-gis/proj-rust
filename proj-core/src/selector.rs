use crate::coord::{Bounds, Coord};
use crate::crs::CrsDef;
use crate::error::{Error, Result};
use crate::grid::GridFormat;
use crate::operation::{
    AreaOfInterest, AreaOfUse, CoordinateOperation, CoordinateOperationMetadata, GridId,
    OperationAccuracy, OperationMatchKind, OperationMethod, OperationStepDirection,
    SelectionOptions, SelectionPolicy, SelectionReason, SkippedOperation, SkippedOperationReason,
};
use crate::projection::{make_projection, validate_lon_lat, validate_projected};
use crate::registry;
use crate::transform::bounds_densify_segments;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub(crate) enum SelectedOperationKind {
    Identity,
    Registry(Box<Cow<'static, CoordinateOperation>>),
    Custom(Box<CoordinateOperation>),
}

impl SelectedOperationKind {
    pub(crate) fn registry_owned(operation: CoordinateOperation) -> Self {
        Self::Registry(Box::new(Cow::Owned(operation)))
    }

    pub(crate) fn registry_borrowed(operation: &'static CoordinateOperation) -> Self {
        Self::Registry(Box::new(Cow::Borrowed(operation)))
    }

    pub(crate) fn custom(operation: CoordinateOperation) -> Self {
        Self::Custom(Box::new(operation))
    }

    pub(crate) fn into_owned(self) -> Self {
        match self {
            Self::Identity => Self::Identity,
            Self::Registry(operation) => {
                Self::Registry(Box::new(Cow::Owned((*operation).into_owned())))
            }
            Self::Custom(operation) => Self::Custom(operation),
        }
    }

    pub(crate) fn as_operation(&self) -> Option<&CoordinateOperation> {
        match self {
            Self::Identity => None,
            Self::Registry(operation) => Some(operation.as_ref().as_ref()),
            Self::Custom(operation) => Some(operation),
        }
    }

    pub(crate) fn metadata_for_direction(
        &self,
        source: &CrsDef,
        target: &CrsDef,
        direction: OperationStepDirection,
        matched_area_of_use: Option<AreaOfUse>,
    ) -> CoordinateOperationMetadata {
        match self {
            Self::Identity => identity_metadata(source, target, direction),
            Self::Registry(operation) => {
                let mut metadata = operation.as_ref().metadata_for_direction(direction);
                metadata.area_of_use =
                    matched_area_of_use.or_else(|| operation.areas_of_use.first().cloned());
                metadata
            }
            Self::Custom(operation) => {
                let mut metadata = operation.metadata_for_direction(direction);
                metadata.area_of_use =
                    matched_area_of_use.or_else(|| operation.areas_of_use.first().cloned());
                metadata
            }
        }
    }

    pub(crate) fn uses_grids(&self) -> bool {
        self.as_operation()
            .map(CoordinateOperation::uses_grids)
            .unwrap_or(false)
    }

    pub(crate) fn deprecated(&self) -> bool {
        self.as_operation()
            .map(|operation| operation.deprecated)
            .unwrap_or(false)
    }

    pub(crate) fn preferred(&self) -> bool {
        self.as_operation()
            .map(|operation| operation.preferred)
            .unwrap_or(true)
    }

    pub(crate) fn approximate(&self) -> bool {
        self.as_operation()
            .map(|operation| operation.approximate)
            .unwrap_or(false)
    }

    pub(crate) fn grid_format_preference(&self) -> u8 {
        self.as_operation()
            .map(operation_grid_format_preference)
            .unwrap_or(0)
    }

    pub(crate) fn accuracy(&self) -> Option<OperationAccuracy> {
        match self {
            Self::Identity => Some(OperationAccuracy { meters: 0.0 }),
            Self::Registry(operation) => operation.accuracy,
            Self::Custom(operation) => operation.accuracy,
        }
    }
}

pub(crate) struct RankedOperationCandidate {
    pub(crate) operation: SelectedOperationKind,
    pub(crate) direction: OperationStepDirection,
    pub(crate) match_kind: OperationMatchKind,
    pub(crate) matched_area_of_use: Option<AreaOfUse>,
    pub(crate) reasons: SmallVec<[SelectionReason; 4]>,
}

impl RankedOperationCandidate {
    pub(crate) fn metadata(&self, source: &CrsDef, target: &CrsDef) -> CoordinateOperationMetadata {
        self.operation.metadata_for_direction(
            source,
            target,
            self.direction,
            self.matched_area_of_use.clone(),
        )
    }
}

pub(crate) struct OperationCandidateSet {
    pub(crate) ranked: Vec<RankedOperationCandidate>,
    pub(crate) skipped: Vec<SkippedOperation>,
}

pub(crate) fn rank_operation_candidates(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
) -> Result<OperationCandidateSet> {
    let resolved_aoi = resolve_area_of_interest(source, target, options)?;
    if let SelectionPolicy::Operation(id) = options.policy {
        let Some(operation) = registry::lookup_operation(id) else {
            return Ok(OperationCandidateSet {
                ranked: Vec::new(),
                skipped: Vec::new(),
            });
        };
        let Some(direction) = compatible_direction(source, target, &operation) else {
            return Ok(OperationCandidateSet {
                ranked: Vec::new(),
                skipped: Vec::new(),
            });
        };
        let matched_area_of_use = matched_area_of_use(resolved_aoi.as_ref(), &operation);
        let reasons = explicit_selection_reasons(matched_area_of_use.is_some(), &operation);
        return Ok(OperationCandidateSet {
            ranked: vec![RankedOperationCandidate {
                operation: SelectedOperationKind::registry_owned(operation),
                direction,
                match_kind: OperationMatchKind::Explicit,
                matched_area_of_use,
                reasons,
            }],
            skipped: Vec::new(),
        });
    }

    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    if requires_no_datum_operation(source, target) {
        candidates.push(RankedOperationCandidate {
            operation: SelectedOperationKind::Identity,
            direction: OperationStepDirection::Forward,
            match_kind: OperationMatchKind::ExactSourceTarget,
            matched_area_of_use: None,
            reasons: SmallVec::from_slice(&[
                SelectionReason::ExactSourceTarget,
                SelectionReason::PreferredOperation,
            ]),
        });
    }

    for operation in &options.coordinate_operations {
        let Some(direction) = custom_operation_direction(source, target, operation) else {
            continue;
        };
        let matched_area_of_use = matched_area_of_use(resolved_aoi.as_ref(), operation);
        if let Some((reason, detail)) = policy_skip_reason(
            source,
            target,
            options,
            matched_area_of_use.is_some(),
            operation,
        ) {
            skipped.push(SkippedOperation {
                metadata: selection_metadata(operation, direction, matched_area_of_use),
                reason,
                detail,
            });
            continue;
        }

        let mut reasons = SmallVec::from_slice(&[SelectionReason::CustomOperation]);
        if matched_area_of_use.is_some() {
            reasons.push(SelectionReason::AreaOfUseMatch);
        }
        if !operation.deprecated {
            reasons.push(SelectionReason::NonDeprecated);
        }
        if operation.preferred {
            reasons.push(SelectionReason::PreferredOperation);
        }

        candidates.push(RankedOperationCandidate {
            operation: SelectedOperationKind::custom(operation.clone()),
            direction,
            match_kind: OperationMatchKind::Custom,
            matched_area_of_use,
            reasons,
        });
    }

    let source_geo = source.base_geographic_crs_epsg();
    let target_geo = target.base_geographic_crs_epsg();
    for operation in registry::related_operations(source, target) {
        for direction in [
            OperationStepDirection::Forward,
            OperationStepDirection::Reverse,
        ] {
            let compatible = match direction {
                OperationStepDirection::Forward => is_compatible(source_geo, target_geo, operation),
                OperationStepDirection::Reverse => {
                    is_compatible_reversed(source_geo, target_geo, operation)
                }
            };
            if !compatible {
                continue;
            }

            let matched_area_of_use = matched_area_of_use(resolved_aoi.as_ref(), operation);
            if let Some((reason, detail)) = policy_skip_reason(
                source,
                target,
                options,
                matched_area_of_use.is_some(),
                operation,
            ) {
                skipped.push(SkippedOperation {
                    metadata: selection_metadata(operation, direction, matched_area_of_use),
                    reason,
                    detail,
                });
                continue;
            }

            let mut reasons = SmallVec::<[SelectionReason; 4]>::new();
            let match_kind = match_kind_for_candidate(source, target, direction, operation);
            if matches!(match_kind, OperationMatchKind::ExactSourceTarget) {
                reasons.push(SelectionReason::ExactSourceTarget);
            }
            if matched_area_of_use.is_some() {
                reasons.push(SelectionReason::AreaOfUseMatch);
            }
            if !operation.deprecated {
                reasons.push(SelectionReason::NonDeprecated);
            }
            if operation.preferred {
                reasons.push(SelectionReason::PreferredOperation);
            }
            candidates.push(RankedOperationCandidate {
                operation: SelectedOperationKind::registry_borrowed(operation),
                direction,
                match_kind,
                matched_area_of_use,
                reasons,
            });
        }
    }

    candidates.sort_by(compare_candidates);
    Ok(OperationCandidateSet {
        ranked: candidates,
        skipped,
    })
}

fn compatible_direction(
    source: &CrsDef,
    target: &CrsDef,
    operation: &CoordinateOperation,
) -> Option<OperationStepDirection> {
    let source_geo = source.base_geographic_crs_epsg();
    let target_geo = target.base_geographic_crs_epsg();
    if is_compatible(source_geo, target_geo, operation) {
        Some(OperationStepDirection::Forward)
    } else if is_compatible_reversed(source_geo, target_geo, operation) {
        Some(OperationStepDirection::Reverse)
    } else {
        None
    }
}

fn custom_operation_direction(
    source: &CrsDef,
    target: &CrsDef,
    operation: &CoordinateOperation,
) -> Option<OperationStepDirection> {
    if custom_operation_matches_pair(source, target, operation) {
        Some(OperationStepDirection::Forward)
    } else if custom_operation_matches_pair_reversed(source, target, operation) {
        Some(OperationStepDirection::Reverse)
    } else {
        None
    }
}

fn custom_operation_matches_pair(
    source: &CrsDef,
    target: &CrsDef,
    operation: &CoordinateOperation,
) -> bool {
    operation_crs_filter_matches(operation.source_crs_epsg, source)
        && operation_crs_filter_matches(operation.target_crs_epsg, target)
        && operation_datum_filter_matches(operation.source_datum_epsg, source)
        && operation_datum_filter_matches(operation.target_datum_epsg, target)
}

fn custom_operation_matches_pair_reversed(
    source: &CrsDef,
    target: &CrsDef,
    operation: &CoordinateOperation,
) -> bool {
    operation_crs_filter_matches(operation.source_crs_epsg, target)
        && operation_crs_filter_matches(operation.target_crs_epsg, source)
        && operation_datum_filter_matches(operation.source_datum_epsg, target)
        && operation_datum_filter_matches(operation.target_datum_epsg, source)
}

fn operation_crs_filter_matches(filter: Option<u32>, crs: &CrsDef) -> bool {
    filter.is_none_or(|code| {
        (crs.epsg() != 0 && crs.epsg() == code) || crs.base_geographic_crs_epsg() == Some(code)
    })
}

fn operation_datum_filter_matches(filter: Option<u32>, crs: &CrsDef) -> bool {
    filter.is_none_or(|code| {
        crs.base_geographic_crs_epsg()
            .and_then(crate::epsg_db::lookup_datum_code_for_crs)
            == Some(code)
            || registry::lookup_datum_epsg(code).is_some_and(|datum| crs.datum().same_datum(&datum))
    })
}

fn explicit_selection_reasons(
    area_matches: bool,
    operation: &CoordinateOperation,
) -> SmallVec<[SelectionReason; 4]> {
    let mut reasons = SmallVec::from_slice(&[SelectionReason::ExplicitOperation]);
    if area_matches {
        reasons.push(SelectionReason::AreaOfUseMatch);
    }
    if !operation.deprecated {
        reasons.push(SelectionReason::NonDeprecated);
    }
    if operation.preferred {
        reasons.push(SelectionReason::PreferredOperation);
    }
    reasons
}

fn match_kind_for_candidate(
    source: &CrsDef,
    target: &CrsDef,
    direction: OperationStepDirection,
    operation: &CoordinateOperation,
) -> OperationMatchKind {
    let (candidate_source, candidate_target) = match direction {
        OperationStepDirection::Forward => (operation.source_crs_epsg, operation.target_crs_epsg),
        OperationStepDirection::Reverse => (operation.target_crs_epsg, operation.source_crs_epsg),
    };

    if candidate_source == Some(source.epsg()) && candidate_target == Some(target.epsg()) {
        OperationMatchKind::ExactSourceTarget
    } else if candidate_source == source.base_geographic_crs_epsg()
        && candidate_target == target.base_geographic_crs_epsg()
    {
        OperationMatchKind::DerivedGeographic
    } else {
        OperationMatchKind::DatumCompatible
    }
}

fn is_compatible(
    source_geo: Option<u32>,
    target_geo: Option<u32>,
    operation: &CoordinateOperation,
) -> bool {
    let source_datum = source_geo.and_then(crate::epsg_db::lookup_datum_code_for_crs);
    let target_datum = target_geo.and_then(crate::epsg_db::lookup_datum_code_for_crs);
    match (source_geo, target_geo) {
        (Some(source_code), Some(target_code)) => {
            (operation.source_crs_epsg == Some(source_code)
                && operation.target_crs_epsg == Some(target_code))
                || (source_datum.is_some()
                    && target_datum.is_some()
                    && operation.source_datum_epsg == source_datum
                    && operation.target_datum_epsg == target_datum)
        }
        _ => false,
    }
}

fn is_compatible_reversed(
    source_geo: Option<u32>,
    target_geo: Option<u32>,
    operation: &CoordinateOperation,
) -> bool {
    let source_datum = source_geo.and_then(crate::epsg_db::lookup_datum_code_for_crs);
    let target_datum = target_geo.and_then(crate::epsg_db::lookup_datum_code_for_crs);
    match (source_geo, target_geo) {
        (Some(source_code), Some(target_code)) => {
            (operation.source_crs_epsg == Some(target_code)
                && operation.target_crs_epsg == Some(source_code))
                || (source_datum.is_some()
                    && target_datum.is_some()
                    && operation.source_datum_epsg == target_datum
                    && operation.target_datum_epsg == source_datum)
        }
        _ => false,
    }
}

fn requires_no_datum_operation(source: &CrsDef, target: &CrsDef) -> bool {
    (source.epsg() != 0 && source.epsg() == target.epsg())
        || (source.base_geographic_crs_epsg().is_some()
            && source.base_geographic_crs_epsg() == target.base_geographic_crs_epsg())
        || source.datum().same_datum(target.datum())
}

fn policy_skip_reason(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
    matched_area: bool,
    operation: &CoordinateOperation,
) -> Option<(SkippedOperationReason, String)> {
    let datum_shift_required = !requires_no_datum_operation(source, target);
    match options.policy {
        SelectionPolicy::BestAvailable => None,
        SelectionPolicy::RequireGrids => {
            if datum_shift_required && !operation.uses_grids() {
                Some((
                    SkippedOperationReason::PolicyFiltered,
                    "selection policy requires a grid-backed datum operation".into(),
                ))
            } else {
                None
            }
        }
        SelectionPolicy::RequireExactAreaMatch => {
            if options.area_of_interest.is_some() && !matched_area {
                Some((
                    SkippedOperationReason::AreaOfUseMismatch,
                    "selection policy requires an exact area-of-use match".into(),
                ))
            } else {
                None
            }
        }
        SelectionPolicy::Operation(_) => None,
    }
}

fn selection_metadata(
    operation: &CoordinateOperation,
    direction: OperationStepDirection,
    matched_area_of_use: Option<AreaOfUse>,
) -> CoordinateOperationMetadata {
    let mut metadata = operation.metadata_for_direction(direction);
    metadata.area_of_use = matched_area_of_use.or_else(|| operation.areas_of_use.first().cloned());
    metadata
}

fn identity_metadata(
    source: &CrsDef,
    target: &CrsDef,
    direction: OperationStepDirection,
) -> CoordinateOperationMetadata {
    let mut source_crs_epsg = source.base_geographic_crs_epsg();
    let mut target_crs_epsg = target.base_geographic_crs_epsg();
    if matches!(direction, OperationStepDirection::Reverse) {
        std::mem::swap(&mut source_crs_epsg, &mut target_crs_epsg);
    }

    CoordinateOperationMetadata {
        id: None,
        name: "Identity".into(),
        direction,
        source_crs_epsg,
        target_crs_epsg,
        source_datum_epsg: None,
        target_datum_epsg: None,
        accuracy: Some(OperationAccuracy { meters: 0.0 }),
        area_of_use: None,
        deprecated: false,
        preferred: true,
        approximate: false,
        uses_grids: false,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedAreaOfInterest {
    pub(crate) point: Option<Coord>,
    pub(crate) bounds: Option<Bounds>,
}

pub(crate) fn resolve_area_of_interest(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
) -> Result<Option<ResolvedAreaOfInterest>> {
    let Some(area) = options.area_of_interest else {
        return Ok(None);
    };
    let point = match area.point {
        Some(point) => Some(resolve_area_point(area, point, source, target)?),
        None => None,
    };
    let bounds = match area.bounds {
        Some(bounds) => Some(resolve_area_bounds(
            area,
            bounds,
            source,
            target,
            options.area_bounds_densify_points,
        )?),
        None => None,
    };
    Ok(Some(ResolvedAreaOfInterest { point, bounds }))
}

fn matched_area_of_use(
    area: Option<&ResolvedAreaOfInterest>,
    operation: &CoordinateOperation,
) -> Option<AreaOfUse> {
    let area = area?;
    if area.point.is_none() && area.bounds.is_none() {
        return None;
    }
    operation
        .areas_of_use
        .iter()
        .find(|candidate| {
            area.point
                .map(|value| candidate.contains_point(value))
                .unwrap_or(false)
                || area
                    .bounds
                    .map(|value| candidate.contains_bounds(value))
                    .unwrap_or(false)
        })
        .cloned()
}

fn compare_candidates(
    left: &RankedOperationCandidate,
    right: &RankedOperationCandidate,
) -> Ordering {
    match_kind_rank(right.match_kind)
        .cmp(&match_kind_rank(left.match_kind))
        .then_with(|| {
            right
                .matched_area_of_use
                .is_some()
                .cmp(&left.matched_area_of_use.is_some())
        })
        .then_with(|| {
            let left_accuracy = left
                .operation
                .accuracy()
                .map(|accuracy| accuracy.meters)
                .unwrap_or(f64::MAX);
            let right_accuracy = right
                .operation
                .accuracy()
                .map(|accuracy| accuracy.meters)
                .unwrap_or(f64::MAX);
            left_accuracy
                .partial_cmp(&right_accuracy)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            // Among equally accurate matches, prefer the operation scoped to
            // the smaller (more specific) area of use, as C PROJ does.
            matched_pseudo_area(left.matched_area_of_use.as_ref())
                .partial_cmp(&matched_pseudo_area(right.matched_area_of_use.as_ref()))
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            left.operation
                .deprecated()
                .cmp(&right.operation.deprecated())
        })
        .then_with(|| {
            right
                .operation
                .grid_format_preference()
                .cmp(&left.operation.grid_format_preference())
        })
        .then_with(|| right.operation.preferred().cmp(&left.operation.preferred()))
}

/// Spherical pseudo-area of a matched area of use: `(sin N − sin S)` times the
/// antimeridian-aware longitude span. Only relative order matters; unmatched
/// candidates rank as infinitely large.
fn matched_pseudo_area(area: Option<&AreaOfUse>) -> f64 {
    let Some(area) = area else {
        return f64::INFINITY;
    };
    let width = if area.east >= area.west {
        area.east - area.west
    } else {
        area.east - area.west + 360.0
    };
    (area.north.to_radians().sin() - area.south.to_radians().sin()) * width
}

fn operation_grid_format_preference(operation: &CoordinateOperation) -> u8 {
    let mut visited = std::collections::HashSet::new();
    operation_grid_format_preference_with_visited(operation, &mut visited)
}

fn operation_grid_format_preference_with_visited(
    operation: &CoordinateOperation,
    visited: &mut std::collections::HashSet<crate::operation::CoordinateOperationId>,
) -> u8 {
    match &operation.method {
        OperationMethod::GridShift { grid_id, .. } => grid_format_preference(*grid_id),
        OperationMethod::Concatenated { steps } => steps
            .iter()
            .filter_map(|step| {
                if !visited.insert(step.operation_id) {
                    return None;
                }
                let preference = registry::lookup_operation(step.operation_id).map(|operation| {
                    operation_grid_format_preference_with_visited(&operation, visited)
                });
                visited.remove(&step.operation_id);
                preference
            })
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

fn grid_format_preference(grid_id: GridId) -> u8 {
    registry::lookup_grid_definition(grid_id.0)
        .map(|grid| match grid.format {
            GridFormat::GeoTiff => 3,
            GridFormat::Ntv2 | GridFormat::Gtx => 2,
            GridFormat::Unsupported => 0,
        })
        .unwrap_or(0)
}

fn match_kind_rank(kind: OperationMatchKind) -> u8 {
    match kind {
        OperationMatchKind::Explicit => 5,
        OperationMatchKind::Custom => 4,
        OperationMatchKind::ExactSourceTarget => 3,
        OperationMatchKind::DerivedGeographic => 2,
        OperationMatchKind::DatumCompatible => 1,
    }
}

fn resolve_area_point(
    area: AreaOfInterest,
    point: Coord,
    source: &CrsDef,
    target: &CrsDef,
) -> Result<Coord> {
    match area.crs {
        crate::operation::AreaOfInterestCrs::GeographicDegrees
        | crate::operation::AreaOfInterestCrs::GeographicDegreesWrapped => {
            validate_geographic_area_point(point)?;
            Ok(point)
        }
        crate::operation::AreaOfInterestCrs::SourceCrs => geographic_from_crs_point(source, point),
        crate::operation::AreaOfInterestCrs::TargetCrs => geographic_from_crs_point(target, point),
    }
}

fn resolve_area_bounds(
    area: AreaOfInterest,
    bounds: Bounds,
    source: &CrsDef,
    target: &CrsDef,
    densify_points: usize,
) -> Result<Bounds> {
    let crs = match area.crs {
        crate::operation::AreaOfInterestCrs::GeographicDegrees => {
            validate_geographic_area_bounds(bounds)?;
            return Ok(bounds);
        }
        crate::operation::AreaOfInterestCrs::GeographicDegreesWrapped => {
            validate_wrapped_geographic_area_bounds(bounds)?;
            return Ok(bounds);
        }
        crate::operation::AreaOfInterestCrs::SourceCrs => source,
        crate::operation::AreaOfInterestCrs::TargetCrs => target,
    };

    if crs.is_geographic() {
        validate_geographic_area_bounds(bounds)?;
        return Ok(bounds);
    }

    validate_area_bounds_shape(bounds)?;

    let segments = bounds_densify_segments(densify_points)?;
    let mut transformed = GeographicBoundsAccumulator::new();
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
            let point = geographic_from_crs_point(crs, sample)?;
            transformed.include(point);
        }
    }
    Ok(transformed.into_bounds().unwrap_or(bounds))
}

struct GeographicBoundsAccumulator {
    longitudes: Vec<f64>,
    min_lat: f64,
    max_lat: f64,
}

impl GeographicBoundsAccumulator {
    fn new() -> Self {
        Self {
            longitudes: Vec::new(),
            min_lat: f64::INFINITY,
            max_lat: f64::NEG_INFINITY,
        }
    }

    fn include(&mut self, point: Coord) {
        self.longitudes.push(point.x);
        self.min_lat = self.min_lat.min(point.y);
        self.max_lat = self.max_lat.max(point.y);
    }

    fn into_bounds(self) -> Option<Bounds> {
        if self.longitudes.is_empty() {
            return None;
        }

        let mut sorted = self
            .longitudes
            .into_iter()
            .map(normalize_longitude_positive)
            .collect::<Vec<_>>();
        sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));

        let first = sorted[0];
        let last = sorted[sorted.len() - 1];
        let mut largest_gap = first + 360.0 - last;
        let mut west = first;
        let mut east = last;

        for pair in sorted.windows(2) {
            let gap = pair[1] - pair[0];
            if gap > largest_gap {
                largest_gap = gap;
                west = pair[1];
                east = pair[0];
            }
        }

        Some(Bounds::new(
            normalize_longitude_signed(west),
            self.min_lat,
            normalize_longitude_signed(east),
            self.max_lat,
        ))
    }
}

fn normalize_longitude_positive(longitude: f64) -> f64 {
    longitude.rem_euclid(360.0)
}

fn normalize_longitude_signed(longitude: f64) -> f64 {
    let normalized = normalize_longitude_positive(longitude);
    if normalized > 180.0 {
        normalized - 360.0
    } else {
        normalized
    }
}

fn geographic_from_crs_point(crs: &CrsDef, point: Coord) -> Result<Coord> {
    match crs {
        _ if crs.is_geographic() => {
            validate_geographic_area_point(point)?;
            Ok(point)
        }
        _ if crs.is_projected() => {
            let projected = crs.as_projected().ok_or_else(|| {
                Error::InvalidDefinition("projected CRS component is missing".into())
            })?;
            validate_projected(point.x, point.y)?;
            let projection = make_projection(&projected.method(), projected.datum())?;
            let (lon, lat) = projection.inverse(
                projected.linear_unit().to_meters(point.x),
                projected.linear_unit().to_meters(point.y),
            )?;
            let geographic = Coord::new(lon.to_degrees(), lat.to_degrees());
            validate_geographic_area_point(geographic)?;
            Ok(geographic)
        }
        _ => Err(Error::InvalidDefinition(
            "area-of-interest CRS must have a horizontal component".into(),
        )),
    }
}

fn validate_area_bounds_shape(bounds: Bounds) -> Result<()> {
    if !bounds.is_valid() {
        return Err(Error::OutOfRange(
            "area-of-interest bounds must be finite and satisfy min <= max".into(),
        ));
    }
    Ok(())
}

fn validate_geographic_area_bounds(bounds: Bounds) -> Result<()> {
    validate_area_bounds_shape(bounds)?;
    validate_geographic_bounds_coordinates(bounds)
}

fn validate_wrapped_geographic_area_bounds(bounds: Bounds) -> Result<()> {
    validate_wrapped_area_bounds_shape(bounds)?;
    validate_geographic_bounds_coordinates(bounds)
}

fn validate_wrapped_area_bounds_shape(bounds: Bounds) -> Result<()> {
    if !bounds.min_x.is_finite()
        || !bounds.min_y.is_finite()
        || !bounds.max_x.is_finite()
        || !bounds.max_y.is_finite()
        || bounds.min_x <= bounds.max_x
        || bounds.min_y > bounds.max_y
    {
        return Err(Error::OutOfRange(
            "wrapped geographic area-of-interest bounds must be finite and satisfy west > east and south <= north".into(),
        ));
    }
    Ok(())
}

fn validate_geographic_bounds_coordinates(bounds: Bounds) -> Result<()> {
    for point in [
        Coord::new(bounds.min_x, bounds.min_y),
        Coord::new(bounds.min_x, bounds.max_y),
        Coord::new(bounds.max_x, bounds.min_y),
        Coord::new(bounds.max_x, bounds.max_y),
    ] {
        validate_geographic_area_point(point)?;
    }
    Ok(())
}

fn validate_geographic_area_point(point: Coord) -> Result<()> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return Err(Error::OutOfRange(
            "geographic area-of-interest coordinate must be finite".into(),
        ));
    }
    if !(-180.0..=180.0).contains(&point.x) {
        return Err(Error::OutOfRange(format!(
            "geographic area-of-interest longitude {:.8}° is outside [-180°, 180°]",
            point.x
        )));
    }
    if !(-90.0..=90.0).contains(&point.y) {
        return Err(Error::OutOfRange(format!(
            "geographic area-of-interest latitude {:.8}° is outside [-90°, 90°]",
            point.y
        )));
    }

    validate_lon_lat(point.x.to_radians(), point.y.to_radians())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_allowed_candidate_kinds(candidates: &OperationCandidateSet) {
        assert!(candidates.ranked.iter().all(|candidate| {
            matches!(
                candidate.operation,
                SelectedOperationKind::Identity
                    | SelectedOperationKind::Registry(_)
                    | SelectedOperationKind::Custom(_)
            )
        }));
    }

    #[test]
    fn ranked_default_candidates_are_registry_or_identity_only() {
        let wgs84 = registry::lookup_epsg(4326).expect("EPSG:4326");
        let same = rank_operation_candidates(&wgs84, &wgs84, &SelectionOptions::new()).unwrap();
        assert_allowed_candidate_kinds(&same);
        assert!(same
            .ranked
            .iter()
            .any(|candidate| matches!(candidate.operation, SelectedOperationKind::Identity)));

        let nad27 = registry::lookup_epsg(4267).expect("EPSG:4267");
        let nad83 = registry::lookup_epsg(4269).expect("EPSG:4269");
        let registry_candidates =
            rank_operation_candidates(&nad27, &nad83, &SelectionOptions::new()).unwrap();
        assert_allowed_candidate_kinds(&registry_candidates);
        assert!(registry_candidates
            .ranked
            .iter()
            .any(|candidate| matches!(candidate.operation, SelectedOperationKind::Registry(_))));
    }

    #[test]
    fn geographic_bounds_accumulator_preserves_antimeridian_wrap() {
        let mut bounds = GeographicBoundsAccumulator::new();
        bounds.include(Coord::new(170.0, -20.0));
        bounds.include(Coord::new(-170.0, -10.0));
        bounds.include(Coord::new(178.0, -15.0));

        let bounds = bounds.into_bounds().unwrap();

        assert_eq!(bounds, Bounds::new(170.0, -20.0, -170.0, -10.0));
    }

    #[test]
    fn projected_area_bounds_use_configured_densification() {
        let source = registry::lookup_epsg(3413).expect("EPSG:3413");
        let target = registry::lookup_epsg(4326).expect("EPSG:4326");
        let projected = source.as_projected().expect("projected CRS");
        let projection = make_projection(&projected.method(), projected.datum()).unwrap();
        let unit = projected.linear_unit();

        let bounds = [
            (-60.0f64, 70.0f64),
            (60.0, 70.0),
            (60.0, 80.0),
            (-60.0, 80.0),
        ]
        .into_iter()
        .map(|(lon, lat)| {
            let (x, y) = projection
                .forward(lon.to_radians(), lat.to_radians())
                .unwrap();
            Coord::new(unit.from_meters(x), unit.from_meters(y))
        })
        .fold(None, |bounds: Option<Bounds>, point| {
            Some(match bounds {
                Some(mut bounds) => {
                    bounds.expand_to_include(point);
                    bounds
                }
                None => Bounds::new(point.x, point.y, point.x, point.y),
            })
        })
        .unwrap();

        let area = AreaOfInterest::source_crs_bounds(bounds);
        let coarse = resolve_area_bounds(area, bounds, &source, &target, 0).unwrap();
        let dense = resolve_area_bounds(area, bounds, &source, &target, 21).unwrap();

        assert_ne!(coarse, dense);
    }

    #[test]
    fn projected_area_bounds_reject_excessive_densification() {
        let source = registry::lookup_epsg(3857).expect("EPSG:3857");
        let target = registry::lookup_epsg(4326).expect("EPSG:4326");
        let bounds = Bounds::new(-1_000.0, -1_000.0, 1_000.0, 1_000.0);
        let area = AreaOfInterest::source_crs_bounds(bounds);

        let err = resolve_area_bounds(
            area,
            bounds,
            &source,
            &target,
            crate::MAX_BOUNDS_DENSIFY_POINTS + 1,
        )
        .unwrap_err();

        assert!(matches!(err, Error::OutOfRange(_)));
        assert!(err.to_string().contains("exceeds maximum"));
    }
}

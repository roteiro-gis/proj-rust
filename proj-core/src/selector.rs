use crate::coord::{Bounds, Coord};
use crate::crs::CrsDef;
use crate::operation::{
    CoordinateOperation, OperationMatchKind, OperationMethod, OperationStepDirection,
    SelectionOptions, SelectionPolicy, SelectionReason,
};
use crate::registry;
use smallvec::SmallVec;
use std::cmp::Ordering;

pub(crate) struct RankedOperationCandidate {
    pub(crate) operation: CoordinateOperation,
    pub(crate) direction: OperationStepDirection,
    pub(crate) match_kind: OperationMatchKind,
    pub(crate) reasons: SmallVec<[SelectionReason; 4]>,
}

pub(crate) fn rank_operation_candidates(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
) -> Vec<RankedOperationCandidate> {
    if let SelectionPolicy::Operation(id) = options.policy {
        let Some(operation) = registry::lookup_operation(id) else {
            return Vec::new();
        };
        let Some(direction) = compatible_direction(source, target, &operation) else {
            return Vec::new();
        };
        let reasons = explicit_selection_reasons(options, &operation);
        return vec![RankedOperationCandidate {
            operation,
            direction,
            match_kind: OperationMatchKind::Explicit,
            reasons,
        }];
    }

    let mut candidates = Vec::new();

    if requires_no_datum_operation(source, target) {
        candidates.push(RankedOperationCandidate {
            operation: CoordinateOperation {
                id: None,
                name: "Identity".into(),
                source_crs_epsg: source.base_geographic_crs_epsg(),
                target_crs_epsg: target.base_geographic_crs_epsg(),
                source_datum_epsg: None,
                target_datum_epsg: None,
                accuracy: Some(crate::operation::OperationAccuracy { meters: 0.0 }),
                areas_of_use: SmallVec::new(),
                deprecated: false,
                preferred: true,
                approximate: false,
                method: OperationMethod::Identity,
            },
            direction: OperationStepDirection::Forward,
            match_kind: OperationMatchKind::ExactSourceTarget,
            reasons: SmallVec::from_slice(&[
                SelectionReason::ExactSourceTarget,
                SelectionReason::PreferredOperation,
            ]),
        });
    }

    let source_geo = source.base_geographic_crs_epsg();
    let target_geo = target.base_geographic_crs_epsg();
    for operation in registry::all_operations() {
        for direction in [OperationStepDirection::Forward, OperationStepDirection::Reverse] {
            let compatible = match direction {
                OperationStepDirection::Forward => is_compatible(source_geo, target_geo, &operation),
                OperationStepDirection::Reverse => is_compatible_reversed(source_geo, target_geo, &operation),
            };
            if !compatible || !policy_allows(source, target, options, &operation) {
                continue;
            }

            let mut reasons = SmallVec::<[SelectionReason; 4]>::new();
            let match_kind = if matches!(
                direction,
                OperationStepDirection::Forward
            ) && operation.source_crs_epsg == source_geo
                && operation.target_crs_epsg == target_geo
                || matches!(direction, OperationStepDirection::Reverse)
                    && operation.source_crs_epsg == target_geo
                    && operation.target_crs_epsg == source_geo
            {
                reasons.push(SelectionReason::ExactSourceTarget);
                OperationMatchKind::ExactSourceTarget
            } else {
                OperationMatchKind::DatumCompatible
            };
            if area_matches(options.point_of_interest, options.area_of_interest, &operation) {
                reasons.push(SelectionReason::AreaOfUseMatch);
            }
            if !operation.deprecated {
                reasons.push(SelectionReason::NonDeprecated);
            }
            if operation.preferred {
                reasons.push(SelectionReason::PreferredOperation);
            }
            candidates.push(RankedOperationCandidate {
                operation: operation.clone(),
                direction,
                match_kind,
                reasons,
            });
        }
    }

    if matches!(
        options.policy,
        SelectionPolicy::BestAvailable | SelectionPolicy::AllowApproximateHelmertFallback
    ) {
        if let Some(operation) = synthetic_helmert_fallback(source, target) {
            candidates.push(RankedOperationCandidate {
                operation,
                direction: OperationStepDirection::Forward,
                match_kind: OperationMatchKind::ApproximateFallback,
                reasons: SmallVec::from_slice(&[SelectionReason::ApproximateFallback]),
            });
        }
    }

    candidates.sort_by(|left, right| compare_candidates(options, left, right));
    candidates
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

fn explicit_selection_reasons(
    options: &SelectionOptions,
    operation: &CoordinateOperation,
) -> SmallVec<[SelectionReason; 4]> {
    let mut reasons = SmallVec::from_slice(&[SelectionReason::ExplicitOperation]);
    if area_matches(options.point_of_interest, options.area_of_interest, operation) {
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
        || source.datum().same_datum(target.datum())
        || (source.datum().is_wgs84_compatible() && target.datum().is_wgs84_compatible())
}

fn policy_allows(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
    operation: &CoordinateOperation,
) -> bool {
    let datum_shift_required = !requires_no_datum_operation(source, target);
    match options.policy {
        SelectionPolicy::BestAvailable => true,
        SelectionPolicy::RequireGrids => !datum_shift_required || operation.uses_grids(),
        SelectionPolicy::RequireExactAreaMatch => {
            if options.point_of_interest.is_none() && options.area_of_interest.is_none() {
                true
            } else {
                area_matches(options.point_of_interest, options.area_of_interest, operation)
            }
        }
        SelectionPolicy::AllowApproximateHelmertFallback => true,
        SelectionPolicy::Operation(_) => true,
    }
}

fn area_matches(point: Option<Coord>, bounds: Option<Bounds>, operation: &CoordinateOperation) -> bool {
    if point.is_none() && bounds.is_none() {
        return false;
    }
    operation.areas_of_use.iter().any(|area| {
        point.map(|value| area.contains_point(value)).unwrap_or(false)
            || bounds.map(|value| area.contains_bounds(value)).unwrap_or(false)
    })
}

fn compare_candidates(
    options: &SelectionOptions,
    left: &RankedOperationCandidate,
    right: &RankedOperationCandidate,
) -> Ordering {
    let left_exact = matches!(left.match_kind, OperationMatchKind::ExactSourceTarget);
    let right_exact = matches!(right.match_kind, OperationMatchKind::ExactSourceTarget);
    right_exact
        .cmp(&left_exact)
        .then_with(|| {
            let left_area = area_matches(options.point_of_interest, options.area_of_interest, &left.operation);
            let right_area = area_matches(options.point_of_interest, options.area_of_interest, &right.operation);
            right_area.cmp(&left_area)
        })
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
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| left.operation.deprecated.cmp(&right.operation.deprecated))
        .then_with(|| right.operation.preferred.cmp(&left.operation.preferred))
}

fn synthetic_helmert_fallback(source: &CrsDef, target: &CrsDef) -> Option<CoordinateOperation> {
    if requires_no_datum_operation(source, target) {
        return None;
    }
    if !source.datum().has_known_wgs84_transform() || !target.datum().has_known_wgs84_transform() {
        return None;
    }
    Some(CoordinateOperation {
        id: None,
        name: format!("Approximate {} to {}", source.epsg(), target.epsg()),
        source_crs_epsg: source.base_geographic_crs_epsg(),
        target_crs_epsg: target.base_geographic_crs_epsg(),
        source_datum_epsg: None,
        target_datum_epsg: None,
        accuracy: None,
        areas_of_use: SmallVec::new(),
        deprecated: false,
        preferred: false,
        approximate: true,
        method: OperationMethod::Identity,
    })
}

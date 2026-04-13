use crate::coord::{Bounds, Coord};
use crate::crs::CrsDef;
use crate::error::Result;
use crate::operation::{
    AreaOfInterest, AreaOfUse, CoordinateOperation, OperationMatchKind, OperationMethod,
    OperationStepDirection, SelectionOptions, SelectionPolicy, SelectionReason,
};
use crate::projection::{make_projection, validate_lon_lat, validate_projected};
use crate::registry;
use smallvec::SmallVec;
use std::cmp::Ordering;

pub(crate) struct RankedOperationCandidate {
    pub(crate) operation: CoordinateOperation,
    pub(crate) direction: OperationStepDirection,
    pub(crate) match_kind: OperationMatchKind,
    pub(crate) matched_area_of_use: Option<AreaOfUse>,
    pub(crate) reasons: SmallVec<[SelectionReason; 4]>,
}

pub(crate) fn rank_operation_candidates(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
) -> Result<Vec<RankedOperationCandidate>> {
    let resolved_aoi = resolve_area_of_interest(source, target, options)?;
    if let SelectionPolicy::Operation(id) = options.policy {
        let Some(operation) = registry::lookup_operation(id) else {
            return Ok(Vec::new());
        };
        let Some(direction) = compatible_direction(source, target, &operation) else {
            return Ok(Vec::new());
        };
        let matched_area_of_use = matched_area_of_use(resolved_aoi.as_ref(), &operation);
        let reasons = explicit_selection_reasons(matched_area_of_use.is_some(), &operation);
        return Ok(vec![RankedOperationCandidate {
            operation,
            direction,
            match_kind: OperationMatchKind::Explicit,
            matched_area_of_use,
            reasons,
        }]);
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
            matched_area_of_use: None,
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
            if !compatible || !policy_allows(source, target, options, resolved_aoi.as_ref(), &operation) {
                continue;
            }

            let matched_area_of_use = matched_area_of_use(resolved_aoi.as_ref(), &operation);
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
                operation: operation.clone(),
                direction,
                match_kind,
                matched_area_of_use,
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
                matched_area_of_use: None,
                reasons: SmallVec::from_slice(&[SelectionReason::ApproximateFallback]),
            });
        }
    }

    candidates.sort_by(compare_candidates);
    Ok(candidates)
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
    resolved_aoi: Option<&ResolvedAreaOfInterest>,
    operation: &CoordinateOperation,
) -> bool {
    let datum_shift_required = !requires_no_datum_operation(source, target);
    match options.policy {
        SelectionPolicy::BestAvailable => true,
        SelectionPolicy::RequireGrids => !datum_shift_required || operation.uses_grids(),
        SelectionPolicy::RequireExactAreaMatch => options.area_of_interest.is_none()
            || matched_area_of_use(resolved_aoi, operation).is_some(),
        SelectionPolicy::AllowApproximateHelmertFallback => true,
        SelectionPolicy::Operation(_) => true,
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
        Some(bounds) => Some(resolve_area_bounds(area, bounds, source, target)?),
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
    operation.areas_of_use.iter().find(|candidate| {
        area.point
            .map(|value| candidate.contains_point(value))
            .unwrap_or(false)
            || area
                .bounds
                .map(|value| candidate.contains_bounds(value))
                .unwrap_or(false)
    }).cloned()
}

fn compare_candidates(left: &RankedOperationCandidate, right: &RankedOperationCandidate) -> Ordering {
    let left_exact = matches!(left.match_kind, OperationMatchKind::ExactSourceTarget);
    let right_exact = matches!(right.match_kind, OperationMatchKind::ExactSourceTarget);
    right_exact
        .cmp(&left_exact)
        .then_with(|| right.matched_area_of_use.is_some().cmp(&left.matched_area_of_use.is_some()))
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

fn resolve_area_point(
    area: AreaOfInterest,
    point: Coord,
    source: &CrsDef,
    target: &CrsDef,
) -> Result<Coord> {
    match area.crs {
        crate::operation::AreaOfInterestCrs::GeographicDegrees => {
            validate_lon_lat(point.x.to_radians(), point.y.to_radians())?;
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
) -> Result<Bounds> {
    let crs = match area.crs {
        crate::operation::AreaOfInterestCrs::GeographicDegrees => return Ok(bounds),
        crate::operation::AreaOfInterestCrs::SourceCrs => source,
        crate::operation::AreaOfInterestCrs::TargetCrs => target,
    };

    if matches!(crs, CrsDef::Geographic(_)) {
        return Ok(bounds);
    }

    let segments = 8usize;
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
            let point = geographic_from_crs_point(crs, sample)?;
            if let Some(accum) = &mut transformed {
                accum.expand_to_include(point);
            } else {
                transformed = Some(Bounds::new(point.x, point.y, point.x, point.y));
            }
        }
    }
    Ok(transformed.unwrap_or(bounds))
}

fn geographic_from_crs_point(crs: &CrsDef, point: Coord) -> Result<Coord> {
    match crs {
        CrsDef::Geographic(_) => {
            validate_lon_lat(point.x.to_radians(), point.y.to_radians())?;
            Ok(point)
        }
        CrsDef::Projected(projected) => {
            validate_projected(point.x, point.y)?;
            let projection = make_projection(&projected.method(), projected.datum())?;
            let (lon, lat) = projection.inverse(
                projected.linear_unit().to_meters(point.x),
                projected.linear_unit().to_meters(point.y),
            )?;
            Ok(Coord::new(lon.to_degrees(), lat.to_degrees()))
        }
    }
}

fn synthetic_helmert_fallback(source: &CrsDef, target: &CrsDef) -> Option<CoordinateOperation> {
    if requires_no_datum_operation(source, target) {
        return None;
    }
    let params = source
        .datum()
        .approximate_helmert_to(target.datum())?;
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
        method: OperationMethod::Helmert { params },
    })
}

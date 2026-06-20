use crate::coord::{Bounds, Coord};
use crate::crs::CrsDef;
use crate::error::{Error, Result};
use crate::operation::{
    AreaOfInterest, AreaOfUse, CoordinateOperation, CoordinateOperationMetadata,
    OperationMatchKind, OperationMethod, OperationStepDirection, SelectionOptions, SelectionPolicy,
    SelectionReason, SkippedOperation, SkippedOperationReason,
};
use crate::projection::{make_projection, validate_lon_lat, validate_projected};
use crate::registry;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::cmp::Ordering;

pub(crate) const APPROXIMATE_HELMERT_FALLBACK_DISABLED_DETAIL: &str =
    "approximate Helmert fallback is available but disabled by SelectionPolicy::BestAvailable; opt in with SelectionOptions::allow_approximate_helmert_fallback()";

pub(crate) struct RankedOperationCandidate {
    pub(crate) operation: Cow<'static, CoordinateOperation>,
    pub(crate) direction: OperationStepDirection,
    pub(crate) match_kind: OperationMatchKind,
    pub(crate) matched_area_of_use: Option<AreaOfUse>,
    pub(crate) reasons: SmallVec<[SelectionReason; 4]>,
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
                operation: Cow::Owned(operation),
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
            operation: Cow::Owned(CoordinateOperation {
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
            }),
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
                operation: Cow::Borrowed(operation),
                direction,
                match_kind,
                matched_area_of_use,
                reasons,
            });
        }
    }

    if let Some(operation) = synthetic_grid_datum_shift(source, target) {
        candidates.push(RankedOperationCandidate {
            operation: Cow::Owned(operation),
            direction: OperationStepDirection::Forward,
            match_kind: OperationMatchKind::DatumCompatible,
            matched_area_of_use: None,
            reasons: SmallVec::from_slice(&[SelectionReason::NonDeprecated]),
        });
    }

    match options.policy {
        SelectionPolicy::AllowApproximateHelmertFallback => {
            if let Some(operation) = synthetic_helmert_fallback(source, target) {
                candidates.push(RankedOperationCandidate {
                    operation: Cow::Owned(operation),
                    direction: OperationStepDirection::Forward,
                    match_kind: OperationMatchKind::ApproximateFallback,
                    matched_area_of_use: None,
                    reasons: SmallVec::from_slice(&[SelectionReason::ApproximateFallback]),
                });
            }
        }
        SelectionPolicy::BestAvailable => {
            if let Some(operation) = synthetic_helmert_fallback(source, target) {
                skipped.push(SkippedOperation {
                    metadata: selection_metadata(&operation, OperationStepDirection::Forward, None),
                    reason: SkippedOperationReason::PolicyFiltered,
                    detail: APPROXIMATE_HELMERT_FALLBACK_DISABLED_DETAIL.into(),
                });
            }
        }
        SelectionPolicy::RequireGrids
        | SelectionPolicy::RequireExactAreaMatch
        | SelectionPolicy::Operation(_) => {}
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
        || (source.datum().is_wgs84_compatible() && target.datum().is_wgs84_compatible())
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
        SelectionPolicy::AllowApproximateHelmertFallback => None,
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

fn match_kind_rank(kind: OperationMatchKind) -> u8 {
    match kind {
        OperationMatchKind::Explicit => 4,
        OperationMatchKind::ExactSourceTarget => 3,
        OperationMatchKind::DerivedGeographic => 2,
        OperationMatchKind::DatumCompatible => 1,
        OperationMatchKind::ApproximateFallback => 0,
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

    let segments = densify_points.checked_add(1).ok_or_else(|| {
        Error::OutOfRange("area-of-interest bounds densify point count is too large".into())
    })?;
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

fn synthetic_helmert_fallback(source: &CrsDef, target: &CrsDef) -> Option<CoordinateOperation> {
    if requires_no_datum_operation(source, target) {
        return None;
    }
    let params = source.datum().approximate_helmert_to(target.datum())?;
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

fn synthetic_grid_datum_shift(source: &CrsDef, target: &CrsDef) -> Option<CoordinateOperation> {
    if requires_no_datum_operation(source, target) {
        return None;
    }
    if !source.datum().uses_grid_shift() && !target.datum().uses_grid_shift() {
        return None;
    }
    if !source.datum().has_known_wgs84_transform() || !target.datum().has_known_wgs84_transform() {
        return None;
    }

    Some(CoordinateOperation {
        id: None,
        name: format!(
            "Grid-backed datum shift {} to {}",
            source.epsg(),
            target.epsg()
        ),
        source_crs_epsg: source.base_geographic_crs_epsg(),
        target_crs_epsg: target.base_geographic_crs_epsg(),
        source_datum_epsg: None,
        target_datum_epsg: None,
        accuracy: None,
        areas_of_use: SmallVec::new(),
        deprecated: false,
        preferred: true,
        approximate: false,
        method: OperationMethod::DatumShift {
            source_to_wgs84: source.datum().to_wgs84().clone(),
            target_to_wgs84: target.datum().to_wgs84().clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

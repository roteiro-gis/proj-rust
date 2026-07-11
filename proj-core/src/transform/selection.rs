use super::pipeline::{compile_pipeline, CompiledOperationFallback, CompiledOperationPipeline};
use crate::crs::CrsDef;
use crate::error::{Error, Result};
use crate::grid::{GridError, GridRuntime};
use crate::operation::{
    CoordinateOperationMetadata, OperationSelectionDiagnostics, OperationStepDirection,
    SelectionOptions, SelectionPolicy, SkippedOperation, SkippedOperationReason,
};
use crate::registry;
use crate::selector;
use smallvec::SmallVec;

pub(super) struct SelectedOperationPipelines {
    pub(super) operation: selector::SelectedOperationKind,
    pub(super) direction: OperationStepDirection,
    pub(super) metadata: CoordinateOperationMetadata,
    pub(super) diagnostics: OperationSelectionDiagnostics,
    pub(super) pipeline: CompiledOperationPipeline,
    pub(super) fallback_pipelines: Vec<CompiledOperationFallback>,
}

pub(super) fn compile_selected_pipelines(
    from: &CrsDef,
    to: &CrsDef,
    options: &SelectionOptions,
    grid_runtime: &GridRuntime,
) -> Result<SelectedOperationPipelines> {
    let candidate_set = selector::rank_operation_candidates(from, to, options)?;
    if candidate_set.ranked.is_empty() {
        return Err(no_ranked_operation_error(from, to, options));
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
                    from,
                    to,
                    candidate,
                    !selected_candidate.operation.deprecated(),
                ));
                continue;
            }
        }

        match compile_pipeline(
            from,
            to,
            &candidate.operation,
            candidate.direction,
            grid_runtime,
        ) {
            Ok(pipeline) => {
                let metadata = candidate.metadata(from, to);
                if let Some((_, selected_candidate, ..)) = &selected {
                    skipped_operations.push(skipped_for_unselected_candidate(
                        from,
                        to,
                        candidate,
                        !selected_candidate.operation.deprecated(),
                    ));
                    fallback_pipelines.push(CompiledOperationFallback {
                        operation: candidate.operation.clone().into_owned(),
                        direction: candidate.direction,
                        metadata: std::sync::Arc::new(metadata),
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
                    metadata: candidate.metadata(from, to),
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
                    metadata: candidate.metadata(from, to),
                    reason: SkippedOperationReason::LessPreferred,
                    detail: error.to_string(),
                });
            }
        }
    }

    if let Some((index, candidate, metadata, pipeline)) = selected {
        let selected_reasons = selected_reasons_for(candidate, &candidate_set.ranked[index + 1..]);
        let diagnostics = OperationSelectionDiagnostics {
            selected_operation: metadata.clone(),
            selected_match_kind: candidate.match_kind,
            selected_reasons,
            fallback_operations: fallback_pipelines
                .iter()
                .map(|fallback| (*fallback.metadata).clone())
                .collect(),
            skipped_operations,
            approximate: candidate.operation.approximate(),
            missing_required_grid,
        };
        return Ok(SelectedOperationPipelines {
            operation: candidate.operation.clone().into_owned(),
            direction: candidate.direction,
            metadata,
            diagnostics,
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

fn no_ranked_operation_error(from: &CrsDef, to: &CrsDef, options: &SelectionOptions) -> Error {
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
        _ => Error::OperationSelection(format!(
            "no compatible registry operation found for source EPSG:{} target EPSG:{}",
            from.epsg(),
            to.epsg()
        )),
    }
}

pub(super) fn selected_metadata(
    operation: &selector::SelectedOperationKind,
    source: &CrsDef,
    target: &CrsDef,
    direction: OperationStepDirection,
    matched_area_of_use: Option<crate::operation::AreaOfUse>,
) -> CoordinateOperationMetadata {
    operation.metadata_for_direction(source, target, direction, matched_area_of_use)
}

pub(super) fn is_grid_coverage_miss(error: &Error) -> bool {
    matches!(error, Error::Grid(GridError::OutsideCoverage(_)))
}

pub(super) fn grid_coverage_miss_detail(error: &Error) -> Option<String> {
    match error {
        Error::Grid(GridError::OutsideCoverage(detail)) => Some(detail.clone()),
        _ => None,
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
    let Some(selected_accuracy) = selected.operation.accuracy().map(|value| value.meters) else {
        return false;
    };

    alternatives.iter().any(|alternative| {
        same_pre_accuracy_priority(selected, alternative)
            && alternative
                .operation
                .accuracy()
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
        crate::operation::OperationMatchKind::Explicit => 5,
        crate::operation::OperationMatchKind::Custom => 4,
        crate::operation::OperationMatchKind::ExactSourceTarget => 3,
        crate::operation::OperationMatchKind::DerivedGeographic => 2,
        crate::operation::OperationMatchKind::DatumCompatible => 1,
    }
}

fn skipped_for_unselected_candidate(
    source: &CrsDef,
    target: &CrsDef,
    candidate: &selector::RankedOperationCandidate,
    prefer_non_deprecated: bool,
) -> SkippedOperation {
    let reason = if prefer_non_deprecated && candidate.operation.deprecated() {
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
        metadata: candidate.metadata(source, target),
        reason,
        detail,
    }
}

use super::Transform;
use crate::coord::Coord3D;
use crate::crs::{CrsDef, LinearUnit, VerticalCrsDef};
use crate::error::{Error, Result};
use crate::grid::{GridHandle, GridRuntime};
use crate::operation::{
    SelectionOptions, SelectionPolicy, VerticalGridOffsetConvention, VerticalGridOperation,
    VerticalGridProvenance, VerticalTransformAction, VerticalTransformDiagnostics,
};
use crate::projection::{make_projection, Projection};
use crate::registry;
use crate::selector;
use std::borrow::Cow;
use std::sync::Arc;

pub(super) enum VerticalTransform {
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

pub(super) struct CompiledVerticalGridShift {
    handle: GridHandle,
    direction: VerticalGridShiftDirection,
    source_unit: LinearUnit,
    target_unit: LinearUnit,
    sample_horizontal: VerticalSampleHorizontal,
    pub(super) diagnostics: VerticalTransformDiagnostics,
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

pub(super) struct VerticalApplyOutcome {
    pub(super) z: f64,
    pub(super) diagnostics: VerticalTransformDiagnostics,
}

impl VerticalTransform {
    pub(super) fn apply_z(&self, coord: Coord3D) -> Result<f64> {
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

    pub(super) fn apply(&self, coord: Coord3D) -> Result<VerticalApplyOutcome> {
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

    pub(super) fn diagnostics(&self) -> &VerticalTransformDiagnostics {
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
    if !(lon.is_finite() && lat.is_finite()) {
        return Err(Error::Grid(crate::grid::GridError::OutsideCoverage(
            format!(
                "non-finite vertical grid sample coordinate: longitude {:.8} latitude {:.8}",
                lon.to_degrees(),
                lat.to_degrees()
            ),
        )));
    }
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

pub(super) fn compile_vertical_transform(
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
            "cannot transform between an explicit vertical CRS and a horizontal-only CRS; use Transform::new_horizontal or Transform::from_horizontal_components for an explicitly XY-only transform".into(),
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
    let mut candidates = matching_vertical_grid_operations(
        source_crs,
        target_crs,
        source_vertical,
        target_vertical,
        options,
        area.as_ref(),
    );

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
    operation: Cow<'a, VerticalGridOperation>,
    area_of_use_match: Option<bool>,
    grid_area_of_use_match: Option<bool>,
}

fn matching_vertical_grid_operations<'a>(
    source_crs: &CrsDef,
    target_crs: &CrsDef,
    source: &VerticalCrsDef,
    target: &VerticalCrsDef,
    options: &'a SelectionOptions,
    area: Option<&selector::ResolvedAreaOfInterest>,
) -> Vec<VerticalGridCandidate<'a>> {
    let mut candidates: Vec<_> = options
        .vertical_grid_operations
        .iter()
        .filter(|operation| vertical_grid_operation_matches(operation, source, target))
        .map(|operation| vertical_grid_candidate(Cow::Borrowed(operation), area))
        .collect();

    candidates.extend(
        registry::vertical_grid_operations_between(source_crs, target_crs)
            .into_iter()
            .filter(|operation| vertical_grid_operation_matches(operation, source, target))
            .map(|operation| vertical_grid_candidate(Cow::Owned(operation), area)),
    );
    candidates
}

fn vertical_grid_candidate<'a>(
    operation: Cow<'a, VerticalGridOperation>,
    area: Option<&selector::ResolvedAreaOfInterest>,
) -> VerticalGridCandidate<'a> {
    let operation_area_match = vertical_operation_area_match(operation.as_ref(), area);
    let grid_area_of_use_match = area_of_use_match(operation.grid.area_of_use.as_ref(), area);
    VerticalGridCandidate {
        operation,
        area_of_use_match: operation_area_match,
        grid_area_of_use_match,
    }
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
    let operation = candidate.operation.as_ref();
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

pub(super) fn vertical_diagnostics(
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

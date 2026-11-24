//! Engineering endpoints.
//!
//! An engineering CRS has no relation to the earth of its own. A transform
//! from one runs the similarity the caller supplies to that frame's base CRS
//! (the operation's other CRS, from the registry), then the ordinary
//! transform from the base on. A transform to one runs the same in reverse.
//! Between two definitions of the same frame only units, axis order, and the
//! height change.

use crate::coord::{Bounds, Coord, Coord3D};
use crate::crs::{CrsDef, EngineeringCrsDef, LinearUnit};
use crate::error::{Error, Result};
use crate::operation::{
    AreaOfInterest, AreaOfInterestCrs, CoordinateOperation, CoordinateOperationMetadata,
    OperationAccuracy, OperationDomain, OperationMethod, OperationStepDirection, SelectionOptions,
    Similarity3DParams,
};
use crate::registry;

/// The requested endpoints of a transform with an engineering CRS at either
/// end, and the stages between them and the transform's base CRSs.
#[derive(Clone, Debug)]
pub(super) struct FrameEnds {
    pub(super) source: CrsDef,
    pub(super) target: CrsDef,
    /// Requested source coordinates to the base source CRS.
    pub(super) source_stage: Option<FrameStage>,
    /// Base target CRS coordinates to the requested target.
    pub(super) target_stage: Option<FrameStage>,
}

impl FrameEnds {
    pub(super) fn inverse(&self) -> Self {
        Self {
            source: self.target.clone(),
            target: self.source.clone(),
            source_stage: self.target_stage.clone(),
            target_stage: self.source_stage.clone(),
        }
    }

    /// Requested source east/north/height to the base source CRS.
    pub(super) fn enter(&self, coord: Coord3D) -> Coord3D {
        match &self.source_stage {
            Some(stage) => stage.frame_to_base(coord),
            None => coord,
        }
    }

    /// Requested source east/north to the base source CRS.
    pub(super) fn enter_2d(&self, coord: Coord) -> Coord {
        match &self.source_stage {
            Some(stage) => stage.frame_to_base_2d(coord),
            None => coord,
        }
    }

    /// Base target CRS east/north/height to the requested target.
    pub(super) fn leave(&self, coord: Coord3D) -> Result<Coord3D> {
        match &self.target_stage {
            Some(stage) => stage.base_to_frame(coord),
            None => Ok(coord),
        }
    }

    /// Base target CRS east/north to the requested target.
    pub(super) fn leave_2d(&self, coord: Coord) -> Result<Coord> {
        match &self.target_stage {
            Some(stage) => stage.base_to_frame_2d(coord),
            None => Ok(coord),
        }
    }

    /// True when either end runs an operation, not only a unit change.
    pub(super) fn has_operation(&self) -> bool {
        [&self.source_stage, &self.target_stage]
            .into_iter()
            .flatten()
            .any(|stage| stage.operation.is_some())
    }

    /// The operation actually run, given the base transform's.
    pub(super) fn metadata(
        &self,
        base: &CoordinateOperationMetadata,
        base_is_identity: bool,
    ) -> CoordinateOperationMetadata {
        let mut parts = Vec::with_capacity(3);
        if let Some(metadata) = self
            .source_stage
            .as_ref()
            .and_then(|stage| stage.metadata(true))
        {
            parts.push(metadata);
        }
        let has_stage_operation = !parts.is_empty()
            || self
                .target_stage
                .as_ref()
                .is_some_and(|stage| stage.operation.is_some());
        if !has_stage_operation {
            return base.clone();
        }
        if !base_is_identity {
            parts.push(base.clone());
        }
        if let Some(metadata) = self
            .target_stage
            .as_ref()
            .and_then(|stage| stage.metadata(false))
        {
            parts.push(metadata);
        }
        if parts.len() == 1 {
            let mut only = parts.remove(0);
            only.area_of_use = only.area_of_use.or_else(|| base.area_of_use.clone());
            return only;
        }

        let first = &parts[0];
        let last = &parts[parts.len() - 1];
        CoordinateOperationMetadata {
            id: None,
            name: parts
                .iter()
                .map(|part| match part.direction {
                    OperationStepDirection::Forward => part.name.clone(),
                    OperationStepDirection::Reverse => format!("Inverse of {}", part.name),
                })
                .collect::<Vec<_>>()
                .join(" + "),
            direction: OperationStepDirection::Forward,
            source_crs_epsg: first.source_crs_epsg,
            target_crs_epsg: last.target_crs_epsg,
            source_datum_epsg: first.source_datum_epsg,
            target_datum_epsg: last.target_datum_epsg,
            accuracy: parts
                .iter()
                .map(|part| part.accuracy.map(|accuracy| accuracy.meters))
                .sum::<Option<f64>>()
                .map(|meters| OperationAccuracy { meters }),
            area_of_use: base.area_of_use.clone(),
            deprecated: parts.iter().any(|part| part.deprecated),
            preferred: parts.iter().all(|part| part.preferred),
            approximate: parts.iter().any(|part| part.approximate),
            uses_grids: base.uses_grids,
            domain: if parts
                .iter()
                .all(|part| part.domain == OperationDomain::HorizontalOnly)
            {
                OperationDomain::HorizontalOnly
            } else {
                OperationDomain::IncludesHeight
            },
        }
    }
}

/// Between an engineering CRS's coordinates and its base CRS's: the units,
/// then the similarity that positions the frame, if any.
///
/// 2D frame coordinates are points on the frame's `h = 0` plane, so a 2D
/// conversion from the base CRS finds the point of that plane above or below
/// the base position, and the two directions stay exact inverses for a tilted
/// frame. A plane within about a tenth of a degree of vertical has no such
/// point, and the conversion fails.
#[derive(Clone, Debug)]
pub(super) struct FrameStage {
    frame_unit: LinearUnit,
    /// True for a 3D frame, whose height goes into the similarity. A 2D
    /// frame's `z` passes through.
    frame_has_height: bool,
    /// The positioning operation, and whether the frame is its source.
    operation: Option<(Box<CoordinateOperation>, Similarity3DParams, bool)>,
    base_xy_unit: LinearUnit,
    base_z_unit: LinearUnit,
}

impl FrameStage {
    /// Frame metres to base metres.
    fn frame_to_base_meters(&self, point: [f64; 3]) -> [f64; 3] {
        match &self.operation {
            Some((_, similarity, true)) => similarity.forward(point),
            Some((_, similarity, false)) => similarity.inverse(point),
            None => point,
        }
    }

    /// Base metres to frame metres.
    fn base_to_frame_meters(&self, point: [f64; 3]) -> [f64; 3] {
        match &self.operation {
            Some((_, similarity, true)) => similarity.inverse(point),
            Some((_, similarity, false)) => similarity.forward(point),
            None => point,
        }
    }

    pub(super) fn frame_to_base(&self, coord: Coord3D) -> Coord3D {
        if !self.frame_has_height {
            let xy = self.frame_to_base_2d(Coord::new(coord.x, coord.y));
            return Coord3D::new(xy.x, xy.y, coord.z);
        }
        let unit = self.frame_unit;
        let point = self.frame_to_base_meters([
            unit.to_meters(coord.x),
            unit.to_meters(coord.y),
            unit.to_meters(coord.z),
        ]);
        Coord3D::new(
            self.base_xy_unit.from_meters(point[0]),
            self.base_xy_unit.from_meters(point[1]),
            self.base_z_unit.from_meters(point[2]),
        )
    }

    pub(super) fn base_to_frame(&self, coord: Coord3D) -> Result<Coord3D> {
        if !self.frame_has_height {
            let xy = self.base_to_frame_2d(Coord::new(coord.x, coord.y))?;
            return Ok(Coord3D::new(xy.x, xy.y, coord.z));
        }
        let point = self.base_to_frame_meters([
            self.base_xy_unit.to_meters(coord.x),
            self.base_xy_unit.to_meters(coord.y),
            self.base_z_unit.to_meters(coord.z),
        ]);
        let unit = self.frame_unit;
        Ok(Coord3D::new(
            unit.from_meters(point[0]),
            unit.from_meters(point[1]),
            unit.from_meters(point[2]),
        ))
    }

    pub(super) fn frame_to_base_2d(&self, coord: Coord) -> Coord {
        let unit = self.frame_unit;
        let point =
            self.frame_to_base_meters([unit.to_meters(coord.x), unit.to_meters(coord.y), 0.0]);
        Coord::new(
            self.base_xy_unit.from_meters(point[0]),
            self.base_xy_unit.from_meters(point[1]),
        )
    }

    /// The point of the frame's `h = 0` plane at this base easting and
    /// northing.
    pub(super) fn base_to_frame_2d(&self, coord: Coord) -> Result<Coord> {
        let easting = self.base_xy_unit.to_meters(coord.x);
        let northing = self.base_xy_unit.to_meters(coord.y);
        let (x, y) = match &self.operation {
            None => (easting, northing),
            Some((_, similarity, frame_is_source)) => {
                // Frame to base is `A·p + b`; at h = 0 its first two rows
                // are a 2×2 system in x and y.
                let r = similarity.rotation_matrix();
                let scale = 1.0 + similarity.scale_ppm() * 1e-6;
                let a = if *frame_is_source {
                    [scale * r[0], scale * r[1], scale * r[3], scale * r[4]]
                } else {
                    [r[0] / scale, r[3] / scale, r[1] / scale, r[4] / scale]
                };
                let b = self.frame_to_base_meters([0.0; 3]);
                let (dx, dy) = (easting - b[0], northing - b[1]);
                // The determinant is the cosine of the plane's tilt, times
                // the scale squared.
                let determinant = a[0] * a[3] - a[1] * a[2];
                if determinant.abs() < 2e-3 * scale.powi(2) {
                    return Err(Error::OutOfRange(
                        "the engineering CRS's h = 0 plane is near vertical in its base CRS, so 2D base coordinates have no point on it; use convert_3d".into(),
                    ));
                }
                (
                    (a[3] * dx - a[1] * dy) / determinant,
                    (a[0] * dy - a[2] * dx) / determinant,
                )
            }
        };
        let unit = self.frame_unit;
        Ok(Coord::new(unit.from_meters(x), unit.from_meters(y)))
    }

    /// Metadata of the positioning operation, from the frame when
    /// `from_frame`, else to it.
    fn metadata(&self, from_frame: bool) -> Option<CoordinateOperationMetadata> {
        let (operation, _, frame_is_source) = self.operation.as_ref()?;
        let direction = if *frame_is_source == from_frame {
            OperationStepDirection::Forward
        } else {
            OperationStepDirection::Reverse
        };
        Some(operation.metadata_for_direction(direction))
    }
}

/// A transform between `from` and `to` when either is an engineering CRS:
/// its endpoints, and the base CRSs and options of the transform between the
/// stages.
pub(super) struct FramePlan {
    pub(super) ends: FrameEnds,
    pub(super) base_source: CrsDef,
    pub(super) base_target: CrsDef,
    pub(super) base_options: SelectionOptions,
}

pub(super) fn plan(from: &CrsDef, to: &CrsDef, options: &SelectionOptions) -> Result<FramePlan> {
    // Between two forms of one frame a height is converted only when both
    // carry one; otherwise it passes through, as for any 2D endpoint.
    let same_frame = match (from.as_engineering(), to.as_engineering()) {
        (Some(source), Some(target)) if source.same_frame(target) => {
            Some(source.dimension() == 3 && target.dimension() == 3)
        }
        _ => None,
    };
    let (source_stage, base_source) = match from.as_engineering() {
        Some(frame) => endpoint(frame, to, same_frame, options)?,
        None => (None, from.clone()),
    };
    let (target_stage, base_target) = match to.as_engineering() {
        Some(frame) => endpoint(frame, from, same_frame, options)?,
        None => (None, to.clone()),
    };

    let mut base_options = options.clone();
    base_options.area_of_interest = match (same_frame, options.area_of_interest) {
        (Some(_), _) | (_, None) => None,
        (None, Some(area)) => Some(base_area_of_interest(
            area,
            from,
            to,
            source_stage.as_ref(),
            target_stage.as_ref(),
        )?),
    };

    Ok(FramePlan {
        ends: FrameEnds {
            source: from.clone(),
            target: to.clone(),
            source_stage,
            target_stage,
        },
        base_source,
        base_target,
        base_options,
    })
}

/// The stage at an engineering endpoint and its base CRS. `same_frame` is
/// set when both endpoints are this frame, to whether both carry a height.
fn endpoint(
    frame: &EngineeringCrsDef,
    other: &CrsDef,
    same_frame: Option<bool>,
    options: &SelectionOptions,
) -> Result<(Option<FrameStage>, CrsDef)> {
    let has_height = frame.dimension() == 3;
    if let Some(both_have_height) = same_frame {
        // The base is the frame itself in metres, so the two ends only differ
        // in units, axis order, and height.
        let base = CrsDef::Engineering(Box::new(
            frame
                .clone()
                .with_linear_unit(LinearUnit::metre())
                .with_axis_order(crate::crs::AxisOrder::EastNorth),
        ));
        let stage = (frame.linear_unit().meters_per_unit() != 1.0).then(|| FrameStage {
            frame_unit: frame.linear_unit(),
            frame_has_height: both_have_height,
            operation: None,
            base_xy_unit: LinearUnit::metre(),
            base_z_unit: LinearUnit::metre(),
        });
        return Ok((stage, base));
    }

    let (operation, params, frame_is_source, base_code) =
        positioning_operation(frame, other, options)?;
    let base = registry::lookup_epsg(base_code).ok_or_else(|| {
        Error::UnknownCrs(format!(
            "operation {} positions {} on EPSG:{base_code}, which the registry does not know",
            operation.name,
            frame_label(frame)
        ))
    })?;
    let Some(projected) = base.as_projected() else {
        return Err(Error::OperationSelection(format!(
            "operation {} positions {} on EPSG:{base_code}; a similarity needs a projected CRS or a compound CRS with a projected horizontal component",
            operation.name,
            frame_label(frame)
        )));
    };
    let base_xy_unit = projected.linear_unit();
    let base_z_unit = base
        .vertical_crs()
        .map_or(LinearUnit::metre(), |vertical| vertical.linear_unit());
    let base = if has_height {
        base
    } else {
        base.horizontal_crs().unwrap_or(base)
    };
    Ok((
        Some(FrameStage {
            frame_unit: frame.linear_unit(),
            frame_has_height: has_height,
            operation: Some((Box::new(operation.clone()), params, frame_is_source)),
            base_xy_unit,
            base_z_unit,
        }),
        base,
    ))
}

/// The CRS `frame` reaches through the operation in `options` that positions
/// it, in the frame's dimension, for a transform between `frame` and `other`.
pub(crate) fn positioned_base_crs(
    frame: &EngineeringCrsDef,
    other: &CrsDef,
    options: &SelectionOptions,
) -> Option<CrsDef> {
    if other
        .as_engineering()
        .is_some_and(|other| other.same_frame(frame))
    {
        return None;
    }
    let (.., base_code) = positioning_operation(frame, other, options).ok()?;
    let base = registry::lookup_epsg(base_code)?;
    if frame.dimension() == 3 {
        Some(base)
    } else {
        base.horizontal_crs()
    }
}

/// The caller's similarity that positions `frame`, the side the frame is on,
/// and the EPSG code of its other CRS. One whose other CRS is `other` wins;
/// otherwise the first that names the frame.
fn positioning_operation<'a>(
    frame: &EngineeringCrsDef,
    other: &CrsDef,
    options: &'a SelectionOptions,
) -> Result<(&'a CoordinateOperation, Similarity3DParams, bool, u32)> {
    let frame_matches = |id: &Option<crate::crs::Identifier>| {
        id.as_ref().is_none_or(|id| frame.identifier() == Some(id))
    };
    let candidates = options
        .coordinate_operations
        .iter()
        .filter_map(|operation| {
            let OperationMethod::Similarity3D { params } = operation.method else {
                return None;
            };
            match (operation.source_crs_epsg, operation.target_crs_epsg) {
                (None, Some(base)) if frame_matches(&operation.source_crs_id) => {
                    Some((operation, params, true, base))
                }
                (Some(base), None) if frame_matches(&operation.target_crs_id) => {
                    Some((operation, params, false, base))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let other_code = other.epsg();
    candidates
        .iter()
        .find(|(.., base)| other_code != 0 && *base == other_code)
        .or_else(|| candidates.first())
        .copied()
        .ok_or_else(|| {
            Error::OperationSelection(format!(
                "no operation positions engineering CRS {}; pass a Similarity3D operation from it to an EPSG CRS in SelectionOptions::coordinate_operations",
                frame_label(frame)
            ))
        })
}

fn frame_label(frame: &EngineeringCrsDef) -> String {
    match frame.identifier() {
        Some(id) => format!("{id} ({})", frame.name()),
        None => format!("\"{}\"", frame.name()),
    }
}

/// Restate an area of interest given in an engineering endpoint's
/// coordinates in its base CRS's.
fn base_area_of_interest(
    area: AreaOfInterest,
    from: &CrsDef,
    to: &CrsDef,
    source_stage: Option<&FrameStage>,
    target_stage: Option<&FrameStage>,
) -> Result<AreaOfInterest> {
    let (crs, stage) = match area.crs {
        AreaOfInterestCrs::SourceCrs => (from, source_stage),
        AreaOfInterestCrs::TargetCrs => (to, target_stage),
        AreaOfInterestCrs::GeographicDegrees | AreaOfInterestCrs::GeographicDegreesWrapped => {
            return Ok(area)
        }
    };
    let Some(stage) = stage else {
        return Ok(area);
    };
    let axes = crs.axis_order().unwrap_or_default();
    let to_base = |point: Coord| stage.frame_to_base_2d(axes.reorder(point));
    let point = area.point.map(to_base);
    let bounds = match area.bounds {
        Some(bounds) => {
            if !bounds.is_valid() {
                return Err(Error::OutOfRange(
                    "area-of-interest bounds must be finite and satisfy min <= max".into(),
                ));
            }
            // A similarity maps the rectangle to a parallelogram, which its
            // corners bound.
            let corners = [
                Coord::new(bounds.min_x, bounds.min_y),
                Coord::new(bounds.min_x, bounds.max_y),
                Coord::new(bounds.max_x, bounds.min_y),
                Coord::new(bounds.max_x, bounds.max_y),
            ]
            .map(to_base);
            let mut base = Bounds::new(corners[0].x, corners[0].y, corners[0].x, corners[0].y);
            for corner in &corners[1..] {
                base.expand_to_include(*corner);
            }
            Some(base)
        }
        None => None,
    };
    Ok(AreaOfInterest {
        crs: area.crs,
        point,
        bounds,
    })
}

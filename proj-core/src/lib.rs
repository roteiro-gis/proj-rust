#![forbid(unsafe_code)]

//! Pure-Rust coordinate transformation library.
//!
//! No C dependencies, no unsafe, WASM-compatible.
//!
//! The primary type is [`Transform`], which provides CRS-to-CRS coordinate
//! transformation using authority codes (e.g., `"EPSG:4326"`).
//! For area-aware or policy-constrained selection, use
//! [`Transform::with_selection_options`] and inspect
//! [`Transform::selected_operation`] /
//! [`Transform::selection_diagnostics`].
//! Operation selection uses registry operations, explicit custom horizontal
//! operations supplied in [`SelectionOptions`], exact identity/no-datum-shift
//! paths, and supported grid/identity custom datum shifts; it does not
//! synthesize last-resort Helmert datum-shift fallbacks from datum metadata.
//! The [`registry`], [`operation`], and [`grid`] modules expose the embedded
//! operation catalog, selection metadata, and NTv2 grid-provider interfaces.
//! `convert_3d` preserves `z` when no explicit vertical CRS is present or when
//! source and target compound CRS definitions have identical vertical
//! components. It converts `z` units when both vertical components use the same
//! vertical reference frame with different linear units. Registry-backed GTX
//! geoid operations can be selected for supported ellipsoidal-to-gravity height
//! CRS pairs, while grid files still resolve through caller-supplied grid
//! providers.
//! Geographic antimeridian AOIs use
//! [`AreaOfInterest::geographic_wrapped_bounds`], while ordinary projected and
//! source/target bounds keep strict `min <= max` validation.
//! With the default `geo-types` feature, [`Transform::convert_geometry`]
//! transforms whole 2D `geo-types` geometries and fails on the first invalid
//! coordinate without returning partial results.
//!
//! # Example
//!
//! ```
//! use proj_core::Transform;
//!
//! // Create a transform from WGS84 geographic to Web Mercator
//! let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
//!
//! // Transform NYC coordinates (lon, lat in degrees) → (x, y in meters)
//! let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
//! assert!((x - (-8238310.0)).abs() < 100.0);
//!
//! // Inverse: Web Mercator → WGS84
//! let inv = Transform::new("EPSG:3857", "EPSG:4326").unwrap();
//! let (lon, lat) = inv.convert((x, y)).unwrap();
//! assert!((lon - (-74.006)).abs() < 1e-6);
//! ```

pub mod coord;
pub mod crs;
pub mod datum;
pub mod ellipsoid;
mod epsg_db;
pub mod error;
mod geocentric;
pub mod grid;
mod helmert;
pub mod operation;
mod projection;
pub mod registry;
mod selector;
pub mod transform;

pub use coord::{
    Bounds, Coord, Coord3D, Transformable, Transformable3D, MAX_BOUNDS_DENSIFY_POINTS,
};
pub use crs::{
    CompoundCrsDef, CrsDef, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    ProjectionMethod, VerticalCrsDef, VerticalCrsKind,
};
pub use datum::{Datum, DatumGridShift, DatumGridShiftEntry, DatumToWgs84, HelmertParams};
pub use ellipsoid::Ellipsoid;
pub use error::{Error, Result};
pub use grid::{
    EmbeddedGridProvider, FilesystemGridProvider, GridDefinition, GridError, GridFormat,
    GridHandle, GridProvider, GridSample, VerticalGridSample,
};
pub use operation::{
    AreaOfInterest, AreaOfInterestCrs, AreaOfUse, CoordinateOperation, CoordinateOperationId,
    CoordinateOperationMetadata, GridCoverageMiss, GridId, GridInterpolation, GridShiftDirection,
    OperationAccuracy, OperationMatchKind, OperationMethod, OperationSelectionDiagnostics,
    OperationStep, OperationStepDirection, SelectionOptions, SelectionPolicy, SelectionReason,
    SkippedOperation, SkippedOperationReason, TransformOutcome, VerticalGridOffsetConvention,
    VerticalGridOperation, VerticalGridProvenance, VerticalTransformAction,
    VerticalTransformDiagnostics,
};
pub use registry::{
    lookup_authority_code, lookup_datum_epsg, lookup_epsg, lookup_operation, lookup_vertical_epsg,
    lookup_vertical_grid_operation, operation_candidates_between,
    operation_candidates_between_with_selection_options, operations_between,
    vertical_grid_operations_between,
};
pub use transform::Transform;
#[cfg(feature = "geo-types")]
pub use transform::TransformableGeometry;

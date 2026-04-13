#![forbid(unsafe_code)]

//! Pure-Rust coordinate transformation library.
//!
//! No C dependencies, no unsafe, WASM-compatible.
//!
//! The primary type is [`Transform`], which provides CRS-to-CRS coordinate
//! transformation using authority codes (e.g., `"EPSG:4326"`).
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
pub mod grid;
pub mod error;
mod geocentric;
mod helmert;
pub mod operation;
mod projection;
pub mod registry;
mod selector;
pub mod transform;

pub use coord::{Bounds, Coord, Coord3D, Transformable, Transformable3D};
pub use crs::{CrsDef, GeographicCrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod};
pub use datum::{Datum, DatumToWgs84, HelmertParams};
pub use ellipsoid::Ellipsoid;
pub use grid::{
    EmbeddedGridProvider, FilesystemGridProvider, GridDefinition, GridError, GridFormat,
    GridHandle, GridProvider, GridSample,
};
pub use error::{Error, Result};
pub use operation::{
    AreaOfInterest, AreaOfInterestCrs, AreaOfUse, CoordinateOperation, CoordinateOperationId,
    CoordinateOperationMetadata, GridId, GridInterpolation, GridShiftDirection, OperationAccuracy,
    OperationMatchKind, OperationMethod, OperationSelectionDiagnostics, OperationStep,
    OperationStepDirection, SelectionOptions, SelectionPolicy, SelectionReason, SkippedOperation,
    SkippedOperationReason,
};
pub use registry::{lookup_authority_code, lookup_datum_epsg, lookup_epsg, lookup_operation, operations_between};
pub use transform::Transform;

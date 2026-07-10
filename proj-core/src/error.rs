use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by CRS resolution, transform construction, and coordinate
/// conversion.
///
/// The enum is `#[non_exhaustive]`: match the variants you handle and keep a
/// wildcard arm. Message payloads carry human-readable context; structured
/// variants are added as concrete matching needs appear.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("unknown CRS: {0}")]
    UnknownCrs(String),

    #[error("unsupported projection: {0}")]
    UnsupportedProjection(String),

    #[error("unknown operation: {0}")]
    UnknownOperation(String),

    #[error("operation selection failed: {0}")]
    OperationSelection(String),

    #[error("invalid CRS definition: {0}")]
    InvalidDefinition(String),

    #[error("coordinate out of range: {0}")]
    OutOfRange(String),

    /// A fixed-point iteration exhausted its budget without meeting its
    /// tolerance: the input is outside the domain where the inverse series
    /// contracts.
    #[error("{context} did not converge after {iterations} iterations")]
    NonConvergence {
        context: &'static str,
        iterations: usize,
    },

    #[error(transparent)]
    Grid(#[from] crate::grid::GridError),
}

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown CRS: {0}")]
    UnknownCrs(String),

    #[error("unsupported projection: {0}")]
    UnsupportedProjection(String),

    #[error("coordinate out of range: {0}")]
    OutOfRange(String),
}

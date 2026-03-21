#![forbid(unsafe_code)]

//! Parser for WKT and PROJ format CRS strings.
//!
//! Converts CRS definition strings into [`proj_core::CrsDef`] values that can
//! be used with [`proj_core::Transform::from_crs_defs()`].
//!
//! # Supported formats
//!
//! - **Authority codes**: `"EPSG:4326"` — delegates to proj-core's registry
//! - **PROJ strings**: `"+proj=utm +zone=18 +datum=WGS84"` — parsed into CrsDef
//! - **WKT1**: `GEOGCS[...]` / `PROJCS[...]` — extracts AUTHORITY tag when present,
//!   otherwise parses projection parameters
//!
//! # Example
//!
//! ```
//! use proj_wkt::parse_crs;
//! use proj_core::Transform;
//!
//! let from = parse_crs("+proj=longlat +datum=WGS84").unwrap();
//! let to = parse_crs("EPSG:3857").unwrap();
//! let t = Transform::from_crs_defs(&from, &to).unwrap();
//! let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
//! ```

mod proj_string;
mod projjson;
mod wkt;

use proj_core::CrsDef;

/// Parse error.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("failed to parse CRS string: {0}")]
    Parse(String),
    #[error(transparent)]
    Core(#[from] proj_core::Error),
}

pub type Result<T> = std::result::Result<T, ParseError>;

/// Parse a CRS definition string in any supported format.
///
/// Automatically detects the format:
/// - Strings containing `":"` are tried as authority codes first
/// - Strings starting with `"+"` are parsed as PROJ strings
/// - Strings starting with `GEOGCS`, `PROJCS`, `GEODCRS`, or `PROJCRS` are parsed as WKT
pub fn parse_crs(s: &str) -> Result<CrsDef> {
    let s = s.trim();

    // Try authority code first (EPSG:XXXX)
    if s.contains(':') && !s.starts_with('+') && !s.starts_with("GEOG") && !s.starts_with("PROJ") {
        if let Ok(crs) = proj_core::lookup_authority_code(s) {
            return Ok(crs);
        }
    }

    // PROJ string
    if s.starts_with('+') {
        return proj_string::parse_proj_string(s);
    }

    // PROJJSON
    if s.starts_with('{') {
        return projjson::parse_projjson(s);
    }

    // WKT
    let upper = s.to_uppercase();
    if upper.starts_with("GEOGCS")
        || upper.starts_with("PROJCS")
        || upper.starts_with("GEODCRS")
        || upper.starts_with("PROJCRS")
    {
        return wkt::parse_wkt(s);
    }

    Err(ParseError::Parse(format!(
        "unrecognized CRS format: {:.80}",
        s
    )))
}

/// Create a [`Transform`](proj_core::Transform) from two CRS strings in any format.
///
/// Convenience function for downstream projects that need to handle free-form CRS strings.
pub fn transform_from_crs_strings(
    from: &str,
    to: &str,
) -> std::result::Result<proj_core::Transform, ParseError> {
    let from_crs = parse_crs(from)?;
    let to_crs = parse_crs(to)?;
    Ok(proj_core::Transform::from_crs_defs(&from_crs, &to_crs)?)
}

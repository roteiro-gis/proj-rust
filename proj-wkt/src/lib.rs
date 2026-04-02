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
//! Custom CRS definitions are only accepted when their semantics fit the
//! `proj_core::CrsDef` model: 2D longitude/latitude geographic coordinates in
//! degrees with a Greenwich prime meridian, and projected coordinates with
//! easting/northing axis order. Unsupported axis-order, prime-meridian, and
//! geographic angular-unit semantics are rejected.
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
mod semantics;
mod wkt;

use proj_core::{Bounds, Coord, Coord3D, CrsDef, Transform, Transformable, Transformable3D};

/// Parse error.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("failed to parse CRS string: {0}")]
    Parse(String),
    #[error("unsupported CRS semantics: {0}")]
    UnsupportedSemantics(String),
    #[error(transparent)]
    Core(#[from] proj_core::Error),
}

pub type Result<T> = std::result::Result<T, ParseError>;

/// Parse a CRS definition string in any supported format.
///
/// Automatically detects and handles:
/// - **Authority codes**: `"EPSG:4326"`
/// - **Bare EPSG codes**: `"4326"` (numeric-only strings)
/// - **URN format**: `"urn:ogc:def:crs:EPSG::4326"`
/// - **OGC CRS84**: `"CRS:84"`, `"OGC:CRS84"`
/// - **PROJ strings**: `"+proj=utm +zone=18 +datum=WGS84"`
/// - **PROJJSON**: `{"type": "ProjectedCRS", ...}`
/// - **WKT1**: `GEOGCS[...]` / `PROJCS[...]`
/// - **WKT2**: `GEODCRS[...]` / `PROJCRS[...]`
pub fn parse_crs(s: &str) -> Result<CrsDef> {
    let s = s.trim();

    // Normalize common aliases
    let upper = s.to_uppercase();
    if upper == "CRS:84" || upper == "OGC:CRS84" {
        return proj_core::lookup_epsg(4326)
            .ok_or_else(|| ParseError::Parse("CRS:84 not found in registry".into()));
    }

    // URN format: urn:ogc:def:crs:EPSG::4326
    if upper.starts_with("URN:OGC:DEF:CRS:") {
        let parts: Vec<&str> = s.split(':').collect();
        // Format: urn:ogc:def:crs:AUTHORITY::CODE or urn:ogc:def:crs:AUTHORITY:VERSION:CODE
        if parts.len() >= 7 {
            let code_str = parts.last().unwrap_or(&"");
            if let Ok(code) = code_str.parse::<u32>() {
                return proj_core::lookup_epsg(code)
                    .ok_or_else(|| ParseError::Parse(format!("unknown EPSG code in URN: {code}")));
            }
        }
        return Err(ParseError::Parse(format!("invalid URN format: {s}")));
    }

    // Try authority code (EPSG:XXXX)
    if s.contains(':')
        && !s.starts_with('+')
        && !upper.starts_with("GEOG")
        && !upper.starts_with("PROJ")
    {
        if let Ok(crs) = proj_core::lookup_authority_code(s) {
            return Ok(crs);
        }
    }

    // Bare numeric EPSG code (e.g., "4326")
    if let Ok(code) = s.parse::<u32>() {
        if let Some(crs) = proj_core::lookup_epsg(code) {
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
    if upper.starts_with("GEOGCS")
        || upper.starts_with("PROJCS")
        || upper.starts_with("GEODCRS")
        || upper.starts_with("GEOGCRS")
        || upper.starts_with("PROJCRS")
    {
        return wkt::parse_wkt(s);
    }

    Err(ParseError::Parse(format!(
        "unrecognized CRS format: {:.80}",
        s
    )))
}

/// Create a [`Transform`] from two CRS strings in any format.
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

/// Lightweight compatibility facade for downstream code that currently expects
/// a `proj::Proj`-like flow:
/// 1. parse a CRS definition with [`Proj::new`]
/// 2. build a CRS-to-CRS transform with [`Proj::create_crs_to_crs_from_pj`]
/// 3. convert coordinates with [`Proj::convert`]
pub struct Proj {
    inner: ProjInner,
}

enum ProjInner {
    Definition(CrsDef),
    Transform(Box<Transform>),
}

impl Proj {
    /// Parse a single CRS definition in any supported format.
    pub fn new(definition: &str) -> Result<Self> {
        Ok(Self {
            inner: ProjInner::Definition(parse_crs(definition)?),
        })
    }

    /// Build a transform directly from two CRS strings.
    pub fn new_known_crs(from: &str, to: &str, _area: Option<&str>) -> Result<Self> {
        Ok(Self {
            inner: ProjInner::Transform(Box::new(transform_from_crs_strings(from, to)?)),
        })
    }

    /// Build a transform from two parsed CRS definitions.
    pub fn create_crs_to_crs_from_pj(
        &self,
        target: &Self,
        _area: Option<&str>,
        _options: Option<&str>,
    ) -> Result<Self> {
        let source = self.definition()?;
        let target = target.definition()?;
        Ok(Self {
            inner: ProjInner::Transform(Box::new(Transform::from_crs_defs(source, target)?)),
        })
    }

    /// Transform a coordinate using a CRS-to-CRS transform.
    pub fn convert<T: Transformable>(&self, coord: T) -> proj_core::Result<T> {
        match &self.inner {
            ProjInner::Transform(transform) => transform.convert(coord),
            ProjInner::Definition(_) => Err(proj_core::Error::InvalidDefinition(
                "coordinate conversion requires a CRS-to-CRS transform, not a standalone CRS definition".into(),
            )),
        }
    }

    /// Transform a 3D coordinate using a CRS-to-CRS transform.
    pub fn convert_3d<T: Transformable3D>(&self, coord: T) -> proj_core::Result<T> {
        match &self.inner {
            ProjInner::Transform(transform) => transform.convert_3d(coord),
            ProjInner::Definition(_) => Err(proj_core::Error::InvalidDefinition(
                "coordinate conversion requires a CRS-to-CRS transform, not a standalone CRS definition".into(),
            )),
        }
    }

    /// Transform a coordinate using the native [`Coord`] type.
    pub fn convert_coord(&self, coord: Coord) -> proj_core::Result<Coord> {
        self.convert(coord)
    }

    /// Transform a 3D coordinate using the native [`Coord3D`] type.
    pub fn convert_coord_3d(&self, coord: Coord3D) -> proj_core::Result<Coord3D> {
        self.convert_3d(coord)
    }

    /// Return the inverse of a CRS-to-CRS transform.
    pub fn inverse(&self) -> Result<Self> {
        match &self.inner {
            ProjInner::Transform(transform) => Ok(Self {
                inner: ProjInner::Transform(Box::new(transform.inverse()?)),
            }),
            ProjInner::Definition(_) => Err(ParseError::Parse(
                "inverse requires a CRS-to-CRS transform, not a standalone CRS definition".into(),
            )),
        }
    }

    /// Reproject an axis-aligned bounding box by sampling its perimeter.
    pub fn transform_bounds(
        &self,
        bounds: Bounds,
        densify_points: usize,
    ) -> proj_core::Result<Bounds> {
        match &self.inner {
            ProjInner::Transform(transform) => transform.transform_bounds(bounds, densify_points),
            ProjInner::Definition(_) => Err(proj_core::Error::InvalidDefinition(
                "bounds reprojection requires a CRS-to-CRS transform, not a standalone CRS definition".into(),
            )),
        }
    }

    fn definition(&self) -> Result<&CrsDef> {
        match &self.inner {
            ProjInner::Definition(crs) => Ok(crs),
            ProjInner::Transform(_) => Err(ParseError::Parse(
                "expected a CRS definition, found a transform".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_epsg_code() {
        let crs = parse_crs("4326").unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn bare_epsg_projected() {
        let crs = parse_crs("32618").unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn urn_format() {
        let crs = parse_crs("urn:ogc:def:crs:EPSG::4326").unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn urn_with_version() {
        let crs = parse_crs("urn:ogc:def:crs:EPSG:9.8.15:3857").unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn crs84() {
        let crs = parse_crs("CRS:84").unwrap();
        assert!(crs.is_geographic());
    }

    #[test]
    fn ogc_crs84() {
        let crs = parse_crs("OGC:CRS84").unwrap();
        assert!(crs.is_geographic());
    }

    #[test]
    fn epsg_authority_code() {
        let crs = parse_crs("EPSG:3857").unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn unsupported_format_error() {
        assert!(parse_crs("not a crs").is_err());
    }

    #[test]
    fn transform_from_strings() {
        let t = transform_from_crs_strings("EPSG:4326", "EPSG:3857").unwrap();
        let (x, _y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn transform_bare_to_authority() {
        let t = transform_from_crs_strings("4326", "EPSG:3857").unwrap();
        let (x, _y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn proj_facade_from_known_crs() {
        let proj = Proj::new_known_crs("EPSG:4326", "EPSG:3857", None).unwrap();
        let (x, _y) = proj.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn proj_facade_from_known_crs_3d() {
        let proj = Proj::new_known_crs("EPSG:4326", "EPSG:3857", None).unwrap();
        let (x, _y, z) = proj.convert_3d((-74.006, 40.7128, 25.0)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((z - 25.0).abs() < 1e-12);
    }

    #[test]
    fn proj_facade_create_from_definitions() {
        let from = Proj::new("+proj=longlat +datum=WGS84").unwrap();
        let to = Proj::new("EPSG:3857").unwrap();
        let proj = from.create_crs_to_crs_from_pj(&to, None, None).unwrap();
        let (x, _y) = proj.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn proj_facade_inverse() {
        let proj = Proj::new_known_crs("EPSG:4326", "EPSG:3857", None).unwrap();
        let inv = proj.inverse().unwrap();
        let (lon, lat) = inv.convert((-8_238_310.0, 4_970_072.0)).unwrap();
        assert!(lon < -70.0);
        assert!(lat > 40.0);
    }

    #[test]
    fn proj_facade_transform_bounds() {
        let proj = Proj::new_known_crs("EPSG:4326", "EPSG:3857", None).unwrap();
        let result = proj
            .transform_bounds(Bounds::new(-74.3, 40.45, -73.65, 40.95), 4)
            .unwrap();
        assert!(result.max_x > result.min_x);
        assert!(result.max_y > result.min_y);
    }
}

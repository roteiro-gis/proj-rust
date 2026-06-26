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
//! - **WKT2/PROJJSON compound CRS**: parses explicit vertical CRS components for
//!   equality-checked z preservation and same-reference vertical unit conversion
//!
//! Custom CRS definitions are only accepted when their semantics fit the
//! `proj_core::CrsDef` model: longitude/latitude geographic coordinates in
//! degrees with a Greenwich prime meridian, projected coordinates with
//! easting/northing axis order, and compound vertical components that can be
//! preserved or unit-converted only when source and target vertical CRS
//! definitions use the same vertical reference frame.
//! Unsupported axis-order, prime-meridian, geographic angular-unit, and vertical
//! transformation semantics are rejected.
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

use proj_core::{
    AreaOfInterest, Bounds, Coord, Coord3D, CoordinateOperationId, CrsDef, SelectionOptions,
    Transform, Transformable, Transformable3D,
};

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
/// - **WKT2**: `GEODCRS[...]` / `PROJCRS[...]` / `COMPOUNDCRS[...]`
pub fn parse_crs(s: &str) -> Result<CrsDef> {
    let s = s.trim();

    // Normalize common aliases
    let upper = s.to_uppercase();
    if upper == "CRS:84" || upper == "OGC:CRS84" {
        return proj_core::lookup_epsg(4326)
            .ok_or_else(|| ParseError::Parse("CRS:84 not found in registry".into()));
    }

    if upper.starts_with("URN:") {
        return parse_ogc_crs_urn(s);
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
        || upper.starts_with("COMPD_CS")
        || upper.starts_with("COMPOUNDCRS")
        || upper.starts_with("VERT_CS")
        || upper.starts_with("VERTCRS")
        || upper.starts_with("VERTICALCRS")
    {
        return wkt::parse_wkt(s);
    }

    Err(ParseError::Parse(format!(
        "unrecognized CRS format: {:.80}",
        s
    )))
}

fn parse_ogc_crs_urn(s: &str) -> Result<CrsDef> {
    let parts = s.split(':').collect::<Vec<_>>();
    if parts.len() != 7
        || !parts[0].eq_ignore_ascii_case("urn")
        || !parts[1].eq_ignore_ascii_case("ogc")
        || !parts[2].eq_ignore_ascii_case("def")
        || !parts[3].eq_ignore_ascii_case("crs")
    {
        return Err(ParseError::Parse(format!(
            "invalid CRS URN `{s}`; expected urn:ogc:def:crs:AUTHORITY:VERSION:CODE"
        )));
    }

    let authority = parts[4];
    if authority.is_empty() {
        return Err(ParseError::Parse(format!(
            "invalid CRS URN `{s}`; missing authority"
        )));
    }
    if !authority.eq_ignore_ascii_case("EPSG") {
        return Err(ParseError::Parse(format!(
            "unsupported CRS URN authority `{authority}`; only EPSG is supported"
        )));
    }

    let code = parts[6]
        .parse::<u32>()
        .map_err(|_| ParseError::Parse(format!("invalid EPSG code in CRS URN `{s}`")))?;
    proj_core::lookup_epsg(code)
        .ok_or_else(|| ParseError::Parse(format!("unknown EPSG code in CRS URN: {code}")))
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

/// Create a [`Transform`] from two CRS strings using explicit selection options.
///
/// This is the path for parsed PROJ strings that reference external resources
/// such as `+nadgrids`, because callers can provide a [`proj_core::GridProvider`]
/// through [`SelectionOptions::with_grid_provider`].
pub fn transform_from_crs_strings_with_selection_options(
    from: &str,
    to: &str,
    options: SelectionOptions,
) -> std::result::Result<proj_core::Transform, ParseError> {
    let from_crs = parse_crs(from)?;
    let to_crs = parse_crs(to)?;
    Ok(proj_core::Transform::from_crs_defs_with_selection_options(
        &from_crs, &to_crs, options,
    )?)
}

/// Create a horizontal-only [`Transform`] from two CRS strings in any format.
///
/// Compound CRS definitions are reduced to their horizontal component before
/// operation selection. This is intended for AOI, footprint, and preview
/// workflows where vertical coordinates are not part of the operation.
pub fn transform_from_crs_strings_horizontal(
    from: &str,
    to: &str,
) -> std::result::Result<proj_core::Transform, ParseError> {
    transform_from_crs_strings_horizontal_with_selection_options(
        from,
        to,
        SelectionOptions::default(),
    )
}

/// Create a horizontal-only [`Transform`] from two CRS strings using explicit
/// selection options.
pub fn transform_from_crs_strings_horizontal_with_selection_options(
    from: &str,
    to: &str,
    options: SelectionOptions,
) -> std::result::Result<proj_core::Transform, ParseError> {
    let from_crs = parse_crs(from)?;
    let to_crs = parse_crs(to)?;
    Ok(
        proj_core::Transform::from_horizontal_components_with_selection_options(
            &from_crs, &to_crs, options,
        )?,
    )
}

fn compatibility_selection_options(
    area: Option<&str>,
    options: Option<&str>,
) -> Result<SelectionOptions> {
    let mut selection_options = SelectionOptions::default();
    if let Some(area) = parse_compatibility_area(area)? {
        selection_options = selection_options.with_area_of_interest(area);
    }
    if let Some(options) = options {
        selection_options = apply_compatibility_options(selection_options, options)?;
    }
    Ok(selection_options)
}

fn parse_compatibility_area(area: Option<&str>) -> Result<Option<AreaOfInterest>> {
    let Some(area) = area.map(str::trim).filter(|area| !area.is_empty()) else {
        return Ok(None);
    };
    let area = strip_area_prefix(area);
    let values = split_compatibility_values(area)
        .map(|value| {
            value.parse::<f64>().map_err(|_| {
                ParseError::Parse(format!(
                    "unsupported Proj compatibility area value: {value}"
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    match values.as_slice() {
        [lon, lat] => Ok(Some(AreaOfInterest::geographic_point(Coord::new(
            *lon, *lat,
        )))),
        [west, south, east, north] => Ok(Some(AreaOfInterest::geographic_bounds(Bounds::new(
            *west, *south, *east, *north,
        )))),
        _ => Err(ParseError::Parse(
            "unsupported Proj compatibility area; expected lon,lat or west,south,east,north".into(),
        )),
    }
}

fn strip_area_prefix(area: &str) -> &str {
    for prefix in [
        "bbox=", "bounds=", "area=", "point=", "aoi=", "bbox:", "bounds:", "area:", "point:",
        "aoi:",
    ] {
        if area
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        {
            return area[prefix.len()..].trim();
        }
    }
    area
}

fn apply_compatibility_options(
    mut selection_options: SelectionOptions,
    options: &str,
) -> Result<SelectionOptions> {
    for option in split_compatibility_values(options) {
        selection_options = apply_compatibility_option(selection_options, option)?;
    }
    Ok(selection_options)
}

fn apply_compatibility_option(
    selection_options: SelectionOptions,
    option: &str,
) -> Result<SelectionOptions> {
    let normalized = option.trim().trim_start_matches('+').replace('-', "_");
    let normalized = normalized.as_str();
    if normalized.is_empty() {
        return Ok(selection_options);
    }

    match normalized.to_ascii_lowercase().as_str() {
        "best_available" => Ok(selection_options.best_available()),
        "require_grids" | "require_grid" | "grids" => Ok(selection_options.require_grids()),
        "require_exact_area_match" | "exact_area" | "exact_area_match" => {
            Ok(selection_options.require_exact_area_match())
        }
        _ => match normalized
            .split_once('=')
            .or_else(|| normalized.split_once(':'))
        {
            Some((key, value))
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "operation" | "operation_id" | "op"
                ) =>
            {
                let id = value.parse::<u32>().map_err(|_| {
                    ParseError::Parse(format!(
                        "unsupported Proj compatibility operation id: {value}"
                    ))
                })?;
                Ok(selection_options.with_operation(CoordinateOperationId(id)))
            }
            _ => Err(ParseError::Parse(format!(
                "unsupported Proj compatibility option: {option}"
            ))),
        },
    }
}

fn split_compatibility_values(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|candidate: char| candidate == ',' || candidate == ';' || candidate.is_whitespace())
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
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
    pub fn new_known_crs(from: &str, to: &str, area: Option<&str>) -> Result<Self> {
        Self::new_known_crs_with_selection_options(
            from,
            to,
            compatibility_selection_options(area, None)?,
        )
    }

    /// Build a transform directly from two CRS strings using selection options.
    pub fn new_known_crs_with_selection_options(
        from: &str,
        to: &str,
        options: SelectionOptions,
    ) -> Result<Self> {
        Ok(Self {
            inner: ProjInner::Transform(Box::new(
                transform_from_crs_strings_with_selection_options(from, to, options)?,
            )),
        })
    }

    /// Build a horizontal-only transform directly from two CRS strings.
    pub fn new_known_crs_horizontal(from: &str, to: &str, area: Option<&str>) -> Result<Self> {
        Self::new_known_crs_horizontal_with_selection_options(
            from,
            to,
            compatibility_selection_options(area, None)?,
        )
    }

    /// Build a horizontal-only transform directly from two CRS strings using selection options.
    pub fn new_known_crs_horizontal_with_selection_options(
        from: &str,
        to: &str,
        options: SelectionOptions,
    ) -> Result<Self> {
        Ok(Self {
            inner: ProjInner::Transform(Box::new(
                transform_from_crs_strings_horizontal_with_selection_options(from, to, options)?,
            )),
        })
    }

    /// Build a transform from two parsed CRS definitions.
    pub fn create_crs_to_crs_from_pj(
        &self,
        target: &Self,
        area: Option<&str>,
        options: Option<&str>,
    ) -> Result<Self> {
        self.create_crs_to_crs_from_pj_with_selection_options(
            target,
            compatibility_selection_options(area, options)?,
        )
    }

    /// Build a transform from two parsed CRS definitions using selection options.
    pub fn create_crs_to_crs_from_pj_with_selection_options(
        &self,
        target: &Self,
        options: SelectionOptions,
    ) -> Result<Self> {
        let source = self.definition()?;
        let target = target.definition()?;
        Ok(Self {
            inner: ProjInner::Transform(Box::new(Transform::from_crs_defs_with_selection_options(
                source, target, options,
            )?)),
        })
    }

    /// Build a horizontal-only transform from two parsed CRS definitions.
    pub fn create_horizontal_crs_to_crs_from_pj(
        &self,
        target: &Self,
        area: Option<&str>,
        options: Option<&str>,
    ) -> Result<Self> {
        self.create_horizontal_crs_to_crs_from_pj_with_selection_options(
            target,
            compatibility_selection_options(area, options)?,
        )
    }

    /// Build a horizontal-only transform from two parsed CRS definitions using selection options.
    pub fn create_horizontal_crs_to_crs_from_pj_with_selection_options(
        &self,
        target: &Self,
        options: SelectionOptions,
    ) -> Result<Self> {
        let source = self.definition()?;
        let target = target.definition()?;
        Ok(Self {
            inner: ProjInner::Transform(Box::new(
                Transform::from_horizontal_components_with_selection_options(
                    source, target, options,
                )?,
            )),
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

    fn expect_proj_error(result: Result<Proj>) -> ParseError {
        match result {
            Ok(_) => panic!("expected Proj construction to fail"),
            Err(err) => err,
        }
    }

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
        let crs = parse_crs("urn:ogc:def:crs:EPSG:9.9:4326").unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn urn_rejects_unsupported_authority() {
        let err = parse_crs("urn:ogc:def:crs:FOO::4326").unwrap_err();
        assert!(matches!(err, ParseError::Parse(_)));
        assert!(err
            .to_string()
            .contains("unsupported CRS URN authority `FOO`"));
    }

    #[test]
    fn malformed_urn_rejects_with_clear_error() {
        let err = parse_crs("urn:ogc:def:crs:EPSG:4326").unwrap_err();
        assert!(matches!(err, ParseError::Parse(_)));
        assert!(err
            .to_string()
            .contains("expected urn:ogc:def:crs:AUTHORITY:VERSION:CODE"));
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
    fn epsg_authority_code_3d_geographic() {
        let crs = parse_crs("EPSG:4979").unwrap();
        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert!(crs.vertical_crs().is_some());
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
    fn transform_from_strings_with_selection_options() {
        let t = transform_from_crs_strings_with_selection_options(
            "EPSG:4326",
            "EPSG:3857",
            SelectionOptions::default(),
        )
        .unwrap();
        let (x, _y) = t.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn horizontal_transform_from_compound_strings() {
        let err = match transform_from_crs_strings("EPSG:4979", "EPSG:3857") {
            Ok(_) => panic!("expected compound-to-horizontal transform to fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("explicit vertical CRS"));

        let t = transform_from_crs_strings_horizontal("EPSG:4979", "EPSG:3857").unwrap();
        let (x, _y, z) = t.convert_3d((-74.006, 40.7128, 25.0)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((z - 25.0).abs() < 1e-12);
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
    fn proj_facade_new_known_crs_accepts_area_string() {
        let proj =
            Proj::new_known_crs("EPSG:4326", "EPSG:3857", Some("bbox=-75,40,-74,41")).unwrap();
        let (x, _y) = proj.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn proj_facade_new_known_crs_rejects_invalid_area_string() {
        let err = expect_proj_error(Proj::new_known_crs(
            "EPSG:4326",
            "EPSG:3857",
            Some("not an area"),
        ));
        assert!(err
            .to_string()
            .contains("unsupported Proj compatibility area"));
    }

    #[test]
    fn proj_facade_new_known_crs_with_selection_options_uses_options() {
        let err = expect_proj_error(Proj::new_known_crs_with_selection_options(
            "EPSG:4326",
            "EPSG:3857",
            SelectionOptions::new().with_operation(CoordinateOperationId(999_999)),
        ));
        assert!(err.to_string().contains("unknown operation id 999999"));
    }

    #[test]
    fn proj_facade_from_known_crs_3d() {
        let proj = Proj::new_known_crs("EPSG:4326", "EPSG:3857", None).unwrap();
        let (x, _y, z) = proj.convert_3d((-74.006, 40.7128, 25.0)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((z - 25.0).abs() < 1e-12);
    }

    #[test]
    fn proj_facade_from_known_crs_horizontal() {
        let proj = Proj::new_known_crs_horizontal("EPSG:4979", "EPSG:3857", None).unwrap();
        let (x, _y, z) = proj.convert_3d((-74.006, 40.7128, 25.0)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
        assert!((z - 25.0).abs() < 1e-12);
    }

    #[test]
    fn proj_facade_from_known_crs_horizontal_with_selection_options() {
        let proj = Proj::new_known_crs_horizontal_with_selection_options(
            "EPSG:4979",
            "EPSG:3857",
            SelectionOptions::new(),
        )
        .unwrap();
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
    fn proj_facade_create_from_definitions_applies_options_string() {
        let from = Proj::new("+proj=longlat +datum=WGS84").unwrap();
        let to = Proj::new("EPSG:3857").unwrap();
        let err =
            expect_proj_error(from.create_crs_to_crs_from_pj(&to, None, Some("operation=999999")));
        assert!(err.to_string().contains("unknown operation id 999999"));
    }

    #[test]
    fn proj_facade_create_from_definitions_rejects_approximate_fallback_option() {
        let from = Proj::new("+proj=longlat +datum=NAD27").unwrap();
        let to = Proj::new("+proj=longlat +datum=OSGB36").unwrap();

        let err = expect_proj_error(from.create_crs_to_crs_from_pj(&to, None, None));
        assert!(err
            .to_string()
            .contains("no compatible registry operation found"));

        let err = expect_proj_error(from.create_crs_to_crs_from_pj(
            &to,
            None,
            Some("allow_approximate_helmert_fallback"),
        ));
        assert!(err
            .to_string()
            .contains("unsupported Proj compatibility option"));
    }

    #[test]
    fn proj_facade_create_from_definitions_with_selection_options() {
        let from = Proj::new("+proj=longlat +datum=WGS84").unwrap();
        let to = Proj::new("EPSG:3857").unwrap();
        let proj = from
            .create_crs_to_crs_from_pj_with_selection_options(&to, SelectionOptions::new())
            .unwrap();
        let (x, _y) = proj.convert((-74.006, 40.7128)).unwrap();
        assert!((x - (-8238310.0)).abs() < 100.0);
    }

    #[test]
    fn proj_facade_create_horizontal_from_compound_definition() {
        let from = Proj::new("EPSG:4979").unwrap();
        let to = Proj::new("EPSG:3857").unwrap();
        let proj = from
            .create_horizontal_crs_to_crs_from_pj(&to, None, None)
            .unwrap();
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

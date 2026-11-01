#![forbid(unsafe_code)]

//! Parser for WKT and PROJ format CRS strings.
//!
//! Converts CRS definition strings into [`proj_core::CrsDef`] values that can
//! be used with [`proj_core::Transform::from_crs_defs()`].
//!
//! # Supported formats
//!
//! - **Authority codes**: `"EPSG:4326"` — delegates to proj-core's registry
//! - **PROJ strings**: `"+proj=utm +zone=18 +datum=WGS84"`, `"+proj=cart +datum=WGS84"` —
//!   parsed into CrsDef
//! - **WKT1**: `GEOGCS[...]` / `PROJCS[...]` / `GEOCCS[...]` / `COMPD_CS[...]` —
//!   extracts an AUTHORITY tag when present, otherwise parses projection parameters
//! - **WKT1/WKT2/PROJJSON compound CRS**: parses explicit vertical CRS components for
//!   equality-checked z preservation and same-reference vertical unit conversion
//! - **WKT output**: serializes [`proj_core::CrsDef`] values to WKT1-style
//!   `GEOGCS[...]`, `PROJCS[...]`, `GEOCCS[...]`, and `COMPD_CS[...]` definitions
//!
//! Custom CRS definitions are only accepted when their semantics fit the
//! `proj_core::CrsDef` model: longitude/latitude geographic coordinates in
//! degrees with a Greenwich prime meridian, geocentric ECEF coordinates in
//! metres with geocentric X/Y/Z axes, projected coordinates with
//! easting/northing axis order, and compound vertical components that can be
//! preserved or unit-converted only when source and target vertical CRS
//! definitions use the same vertical reference frame.
//! Parsed PROJ `+nadgrids` metadata is applied by
//! [`transform_from_crs_strings`], [`transform_from_crs_strings_with_selection_options`],
//! and the [`Proj`] facade as explicit custom coordinate operations; plain
//! [`parse_crs`] returns only the CRS definition.
//! Unsupported axis-order, prime-meridian, geographic angular-unit, and vertical
//! transformation semantics are rejected.
//! EPSG-tagged WKT1 definitions with an authority-native axis permutation are
//! validated and canonicalized to the library's longitude/latitude or
//! easting/northing coordinate order.
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
mod projjson_writer;
mod semantics;
mod wkt;
mod wkt2_writer;
mod wkt_writer;

use proj_core::{
    AreaOfInterest, Bounds, CompoundCrsDef, Coord, Coord3D, CoordinateOperation,
    CoordinateOperationId, CrsDef, DatumGridShift, DatumToWgs84, OperationMethod, SelectionOptions,
    Transform, Transformable, Transformable3D,
};

/// Parse error.
///
/// `#[non_exhaustive]`: match the variants you handle and keep a wildcard arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    #[error("failed to parse CRS string: {0}")]
    Parse(String),
    #[error("unsupported CRS semantics: {0}")]
    UnsupportedSemantics(String),
    #[error(transparent)]
    Core(#[from] proj_core::Error),
}

pub type Result<T> = std::result::Result<T, ParseError>;

#[derive(Clone)]
struct ParsedCrs {
    crs: CrsDef,
    grid_shift_to_wgs84: Option<DatumGridShift>,
}

impl ParsedCrs {
    fn new(crs: CrsDef) -> Self {
        Self {
            crs,
            grid_shift_to_wgs84: None,
        }
    }
}

/// Parse a CRS definition string in any supported format.
///
/// Automatically detects and handles:
/// - **Authority codes**: `"EPSG:4326"`
/// - **Bare EPSG codes**: `"4326"` (numeric-only strings)
/// - **Compound EPSG codes**: `"EPSG:32618+5703"` (horizontal + vertical)
/// - **URN format**: `"urn:ogc:def:crs:EPSG::4326"`, `"urn:ogc:def:crs:OGC:1.3:CRS84"`,
///   and `"urn:ogc:def:crs,crs:EPSG::32618,crs:EPSG::5703"`
/// - **OGC URIs**: `"http://www.opengis.net/def/crs/EPSG/0/4326"`,
///   `"http://www.opengis.net/def/crs/OGC/1.3/CRS84"`
/// - **OGC CRS84, CRS83, CRS27**: `"CRS:84"`, `"OGC:CRS84"`, `"CRS84"`
/// - **PROJ strings**: `"+proj=utm +zone=18 +datum=WGS84"`, `"+proj=cart +datum=WGS84"`
/// - **PROJJSON**: `{"type": "ProjectedCRS", ...}` or geocentric `GeodeticCRS`
/// - **WKT1**: `GEOGCS[...]` / `PROJCS[...]` / `GEOCCS[...]` / `COMPD_CS[...]`
/// - **WKT2**: `GEODCRS[...]` / `GEOGCRS[...]` / `PROJCRS[...]` / `COMPOUNDCRS[...]`
pub fn parse_crs(s: &str) -> Result<CrsDef> {
    Ok(parse_crs_definition(s)?.crs)
}

/// Serialize a CRS definition as WKT1-style `GEOGCS`, `PROJCS`, `GEOCCS`, or
/// `COMPD_CS`.
///
/// Projection false easting/northing parameters are emitted in the CRS native
/// linear unit, while the `proj_core::ProjectionMethod` model stores them in
/// meters.
pub fn to_wkt(crs: &CrsDef) -> Result<String> {
    wkt_writer::to_wkt(crs)
}

/// Serialize a CRS definition as WKT2 (ISO 19162) `GEOGCRS`, geocentric
/// `GEODCRS`, `PROJCRS`, or `COMPOUNDCRS`.
///
/// Projection false easting/northing parameters are emitted in the CRS native
/// linear unit, while the `proj_core::ProjectionMethod` model stores them in
/// meters. The emitted text reparses through [`parse_crs`] to an equivalent
/// definition.
pub fn to_wkt2(crs: &CrsDef) -> Result<String> {
    wkt2_writer::to_wkt2(crs)
}

/// Serialize a CRS definition as PROJJSON.
///
/// The emitted document reparses through [`parse_crs`] to an equivalent
/// definition; EPSG identities present on the definition are preserved.
pub fn to_projjson(crs: &CrsDef) -> Result<String> {
    let value = projjson_writer::to_projjson_value(crs)?;
    serde_json::to_string(&value)
        .map_err(|e| ParseError::Parse(format!("PROJJSON serialization failed: {e}")))
}

fn parse_crs_definition(s: &str) -> Result<ParsedCrs> {
    let s = s.trim();

    let upper = s.to_uppercase();
    if let Some(code) = ogc_crs_alias(&upper) {
        return lookup_ogc_crs(code).map(ParsedCrs::new);
    }

    if upper.starts_with("URN:") {
        return parse_ogc_crs_urn(s).map(ParsedCrs::new);
    }

    if upper.starts_with("HTTP://") || upper.starts_with("HTTPS://") {
        return parse_ogc_crs_uri(s).map(ParsedCrs::new);
    }

    // EPSG authority code, optionally a horizontal+vertical pair
    if let Some((authority, codes)) = s.split_once(':') {
        if authority.trim().eq_ignore_ascii_case("EPSG") {
            if let Some((horizontal, vertical)) = codes.split_once('+') {
                return lookup_epsg_compound(horizontal, vertical).map(ParsedCrs::new);
            }
            return Ok(ParsedCrs::new(proj_core::lookup_authority_code(s)?));
        }
    }

    // Bare numeric EPSG code (e.g., "4326")
    if let Ok(code) = s.parse::<u32>() {
        if let Some(crs) = proj_core::lookup_epsg(code) {
            return Ok(ParsedCrs::new(crs));
        }
    }

    // PROJ string
    if s.starts_with('+') {
        return proj_string::parse_proj_string_with_operations(s);
    }

    // PROJJSON
    if s.starts_with('{') {
        return projjson::parse_projjson(s).map(ParsedCrs::new);
    }

    // WKT
    if upper.starts_with("GEOGCS")
        || upper.starts_with("PROJCS")
        || upper.starts_with("GEOCCS")
        || upper.starts_with("GEODCRS")
        || upper.starts_with("GEOGCRS")
        || upper.starts_with("PROJCRS")
        || upper.starts_with("COMPD_CS")
        || upper.starts_with("COMPOUNDCRS")
        || upper.starts_with("VERT_CS")
        || upper.starts_with("VERTCRS")
        || upper.starts_with("VERTICALCRS")
    {
        return wkt::parse_wkt(s).map(ParsedCrs::new);
    }

    Err(ParseError::Parse(format!(
        "unrecognized CRS format: {:.80}",
        s
    )))
}

/// Short OGC CRS identifier, such as `CRS:84` or `OGC:CRS84`, without its prefix.
fn ogc_crs_alias(upper: &str) -> Option<&str> {
    upper
        .strip_prefix("OGC:")
        .or_else(|| upper.strip_prefix("CRS:"))
        .or_else(|| upper.starts_with("CRS").then_some(upper))
        .filter(|code| ogc_crs_epsg(code).is_some())
}

/// EPSG CRS equivalent to an OGC CRS code. These are longitude-first by
/// definition, which is the library's coordinate order for every CRS.
fn ogc_crs_epsg(code: &str) -> Option<u32> {
    match code.to_ascii_uppercase().as_str() {
        "CRS84" | "84" => Some(4326),
        "CRS83" | "83" => Some(4269),
        "CRS27" | "27" => Some(4267),
        _ => None,
    }
}

fn lookup_ogc_crs(code: &str) -> Result<CrsDef> {
    let epsg = ogc_crs_epsg(code)
        .ok_or_else(|| ParseError::Parse(format!("unsupported OGC CRS code `{code}`")))?;
    proj_core::lookup_epsg(epsg)
        .ok_or_else(|| ParseError::Parse(format!("OGC CRS {code} not found in registry")))
}

fn lookup_epsg_compound(horizontal: &str, vertical: &str) -> Result<CrsDef> {
    let code = |value: &str| {
        value
            .trim()
            .parse::<u32>()
            .map_err(|_| ParseError::Parse(format!("invalid EPSG code: {value}")))
    };
    let (horizontal, vertical) = (code(horizontal)?, code(vertical)?);
    let horizontal = proj_core::lookup_epsg(horizontal)
        .ok_or_else(|| ParseError::Parse(format!("unknown EPSG code: {horizontal}")))?;
    let vertical = proj_core::lookup_vertical_epsg(vertical)
        .ok_or_else(|| ParseError::Parse(format!("unknown EPSG vertical code: {vertical}")))?;
    let compound = CompoundCrsDef::from_crs_def(0, horizontal, vertical, "")?;
    Ok(CrsDef::Compound(Box::new(compound)))
}

/// Resolve the authority and code of a CRS identifier of the given `kind`.
fn lookup_authority_crs(kind: &str, s: &str, authority: &str, code: &str) -> Result<CrsDef> {
    if authority.eq_ignore_ascii_case("OGC") {
        return lookup_ogc_crs(code);
    }
    if authority.is_empty() {
        return Err(ParseError::Parse(format!(
            "invalid CRS {kind} `{s}`; missing authority"
        )));
    }
    if !authority.eq_ignore_ascii_case("EPSG") {
        return Err(ParseError::Parse(format!(
            "unsupported CRS {kind} authority `{authority}`; only EPSG and OGC are supported"
        )));
    }
    let epsg = code
        .parse::<u32>()
        .map_err(|_| ParseError::Parse(format!("invalid EPSG code in CRS {kind} `{s}`")))?;
    proj_core::lookup_epsg(epsg)
        .ok_or_else(|| ParseError::Parse(format!("unknown EPSG code in CRS {kind}: {epsg}")))
}

fn parse_ogc_crs_urn(s: &str) -> Result<CrsDef> {
    if let Some(components) = s
        .get(.."urn:ogc:def:crs,".len())
        .filter(|prefix| prefix.eq_ignore_ascii_case("urn:ogc:def:crs,"))
        .map(|prefix| &s[prefix.len()..])
    {
        return parse_ogc_compound_crs_urn(s, components);
    }

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

    lookup_authority_crs("URN", s, parts[4], parts[6])
}

/// Parse the `urn:ogc:def:crs,crs:EPSG::H,crs:EPSG::V` compound form.
fn parse_ogc_compound_crs_urn(s: &str, components: &str) -> Result<CrsDef> {
    let component_code = |component: &str| {
        let parts = component.split(':').collect::<Vec<_>>();
        match parts.as_slice() {
            [crs, authority, _version, code]
                if crs.eq_ignore_ascii_case("crs") && authority.eq_ignore_ascii_case("EPSG") =>
            {
                Ok(code.to_string())
            }
            _ => Err(ParseError::Parse(format!(
                "invalid compound CRS URN `{s}`; expected urn:ogc:def:crs,crs:EPSG::CODE,crs:EPSG::CODE"
            ))),
        }
    };
    let Some((horizontal, vertical)) = components.split_once(',') else {
        return Err(ParseError::Parse(format!(
            "invalid compound CRS URN `{s}`; expected two components"
        )));
    };
    lookup_epsg_compound(&component_code(horizontal)?, &component_code(vertical)?)
}

/// Parse `http(s)://www.opengis.net/def/crs/AUTHORITY/VERSION/CODE` and the
/// legacy `http://www.opengis.net/gml/srs/epsg.xml#CODE` form.
fn parse_ogc_crs_uri(s: &str) -> Result<CrsDef> {
    let invalid = || {
        ParseError::Parse(format!(
            "invalid CRS URI `{s}`; expected http://www.opengis.net/def/crs/AUTHORITY/VERSION/CODE"
        ))
    };
    let (_, rest) = s.split_once("://").ok_or_else(invalid)?;
    let (host, path) = rest.split_once('/').ok_or_else(invalid)?;
    if !host.eq_ignore_ascii_case("www.opengis.net") && !host.eq_ignore_ascii_case("opengis.net") {
        return Err(invalid());
    }
    if let Some(code) = path.strip_prefix("gml/srs/epsg.xml#") {
        return lookup_authority_crs("URI", s, "EPSG", code);
    }
    match path.trim_end_matches('/').split('/').collect::<Vec<_>>()[..] {
        ["def", "crs", authority, _version, code] => {
            lookup_authority_crs("URI", s, authority, code)
        }
        _ => Err(invalid()),
    }
}

/// Create a [`Transform`] from two CRS strings in any format.
///
/// Convenience function for downstream projects that need to handle free-form CRS strings.
pub fn transform_from_crs_strings(
    from: &str,
    to: &str,
) -> std::result::Result<proj_core::Transform, ParseError> {
    transform_from_crs_strings_with_selection_options(from, to, SelectionOptions::default())
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
    let from_crs = parse_crs_definition(from)?;
    let to_crs = parse_crs_definition(to)?;
    let options = with_parsed_coordinate_operations(options, &from_crs, &to_crs);
    Ok(proj_core::Transform::from_crs_defs_with_selection_options(
        &from_crs.crs,
        &to_crs.crs,
        options,
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
    let from_crs = parse_crs_definition(from)?;
    let to_crs = parse_crs_definition(to)?;
    let options = with_parsed_coordinate_operations(options, &from_crs, &to_crs);
    Ok(
        proj_core::Transform::from_horizontal_components_with_selection_options(
            &from_crs.crs,
            &to_crs.crs,
            options,
        )?,
    )
}

fn with_parsed_coordinate_operations(
    options: SelectionOptions,
    source: &ParsedCrs,
    target: &ParsedCrs,
) -> SelectionOptions {
    if let Some(operation) = parsed_grid_datum_operation(source, target) {
        options.with_coordinate_operation(operation)
    } else {
        options
    }
}

fn parsed_grid_datum_operation(
    source: &ParsedCrs,
    target: &ParsedCrs,
) -> Option<CoordinateOperation> {
    if source.grid_shift_to_wgs84.is_none() && target.grid_shift_to_wgs84.is_none() {
        return None;
    }

    if parsed_grid_datums_match(source, target) {
        return Some(parsed_coordinate_operation(
            source,
            target,
            "Parsed +nadgrids identity datum match",
            OperationMethod::Identity,
        ));
    }

    let source_to_wgs84 = parsed_datum_to_wgs84(source)?;
    let target_to_wgs84 = parsed_datum_to_wgs84(target)?;

    Some(parsed_coordinate_operation(
        source,
        target,
        "Parsed +nadgrids datum shift",
        OperationMethod::DatumShift {
            source_to_wgs84,
            target_to_wgs84,
        },
    ))
}

fn parsed_coordinate_operation(
    source: &ParsedCrs,
    target: &ParsedCrs,
    name: &str,
    method: OperationMethod,
) -> CoordinateOperation {
    CoordinateOperation {
        id: None,
        name: name.into(),
        source_crs_epsg: source.crs.base_geographic_crs_epsg(),
        target_crs_epsg: target.crs.base_geographic_crs_epsg(),
        source_datum_epsg: None,
        target_datum_epsg: None,
        accuracy: None,
        areas_of_use: smallvec::SmallVec::new(),
        deprecated: false,
        preferred: true,
        approximate: false,
        superseded: false,
        domain: proj_core::OperationDomain::HorizontalOnly,
        method,
    }
}

fn parsed_grid_datums_match(source: &ParsedCrs, target: &ParsedCrs) -> bool {
    source.grid_shift_to_wgs84.is_some()
        && source.grid_shift_to_wgs84 == target.grid_shift_to_wgs84
        && same_ellipsoid(
            source.crs.datum().ellipsoid(),
            target.crs.datum().ellipsoid(),
        )
}

fn same_ellipsoid(left: proj_core::Ellipsoid, right: proj_core::Ellipsoid) -> bool {
    (left.semi_major_axis() - right.semi_major_axis()).abs() < 1e-6
        && (left.flattening() - right.flattening()).abs() < 1e-12
}

fn parsed_datum_to_wgs84(parsed: &ParsedCrs) -> Option<DatumToWgs84> {
    if let Some(grid_shift) = &parsed.grid_shift_to_wgs84 {
        return Some(DatumToWgs84::GridShift(Box::new(grid_shift.clone())));
    }

    if is_wgs84_datum_definition(parsed) {
        Some(DatumToWgs84::Identity)
    } else {
        None
    }
}

fn is_wgs84_datum_definition(parsed: &ParsedCrs) -> bool {
    parsed.crs.base_geographic_crs_epsg() == Some(4326)
        || parsed.crs.datum().same_datum(&proj_core::datum::WGS84)
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
    Definition(Box<ParsedCrs>),
    Transform(Box<Transform>),
}

impl Proj {
    /// Parse a single CRS definition in any supported format.
    pub fn new(definition: &str) -> Result<Self> {
        Ok(Self {
            inner: ProjInner::Definition(Box::new(parse_crs_definition(definition)?)),
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
        let source = self.parsed_definition()?;
        let target = target.parsed_definition()?;
        let options = with_parsed_coordinate_operations(options, source, target);
        Ok(Self {
            inner: ProjInner::Transform(Box::new(Transform::from_crs_defs_with_selection_options(
                &source.crs,
                &target.crs,
                options,
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
        let source = self.parsed_definition()?;
        let target = target.parsed_definition()?;
        let options = with_parsed_coordinate_operations(options, source, target);
        Ok(Self {
            inner: ProjInner::Transform(Box::new(
                Transform::from_horizontal_components_with_selection_options(
                    &source.crs,
                    &target.crs,
                    options,
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

    fn parsed_definition(&self) -> Result<&ParsedCrs> {
        match &self.inner {
            ProjInner::Definition(parsed) => Ok(parsed.as_ref()),
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
    fn ogc_crs_identifiers_resolve_to_their_epsg_equivalents() {
        for (identifier, epsg) in [
            ("CRS84", 4326),
            ("CRS:84", 4326),
            ("OGC:CRS84", 4326),
            ("urn:ogc:def:crs:OGC:1.3:CRS84", 4326),
            ("urn:ogc:def:crs:OGC::CRS84", 4326),
            ("URN:OGC:DEF:CRS:OGC:1.3:CRS84", 4326),
            ("http://www.opengis.net/def/crs/OGC/1.3/CRS84", 4326),
            ("https://www.opengis.net/def/crs/OGC/1.3/CRS84", 4326),
            ("http://www.opengis.net/def/crs/EPSG/0/3857", 3857),
            ("http://www.opengis.net/gml/srs/epsg.xml#4326", 4326),
            ("CRS:83", 4269),
            ("OGC:CRS27", 4267),
        ] {
            let crs = parse_crs(identifier).unwrap_or_else(|err| panic!("{identifier}: {err}"));
            assert_eq!(crs.epsg(), epsg, "{identifier}");
        }
    }

    #[test]
    fn compound_identifiers_pair_a_horizontal_and_vertical_crs() {
        for identifier in [
            "EPSG:32618+5703",
            "epsg:32618+5703",
            "urn:ogc:def:crs,crs:EPSG::32618,crs:EPSG::5703",
            "urn:ogc:def:crs,crs:EPSG:9.9:32618,crs:EPSG:9.9:5703",
        ] {
            let crs = parse_crs(identifier).unwrap_or_else(|err| panic!("{identifier}: {err}"));
            assert!(crs.is_compound(), "{identifier}");
            assert!(crs.is_projected(), "{identifier}");
            assert_eq!(crs.base_geographic_crs_epsg(), Some(4326), "{identifier}");
            assert_eq!(crs.vertical_crs().unwrap().epsg(), 5703, "{identifier}");
        }
    }

    #[test]
    fn invalid_identifiers_explain_what_is_wrong() {
        for (identifier, expected) in [
            ("EPSG:99999", "unknown EPSG code: 99999"),
            ("EPSG:32618+99999", "unknown EPSG vertical code: 99999"),
            ("EPSG:99999+5703", "unknown EPSG code: 99999"),
            ("EPSG:abc+5703", "invalid EPSG code"),
            ("EPSG:4979+5773", "cannot itself be compound"),
            ("EPSG:4978+5773", "geocentric CRS cannot be used"),
            ("urn:ogc:def:crs:OGC:1.3:CRS99", "unsupported OGC CRS code"),
            (
                "http://www.opengis.net/def/crs/FOO/0/1",
                "only EPSG and OGC",
            ),
            ("http://example.com/def/crs/EPSG/0/4326", "invalid CRS URI"),
            ("http://www.opengis.net/def/crs/EPSG/0", "invalid CRS URI"),
            ("urn:ogc:def:crs,crs:EPSG::4326", "expected two components"),
            (
                "urn:ogc:def:crs,crs:EPSG:4326,crs:EPSG::5773",
                "compound CRS URN",
            ),
        ] {
            let err = parse_crs(identifier).unwrap_err().to_string();
            assert!(err.contains(expected), "{identifier}: {err}");
        }
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
    fn proj_facade_create_from_nadgrids_definition() {
        let from =
            Proj::new("+proj=longlat +ellps=clrk66 +nadgrids=@missing.gsb,ntv2_0.gsb").unwrap();
        let to = Proj::new("+proj=longlat +datum=WGS84").unwrap();
        let proj = from.create_crs_to_crs_from_pj(&to, None, None).unwrap();

        let (lon, lat) = proj.convert((-80.5041667, 44.5458333)).unwrap();

        assert!((lon - (-80.50401615833)).abs() < 1e-6, "lon={lon}");
        assert!((lat - 44.5458827236).abs() < 3e-6, "lat={lat}");
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

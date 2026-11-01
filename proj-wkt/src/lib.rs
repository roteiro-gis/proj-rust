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
//! Datum shifts a definition declares itself, through PROJ `+nadgrids` or
//! `+towgs84`, WKT1 `TOWGS84`, or a WKT2/PROJJSON bound CRS, are applied by
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
    CoordinateOperationId, CrsDef, DatumGridShift, DatumToWgs84, HelmertParams, OperationMethod,
    SelectionOptions, Transform, Transformable, Transformable3D,
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
    helmert_to_wgs84: Option<HelmertParams>,
}

impl ParsedCrs {
    fn new(crs: CrsDef) -> Self {
        Self {
            crs,
            grid_shift_to_wgs84: None,
            helmert_to_wgs84: None,
        }
    }

    fn declares_datum_shift(&self) -> bool {
        self.grid_shift_to_wgs84.is_some() || self.helmert_to_wgs84.is_some()
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
/// - **WKT2**: `GEODCRS[...]` / `GEOGCRS[...]` / `PROJCRS[...]` / `COMPOUNDCRS[...]` /
///   `BOUNDCRS[...]`, which returns its source CRS, and the long-form `GEODETICCRS`,
///   `GEOGRAPHICCRS`, and `PROJECTEDCRS` keywords
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
        return projjson::parse_projjson_with_operations(s);
    }

    // WKT
    if upper.starts_with("GEOGCS")
        || upper.starts_with("PROJCS")
        || upper.starts_with("GEOCCS")
        || upper.starts_with("GEODCRS")
        || upper.starts_with("GEODETICCRS")
        || upper.starts_with("GEOGCRS")
        || upper.starts_with("GEOGRAPHICCRS")
        || upper.starts_with("PROJCRS")
        || upper.starts_with("PROJECTEDCRS")
        || upper.starts_with("COMPD_CS")
        || upper.starts_with("COMPOUNDCRS")
        || upper.starts_with("BOUNDCRS")
        || upper.starts_with("VERT_CS")
        || upper.starts_with("VERTCRS")
        || upper.starts_with("VERTICALCRS")
    {
        return wkt::parse_wkt_with_operations(s);
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
    if let Some(operation) = parsed_datum_operation(source, target) {
        options.with_coordinate_operation(operation)
    } else {
        options
    }
}

/// Build the explicit operation for a shift the definitions declare through
/// `+nadgrids`, `+towgs84`, or WKT1 `TOWGS84`. Selection never derives one from
/// datum metadata, so `parse_crs` with `Transform::from_crs_defs` omits it.
fn parsed_datum_operation(source: &ParsedCrs, target: &ParsedCrs) -> Option<CoordinateOperation> {
    if !source.declares_datum_shift() && !target.declares_datum_shift() {
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
    if source.crs.datum().same_datum(target.crs.datum()) {
        return None;
    }

    let source_to_wgs84 = parsed_datum_to_wgs84(source)?;
    let target_to_wgs84 = parsed_datum_to_wgs84(target)?;

    Some(parsed_coordinate_operation(
        source,
        target,
        if source.grid_shift_to_wgs84.is_some() || target.grid_shift_to_wgs84.is_some() {
            "Parsed +nadgrids datum shift"
        } else {
            "Parsed +towgs84 datum shift"
        },
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
    if let Some(helmert) = parsed.helmert_to_wgs84 {
        return Some(DatumToWgs84::Helmert(helmert));
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

    const PROJJSON_LATLON_BASE: &str = r#"{"name":"WGS 84","datum_ensemble":{"name":"World Geodetic System 1984 ensemble","members":[{"name":"World Geodetic System 1984 (Transit)"}],"ellipsoid":{"name":"WGS 84","semi_major_axis":6378137,"inverse_flattening":298.257223563},"accuracy":"2.0","id":{"authority":"EPSG","code":6326}},"coordinate_system":{"subtype":"ellipsoidal","axis":[{"name":"Geodetic latitude","abbreviation":"Lat","direction":"north","unit":"degree"},{"name":"Geodetic longitude","abbreviation":"Lon","direction":"east","unit":"degree"}]},"id":{"authority":"EPSG","code":4326}}"#;
    const PROJJSON_UTM18N_CONVERSION: &str = r#"{"name":"UTM zone 18N","method":{"name":"Transverse Mercator","id":{"authority":"EPSG","code":9807}},"parameters":[{"name":"Latitude of natural origin","value":0,"unit":"degree"},{"name":"Longitude of natural origin","value":-75,"unit":"degree"},{"name":"Scale factor at natural origin","value":0.9996,"unit":"unity"},{"name":"False easting","value":500000,"unit":"metre"},{"name":"False northing","value":0,"unit":"metre"}]}"#;
    const PROJJSON_EN_AXES: &str = r#"{"subtype":"Cartesian","axis":[{"name":"Easting","abbreviation":"E","direction":"east","unit":"metre"},{"name":"Northing","abbreviation":"N","direction":"north","unit":"metre"}]}"#;

    fn projjson_utm18n(id: &str) -> String {
        format!(
            r#"{{"type":"ProjectedCRS","name":"WGS 84 / UTM zone 18N","base_crs":{PROJJSON_LATLON_BASE},"conversion":{PROJJSON_UTM18N_CONVERSION},"coordinate_system":{PROJJSON_EN_AXES}{id}}}"#
        )
    }

    #[test]
    fn projected_definitions_ignore_base_crs_axis_order() {
        // PROJ writes `base_crs` latitude-first even for easting/northing CRSs.
        let tagged = projjson_utm18n(r#","id":{"authority":"EPSG","code":32618}"#);
        assert_eq!(parse_crs(&tagged).unwrap().epsg(), 32618);

        let custom = parse_crs(&projjson_utm18n("")).unwrap();
        assert_eq!(custom.epsg(), 0);
        assert!(custom.is_projected());

        let wkt1 = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AXIS["Latitude",NORTH],AXIS["Longitude",EAST]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1],AXIS["Easting",EAST],AXIS["Northing",NORTH]]"#;
        assert!(parse_crs(wkt1).unwrap().is_projected());
    }

    #[test]
    fn projected_crs_axis_order_stays_strict() {
        let swapped = projjson_utm18n("").replace(
            PROJJSON_EN_AXES,
            r#"{"subtype":"Cartesian","axis":[{"name":"Northing","abbreviation":"N","direction":"north","unit":"metre"},{"name":"Easting","abbreviation":"E","direction":"east","unit":"metre"}]}"#,
        );
        assert!(matches!(
            parse_crs(&swapped),
            Err(ParseError::UnsupportedSemantics(_))
        ));
    }

    #[test]
    fn datum_only_definitions_transform_like_their_epsg_codes() {
        // `+datum` and named WKT datums carry no EPSG CRS code, only a datum.
        const ESRI_NAD83_UTM18N: &str = r#"PROJCS["NAD_1983_UTM_Zone_18N",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-75.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        let cases = [
            (
                "+proj=utm +zone=18 +datum=NAD83 +units=m +no_defs",
                "EPSG:26918",
                (583_960.0, 4_507_523.0),
            ),
            (ESRI_NAD83_UTM18N, "EPSG:26918", (583_960.0, 4_507_523.0)),
            ("+proj=longlat +datum=NAD27", "EPSG:4267", (-74.0, 40.7)),
            ("+proj=longlat +datum=OSGB36", "EPSG:4277", (-1.5, 52.0)),
        ];

        for (definition, code, coord) in cases {
            let parsed = transform_from_crs_strings(definition, "EPSG:4326")
                .unwrap_or_else(|err| panic!("{definition}: {err}"));
            let registry = Transform::new(code, "EPSG:4326").unwrap();
            let (x, y) = parsed.convert(coord).unwrap();
            let (expected_x, expected_y) = registry.convert(coord).unwrap();
            assert_eq!(
                parsed.selected_operation().id,
                registry.selected_operation().id,
                "{definition}"
            );
            assert!((x - expected_x).abs() < 1e-9, "{definition}");
            assert!((y - expected_y).abs() < 1e-9, "{definition}");
            assert!(parsed.inverse().is_ok(), "{definition}");
        }
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

    const RD_TOWGS84: &str = "565.4171,50.3319,465.5524,0.398957,-0.343988,1.8774,4.0725";
    /// C PROJ 9.6 writes these for the `+ellps=bessel +towgs84=RD_TOWGS84` CRS.
    const BESSEL_TOWGS84_WKT1: &str = r#"GEOGCS["unknown",DATUM["Unknown based on Bessel 1841 ellipsoid using towgs84=565.4171,50.3319,465.5524,0.398957,-0.343988,1.87740,4.0725",SPHEROID["Bessel 1841",6377397.155,299.1528128],TOWGS84[565.4171,50.3319,465.5524,0.398957,-0.343988,1.8774,4.0725]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]]"#;
    const BESSEL_BOUND_WKT2: &str = r#"BOUNDCRS[SOURCECRS[GEOGCRS["unknown",DATUM["Unknown based on Bessel 1841 ellipsoid using towgs84=565.4171,50.3319,465.5524,0.398957,-0.343988,1.87740,4.0725",ELLIPSOID["Bessel 1841",6377397.155,299.1528128,LENGTHUNIT["metre",1,ID["EPSG",9001]]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]],CS[ellipsoidal,2],AXIS["longitude",east,ORDER[1],ANGLEUNIT["degree",0.0174532925199433,ID["EPSG",9122]]],AXIS["latitude",north,ORDER[2],ANGLEUNIT["degree",0.0174532925199433,ID["EPSG",9122]]]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]],ABRIDGEDTRANSFORMATION["Transformation from unknown to WGS84",METHOD["Position Vector transformation (geog2D domain)",ID["EPSG",9606]],PARAMETER["X-axis translation",565.4171,ID["EPSG",8605]],PARAMETER["Y-axis translation",50.3319,ID["EPSG",8606]],PARAMETER["Z-axis translation",465.5524,ID["EPSG",8607]],PARAMETER["X-axis rotation",0.398957,ID["EPSG",8608]],PARAMETER["Y-axis rotation",-0.343988,ID["EPSG",8609]],PARAMETER["Z-axis rotation",1.8774,ID["EPSG",8610]],PARAMETER["Scale difference",1.0000040725,ID["EPSG",8611]]]]"#;
    const BESSEL_BOUND_PROJJSON: &str = r#"{"$schema":"https://proj.org/schemas/v0.7/projjson.schema.json","type":"BoundCRS","source_crs":{"type":"GeographicCRS","name":"unknown","datum":{"type":"GeodeticReferenceFrame","name":"Unknown based on Bessel 1841 ellipsoid using towgs84=565.4171,50.3319,465.5524,0.398957,-0.343988,1.87740,4.0725","ellipsoid":{"name":"Bessel 1841","semi_major_axis":6377397.155,"inverse_flattening":299.1528128}},"coordinate_system":{"subtype":"ellipsoidal","axis":[{"name":"Longitude","abbreviation":"lon","direction":"east","unit":"degree"},{"name":"Latitude","abbreviation":"lat","direction":"north","unit":"degree"}]}},"target_crs":{"type":"GeographicCRS","name":"WGS 84","datum":{"type":"GeodeticReferenceFrame","name":"World Geodetic System 1984","ellipsoid":{"name":"WGS 84","semi_major_axis":6378137,"inverse_flattening":298.257223563}},"coordinate_system":{"subtype":"ellipsoidal","axis":[{"name":"Latitude","abbreviation":"lat","direction":"north","unit":"degree"},{"name":"Longitude","abbreviation":"lon","direction":"east","unit":"degree"}]},"id":{"authority":"EPSG","code":4326}},"transformation":{"name":"Transformation from unknown to WGS84","method":{"name":"Position Vector transformation (geog2D domain)","id":{"authority":"EPSG","code":9606}},"parameters":[{"name":"X-axis translation","value":565.4171,"unit":"metre","id":{"authority":"EPSG","code":8605}},{"name":"Y-axis translation","value":50.3319,"unit":"metre","id":{"authority":"EPSG","code":8606}},{"name":"Z-axis translation","value":465.5524,"unit":"metre","id":{"authority":"EPSG","code":8607}},{"name":"X-axis rotation","value":0.398957,"unit":{"type":"AngularUnit","name":"arc-second","conversion_factor":4.84813681109536e-06},"id":{"authority":"EPSG","code":8608}},{"name":"Y-axis rotation","value":-0.343988,"unit":{"type":"AngularUnit","name":"arc-second","conversion_factor":4.84813681109536e-06},"id":{"authority":"EPSG","code":8609}},{"name":"Z-axis rotation","value":1.8774,"unit":{"type":"AngularUnit","name":"arc-second","conversion_factor":4.84813681109536e-06},"id":{"authority":"EPSG","code":8610}},{"name":"Scale difference","value":4.0725,"unit":{"type":"ScaleUnit","name":"parts per million","conversion_factor":1e-06},"id":{"authority":"EPSG","code":8611}}]}}"#;

    fn assert_close(actual: (f64, f64), expected: (f64, f64), label: &str) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-9 && (actual.1 - expected.1).abs() < 1e-9,
            "{label}"
        );
    }

    #[test]
    fn declared_towgs84_shift_matches_c_proj() {
        // Reference values from C PROJ 9.6 `proj_create_crs_to_crs`.
        let cases = [
            (
                "+proj=longlat +ellps=bessel +towgs84=565.4,50.3,465.6,0,0,0,0 +no_defs".to_string(),
                (5.0, 52.0),
                (5.000012095836775, 51.999136506957),
            ),
            (
                format!("+proj=sterea +lat_0=52.15616055555555 +lon_0=5.38763888888889 +k=0.9999079 +x_0=155000 +y_0=463000 +ellps=bessel +towgs84={RD_TOWGS84} +units=m +no_defs"),
                (155_000.0, 463_000.0),
                (5.3879872038843555, 52.15538300084578),
            ),
        ];

        for (definition, input, expected) in cases {
            let forward = transform_from_crs_strings(&definition, "EPSG:4326").unwrap();
            let output = forward.convert(input).unwrap();
            assert_close(output, expected, &definition);

            // Sub-millimetre in the projected case; a 2D Helmert roundtrip
            // does not close exactly because height is held at zero each way.
            let back = forward.inverse().unwrap().convert(output).unwrap();
            assert!(
                (back.0 - input.0).abs() < 1e-3 && (back.1 - input.1).abs() < 1e-3,
                "{definition}"
            );
            let reversed = transform_from_crs_strings("EPSG:4326", &definition).unwrap();
            let back = reversed.convert(output).unwrap();
            assert!((back.0 - input.0).abs() < 1e-3, "{definition}");
        }
    }

    #[test]
    fn every_towgs84_spelling_declares_the_same_shift() {
        let proj_string = format!("+proj=longlat +ellps=bessel +towgs84={RD_TOWGS84} +no_defs");
        let expected = transform_from_crs_strings(&proj_string, "EPSG:4326")
            .unwrap()
            .convert((5.0, 52.0))
            .unwrap();

        for definition in [
            BESSEL_TOWGS84_WKT1,
            BESSEL_BOUND_WKT2,
            BESSEL_BOUND_PROJJSON,
        ] {
            let transform = transform_from_crs_strings(definition, "EPSG:4326")
                .unwrap_or_else(|err| panic!("{definition}: {err}"));
            assert_eq!(
                transform.selected_operation().name,
                "Parsed +towgs84 datum shift"
            );
            assert_close(
                transform.convert((5.0, 52.0)).unwrap(),
                expected,
                definition,
            );
        }
    }

    #[test]
    fn parse_crs_returns_the_source_of_a_bound_crs() {
        for definition in [BESSEL_BOUND_WKT2, BESSEL_BOUND_PROJJSON] {
            let crs = parse_crs(definition).unwrap();
            assert!(crs.is_geographic());
            assert_eq!(crs.epsg(), 0);
        }
    }

    #[test]
    fn declared_shift_needs_the_transform_helpers() {
        // Selection does not derive operations from datum metadata.
        let source = parse_crs("+proj=longlat +ellps=bessel +towgs84=565,50,465").unwrap();
        let target = parse_crs("EPSG:4326").unwrap();
        assert!(Transform::from_crs_defs(&source, &target).is_err());
    }

    #[test]
    fn null_towgs84_compiles_without_a_shift() {
        let transform =
            transform_from_crs_strings("+proj=longlat +ellps=GRS80 +towgs84=0,0,0", "EPSG:4326")
                .unwrap();
        assert_eq!(transform.convert((5.0, 52.0)).unwrap(), (5.0, 52.0));

        let identity =
            transform_from_crs_strings("+proj=longlat +ellps=WGS84 +towgs84=0,0,0", "EPSG:4326")
                .unwrap();
        assert_eq!(identity.selected_operation().name, "Identity");
    }

    #[test]
    fn identical_declared_shifts_cancel() {
        let definition = format!("+proj=longlat +ellps=bessel +towgs84={RD_TOWGS84}");
        let transform = transform_from_crs_strings(&definition, &definition).unwrap();
        assert_eq!(transform.selected_operation().name, "Identity");
    }

    #[test]
    fn epsg_tagged_wkt_keeps_the_registry_operation() {
        // GDAL's classic EPSG:27700 tags a TOWGS84 onto the authority definition.
        let wkt = r#"PROJCS["OSGB36 / British National Grid",GEOGCS["OSGB36",DATUM["Ordnance_Survey_of_Great_Britain_1936",SPHEROID["Airy 1830",6377563.396,299.3249646],TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489],AUTHORITY["EPSG","6277"]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4277"]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",49],PARAMETER["central_meridian",-2],PARAMETER["scale_factor",0.9996012717],PARAMETER["false_easting",400000],PARAMETER["false_northing",-100000],UNIT["metre",1],AXIS["Easting",EAST],AXIS["Northing",NORTH],AUTHORITY["EPSG","27700"]]"#;
        let tagged = transform_from_crs_strings(wkt, "EPSG:4326").unwrap();
        let registry = Transform::new("EPSG:27700", "EPSG:4326").unwrap();
        assert_eq!(
            tagged.selected_operation().id,
            registry.selected_operation().id
        );
    }

    #[test]
    fn unsupported_declared_shifts_are_rejected() {
        let malformed = BESSEL_TOWGS84_WKT1.replace(
            "TOWGS84[565.4171,50.3319,465.5524,0.398957,-0.343988,1.8774,4.0725]",
            "TOWGS84[1,2]",
        );
        let err = transform_from_crs_strings(&malformed, "EPSG:4326")
            .err()
            .unwrap();
        assert!(err.to_string().contains("TOWGS84 requires 3 or 7"), "{err}");

        let other_hub = BESSEL_BOUND_WKT2.replace(r#"ID["EPSG",4326]"#, r#"ID["EPSG",4258]"#);
        let err = parse_crs(&other_hub).unwrap_err();
        assert!(err.to_string().contains("EPSG:4326 target"), "{err}");

        let other_method = BESSEL_BOUND_WKT2.replace(
            "Position Vector transformation (geog2D domain)",
            "Ballpark geographic offset",
        );
        let err = parse_crs(&other_method).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported transformation method"),
            "{err}"
        );

        let other_unit = BESSEL_BOUND_PROJJSON.replace("4.84813681109536e-06", "1.0");
        let err = parse_crs(&other_unit).unwrap_err();
        assert!(err.to_string().contains("unsupported unit"), "{err}");
    }

    #[test]
    fn coordinate_frame_bound_crs_negates_rotations() {
        let coordinate_frame = BESSEL_BOUND_WKT2
            .replace(
                "Position Vector transformation (geog2D domain)",
                "Coordinate Frame rotation (geog2D domain)",
            )
            .replace("0.398957", "-0.398957")
            .replace("-0.343988", "0.343988")
            .replace("1.8774", "-1.8774");
        let negated = transform_from_crs_strings(&coordinate_frame, "EPSG:4326")
            .unwrap()
            .convert((5.0, 52.0))
            .unwrap();
        let position_vector = transform_from_crs_strings(BESSEL_BOUND_WKT2, "EPSG:4326")
            .unwrap()
            .convert((5.0, 52.0))
            .unwrap();
        assert_close(negated, position_vector, "coordinate frame");
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

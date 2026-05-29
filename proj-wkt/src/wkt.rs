use crate::semantics::{
    approx_eq, linear_unit_from_meters_per_unit, normalize_key, projection_parameter_unit_kind,
    radians_to_degrees_factor, resolve_structured_datum,
    validate_supported_geographic_or_ellipsoidal_height_semantics,
    validate_supported_geographic_semantics, validate_supported_projected_semantics,
    validate_supported_vertical_coordinate_system, validate_vertical_unit_matches_authority,
    AxisDirection, CoordinateSystemSpec, DatumAliasScope, GeographicCoordinateSystemKind,
    ProjectionParameterUnitKind, StructuredEllipsoid,
};
use crate::{ParseError, Result};
use proj_core::{
    CompoundCrsDef, CrsDef, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    ProjectionMethod, VerticalCrsDef,
};
use std::collections::HashMap;

/// Parse a WKT CRS string.
///
/// Strategy:
/// 1. Extract a top-level AUTHORITY["EPSG","XXXX"] or ID["EPSG",XXXX] if present
///    → look up in registry
/// 2. Otherwise, extract projection parameters from the WKT structure
pub(crate) fn parse_wkt(s: &str) -> Result<CrsDef> {
    let s = s.trim();
    let top_level_epsg = extract_top_level_epsg(s);
    let parsed = parse_wkt_structure(s)?;

    if let Some(epsg) = top_level_epsg {
        return canonicalize_authoritative_crs(parsed, epsg, "WKT");
    }

    Ok(parsed)
}

/// Extract a top-level EPSG code from AUTHORITY[...] or ID[...].
fn extract_top_level_epsg(s: &str) -> Option<u32> {
    let root_start = s.find('[')?;
    let mut depth = 1usize;
    let mut i = root_start + 1;
    let bytes = s.as_bytes();
    let mut in_string = false;

    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                } else {
                    in_string = !in_string;
                    i += 1;
                }
            }
            _ if in_string => i += 1,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
            }
            _ if depth == 1 => {
                if let Some((name, inner, next)) = parse_wkt_element(s, i) {
                    if name.eq_ignore_ascii_case("AUTHORITY") || name.eq_ignore_ascii_case("ID") {
                        if let Some(epsg) = parse_epsg_element(inner) {
                            return Some(epsg);
                        }
                    }
                    i = next;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    None
}

fn parse_wkt_element(s: &str, start: usize) -> Option<(&str, &str, usize)> {
    let bytes = s.as_bytes();
    let first = *bytes.get(start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }

    let mut name_end = start + 1;
    while let Some(byte) = bytes.get(name_end) {
        if byte.is_ascii_alphanumeric() || *byte == b'_' {
            name_end += 1;
        } else {
            break;
        }
    }

    let mut bracket_start = name_end;
    while let Some(byte) = bytes.get(bracket_start) {
        if byte.is_ascii_whitespace() {
            bracket_start += 1;
        } else {
            break;
        }
    }
    if bytes.get(bracket_start) != Some(&b'[') {
        return None;
    }

    let mut depth = 1usize;
    let mut i = bracket_start + 1;
    let mut in_string = false;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                } else {
                    in_string = !in_string;
                    i += 1;
                }
            }
            _ if in_string => i += 1,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    let name = &s[start..name_end];
                    let inner = &s[bracket_start + 1..i - 1];
                    return Some((name, inner, i));
                }
            }
            _ => i += 1,
        }
    }

    None
}

fn parse_epsg_element(inner: &str) -> Option<u32> {
    let (authority, code) = first_two_fields(inner)?;
    if !trim_wkt_token(authority).eq_ignore_ascii_case("EPSG") {
        return None;
    }
    trim_wkt_token(code).parse().ok()
}

fn first_two_fields(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut in_string = false;
    let bytes = s.as_bytes();

    for (idx, byte) in bytes.iter().enumerate() {
        match *byte {
            b'"' => {
                if in_string && bytes.get(idx + 1) == Some(&b'"') {
                    continue;
                }
                in_string = !in_string;
            }
            _ if in_string => {}
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                return Some((&s[..idx], &s[idx + 1..]));
            }
            _ => {}
        }
    }

    None
}

fn trim_wkt_token(token: &str) -> &str {
    token.trim().trim_matches('"')
}

/// Attempt to parse WKT structure to extract projection parameters.
fn parse_wkt_structure(s: &str) -> Result<CrsDef> {
    let Some((root_name, _, _)) = parse_wkt_element(s, 0) else {
        return Err(ParseError::Parse(format!(
            "unrecognized WKT root element: {:.40}",
            s
        )));
    };

    if root_name.eq_ignore_ascii_case("GEOGCS")
        || root_name.eq_ignore_ascii_case("GEODCRS")
        || root_name.eq_ignore_ascii_case("GEOGCRS")
    {
        return parse_wkt_geographic(s);
    }

    if root_name.eq_ignore_ascii_case("PROJCS") || root_name.eq_ignore_ascii_case("PROJCRS") {
        return parse_wkt_projected(s);
    }

    if root_name.eq_ignore_ascii_case("COMPD_CS") || root_name.eq_ignore_ascii_case("COMPOUNDCRS") {
        return parse_wkt_compound(s);
    }

    if root_name.eq_ignore_ascii_case("VERT_CS")
        || root_name.eq_ignore_ascii_case("VERTCRS")
        || root_name.eq_ignore_ascii_case("VERTICALCRS")
    {
        return Err(ParseError::UnsupportedSemantics(
            "standalone vertical CRS definitions are not supported by the horizontal transform API; use a compound CRS with an identical vertical component on both sides".into(),
        ));
    }

    Err(ParseError::Parse(format!(
        "unrecognized WKT root element: {:.40}",
        s
    )))
}

fn parse_wkt_geographic(s: &str) -> Result<CrsDef> {
    let root_inner = root_inner(s)
        .ok_or_else(|| ParseError::Parse(format!("unrecognized WKT root element: {:.40}", s)))?;
    let coordinate_system = extract_coordinate_system(root_inner);
    let angle_unit_to_degree = extract_geographic_angle_unit_to_degree(
        root_inner,
        "WKT geographic CRS",
        &coordinate_system,
    )?;
    let prime_meridian_degrees =
        extract_prime_meridian_degrees(root_inner, angle_unit_to_degree.unwrap_or(1.0));
    let coordinate_system_kind = validate_supported_geographic_or_ellipsoidal_height_semantics(
        "WKT geographic CRS",
        angle_unit_to_degree,
        prime_meridian_degrees,
        &coordinate_system,
    )?;

    let datum = infer_datum_from_geographic_inner(root_inner)?;
    let horizontal = GeographicCrsDef::new(0, datum.clone(), "");

    match coordinate_system_kind {
        GeographicCoordinateSystemKind::TwoDimensional => Ok(CrsDef::Geographic(horizontal)),
        GeographicCoordinateSystemKind::ThreeDimensionalEllipsoidalHeight => {
            let vertical = VerticalCrsDef::ellipsoidal_height(
                0,
                datum,
                extract_ellipsoidal_height_linear_unit(root_inner)
                    .unwrap_or_else(LinearUnit::metre),
                "",
            );
            Ok(CrsDef::Compound(Box::new(CompoundCrsDef::new(
                0,
                HorizontalCrsDef::Geographic(horizontal),
                vertical,
                "",
            ))))
        }
    }
}

fn parse_wkt_projected(s: &str) -> Result<CrsDef> {
    let root_inner = root_inner(s)
        .ok_or_else(|| ParseError::Parse(format!("unrecognized WKT root element: {:.40}", s)))?;
    // WKT1 uses PROJECTION["name"], WKT2 uses METHOD["name"].
    let proj_name = extract_wkt_value_case_insensitive(s, "PROJECTION")
        .or_else(|| extract_wkt_value_case_insensitive(s, "METHOD"));
    let normalized_method = proj_name.as_deref().map(normalize_key).ok_or_else(|| {
        ParseError::Parse("WKT projected CRS is missing a projection method".into())
    })?;

    let coordinate_system = extract_coordinate_system(root_inner);
    let projected_linear_unit = extract_projected_linear_unit(root_inner, &coordinate_system)?
        .unwrap_or_else(LinearUnit::metre);
    validate_supported_projected_semantics("WKT projected CRS", &coordinate_system)?;

    let base_geographic_inner =
        find_top_level_element_inner(root_inner, &["BASEGEOGCRS", "GEOGCRS", "GEODCRS", "GEOGCS"])
            .ok_or_else(|| {
                ParseError::Parse("WKT projected CRS is missing a base geographic CRS".into())
            })?;
    let base_coordinate_system = extract_coordinate_system(base_geographic_inner);
    let base_angle_unit_to_degree = extract_geographic_angle_unit_to_degree(
        base_geographic_inner,
        "WKT projected base geographic CRS",
        &base_coordinate_system,
    )?
    .unwrap_or(1.0);
    validate_supported_geographic_semantics(
        "WKT projected base geographic CRS",
        Some(base_angle_unit_to_degree),
        extract_prime_meridian_degrees(base_geographic_inner, base_angle_unit_to_degree),
        &base_coordinate_system,
    )?;
    let params = parse_wkt_parameters(s, projected_linear_unit, base_angle_unit_to_degree)?;

    // Extract common parameters
    let lon0 = first_param(
        &params,
        &[
            "centralmeridian",
            "longitudeofcenter",
            "longitudeofcentre",
            "longitudeofprojectioncenter",
            "longitudeofprojectioncentre",
            "longitudeofnaturalorigin",
            "longitudeoffalseorigin",
        ],
    )
    .unwrap_or(0.0);
    let lat0 = first_param(
        &params,
        &[
            "latitudeoforigin",
            "latitudeofcenter",
            "latitudeofcentre",
            "latitudeofprojectioncenter",
            "latitudeofprojectioncentre",
            "latitudeofnaturalorigin",
            "latitudeoffalseorigin",
        ],
    )
    .unwrap_or(0.0);
    let k0 = first_param(
        &params,
        &[
            "scalefactor",
            "scalefactoratnaturalorigin",
            "scalefactoratprojectionorigin",
            "scalefactoratprojectioncenter",
            "scalefactoratprojectioncentre",
        ],
    )
    .unwrap_or(1.0);
    let fe = first_param(
        &params,
        &[
            "falseeasting",
            "eastingatprojectioncenter",
            "eastingatprojectioncentre",
        ],
    )
    .unwrap_or(0.0);
    let fn_ = first_param(
        &params,
        &[
            "falsenorthing",
            "northingatprojectioncenter",
            "northingatprojectioncentre",
        ],
    )
    .unwrap_or(0.0);

    let datum = infer_datum_from_geographic_inner(base_geographic_inner)?;

    let method = match normalized_method.as_str() {
        "transversemercator" => ProjectionMethod::TransverseMercator {
            lon0,
            lat0,
            k0,
            false_easting: fe,
            false_northing: fn_,
        },
        name if name.starts_with("mercator") => ProjectionMethod::Mercator {
            lon0,
            lat_ts: first_param(
                &params,
                &[
                    "standardparallel1",
                    "latitudeof1ststandardparallel",
                    "latitudeofstandardparallel",
                ],
            )
            .unwrap_or(0.0),
            k0,
            false_easting: fe,
            false_northing: fn_,
        },
        "lambertconformalconic1sp" | "lambertconformalconic2sp" | "lambertconformalconic" => {
            ProjectionMethod::LambertConformalConic {
                lon0,
                lat0,
                lat1: first_param(
                    &params,
                    &["standardparallel1", "latitudeof1ststandardparallel"],
                )
                .unwrap_or(lat0),
                lat2: first_param(
                    &params,
                    &["standardparallel2", "latitudeof2ndstandardparallel"],
                )
                .unwrap_or(lat0),
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "albersequalareaconic" | "albersequalarea" => ProjectionMethod::AlbersEqualArea {
            lon0,
            lat0,
            lat1: first_param(
                &params,
                &["standardparallel1", "latitudeof1ststandardparallel"],
            )
            .unwrap_or(lat0),
            lat2: first_param(
                &params,
                &["standardparallel2", "latitudeof2ndstandardparallel"],
            )
            .unwrap_or(lat0),
            false_easting: fe,
            false_northing: fn_,
        },
        "lambertazimuthalequalarea" | "lambertazimuthalequalareaellipsoidal" => {
            ProjectionMethod::LambertAzimuthalEqualArea {
                lon0,
                lat0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "lambertazimuthalequalareaspherical" => {
            ProjectionMethod::LambertAzimuthalEqualAreaSpherical {
                lon0,
                lat0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "obliquestereographic" | "doublestereographic" | "stereographic" => {
            ProjectionMethod::ObliqueStereographic {
                lon0,
                lat0,
                k0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "hotineobliquemercator"
        | "hotineobliquemercatorvarianta"
        | "hotineobliquemercatorvariantb"
        | "obliquemercator"
        | "rectifiedskeworthomorphic" => {
            let variant_b = normalized_method == "hotineobliquemercatorvariantb"
                || params.contains_key("eastingatprojectioncenter")
                || params.contains_key("eastingatprojectioncentre")
                || params.contains_key("northingatprojectioncenter")
                || params.contains_key("northingatprojectioncentre");
            let azimuth = first_param(
                &params,
                &[
                    "azimuth",
                    "azimuthinitialline",
                    "azimuthofinitialline",
                    "azimuthatprojectioncenter",
                    "azimuthatprojectioncentre",
                ],
            )
            .unwrap_or(0.0);
            let rectified_grid_angle = first_param(
                &params,
                &["rectifiedgridangle", "anglefromrectifiedtoskewgrid"],
            )
            .unwrap_or(azimuth);
            ProjectionMethod::HotineObliqueMercator {
                latc: lat0,
                lonc: lon0,
                azimuth,
                rectified_grid_angle,
                k0,
                false_easting: fe,
                false_northing: fn_,
                variant_b,
            }
        }
        "cassinisoldner" | "cassini" => ProjectionMethod::CassiniSoldner {
            lon0,
            lat0,
            false_easting: fe,
            false_northing: fn_,
        },
        "polarstereographicvarianta" | "polarstereographicvariantb" | "polarstereographic" => {
            ProjectionMethod::PolarStereographic {
                lon0,
                lat_ts: first_param(
                    &params,
                    &[
                        "standardparallel",
                        "latitudeofstandardparallel",
                        "latitudeof1ststandardparallel",
                    ],
                )
                .unwrap_or(lat0),
                k0,
                false_easting: fe,
                false_northing: fn_,
            }
        }
        "equidistantcylindrical" | "platecarree" => ProjectionMethod::EquidistantCylindrical {
            lon0,
            lat_ts: first_param(
                &params,
                &[
                    "standardparallel1",
                    "latitudeof1ststandardparallel",
                    "latitudeofstandardparallel",
                ],
            )
            .unwrap_or(0.0),
            false_easting: fe,
            false_northing: fn_,
        },
        _ => {
            return Err(ParseError::Parse(format!(
                "unsupported WKT projection: {}",
                proj_name.as_deref().unwrap_or("(none)")
            )));
        }
    };

    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        datum,
        method,
        projected_linear_unit,
        "",
    )))
}

fn parse_wkt_compound(s: &str) -> Result<CrsDef> {
    let root_inner = root_inner(s)
        .ok_or_else(|| ParseError::Parse(format!("unrecognized WKT root element: {:.40}", s)))?;
    let mut horizontal = None;
    let mut vertical = None;

    for_each_top_level_element(root_inner, |name, element_inner| {
        if horizontal.is_none()
            && (name.eq_ignore_ascii_case("GEOGCS")
                || name.eq_ignore_ascii_case("GEODCRS")
                || name.eq_ignore_ascii_case("GEOGCRS")
                || name.eq_ignore_ascii_case("PROJCS")
                || name.eq_ignore_ascii_case("PROJCRS"))
        {
            horizontal = Some(parse_wkt_structure(&format!("{name}[{element_inner}]")));
            return;
        }

        if vertical.is_none()
            && (name.eq_ignore_ascii_case("VERT_CS")
                || name.eq_ignore_ascii_case("VERTCRS")
                || name.eq_ignore_ascii_case("VERTICALCRS"))
        {
            vertical = Some(parse_wkt_vertical(element_inner));
        }
    });

    let horizontal = horizontal.ok_or_else(|| {
        ParseError::Parse("WKT compound CRS is missing a horizontal CRS".into())
    })??;
    if horizontal.vertical_crs().is_some() {
        return Err(ParseError::UnsupportedSemantics(
            "WKT compound CRS cannot use a 3D horizontal CRS as its horizontal component".into(),
        ));
    }

    let vertical = vertical
        .ok_or_else(|| ParseError::Parse("WKT compound CRS is missing a vertical CRS".into()))??;
    let compound = CompoundCrsDef::from_crs_def(0, horizontal, vertical, "")?;
    Ok(CrsDef::Compound(Box::new(compound)))
}

fn parse_wkt_vertical(inner: &str) -> Result<VerticalCrsDef> {
    validate_supported_vertical_coordinate_system(
        "WKT vertical CRS",
        &extract_coordinate_system(inner),
    )?;
    let linear_unit = extract_vertical_crs_linear_unit(inner).unwrap_or_else(LinearUnit::metre);
    let epsg = extract_top_level_epsg_from_inner(inner).unwrap_or(0);
    if let Some(canonical) = proj_core::lookup_vertical_epsg(epsg) {
        validate_vertical_unit_matches_authority("WKT vertical CRS", linear_unit, &canonical)?;
        return Ok(canonical);
    }

    let datum_inner =
        find_top_level_element_inner(inner, &["VERT_DATUM", "VERTICALDATUM", "VDATUM", "VRF"])
            .ok_or_else(|| {
                ParseError::Parse("WKT vertical CRS is missing a vertical datum".into())
            })?;

    let vertical_datum_epsg = extract_top_level_epsg_from_inner(datum_inner).ok_or_else(|| {
        ParseError::UnsupportedSemantics(
            "WKT gravity-related vertical CRS requires a vertical datum EPSG identifier".into(),
        )
    })?;

    Ok(VerticalCrsDef::gravity_related_height(
        epsg,
        vertical_datum_epsg,
        linear_unit,
        "",
    )?)
}

fn infer_datum_from_geographic_inner(inner: &str) -> Result<proj_core::Datum> {
    let datum_inner = find_top_level_element_inner(
        inner,
        &["DATUM", "GEODETICDATUM", "DATUMENSEMBLE", "ENSEMBLE"],
    )
    .ok_or_else(|| ParseError::Parse("WKT geographic CRS is missing a datum definition".into()))?;

    if let Some(epsg) = extract_top_level_epsg_from_inner(datum_inner) {
        return proj_core::lookup_datum_epsg(epsg)
            .ok_or_else(|| ParseError::Parse(format!("unsupported WKT datum EPSG:{epsg}")));
    }

    let fields = split_top_level_fields(datum_inner);
    let datum_name = fields
        .first()
        .map(|field| normalize_key(trim_wkt_token(field)))
        .ok_or_else(|| ParseError::Parse("WKT datum is missing a name".into()))?;
    let ellipsoid = parse_structured_ellipsoid(datum_inner)
        .ok_or_else(|| ParseError::Parse("WKT datum is missing a supported ellipsoid".into()))?;

    resolve_structured_datum(DatumAliasScope::Wkt, &datum_name, &ellipsoid)
        .ok_or_else(|| ParseError::Parse("unsupported or unrecognized WKT datum".into()))
}

fn canonicalize_authoritative_crs(parsed: CrsDef, epsg: u32, format: &str) -> Result<CrsDef> {
    let registry = proj_core::lookup_epsg(epsg)
        .ok_or_else(|| ParseError::Parse(format!("unsupported EPSG code in {format}: {epsg}")))?;
    if parsed.semantically_equivalent(&registry) {
        Ok(registry)
    } else {
        Err(ParseError::UnsupportedSemantics(format!(
            "{format} definition tagged as EPSG:{epsg} does not match the embedded EPSG semantics"
        )))
    }
}

fn parse_structured_ellipsoid(inner: &str) -> Option<StructuredEllipsoid> {
    let ellipsoid_inner = find_top_level_element_inner(inner, &["SPHEROID", "ELLIPSOID"])?;
    let fields = split_top_level_fields(ellipsoid_inner);
    let name = normalize_key(trim_wkt_token(fields.first()?));
    let semi_major_axis = fields.get(1)?.trim().parse().ok()?;
    let inverse_flattening = fields.get(2)?.trim().parse().ok()?;
    Some(StructuredEllipsoid {
        epsg: extract_top_level_epsg_from_inner(ellipsoid_inner),
        name,
        semi_major_axis,
        inverse_flattening,
    })
}

fn extract_top_level_epsg_from_inner(inner: &str) -> Option<u32> {
    let mut found = None;
    for_each_top_level_element(inner, |name, element_inner| {
        if found.is_none()
            && (name.eq_ignore_ascii_case("AUTHORITY") || name.eq_ignore_ascii_case("ID"))
        {
            found = parse_epsg_element(element_inner);
        }
    });
    found
}

/// Extract a quoted value like PROJECTION["Transverse_Mercator"].
fn extract_wkt_value_case_insensitive(s: &str, key: &str) -> Option<String> {
    let marker = format!("{key}[\"");
    let pos = find_ascii_case_insensitive(s, &marker)?;
    let start = pos + marker.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_wkt_parameters(
    s: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> Result<HashMap<String, f64>> {
    let mut params = HashMap::new();
    let mut search_start = 0usize;

    while let Some(rel) = find_ascii_case_insensitive(&s[search_start..], "PARAMETER[") {
        let start = search_start + rel;
        if let Some((name, inner, next)) = parse_wkt_element(s, start) {
            if name.eq_ignore_ascii_case("PARAMETER") {
                let (key, value) = parse_parameter_element(
                    inner,
                    projected_linear_unit,
                    base_angle_unit_to_degree,
                )?;
                params.insert(key, value);
                search_start = next;
                continue;
            }
        }
        search_start = start + "PARAMETER[".len();
    }

    Ok(params)
}

fn parse_parameter_element(
    inner: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> Result<(String, f64)> {
    let fields = split_top_level_fields(inner);
    let name = fields
        .first()
        .map(|field| trim_wkt_token(field))
        .unwrap_or("");
    if name.is_empty() {
        return Err(ParseError::Parse(
            "WKT projection parameter is missing a name".into(),
        ));
    }
    if fields.len() < 2 {
        return Err(ParseError::Parse(format!(
            "WKT projection parameter `{name}` is missing a numeric value"
        )));
    }

    let normalized_name = normalize_key(name);
    let value = parse_wkt_projection_parameter_number(name, fields[1].trim())?;
    let unit_kind = projection_parameter_unit_kind(&normalized_name);

    let mut nested_factor = None;
    for field in fields.iter().skip(2) {
        let field = field.trim();
        let Some((unit_name, unit_inner, _)) = parse_wkt_element(field, 0) else {
            continue;
        };
        nested_factor = match unit_name.to_ascii_uppercase().as_str() {
            "ANGLEUNIT" => Some(radians_to_degrees_factor(parse_wkt_parameter_unit_factor(
                name, unit_inner,
            )?)),
            "LENGTHUNIT" | "UNIT" | "SCALEUNIT" => {
                Some(parse_wkt_parameter_unit_factor(name, unit_inner)?)
            }
            _ => None,
        };
        if nested_factor.is_some() {
            break;
        }
    }

    let default_factor = match unit_kind {
        ProjectionParameterUnitKind::Angle => base_angle_unit_to_degree,
        ProjectionParameterUnitKind::Length => projected_linear_unit.meters_per_unit(),
        ProjectionParameterUnitKind::Scale | ProjectionParameterUnitKind::Other => 1.0,
    };

    Ok((
        normalized_name,
        value * nested_factor.unwrap_or(default_factor),
    ))
}

fn parse_wkt_projection_parameter_number(name: &str, raw: &str) -> Result<f64> {
    let value = raw.parse::<f64>().map_err(|_| {
        ParseError::Parse(format!(
            "invalid WKT projection parameter `{name}` value: {raw}"
        ))
    })?;
    if !value.is_finite() {
        return Err(ParseError::Parse(format!(
            "WKT projection parameter `{name}` value must be finite"
        )));
    }
    Ok(value)
}

fn parse_wkt_parameter_unit_factor(parameter_name: &str, inner: &str) -> Result<f64> {
    let factor = parse_unit_factor(inner).ok_or_else(|| {
        ParseError::Parse(format!(
            "invalid WKT projection parameter `{parameter_name}` unit factor"
        ))
    })?;
    if !factor.is_finite() {
        return Err(ParseError::Parse(format!(
            "WKT projection parameter `{parameter_name}` unit factor must be finite"
        )));
    }
    Ok(factor)
}

fn split_top_level_fields(s: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut field_start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                    continue;
                }
                in_string = !in_string;
            }
            _ if in_string => {}
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                fields.push(s[field_start..i].trim());
                field_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }

    fields.push(s[field_start..].trim());
    fields
}

fn root_inner(s: &str) -> Option<&str> {
    parse_wkt_element(s, 0).map(|(_, inner, _)| inner)
}

fn extract_projected_linear_unit(
    inner: &str,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<Option<LinearUnit>> {
    let top_level_linear_unit = extract_top_level_length_unit(inner, true);
    let axis_linear_unit = coordinate_system_linear_unit("WKT projected CRS", coordinate_system)?;
    if let (Some(top_level_linear_unit), Some(axis_linear_unit)) =
        (top_level_linear_unit, axis_linear_unit)
    {
        if !approx_eq(
            top_level_linear_unit.meters_per_unit(),
            axis_linear_unit.meters_per_unit(),
        ) {
            return Err(ParseError::UnsupportedSemantics(
                "WKT projected CRS declares conflicting projected linear units".into(),
            ));
        }
    }
    Ok(top_level_linear_unit.or(axis_linear_unit))
}

fn extract_top_level_angle_unit_to_degree(inner: &str) -> Option<f64> {
    let mut factor = None;
    for_each_top_level_element(inner, |unit_name, unit_inner| {
        if unit_name.eq_ignore_ascii_case("UNIT") || unit_name.eq_ignore_ascii_case("ANGLEUNIT") {
            factor = parse_unit_factor(unit_inner).map(radians_to_degrees_factor);
        }
    });
    factor
}

fn extract_geographic_angle_unit_to_degree(
    inner: &str,
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<Option<f64>> {
    let top_level_angle_unit_to_degree = extract_top_level_angle_unit_to_degree(inner);
    let axis_angle_unit_to_degree =
        coordinate_system_angle_unit_to_degree(context, coordinate_system)?;
    if let (Some(top_level_angle_unit_to_degree), Some(axis_angle_unit_to_degree)) =
        (top_level_angle_unit_to_degree, axis_angle_unit_to_degree)
    {
        if !approx_eq(top_level_angle_unit_to_degree, axis_angle_unit_to_degree) {
            return Err(ParseError::UnsupportedSemantics(format!(
                "{context} declares conflicting angular units"
            )));
        }
    }
    Ok(top_level_angle_unit_to_degree.or(axis_angle_unit_to_degree))
}

fn extract_prime_meridian_degrees(inner: &str, default_angle_unit_to_degree: f64) -> Option<f64> {
    let mut prime_meridian = None;
    for_each_top_level_element(inner, |name, element_inner| {
        if !name.eq_ignore_ascii_case("PRIMEM") {
            return;
        }

        let fields = split_top_level_fields(element_inner);
        let Some(value) = fields
            .get(1)
            .and_then(|field| field.trim().parse::<f64>().ok())
        else {
            return;
        };

        let factor = fields
            .iter()
            .skip(2)
            .find_map(|field| {
                let field = field.trim();
                let (unit_name, unit_inner, _) = parse_wkt_element(field, 0)?;
                if unit_name.eq_ignore_ascii_case("UNIT")
                    || unit_name.eq_ignore_ascii_case("ANGLEUNIT")
                {
                    parse_unit_factor(unit_inner).map(radians_to_degrees_factor)
                } else {
                    None
                }
            })
            .unwrap_or(default_angle_unit_to_degree);

        prime_meridian = Some(value * factor);
    });
    prime_meridian
}

fn extract_coordinate_system(inner: &str) -> CoordinateSystemSpec {
    let mut coordinate_system = CoordinateSystemSpec::default();
    for_each_top_level_element(inner, |name, element_inner| {
        if name.eq_ignore_ascii_case("CS") {
            let fields = split_top_level_fields(element_inner);
            coordinate_system.subtype = fields
                .first()
                .map(|field| trim_wkt_token(field).to_string());
            coordinate_system.dimension = fields.get(1).and_then(|field| field.trim().parse().ok());
            return;
        }

        if name.eq_ignore_ascii_case("AXIS") {
            let axis_direction = parse_axis_direction(element_inner);
            coordinate_system.axes.push(axis_direction);
            coordinate_system
                .axis_linear_units
                .push(axis_linear_unit(element_inner));
            coordinate_system
                .axis_angle_unit_to_degree
                .push(axis_angle_unit_to_degree(element_inner));
        }
    });
    coordinate_system
}

fn coordinate_system_linear_unit(
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<Option<LinearUnit>> {
    let mut linear_unit: Option<LinearUnit> = None;
    for axis_unit in coordinate_system
        .axis_linear_units
        .iter()
        .flatten()
        .copied()
    {
        if let Some(existing_linear_unit) = linear_unit {
            if !approx_eq(
                existing_linear_unit.meters_per_unit(),
                axis_unit.meters_per_unit(),
            ) {
                return Err(ParseError::UnsupportedSemantics(format!(
                    "{context} uses inconsistent projected axis units"
                )));
            }
        } else {
            linear_unit = Some(axis_unit);
        }
    }

    Ok(linear_unit)
}

fn coordinate_system_angle_unit_to_degree(
    context: &str,
    coordinate_system: &CoordinateSystemSpec,
) -> Result<Option<f64>> {
    let mut angle_unit_to_degree = None;
    for axis_angle_unit_to_degree in coordinate_system
        .axis_angle_unit_to_degree
        .iter()
        .flatten()
        .copied()
    {
        if let Some(existing_angle_unit_to_degree) = angle_unit_to_degree {
            if !approx_eq(existing_angle_unit_to_degree, axis_angle_unit_to_degree) {
                return Err(ParseError::UnsupportedSemantics(format!(
                    "{context} uses inconsistent angular axis units"
                )));
            }
        } else {
            angle_unit_to_degree = Some(axis_angle_unit_to_degree);
        }
    }

    Ok(angle_unit_to_degree)
}

fn parse_axis_direction(inner: &str) -> AxisDirection {
    split_top_level_fields(inner)
        .get(1)
        .map(|field| AxisDirection::from_str(trim_wkt_token(field)))
        .unwrap_or(AxisDirection::Other)
}

fn extract_vertical_axis_linear_unit(inner: &str) -> Option<LinearUnit> {
    let mut linear_unit = None;
    for_each_top_level_element(inner, |name, element_inner| {
        if linear_unit.is_some() || !name.eq_ignore_ascii_case("AXIS") {
            return;
        }
        if parse_axis_direction(element_inner) != AxisDirection::Up {
            return;
        }
        linear_unit = axis_linear_unit(element_inner);
    });
    linear_unit
}

fn extract_ellipsoidal_height_linear_unit(inner: &str) -> Option<LinearUnit> {
    extract_vertical_axis_linear_unit(inner).or_else(|| extract_top_level_length_unit(inner, false))
}

fn extract_vertical_crs_linear_unit(inner: &str) -> Option<LinearUnit> {
    extract_vertical_axis_linear_unit(inner).or_else(|| extract_top_level_length_unit(inner, true))
}

fn extract_top_level_length_unit(inner: &str, include_legacy_unit: bool) -> Option<LinearUnit> {
    let mut linear_unit = None;
    for_each_top_level_element(inner, |name, element_inner| {
        if linear_unit.is_some() {
            return;
        }
        if name.eq_ignore_ascii_case("LENGTHUNIT")
            || (include_legacy_unit && name.eq_ignore_ascii_case("UNIT"))
        {
            linear_unit =
                parse_unit_factor(element_inner).and_then(linear_unit_from_meters_per_unit);
        }
    });
    linear_unit
}

fn axis_linear_unit(axis_inner: &str) -> Option<LinearUnit> {
    split_top_level_fields(axis_inner)
        .iter()
        .skip(2)
        .find_map(|field| {
            let (unit_name, unit_inner, _) = parse_wkt_element(field.trim(), 0)?;
            if unit_name.eq_ignore_ascii_case("LENGTHUNIT")
                || unit_name.eq_ignore_ascii_case("UNIT")
            {
                parse_unit_factor(unit_inner).and_then(linear_unit_from_meters_per_unit)
            } else {
                None
            }
        })
}

fn axis_angle_unit_to_degree(axis_inner: &str) -> Option<f64> {
    split_top_level_fields(axis_inner)
        .iter()
        .skip(2)
        .find_map(|field| {
            let (unit_name, unit_inner, _) = parse_wkt_element(field.trim(), 0)?;
            if unit_name.eq_ignore_ascii_case("ANGLEUNIT") || unit_name.eq_ignore_ascii_case("UNIT")
            {
                parse_unit_factor(unit_inner).map(radians_to_degrees_factor)
            } else {
                None
            }
        })
}

fn find_top_level_element_inner<'a>(inner: &'a str, names: &[&str]) -> Option<&'a str> {
    let mut found = None;
    for_each_top_level_element(inner, |name, element_inner| {
        if found.is_none()
            && names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
        {
            found = Some(element_inner);
        }
    });
    found
}

fn for_each_top_level_element<'a, F>(inner: &'a str, mut f: F)
where
    F: FnMut(&'a str, &'a str),
{
    let mut i = 0usize;
    let bytes = inner.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if in_string && bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                    continue;
                }
                in_string = !in_string;
                i += 1;
            }
            _ if in_string => i += 1,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ if depth == 0 => {
                if let Some((name, element_inner, next)) = parse_wkt_element(inner, i) {
                    f(name, element_inner);
                    i = next;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

fn parse_unit_factor(inner: &str) -> Option<f64> {
    let fields = split_top_level_fields(inner);
    fields.get(1)?.trim().parse::<f64>().ok()
}

fn first_param(params: &HashMap<String, f64>, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| params.get(&normalize_key(name)).copied())
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    haystack.char_indices().find_map(|(idx, _)| {
        haystack
            .get(idx..idx + needle.len())
            .filter(|slice| slice.eq_ignore_ascii_case(needle))
            .map(|_| idx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

    #[test]
    fn extract_top_level_epsg_from_wkt() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(extract_top_level_epsg(wkt), Some(4326));
    }

    #[test]
    fn parse_wkt_geogcs_with_authority() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn parse_wkt_projcs_utm() {
        let wkt = r#"PROJCS["WGS 84 / UTM zone 18N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],AUTHORITY["EPSG","32618"]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 32618);
    }

    #[test]
    fn parse_wkt_without_authority() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
    }

    #[test]
    fn parse_wkt2_geographic_3d_as_compound_ellipsoidal_height() {
        let wkt = r#"GEODCRS["WGS 84 3D",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,3],AXIS["longitude",east,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["latitude",north,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],AXIS["ellipsoidal height",up,ORDER[3],LENGTHUNIT["metre",1]]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert!(crs.vertical_crs().is_some());
    }

    #[test]
    fn parse_wkt_compound_with_vertical_crs() {
        let wkt = r#"COMPOUNDCRS["WGS 84 + NAVD88 height",GEODCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433]],VERTCRS["NAVD88 height",VDATUM["North American Vertical Datum 1988",ID["EPSG",5103]],CS[vertical,1],AXIS["gravity-related height",up,LENGTHUNIT["metre",1]],LENGTHUNIT["metre",1]]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert_eq!(
            crs.vertical_crs().unwrap().linear_unit_to_meter(),
            LinearUnit::metre().meters_per_unit()
        );
    }

    #[test]
    fn parse_wkt_vertical_crs_canonicalized_from_crs_epsg() {
        let wkt = r#"COMPOUNDCRS["WGS 84 + NAVD88 height",GEODCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433]],VERTCRS["NAVD88 height",VDATUM["North American Vertical Datum 1988"],CS[vertical,1],AXIS["gravity-related height",up,LENGTHUNIT["metre",1]],LENGTHUNIT["metre",1],ID["EPSG",5703]]]"#;
        let crs = parse_wkt(wkt).unwrap();
        let vertical = crs.vertical_crs().unwrap();
        assert_eq!(vertical.epsg(), 5703);
        assert_eq!(vertical.vertical_datum_epsg(), Some(5103));
    }

    #[test]
    fn reject_standalone_vertical_wkt() {
        let wkt = r#"VERTCRS["NAVD88 height",VDATUM["North American Vertical Datum 1988",ID["EPSG",5103]],CS[vertical,1],AXIS["gravity-related height",up,LENGTHUNIT["metre",1]],LENGTHUNIT["metre",1]]"#;
        let err = parse_wkt(wkt).unwrap_err();
        assert!(err.to_string().contains("standalone vertical CRS"));
    }

    #[test]
    fn reject_geographic_wkt_with_non_degree_unit() {
        let err = parse_wkt(
            r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],UNIT["radian",1]]"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("angular units other than degrees"));
    }

    #[test]
    fn reject_geographic_wkt_with_reversed_axes() {
        let err = parse_wkt(
            r#"GEOGCRS["Custom",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["latitude",north],AXIS["longitude",east],ANGLEUNIT["degree",0.0174532925199433]]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported axis order/directions"));
    }

    #[test]
    fn parse_wkt_projcs_no_authority() {
        let wkt = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn reject_projected_wkt_with_invalid_parameter_value() {
        let wkt = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian","not-a-number"],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0]]"#;
        let err = parse_wkt(wkt).unwrap_err();
        assert!(err
            .to_string()
            .contains("invalid WKT projection parameter `central_meridian` value"));
    }

    #[test]
    fn reject_projected_wkt_with_missing_parameter_value() {
        let wkt = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian"],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0]]"#;
        let err = parse_wkt(wkt).unwrap_err();
        assert!(err
            .to_string()
            .contains("WKT projection parameter `central_meridian` is missing a numeric value"));
    }

    #[test]
    fn parse_wkt_projcs_ignores_nested_base_authority() {
        let wkt = r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],AUTHORITY["EPSG","4326"]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 0);
    }

    #[test]
    fn reject_unknown_geographic_datum() {
        let err =
            parse_wkt(r#"GEOGCS["Unknown",DATUM["Custom",SPHEROID["Custom",1,1]]]"#).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported or unrecognized WKT datum"));
    }

    #[test]
    fn parse_wkt2_projected_without_authority() {
        let wkt = r#"PROJCRS["WGS 84 / UTM zone 18N",BASEGEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]]],CONVERSION["UTM zone 18N",METHOD["Transverse Mercator"],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",-75,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1]],PARAMETER["False northing",0,LENGTHUNIT["metre",1]]],CS[Cartesian,2],AXIS["easting",east],AXIS["northing",north],LENGTHUNIT["metre",1]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_projected());
    }

    #[test]
    fn parse_wkt2_projected_with_axis_only_foot_units() {
        let meter_wkt = r#"PROJCRS["WGS 84 / UTM zone 18N metre",BASEGEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]]],CONVERSION["UTM zone 18N",METHOD["Transverse Mercator"],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",-75,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1]],PARAMETER["False easting",500000],PARAMETER["False northing",0]],CS[Cartesian,2],AXIS["easting",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["northing",north,ORDER[2],LENGTHUNIT["metre",1]]]"#;
        let foot_wkt = r#"PROJCRS["WGS 84 / UTM zone 18N ftUS",BASEGEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]]],CONVERSION["UTM zone 18N",METHOD["Transverse Mercator"],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",-75,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1]],PARAMETER["False easting",1640416.6666666667],PARAMETER["False northing",0]],CS[Cartesian,2],AXIS["easting",east,ORDER[1],LENGTHUNIT["US survey foot",0.3048006096012192]],AXIS["northing",north,ORDER[2],LENGTHUNIT["US survey foot",0.3048006096012192]]]"#;

        let meter_crs = parse_wkt(meter_wkt).unwrap();
        let foot_crs = parse_wkt(foot_wkt).unwrap();
        let from = proj_core::lookup_epsg(4326).unwrap();

        let meter_tx = proj_core::Transform::from_crs_defs(&from, &meter_crs).unwrap();
        let foot_tx = proj_core::Transform::from_crs_defs(&from, &foot_crs).unwrap();

        let (mx, my) = meter_tx.convert((-74.006, 40.7128)).unwrap();
        let (fx, fy) = foot_tx.convert((-74.006, 40.7128)).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02, "x mismatch");
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02, "y mismatch");
    }

    #[test]
    fn reject_wkt2_geographic_with_axis_only_grad_units() {
        let err = parse_wkt(
            r#"GEODCRS["Custom grad",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east,ORDER[1],ANGLEUNIT["grad",0.015707963267949]],AXIS["latitude",north,ORDER[2],ANGLEUNIT["grad",0.015707963267949]]]"#,
        )
        .unwrap_err();

        assert!(matches!(&err, ParseError::UnsupportedSemantics(_)));
        assert!(err.to_string().contains("angular units other than degrees"));
    }

    #[test]
    fn reject_wkt2_projected_with_inconsistent_axis_units() {
        let err = parse_wkt(
            r#"PROJCRS["Custom inconsistent axes",BASEGEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]]],CONVERSION["UTM zone 18N",METHOD["Transverse Mercator"],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",-75,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1]],PARAMETER["False easting",500000],PARAMETER["False northing",0]],CS[Cartesian,2],AXIS["easting",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["northing",north,ORDER[2],LENGTHUNIT["US survey foot",0.3048006096012192]]]"#,
        )
        .unwrap_err();

        assert!(matches!(&err, ParseError::UnsupportedSemantics(_)));
        assert!(err
            .to_string()
            .contains("inconsistent projected axis units"));
    }

    #[test]
    fn parse_wkt_projcs_with_foot_units() {
        let meter_wkt = r#"PROJCS["UTM 18N metre",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
        let foot_wkt = r#"PROJCS["UTM 18N ftUS",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",1640416.6666666667],PARAMETER["false_northing",0],UNIT["Foot_US",0.3048006096012192]]"#;

        let meter_crs = parse_wkt(meter_wkt).unwrap();
        let foot_crs = parse_wkt(foot_wkt).unwrap();
        let from = proj_core::lookup_epsg(4326).unwrap();

        let meter_tx = proj_core::Transform::from_crs_defs(&from, &meter_crs).unwrap();
        let foot_tx = proj_core::Transform::from_crs_defs(&from, &foot_crs).unwrap();

        let (mx, my) = meter_tx.convert((-74.006, 40.7128)).unwrap();
        let (fx, fy) = foot_tx.convert((-74.006, 40.7128)).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02, "x mismatch");
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02, "y mismatch");
    }

    #[test]
    fn reject_projected_wkt_with_non_greenwich_base_prime_meridian() {
        let err = parse_wkt(
            r#"PROJCS["custom",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Paris",2.33722917],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1]]"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("non-Greenwich prime meridian"));
    }

    #[test]
    fn reject_projected_wkt_with_reversed_projected_axes() {
        let err = parse_wkt(
            r#"PROJCRS["Custom",BASEGEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433]],CONVERSION["UTM zone 18N",METHOD["Transverse Mercator"],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",-75,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1]],PARAMETER["False northing",0,LENGTHUNIT["metre",1]]],CS[Cartesian,2],AXIS["northing",north],AXIS["easting",east],LENGTHUNIT["metre",1]]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported axis order/directions"));
    }

    #[test]
    fn parse_wkt2_geographic_with_id() {
        let wkt = r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]"#;
        let crs = parse_wkt(wkt).unwrap();
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn reject_wkt_with_top_level_epsg_mismatch() {
        let err = parse_wkt(
            r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4269]]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("does not match the embedded EPSG semantics"));
    }

    #[test]
    fn reject_wkt_with_top_level_epsg_and_reversed_axes() {
        let err = parse_wkt(
            r#"GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563]],CS[ellipsoidal,2],AXIS["latitude",north],AXIS["longitude",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4326]]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported axis order/directions"));
    }

    #[test]
    fn reject_custom_airy_datum_without_structured_match() {
        let err = parse_wkt(
            r#"GEOGCRS["Custom Airy",DATUM["Custom Airy Datum",ELLIPSOID["Airy 1830",6377563.396,299.3249646]],CS[ellipsoidal,2],AXIS["longitude",east],AXIS["latitude",north],ANGLEUNIT["degree",0.0174532925199433]]"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported or unrecognized WKT datum"));
    }
}

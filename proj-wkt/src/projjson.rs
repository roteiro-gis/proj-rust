use serde_json::Value;
use std::collections::HashMap;

use crate::semantics::{
    angle_unit_name_to_degree, approx_eq, linear_unit_from_meters_per_unit, linear_unit_name,
    normalize_key, projection_parameter_unit_kind, radians_to_degrees_factor, resolve_named_datum,
    resolve_structured_datum_or_custom,
    validate_supported_geographic_or_ellipsoidal_height_semantics,
    validate_supported_geographic_semantics, validate_supported_projected_semantics,
    validate_supported_vertical_coordinate_system, validate_vertical_unit_matches_authority,
    AxisDirection, AxisOrderPolicy, CoordinateSystemSpec, DatumAliasScope,
    GeographicCoordinateSystemKind, ProjectionParameterUnitKind, StructuredEllipsoid,
};
use crate::{ParseError, Result};
use proj_core::{
    CompoundCrsDef, CrsDef, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    ProjectionMethod, VerticalCrsDef,
};

pub(crate) fn parse_projjson(s: &str) -> Result<CrsDef> {
    let value: Value =
        serde_json::from_str(s).map_err(|e| ParseError::Parse(format!("invalid PROJJSON: {e}")))?;
    let top_level_epsg = top_level_epsg_id(&value);

    if let Some(epsg) = top_level_epsg {
        if is_semantically_neutral_authority_wrapper(&value) {
            let registry = proj_core::lookup_epsg(epsg).ok_or_else(|| {
                ParseError::Parse(format!("unsupported EPSG code in PROJJSON: {epsg}"))
            })?;
            let declared_type = value
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| ParseError::Parse("PROJJSON object is missing a CRS type".into()))?;
            validate_wrapper_type_matches_registry(declared_type, &registry)?;
            return Ok(registry);
        }
    }

    let crs_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ParseError::Parse("PROJJSON object is missing a CRS type".into()))?;

    let parsed = match crs_type {
        "GeographicCRS" | "GeodeticCRS" => parse_geographic_projjson(&value)?,
        "ProjectedCRS" => parse_projected_projjson(&value)?,
        "CompoundCRS" => parse_compound_projjson(&value)?,
        "VerticalCRS" => {
            return Err(ParseError::UnsupportedSemantics(
                "standalone vertical CRS definitions are not supported by the horizontal transform API; use a compound CRS with an identical vertical component on both sides".into(),
            ));
        }
        other => {
            return Err(ParseError::Parse(format!(
                "unsupported PROJJSON CRS without an EPSG id: {other}"
            )));
        }
    };

    if let Some(epsg) = top_level_epsg {
        return canonicalize_authoritative_crs(parsed, epsg, "PROJJSON");
    }

    Ok(parsed)
}

fn parse_geographic_projjson(value: &Value) -> Result<CrsDef> {
    let coordinate_system = coordinate_system_from_json(value);
    let coordinate_system_kind = validate_supported_geographic_or_ellipsoidal_height_semantics(
        "PROJJSON geographic CRS",
        coordinate_system_angle_unit_to_degree(value)?,
        prime_meridian_degrees_from_json(value),
        &coordinate_system,
        AxisOrderPolicy::Strict,
    )?;
    let datum = infer_datum_from_json_crs(value)?;
    let horizontal = GeographicCrsDef::new(0, datum.clone(), "");

    match coordinate_system_kind {
        GeographicCoordinateSystemKind::TwoDimensional => Ok(CrsDef::Geographic(horizontal)),
        GeographicCoordinateSystemKind::ThreeDimensionalEllipsoidalHeight => {
            let vertical = VerticalCrsDef::ellipsoidal_height(
                0,
                datum,
                vertical_axis_linear_unit_from_json(value).unwrap_or_else(LinearUnit::metre),
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

fn parse_projected_projjson(value: &Value) -> Result<CrsDef> {
    let conversion = value
        .get("conversion")
        .ok_or_else(|| ParseError::Parse("PROJJSON projected CRS is missing conversion".into()))?;
    let base_crs = value
        .get("base_crs")
        .ok_or_else(|| ParseError::Parse("PROJJSON projected CRS is missing base_crs".into()))?;
    let datum = infer_datum_from_json_crs(base_crs)?;
    let linear_unit = projected_linear_unit(value)?.unwrap_or_else(LinearUnit::metre);
    validate_supported_projected_semantics(
        "PROJJSON projected CRS",
        &coordinate_system_from_json(value),
        AxisOrderPolicy::Strict,
    )?;

    let base_angle_unit_to_degree =
        coordinate_system_angle_unit_to_degree(base_crs)?.unwrap_or(1.0);
    validate_supported_geographic_semantics(
        "PROJJSON projected base geographic CRS",
        Some(base_angle_unit_to_degree),
        prime_meridian_degrees_from_json(base_crs),
        &coordinate_system_from_json(base_crs),
        AxisOrderPolicy::Strict,
    )?;
    let method_name = conversion
        .get("method")
        .and_then(|method| method.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ParseError::Parse("PROJJSON projected CRS is missing conversion.method.name".into())
        })?;
    let params = parse_parameters(conversion, linear_unit, base_angle_unit_to_degree)?;

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
    let normalized_method = normalize_key(method_name);

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
                k0,
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
        "albersequalarea" | "albersequalareaconic" => ProjectionMethod::AlbersEqualArea {
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
        "colombiaurban" => ProjectionMethod::ColombiaUrban {
            lon0,
            lat0,
            h0: first_param(&params, &["projectionplaneoriginheight"]).unwrap_or(0.0),
            false_easting: fe,
            false_northing: fn_,
        },
        "equalearth" => ProjectionMethod::EqualEarth {
            lon0,
            false_easting: fe,
            false_northing: fn_,
        },
        "polyconic" | "americanpolyconic" => ProjectionMethod::AmericanPolyconic {
            lon0,
            lat0,
            false_easting: fe,
            false_northing: fn_,
        },
        "krovak"
        | "krovaknorthorientated"
        | "krovakmodified"
        | "modifiedkrovak"
        | "krovakmodifiednorthorientated" => {
            let modified = normalized_method != "krovak" && normalized_method.contains("modified");
            // The cone geometry defaults are the EPSG/PROJ standard values
            // shared by every Krovak definition.
            let co_latitude_cone_axis = first_param(&params, &["azimuth", "colatitudeofconeaxis"])
                .unwrap_or(30.288_139_752_777_78);
            let lat_pseudo_standard_parallel = first_param(
                &params,
                &[
                    "pseudostandardparallel1",
                    "latitudeofpseudostandardparallel",
                ],
            )
            .unwrap_or(78.5);
            let k0 = first_param(&params, &["scalefactoronpseudostandardparallel"]).unwrap_or(k0);
            if modified {
                ProjectionMethod::KrovakModifiedNorthOrientated {
                    lon0,
                    lat0,
                    co_latitude_cone_axis,
                    lat_pseudo_standard_parallel,
                    k0,
                    false_easting: fe,
                    false_northing: fn_,
                }
            } else {
                ProjectionMethod::KrovakNorthOrientated {
                    lon0,
                    lat0,
                    co_latitude_cone_axis,
                    lat_pseudo_standard_parallel,
                    k0,
                    false_easting: fe,
                    false_northing: fn_,
                }
            }
        }
        "lambertconformalconic2spmichigan" => ProjectionMethod::LambertConformalConicMichigan {
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
            ellipsoid_scaling_factor: first_param(&params, &["ellipsoidscalingfactor"])
                .unwrap_or(1.0),
            false_easting: fe,
            false_northing: fn_,
        },
        "lambertconformalconic1spvariantb" | "lambertconicconformal1spvariantb" => {
            ProjectionMethod::LambertConformalConic1SPVariantB {
                lon0,
                lat0,
                k0,
                lat_false_origin: first_param(&params, &["latitudeoffalseorigin"]).unwrap_or(lat0),
                false_easting: fe,
                false_northing: fn_,
            }
        }
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
        "popularvisualisationpseudomercator" | "webmercator" => ProjectionMethod::WebMercator,
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
                "unsupported PROJJSON conversion method: {method_name}"
            )));
        }
    };

    Ok(CrsDef::Projected(ProjectedCrsDef::new(
        0,
        datum,
        method,
        linear_unit,
        "",
    )))
}

fn parse_compound_projjson(value: &Value) -> Result<CrsDef> {
    let components = value
        .get("components")
        .and_then(Value::as_array)
        .ok_or_else(|| ParseError::Parse("PROJJSON compound CRS is missing components".into()))?;
    let mut horizontal = None;
    let mut vertical = None;

    for component in components {
        match component.get("type").and_then(Value::as_str) {
            Some("GeographicCRS" | "GeodeticCRS") if horizontal.is_none() => {
                horizontal = Some(parse_geographic_projjson(component));
            }
            Some("ProjectedCRS") if horizontal.is_none() => {
                horizontal = Some(parse_projected_projjson(component));
            }
            Some("VerticalCRS") if vertical.is_none() => {
                vertical = Some(parse_vertical_projjson(component));
            }
            _ => {}
        }
    }

    let horizontal = horizontal.ok_or_else(|| {
        ParseError::Parse("PROJJSON compound CRS is missing a horizontal CRS".into())
    })??;
    if horizontal.vertical_crs().is_some() {
        return Err(ParseError::UnsupportedSemantics(
            "PROJJSON compound CRS cannot use a 3D horizontal CRS as its horizontal component"
                .into(),
        ));
    }

    let vertical = vertical.ok_or_else(|| {
        ParseError::Parse("PROJJSON compound CRS is missing a vertical CRS".into())
    })??;
    let compound = CompoundCrsDef::from_crs_def(0, horizontal, vertical, "")?;
    Ok(CrsDef::Compound(Box::new(compound)))
}

fn parse_vertical_projjson(value: &Value) -> Result<VerticalCrsDef> {
    validate_supported_vertical_coordinate_system(
        "PROJJSON vertical CRS",
        &coordinate_system_from_json(value),
    )?;
    let epsg = top_level_epsg_id(value).unwrap_or(0);
    let linear_unit = vertical_axis_linear_unit_from_json(value).unwrap_or_else(LinearUnit::metre);
    if let Some(canonical) = proj_core::lookup_vertical_epsg(epsg) {
        validate_vertical_unit_matches_authority("PROJJSON vertical CRS", linear_unit, &canonical)?;
        return Ok(canonical);
    }

    let datum = value
        .get("datum")
        .ok_or_else(|| ParseError::Parse("PROJJSON vertical CRS is missing a datum".into()))?;
    let vertical_datum_epsg = epsg_id_from_object(datum.get("id")).ok_or_else(|| {
        ParseError::UnsupportedSemantics(
            "PROJJSON gravity-related vertical CRS requires a vertical datum EPSG identifier"
                .into(),
        )
    })?;
    Ok(VerticalCrsDef::gravity_related_height(
        epsg,
        vertical_datum_epsg,
        linear_unit,
        "",
    )?)
}

fn top_level_epsg_id(value: &Value) -> Option<u32> {
    let id = value.get("id")?;
    let authority = id.get("authority")?.as_str()?;
    if !authority.eq_ignore_ascii_case("EPSG") {
        return None;
    }

    match id.get("code")? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn is_semantically_neutral_authority_wrapper(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.keys()
        .all(|key| matches!(key.as_str(), "$schema" | "type" | "name" | "id"))
}

fn validate_wrapper_type_matches_registry(declared_type: &str, registry: &CrsDef) -> Result<()> {
    let type_matches = match declared_type {
        "GeographicCRS" | "GeodeticCRS" => registry.is_geographic(),
        "ProjectedCRS" => registry.is_projected(),
        _ => false,
    };

    if type_matches {
        Ok(())
    } else {
        Err(ParseError::UnsupportedSemantics(format!(
            "PROJJSON authority wrapper type `{declared_type}` does not match EPSG:{}",
            registry.epsg()
        )))
    }
}

use crate::wkt::canonicalize_authoritative_crs;

fn infer_datum_from_json_crs(value: &Value) -> Result<proj_core::Datum> {
    let datum_value = value
        .get("datum")
        .or_else(|| value.get("datum_ensemble"))
        .ok_or_else(|| ParseError::Parse("PROJJSON CRS is missing a datum definition".into()))?;

    if let Some(epsg) = epsg_id_from_object(datum_value.get("id")) {
        return proj_core::lookup_datum_epsg(epsg)
            .ok_or_else(|| ParseError::Parse(format!("unsupported PROJJSON datum EPSG:{epsg}")));
    }

    let datum_name = datum_value
        .get("name")
        .and_then(Value::as_str)
        .map(normalize_key)
        .ok_or_else(|| ParseError::Parse("PROJJSON datum is missing a name".into()))?;
    let ellipsoid = parse_structured_ellipsoid_from_json(datum_value);

    match ellipsoid.as_ref() {
        Some(ellipsoid) => {
            resolve_structured_datum_or_custom(DatumAliasScope::ProjJson, &datum_name, ellipsoid)
                .ok_or_else(|| {
                    ParseError::Parse("unsupported PROJJSON datum or CRS definition".into())
                })
        }
        None => resolve_named_datum(DatumAliasScope::ProjJson, &datum_name).ok_or_else(|| {
            ParseError::Parse("unsupported PROJJSON datum or CRS definition".into())
        }),
    }
}

fn parse_structured_ellipsoid_from_json(value: &Value) -> Option<StructuredEllipsoid> {
    let ellipsoid = value.get("ellipsoid")?;
    Some(StructuredEllipsoid {
        epsg: epsg_id_from_object(ellipsoid.get("id")),
        name: ellipsoid
            .get("name")
            .and_then(Value::as_str)
            .map(normalize_key)?,
        semi_major_axis: ellipsoid.get("semi_major_axis").and_then(Value::as_f64)?,
        inverse_flattening: ellipsoid
            .get("inverse_flattening")
            .and_then(Value::as_f64)?,
    })
}

fn epsg_id_from_object(value: Option<&Value>) -> Option<u32> {
    let id = value?;
    let authority = id.get("authority")?.as_str()?;
    if !authority.eq_ignore_ascii_case("EPSG") {
        return None;
    }

    match id.get("code")? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn parse_parameters(
    conversion: &Value,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> Result<HashMap<String, f64>> {
    let mut params = HashMap::new();
    let values = match conversion.get("parameters") {
        Some(Value::Array(values)) => values,
        Some(_) => {
            return Err(ParseError::Parse(
                "PROJJSON conversion.parameters must be an array".into(),
            ));
        }
        None => return Ok(params),
    };

    for param in values {
        let name = param.get("name").and_then(Value::as_str).ok_or_else(|| {
            ParseError::Parse("PROJJSON conversion parameter is missing a name".into())
        })?;
        let normalized_name = normalize_key(name);
        let value = parse_projjson_parameter_value(name, param.get("value"))?;
        let factor = parameter_factor_from_json(
            param,
            &normalized_name,
            projected_linear_unit,
            base_angle_unit_to_degree,
        );
        params.insert(normalized_name, value * factor);
    }

    Ok(params)
}

fn parse_projjson_parameter_value(name: &str, value: Option<&Value>) -> Result<f64> {
    let Some(value) = value else {
        return Err(ParseError::Parse(format!(
            "PROJJSON conversion parameter `{name}` is missing value"
        )));
    };

    let parsed = match value {
        Value::Number(n) => n.as_f64().ok_or_else(|| {
            ParseError::Parse(format!(
                "invalid PROJJSON conversion parameter `{name}` value"
            ))
        })?,
        Value::String(s) => s.parse::<f64>().map_err(|_| {
            ParseError::Parse(format!(
                "invalid PROJJSON conversion parameter `{name}` value: {s}"
            ))
        })?,
        _ => {
            return Err(ParseError::Parse(format!(
                "invalid PROJJSON conversion parameter `{name}` value"
            )));
        }
    };

    if !parsed.is_finite() {
        return Err(ParseError::Parse(format!(
            "PROJJSON conversion parameter `{name}` value must be finite"
        )));
    }
    Ok(parsed)
}

fn parameter_factor_from_json(
    param: &Value,
    normalized_name: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> f64 {
    let unit_kind = projection_parameter_unit_kind(normalized_name);
    match unit_kind {
        ProjectionParameterUnitKind::Angle => param
            .get("unit")
            .and_then(angle_unit_to_degree_from_json)
            .or_else(|| {
                param
                    .get("unit_conversion_factor")
                    .and_then(Value::as_f64)
                    .map(radians_to_degrees_factor)
            })
            .or_else(|| {
                param
                    .get("conversion_factor")
                    .and_then(Value::as_f64)
                    .map(radians_to_degrees_factor)
            })
            .unwrap_or(base_angle_unit_to_degree),
        ProjectionParameterUnitKind::Length => param
            .get("unit")
            .and_then(linear_unit_from_json)
            .map(LinearUnit::meters_per_unit)
            .or_else(|| param.get("unit_conversion_factor").and_then(Value::as_f64))
            .or_else(|| param.get("conversion_factor").and_then(Value::as_f64))
            .unwrap_or(projected_linear_unit.meters_per_unit()),
        ProjectionParameterUnitKind::Scale | ProjectionParameterUnitKind::Other => 1.0,
    }
}

fn projected_linear_unit(value: &Value) -> Result<Option<LinearUnit>> {
    let Some(axis) = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };

    let mut linear_unit: Option<LinearUnit> = None;
    for axis in axis {
        let Some(axis_unit) = axis_linear_unit(axis) else {
            continue;
        };

        if let Some(existing_linear_unit) = linear_unit {
            if !approx_eq(
                existing_linear_unit.meters_per_unit(),
                axis_unit.meters_per_unit(),
            ) {
                return Err(ParseError::UnsupportedSemantics(
                    "PROJJSON projected CRS uses inconsistent projected axis units".into(),
                ));
            }
        } else {
            linear_unit = Some(axis_unit);
        }
    }

    Ok(linear_unit)
}

fn coordinate_system_angle_unit_to_degree(value: &Value) -> Result<Option<f64>> {
    let Some(axis) = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };

    let mut angle_unit_to_degree: Option<f64> = None;
    for axis in axis {
        let Some(axis_angle_unit_to_degree) = axis_angle_unit_to_degree(axis) else {
            continue;
        };

        if let Some(existing_angle_unit_to_degree) = angle_unit_to_degree {
            if !approx_eq(existing_angle_unit_to_degree, axis_angle_unit_to_degree) {
                return Err(ParseError::UnsupportedSemantics(
                    "PROJJSON geographic CRS uses inconsistent angular axis units".into(),
                ));
            }
        } else {
            angle_unit_to_degree = Some(axis_angle_unit_to_degree);
        }
    }

    Ok(angle_unit_to_degree)
}

fn coordinate_system_from_json(value: &Value) -> CoordinateSystemSpec {
    let subtype = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("subtype"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let axis_values = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array);
    let axes = axis_values
        .map(|axes| axes.iter().map(axis_direction_from_json).collect())
        .unwrap_or_default();
    let axis_linear_units = axis_values
        .map(|axes| axes.iter().map(axis_linear_unit).collect())
        .unwrap_or_default();
    let axis_angle_unit_to_degree = axis_values
        .map(|axes| axes.iter().map(axis_angle_unit_to_degree).collect())
        .unwrap_or_default();
    let dimension = axis_values.map(Vec::len);

    CoordinateSystemSpec {
        subtype,
        dimension,
        axes,
        axis_linear_units,
        axis_angle_unit_to_degree,
    }
}

fn axis_direction_from_json(axis: &Value) -> AxisDirection {
    axis.get("direction")
        .and_then(Value::as_str)
        .map(AxisDirection::from_str)
        .unwrap_or(AxisDirection::Other)
}

fn vertical_axis_linear_unit_from_json(value: &Value) -> Option<LinearUnit> {
    value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)?
        .iter()
        .find(|axis| axis_direction_from_json(axis) == AxisDirection::Up)
        .and_then(axis_linear_unit)
}

fn prime_meridian_degrees_from_json(value: &Value) -> Option<f64> {
    let prime_meridian = value.get("prime_meridian")?;
    let longitude = match prime_meridian.get("longitude")? {
        Value::Number(number) => number.as_f64()?,
        Value::String(string) => string.parse().ok()?,
        _ => return None,
    };

    let factor = prime_meridian
        .get("unit")
        .and_then(angle_unit_to_degree_from_json)
        .or_else(|| {
            prime_meridian
                .get("unit_conversion_factor")
                .and_then(Value::as_f64)
                .map(radians_to_degrees_factor)
        })
        .or_else(|| {
            prime_meridian
                .get("conversion_factor")
                .and_then(Value::as_f64)
                .map(radians_to_degrees_factor)
        })
        .unwrap_or(1.0);

    Some(longitude * factor)
}

fn axis_linear_unit(axis: &Value) -> Option<LinearUnit> {
    axis.get("unit")
        .and_then(linear_unit_from_json)
        .or_else(|| {
            axis.get("unit_conversion_factor")
                .and_then(Value::as_f64)
                .and_then(linear_unit_from_meters_per_unit)
        })
        .or_else(|| {
            axis.get("conversion_factor")
                .and_then(Value::as_f64)
                .and_then(linear_unit_from_meters_per_unit)
        })
}

fn axis_angle_unit_to_degree(axis: &Value) -> Option<f64> {
    axis.get("unit")
        .and_then(angle_unit_to_degree_from_json)
        .or_else(|| {
            axis.get("unit_conversion_factor")
                .and_then(Value::as_f64)
                .map(radians_to_degrees_factor)
        })
        .or_else(|| {
            axis.get("conversion_factor")
                .and_then(Value::as_f64)
                .map(radians_to_degrees_factor)
        })
}

fn linear_unit_from_json(value: &Value) -> Option<LinearUnit> {
    if let Some(unit) = value.as_str() {
        return linear_unit_name(unit);
    }

    if let Some(unit_type) = value.get("type").and_then(Value::as_str) {
        if unit_type.eq_ignore_ascii_case("AngularUnit") {
            return None;
        }
    }

    if let Some(factor) = value.get("conversion_factor").and_then(Value::as_f64) {
        return linear_unit_from_meters_per_unit(factor);
    }
    if let Some(factor) = value.get("unit_conversion_factor").and_then(Value::as_f64) {
        return linear_unit_from_meters_per_unit(factor);
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .and_then(linear_unit_name)
}

fn angle_unit_to_degree_from_json(value: &Value) -> Option<f64> {
    if let Some(unit) = value.as_str() {
        return angle_unit_name_to_degree(unit);
    }

    // A unit explicitly typed as non-angular is not an angular unit, no
    // matter what its conversion factor is (e.g. the metre unit of an
    // ellipsoidal-height axis).
    if let Some(unit_type) = value.get("type").and_then(Value::as_str) {
        if !unit_type.eq_ignore_ascii_case("AngularUnit") && !unit_type.eq_ignore_ascii_case("Unit")
        {
            return None;
        }
    }

    if let Some(factor) = value.get("conversion_factor").and_then(Value::as_f64) {
        return Some(radians_to_degrees_factor(factor));
    }
    if let Some(factor) = value.get("unit_conversion_factor").and_then(Value::as_f64) {
        return Some(radians_to_degrees_factor(factor));
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .and_then(angle_unit_name_to_degree)
}

fn first_param(params: &HashMap<String, f64>, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| params.get(&normalize_key(name)).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_FOOT_TO_METER: f64 = 0.3048006096012192;

    #[test]
    fn parses_projjson_with_top_level_epsg_id() {
        let crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "WGS 84 / Pseudo-Mercator",
                "id": { "authority": "EPSG", "code": 3857 }
            }"#,
        )
        .unwrap();

        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn parses_projjson_geographic_3d_as_compound_ellipsoidal_height() {
        let crs = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "WGS 84 3D",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378137,
                        "inverse_flattening": 298.257223563
                    }
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" },
                        { "name": "Ellipsoidal height", "abbreviation": "h", "direction": "up", "unit": "metre" }
                    ]
                }
            }"#,
        )
        .unwrap();

        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert!(crs.vertical_crs().is_some());
    }

    #[test]
    fn parses_projjson_compound_with_vertical_crs() {
        let crs = parse_projjson(
            r#"{
                "type": "CompoundCRS",
                "name": "WGS 84 + NAVD88 height",
                "components": [
                    {
                        "type": "GeographicCRS",
                        "name": "WGS 84",
                        "datum": {
                            "type": "GeodeticReferenceFrame",
                            "name": "World Geodetic System 1984",
                            "ellipsoid": {
                                "name": "WGS 84",
                                "semi_major_axis": 6378137,
                                "inverse_flattening": 298.257223563
                            }
                        },
                        "coordinate_system": {
                            "subtype": "ellipsoidal",
                            "axis": [
                                { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                                { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                            ]
                        }
                    },
                    {
                        "type": "VerticalCRS",
                        "name": "NAVD88 height",
                        "datum": {
                            "type": "VerticalReferenceFrame",
                            "name": "North American Vertical Datum 1988",
                            "id": { "authority": "EPSG", "code": 5103 }
                        },
                        "coordinate_system": {
                            "subtype": "vertical",
                            "axis": [
                                { "name": "Gravity-related height", "abbreviation": "H", "direction": "up", "unit": "metre" }
                            ]
                        }
                    }
                ]
            }"#,
        )
        .unwrap();

        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert_eq!(
            crs.vertical_crs().unwrap().linear_unit_to_meter(),
            LinearUnit::metre().meters_per_unit()
        );
    }

    #[test]
    fn parses_projjson_vertical_crs_canonicalized_from_crs_epsg() {
        let crs = parse_projjson(
            r#"{
                "type": "CompoundCRS",
                "name": "WGS 84 + NAVD88 height",
                "components": [
                    {
                        "type": "GeographicCRS",
                        "name": "WGS 84",
                        "datum": {
                            "type": "GeodeticReferenceFrame",
                            "name": "World Geodetic System 1984",
                            "ellipsoid": {
                                "name": "WGS 84",
                                "semi_major_axis": 6378137,
                                "inverse_flattening": 298.257223563
                            }
                        },
                        "coordinate_system": {
                            "subtype": "ellipsoidal",
                            "axis": [
                                { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                                { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                            ]
                        }
                    },
                    {
                        "type": "VerticalCRS",
                        "name": "NAVD88 height",
                        "datum": {
                            "type": "VerticalReferenceFrame",
                            "name": "North American Vertical Datum 1988"
                        },
                        "coordinate_system": {
                            "subtype": "vertical",
                            "axis": [
                                { "name": "Gravity-related height", "abbreviation": "H", "direction": "up", "unit": "metre" }
                            ]
                        },
                        "id": { "authority": "EPSG", "code": 5703 }
                    }
                ]
            }"#,
        )
        .unwrap();

        let vertical = crs.vertical_crs().unwrap();
        assert_eq!(vertical.epsg(), 5703);
        assert_eq!(vertical.vertical_datum_epsg(), Some(5103));
    }

    #[test]
    fn rejects_standalone_vertical_projjson() {
        let err = parse_projjson(
            r#"{
                "type": "VerticalCRS",
                "name": "NAVD88 height",
                "datum": {
                    "type": "VerticalReferenceFrame",
                    "name": "North American Vertical Datum 1988",
                    "id": { "authority": "EPSG", "code": 5103 }
                },
                "coordinate_system": {
                    "subtype": "vertical",
                    "axis": [
                        { "name": "Gravity-related height", "abbreviation": "H", "direction": "up", "unit": "metre" }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("standalone vertical CRS"));
    }

    #[test]
    fn rejects_projjson_with_top_level_epsg_mismatch() {
        let err = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "WGS 84",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378137,
                        "inverse_flattening": 298.257223563
                    }
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                    ]
                },
                "id": { "authority": "EPSG", "code": 4269 }
            }"#,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("does not match the embedded EPSG semantics"));
    }

    #[test]
    fn rejects_projjson_with_top_level_epsg_and_reversed_axes() {
        let err = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "WGS 84",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378137,
                        "inverse_flattening": 298.257223563
                    }
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" },
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" }
                    ]
                },
                "id": { "authority": "EPSG", "code": 4326 }
            }"#,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("unsupported axis order/directions"));
    }

    #[test]
    fn parses_projjson_wgs84_without_epsg_id() {
        let crs = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "WGS 84",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378137,
                        "inverse_flattening": 298.257223563
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(crs.is_geographic());
        assert_eq!(
            crs.datum().ellipsoid().semi_major_axis(),
            proj_core::datum::WGS84.ellipsoid().semi_major_axis()
        );
    }

    #[test]
    fn rejects_projjson_geographic_with_non_degree_unit() {
        let err = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "Custom radians",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984"
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "radian" },
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "radian" }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("angular units other than degrees"));
    }

    #[test]
    fn rejects_projjson_geographic_with_reversed_axes() {
        let err = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "Custom reversed axes",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984"
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" },
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("unsupported axis order/directions"));
    }

    #[test]
    fn rejects_projjson_without_supported_definition() {
        let err = parse_projjson(r#"{ "type": "ProjectedCRS", "name": "Custom" }"#).unwrap_err();
        assert!(err.to_string().contains("missing conversion"));
    }

    #[test]
    /// An unrecognized datum name with valid ellipsoid numbers parses as a
    /// custom datum and must not be conflated with WGS 84 despite sharing
    /// its ellipsoid.
    fn projjson_custom_datum_parses_as_custom_datum() {
        let crs = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "WGS 84 styled custom",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "Custom Datum",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378137,
                        "inverse_flattening": 298.257223563
                    }
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                    ]
                }
            }"#,
        )
        .unwrap();

        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 0);
        let wgs84 = proj_core::lookup_epsg(4326).unwrap();
        assert!(!crs.semantically_equivalent(&wgs84));
        // The custom definition roundtrips through the PROJJSON serializer
        // (structural comparison: custom datums are fail-closed under
        // semantic equivalence by design).
        let reparsed = parse_projjson(&crate::to_projjson(&crs).unwrap()).unwrap();
        assert_eq!(format!("{crs:?}"), format!("{reparsed:?}"));
    }

    #[test]
    fn rejects_projjson_known_datum_name_with_mismatched_ellipsoid() {
        let err = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "name": "Broken WGS 84",
                "datum": {
                    "type": "GeodeticReferenceFrame",
                    "name": "World Geodetic System 1984",
                    "ellipsoid": {
                        "name": "WGS 84",
                        "semi_major_axis": 6378136,
                        "inverse_flattening": 298.257223563
                    }
                },
                "coordinate_system": {
                    "subtype": "ellipsoidal",
                    "axis": [
                        { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                        { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("unsupported PROJJSON datum or CRS definition"));
    }

    #[test]
    fn rejects_projjson_authority_wrapper_with_contradictory_type() {
        let err = parse_projjson(
            r#"{
                "type": "GeographicCRS",
                "id": { "authority": "EPSG", "code": 3857 }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("does not match EPSG:3857"));
    }

    #[test]
    fn name_only_authority_wrapper_still_canonicalizes() {
        let crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Not Web Mercator",
                "id": { "authority": "EPSG", "code": 3857 }
            }"#,
        )
        .unwrap();

        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn parses_projected_projjson_without_epsg_id() {
        let crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": {
                        "name": "World Geodetic System 1984"
                    }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": -75 },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        { "name": "False easting", "value": 500000 },
                        { "name": "False northing", "value": 0 }
                    ]
                }
            }"#,
        )
        .unwrap();

        assert!(crs.is_projected());
    }

    #[test]
    fn rejects_projected_projjson_with_invalid_parameter_value() {
        let err = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": {
                        "name": "World Geodetic System 1984"
                    }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": "not-a-number" },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        { "name": "False easting", "value": 500000 },
                        { "name": "False northing", "value": 0 }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("invalid PROJJSON conversion parameter `Longitude of natural origin` value"));
    }

    #[test]
    fn rejects_projected_projjson_with_missing_parameter_value() {
        let err = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": {
                        "name": "World Geodetic System 1984"
                    }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin" },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        { "name": "False easting", "value": 500000 },
                        { "name": "False northing", "value": 0 }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains(
            "PROJJSON conversion parameter `Longitude of natural origin` is missing value"
        ));
    }

    #[test]
    fn rejects_projected_projjson_with_non_greenwich_base_prime_meridian() {
        let err = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom TM",
                "base_crs": {
                    "type": "GeographicCRS",
                    "name": "Custom base",
                    "datum": {
                        "type": "GeodeticReferenceFrame",
                        "name": "World Geodetic System 1984"
                    },
                    "prime_meridian": {
                        "name": "Paris",
                        "longitude": 2.33722917,
                        "unit": "degree"
                    },
                    "coordinate_system": {
                        "subtype": "ellipsoidal",
                        "axis": [
                            { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                            { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                        ]
                    }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0, "unit": "degree" },
                        { "name": "Longitude of natural origin", "value": -75, "unit": "degree" },
                        { "name": "Scale factor at natural origin", "value": 0.9996, "unit": "unity" },
                        { "name": "False easting", "value": 500000, "unit": "metre" },
                        { "name": "False northing", "value": 0, "unit": "metre" }
                    ]
                },
                "coordinate_system": {
                    "subtype": "Cartesian",
                    "axis": [
                        { "name": "Easting", "abbreviation": "E", "direction": "east", "unit": "metre" },
                        { "name": "Northing", "abbreviation": "N", "direction": "north", "unit": "metre" }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("non-Greenwich prime meridian"));
    }

    #[test]
    fn rejects_projected_projjson_with_reversed_projected_axes() {
        let err = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom TM",
                "base_crs": {
                    "type": "GeographicCRS",
                    "name": "WGS 84",
                    "datum": {
                        "type": "GeodeticReferenceFrame",
                        "name": "World Geodetic System 1984"
                    },
                    "coordinate_system": {
                        "subtype": "ellipsoidal",
                        "axis": [
                            { "name": "Longitude", "abbreviation": "Lon", "direction": "east", "unit": "degree" },
                            { "name": "Latitude", "abbreviation": "Lat", "direction": "north", "unit": "degree" }
                        ]
                    }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0, "unit": "degree" },
                        { "name": "Longitude of natural origin", "value": -75, "unit": "degree" },
                        { "name": "Scale factor at natural origin", "value": 0.9996, "unit": "unity" },
                        { "name": "False easting", "value": 500000, "unit": "metre" },
                        { "name": "False northing", "value": 0, "unit": "metre" }
                    ]
                },
                "coordinate_system": {
                    "subtype": "Cartesian",
                    "axis": [
                        { "name": "Northing", "abbreviation": "N", "direction": "north", "unit": "metre" },
                        { "name": "Easting", "abbreviation": "E", "direction": "east", "unit": "metre" }
                    ]
                }
            }"#,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("unsupported axis order/directions"));
    }

    #[test]
    fn parses_projected_projjson_with_foot_units() {
        let meter_crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N metre",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": { "name": "World Geodetic System 1984" }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": -75 },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        { "name": "False easting", "value": 500000, "unit": "metre" },
                        { "name": "False northing", "value": 0, "unit": "metre" }
                    ]
                },
                "coordinate_system": {
                    "subtype": "Cartesian",
                    "axis": [
                        { "name": "Easting", "direction": "east", "unit": "metre" },
                        { "name": "Northing", "direction": "north", "unit": "metre" }
                    ]
                }
            }"#,
        )
        .unwrap();
        let foot_crs = parse_projjson(
            r#"{
                "type": "ProjectedCRS",
                "name": "Custom UTM 18N ftUS",
                "base_crs": {
                    "name": "WGS 84",
                    "datum": { "name": "World Geodetic System 1984" }
                },
                "conversion": {
                    "method": { "name": "Transverse Mercator" },
                    "parameters": [
                        { "name": "Latitude of natural origin", "value": 0 },
                        { "name": "Longitude of natural origin", "value": -75 },
                        { "name": "Scale factor at natural origin", "value": 0.9996 },
                        {
                            "name": "False easting",
                            "value": 1640416.6666666667,
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        },
                        {
                            "name": "False northing",
                            "value": 0,
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        }
                    ]
                },
                "coordinate_system": {
                    "subtype": "Cartesian",
                    "axis": [
                        {
                            "name": "Easting",
                            "direction": "east",
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        },
                        {
                            "name": "Northing",
                            "direction": "north",
                            "unit": {
                                "type": "LinearUnit",
                                "name": "US survey foot",
                                "conversion_factor": 0.3048006096012192
                            }
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let from = proj_core::lookup_epsg(4326).unwrap();
        let meter_tx = proj_core::Transform::from_crs_defs(&from, &meter_crs).unwrap();
        let foot_tx = proj_core::Transform::from_crs_defs(&from, &foot_crs).unwrap();

        let (mx, my) = meter_tx.convert((-74.006, 40.7128)).unwrap();
        let (fx, fy) = foot_tx.convert((-74.006, 40.7128)).unwrap();

        assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02, "x mismatch");
        assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02, "y mismatch");
    }
}

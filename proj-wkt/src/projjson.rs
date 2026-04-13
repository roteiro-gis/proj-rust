use serde_json::Value;
use std::collections::HashMap;

use crate::semantics::{
    approx_eq, normalize_key, validate_supported_geographic_semantics,
    validate_supported_projected_semantics, AxisDirection, CoordinateSystemSpec,
};
use crate::{ParseError, Result};
use proj_core::{CrsDef, Datum, GeographicCrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod};

pub(crate) fn parse_projjson(s: &str) -> Result<CrsDef> {
    let value: Value =
        serde_json::from_str(s).map_err(|e| ParseError::Parse(format!("invalid PROJJSON: {e}")))?;
    let top_level_epsg = top_level_epsg_id(&value);

    if let Some(epsg) = top_level_epsg {
        if is_semantically_neutral_authority_wrapper(&value) {
            return proj_core::lookup_epsg(epsg).ok_or_else(|| {
                ParseError::Parse(format!("unsupported EPSG code in PROJJSON: {epsg}"))
            });
        }
    }

    let crs_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ParseError::Parse("PROJJSON object is missing a CRS type".into()))?;

    let parsed = match crs_type {
        "GeographicCRS" | "GeodeticCRS" => {
            validate_supported_geographic_semantics(
                "PROJJSON geographic CRS",
                coordinate_system_angle_unit_to_degree(&value)?,
                prime_meridian_degrees_from_json(&value),
                &coordinate_system_from_json(&value),
            )?;
            let datum = infer_datum_from_json_crs(&value)?;
            CrsDef::Geographic(GeographicCrsDef::new(0, datum, ""))
        }
        "ProjectedCRS" => parse_projected_projjson(&value)?,
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
    )?;

    let base_angle_unit_to_degree =
        coordinate_system_angle_unit_to_degree(base_crs)?.unwrap_or(1.0);
    validate_supported_geographic_semantics(
        "PROJJSON projected base geographic CRS",
        Some(base_angle_unit_to_degree),
        prime_meridian_degrees_from_json(base_crs),
        &coordinate_system_from_json(base_crs),
    )?;
    let method_name = conversion
        .get("method")
        .and_then(|method| method.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ParseError::Parse("PROJJSON projected CRS is missing conversion.method.name".into())
        })?;
    let params = parse_parameters(conversion, linear_unit, base_angle_unit_to_degree);

    let lon0 = first_param(
        &params,
        &[
            "centralmeridian",
            "longitudeofcenter",
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
        ],
    )
    .unwrap_or(1.0);
    let fe = first_param(&params, &["falseeasting"]).unwrap_or(0.0);
    let fn_ = first_param(&params, &["falsenorthing"]).unwrap_or(0.0);
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

fn infer_datum_from_json_crs(value: &Value) -> Result<Datum> {
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

    ellipsoid
        .as_ref()
        .and_then(|ellipsoid| resolve_structured_datum(&datum_name, ellipsoid))
        .or_else(|| resolve_named_datum(&datum_name))
        .ok_or_else(|| ParseError::Parse("unsupported PROJJSON datum or CRS definition".into()))
}

#[derive(Debug)]
struct StructuredEllipsoid {
    epsg: Option<u32>,
    name: String,
    semi_major_axis: f64,
    inverse_flattening: f64,
}

type DatumCandidate = (
    &'static [&'static str],
    &'static [&'static str],
    Datum,
    Option<u32>,
);

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

fn resolve_structured_datum(datum_name: &str, ellipsoid: &StructuredEllipsoid) -> Option<Datum> {
    for (datum_aliases, ellipsoid_aliases, datum, ellipsoid_epsg) in datum_candidates() {
        if datum_aliases.contains(&datum_name)
            && ellipsoid_matches(ellipsoid, datum, ellipsoid_aliases, ellipsoid_epsg)
        {
            return Some(datum);
        }
    }

    None
}

fn resolve_named_datum(datum_name: &str) -> Option<Datum> {
    datum_candidates()
        .iter()
        .find_map(|(aliases, _, datum, _)| aliases.contains(&datum_name).then_some(*datum))
}

fn datum_candidates() -> [DatumCandidate; 8] {
    [
        (
            &[
                "wgs84",
                "wgs1984",
                "worldgeodeticsystem1984",
                "worldgeodeticsystem1984ensemble",
            ][..],
            &["wgs84"][..],
            proj_core::datum::WGS84,
            Some(7030),
        ),
        (
            &["northamericandatum1983", "nad83"][..],
            &["grs1980", "grs80"][..],
            proj_core::datum::NAD83,
            Some(7019),
        ),
        (
            &["northamericandatum1927", "nad27"][..],
            &["clarke1866", "clrk66"][..],
            proj_core::datum::NAD27,
            Some(7008),
        ),
        (
            &[
                "europeanterrestrialreferencesystem1989ensemble",
                "europeanterrestrialreferencesystem1989",
                "etrs89",
            ][..],
            &["grs1980", "grs80"][..],
            proj_core::datum::ETRS89,
            Some(7019),
        ),
        (
            &["ordnancesurveyofgreatbritain1936", "osgb36"][..],
            &["airy1830", "airy"][..],
            proj_core::datum::OSGB36,
            Some(7001),
        ),
        (
            &["europeandatum1950", "ed50"][..],
            &["international1924", "intl1924", "intl"][..],
            proj_core::datum::ED50,
            Some(7022),
        ),
        (
            &["pulkovo1942", "pulkovo1942(58)"][..],
            &["krassowsky1940", "krassowsky", "krass"][..],
            proj_core::datum::PULKOVO1942,
            Some(7024),
        ),
        (
            &["tokyo", "tokyodatum"][..],
            &["bessel1841", "bessel"][..],
            proj_core::datum::TOKYO,
            Some(7004),
        ),
    ]
}

fn ellipsoid_matches(
    actual: &StructuredEllipsoid,
    datum: Datum,
    aliases: &[&str],
    epsg: Option<u32>,
) -> bool {
    let expected_rf = if datum.ellipsoid.f == 0.0 {
        0.0
    } else {
        1.0 / datum.ellipsoid.f
    };

    epsg.is_some_and(|expected| actual.epsg == Some(expected))
        || (aliases.iter().any(|alias| *alias == actual.name)
            && (actual.semi_major_axis - datum.ellipsoid.a).abs() < 1e-9
            && (actual.inverse_flattening - expected_rf).abs() < 1e-9)
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
) -> HashMap<String, f64> {
    let mut params = HashMap::new();
    let values = match conversion.get("parameters").and_then(Value::as_array) {
        Some(values) => values,
        None => return params,
    };

    for param in values {
        let Some(name) = param.get("name").and_then(Value::as_str) else {
            continue;
        };
        let normalized_name = normalize_key(name);
        let value = match param.get("value") {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.parse::<f64>().ok(),
            _ => None,
        };
        if let Some(value) = value {
            let factor = parameter_factor_from_json(
                param,
                &normalized_name,
                projected_linear_unit,
                base_angle_unit_to_degree,
            );
            params.insert(normalized_name, value * factor);
        }
    }

    params
}

#[derive(Clone, Copy)]
enum ParameterUnitKind {
    Angle,
    Length,
    Scale,
    Other,
}

fn parameter_factor_from_json(
    param: &Value,
    normalized_name: &str,
    projected_linear_unit: LinearUnit,
    base_angle_unit_to_degree: f64,
) -> f64 {
    let unit_kind = parameter_unit_kind(normalized_name);
    match unit_kind {
        ParameterUnitKind::Angle => param
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
        ParameterUnitKind::Length => param
            .get("unit")
            .and_then(linear_unit_from_json)
            .map(LinearUnit::meters_per_unit)
            .or_else(|| param.get("unit_conversion_factor").and_then(Value::as_f64))
            .or_else(|| param.get("conversion_factor").and_then(Value::as_f64))
            .unwrap_or(projected_linear_unit.meters_per_unit()),
        ParameterUnitKind::Scale | ParameterUnitKind::Other => 1.0,
    }
}

fn parameter_unit_kind(normalized_name: &str) -> ParameterUnitKind {
    match normalized_name {
        "centralmeridian"
        | "longitudeofcenter"
        | "longitudeofnaturalorigin"
        | "longitudeoffalseorigin"
        | "longitudeoforigin"
        | "latitudeoforigin"
        | "latitudeofcenter"
        | "latitudeofnaturalorigin"
        | "latitudeoffalseorigin"
        | "standardparallel"
        | "standardparallel1"
        | "standardparallel2"
        | "latitudeofstandardparallel"
        | "latitudeof1ststandardparallel"
        | "latitudeof2ndstandardparallel" => ParameterUnitKind::Angle,
        "falseeasting" | "falsenorthing" | "eastingatfalseorigin" | "northingatfalseorigin" => {
            ParameterUnitKind::Length
        }
        "scalefactor" | "scalefactoratnaturalorigin" | "scalefactoratprojectionorigin" => {
            ParameterUnitKind::Scale
        }
        _ => ParameterUnitKind::Other,
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
    let axes = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
        .map(|axes| axes.iter().map(axis_direction_from_json).collect())
        .unwrap_or_default();
    let dimension = value
        .get("coordinate_system")
        .and_then(|cs| cs.get("axis"))
        .and_then(Value::as_array)
        .map(Vec::len);

    CoordinateSystemSpec {
        subtype,
        dimension,
        axes,
    }
}

fn axis_direction_from_json(axis: &Value) -> AxisDirection {
    axis.get("direction")
        .and_then(Value::as_str)
        .map(AxisDirection::from_str)
        .unwrap_or(AxisDirection::Other)
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
                .and_then(|factor| LinearUnit::from_meters_per_unit(factor).ok())
        })
        .or_else(|| {
            axis.get("conversion_factor")
                .and_then(Value::as_f64)
                .and_then(|factor| LinearUnit::from_meters_per_unit(factor).ok())
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

    if let Some(factor) = value.get("conversion_factor").and_then(Value::as_f64) {
        return LinearUnit::from_meters_per_unit(factor).ok();
    }
    if let Some(factor) = value.get("unit_conversion_factor").and_then(Value::as_f64) {
        return LinearUnit::from_meters_per_unit(factor).ok();
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

fn linear_unit_name(name: &str) -> Option<LinearUnit> {
    match normalize_key(name).as_str() {
        "metre" | "meter" => Some(LinearUnit::metre()),
        "kilometre" | "kilometer" => Some(LinearUnit::kilometre()),
        "foot" | "internationalfoot" | "ft" => Some(LinearUnit::foot()),
        "ussurveyfoot" | "usfoot" | "usft" => Some(LinearUnit::us_survey_foot()),
        "yard" => LinearUnit::from_meters_per_unit(0.9144).ok(),
        "nauticalmile" => LinearUnit::from_meters_per_unit(1852.0).ok(),
        _ => None,
    }
}

fn angle_unit_name_to_degree(name: &str) -> Option<f64> {
    match normalize_key(name).as_str() {
        "degree" => Some(1.0),
        "radian" => Some(radians_to_degrees_factor(1.0)),
        "grad" | "gon" => Some(0.9),
        _ => None,
    }
}

fn radians_to_degrees_factor(radians_per_unit: f64) -> f64 {
    radians_per_unit.to_degrees()
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
        assert_eq!(crs.datum().ellipsoid.a, proj_core::datum::WGS84.ellipsoid.a);
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
    fn rejects_projjson_custom_datum_even_when_other_names_match() {
        let err = parse_projjson(
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
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("unsupported PROJJSON datum or CRS definition"));
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

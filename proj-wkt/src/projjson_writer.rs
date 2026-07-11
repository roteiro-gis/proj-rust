//! PROJJSON serializer for [`CrsDef`] values.
//!
//! Emission is scoped to what `CrsDef` represents: geographic, projected,
//! and compound CRSs over the supported projection methods, with EPSG
//! identities preserved wherever the definition carries them. Everything the
//! serializer emits reparses through [`crate::parse_crs`]; the roundtrip is
//! asserted by tests and fuzzing.

use proj_core::{
    CompoundCrsDef, CrsDef, Datum, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    VerticalCrsDef, VerticalCrsKind,
};
use serde_json::{json, Map, Value};

use crate::wkt_writer::{
    datum_wkt, projection_parameters, projection_wkt_name, vertical_datum_name, ParameterKind,
};
use crate::{ParseError, Result};

const PROJJSON_SCHEMA: &str = "https://proj.org/schemas/v0.7/projjson.schema.json";

pub(crate) fn to_projjson_value(crs: &CrsDef) -> Result<Value> {
    let mut value = match crs {
        CrsDef::Geographic(geographic) => geographic_crs_value(geographic, None)?,
        CrsDef::Projected(projected) => projected_crs_value(projected)?,
        CrsDef::Compound(compound) => compound_crs_value(compound)?,
    };
    if let Some(object) = value.as_object_mut() {
        let mut with_schema = Map::new();
        with_schema.insert("$schema".into(), Value::String(PROJJSON_SCHEMA.into()));
        with_schema.append(object);
        return Ok(Value::Object(with_schema));
    }
    Ok(value)
}

fn compound_crs_value(compound: &CompoundCrsDef) -> Result<Value> {
    match compound.vertical_crs().kind() {
        VerticalCrsKind::EllipsoidalHeight { .. } => {
            // A geographic CRS with an ellipsoidal-height component is a 3D
            // geographic CRS in PROJJSON terms.
            let HorizontalCrsDef::Geographic(geographic) = compound.horizontal() else {
                return Err(ParseError::UnsupportedSemantics(
                    "PROJJSON serialization of a projected CRS with an ellipsoidal-height \
                     component is not supported"
                        .into(),
                ));
            };
            let mut value =
                geographic_crs_value(geographic, Some(compound.vertical_crs().linear_unit()))?;
            let object = value.as_object_mut().expect("CRS values are objects");
            object.insert(
                "name".into(),
                Value::String(nonempty_name(compound.name(), "unnamed compound CRS")),
            );
            insert_id(object, compound.epsg());
            Ok(value)
        }
        VerticalCrsKind::GravityRelatedHeight { .. } => {
            let horizontal = match compound.horizontal() {
                HorizontalCrsDef::Geographic(geographic) => geographic_crs_value(geographic, None)?,
                HorizontalCrsDef::Projected(projected) => projected_crs_value(projected)?,
            };
            let vertical = vertical_crs_value(compound.vertical_crs())?;
            let mut object = Map::new();
            object.insert("type".into(), Value::String("CompoundCRS".into()));
            object.insert(
                "name".into(),
                Value::String(nonempty_name(compound.name(), "unnamed compound CRS")),
            );
            object.insert(
                "components".into(),
                Value::Array(vec![horizontal, vertical]),
            );
            insert_id(&mut object, compound.epsg());
            Ok(Value::Object(object))
        }
    }
}

fn geographic_crs_value(
    geographic: &GeographicCrsDef,
    ellipsoidal_height_unit: Option<LinearUnit>,
) -> Result<Value> {
    let mut axes = vec![
        json!({
            "name": "Geodetic longitude",
            "abbreviation": "Lon",
            "direction": "east",
            "unit": "degree",
        }),
        json!({
            "name": "Geodetic latitude",
            "abbreviation": "Lat",
            "direction": "north",
            "unit": "degree",
        }),
    ];
    if let Some(unit) = ellipsoidal_height_unit {
        axes.push(json!({
            "name": "Ellipsoidal height",
            "abbreviation": "h",
            "direction": "up",
            "unit": linear_unit_value(unit),
        }));
    }

    let mut object = Map::new();
    object.insert("type".into(), Value::String("GeographicCRS".into()));
    object.insert(
        "name".into(),
        Value::String(nonempty_name(geographic.name(), "unnamed geographic CRS")),
    );
    object.insert(
        "datum".into(),
        datum_value(
            geographic.datum(),
            authority_code(geographic.epsg()).and_then(proj_core::lookup_datum_code_for_crs),
        )?,
    );
    object.insert(
        "coordinate_system".into(),
        json!({ "subtype": "ellipsoidal", "axis": axes }),
    );
    insert_id(&mut object, geographic.epsg());
    Ok(Value::Object(object))
}

fn projected_crs_value(projected: &ProjectedCrsDef) -> Result<Value> {
    let base_epsg = authority_code(projected.base_geographic_crs_epsg());
    let base_name = base_epsg
        .and_then(proj_core::lookup_epsg)
        .map(|crs| crs.name().to_string())
        .unwrap_or_else(|| format!("{} base geographic CRS", projected.name()));
    let base = GeographicCrsDef::new(
        projected.base_geographic_crs_epsg(),
        projected.datum().clone(),
        "",
    );
    let mut base_value = geographic_crs_value(&base, None)?;
    if let Some(object) = base_value.as_object_mut() {
        object.insert(
            "name".into(),
            Value::String(nonempty_name(&base_name, "unnamed base geographic CRS")),
        );
    }

    let method = projected.method();
    let method_name = projection_wkt_name(method);
    let parameters = projection_parameters(method)
        .into_iter()
        .map(|param| {
            let (value, unit) = match param.kind {
                ParameterKind::Angle => (param.value, Value::String("degree".into())),
                ParameterKind::Length => (
                    projected.linear_unit().from_meters(param.value),
                    linear_unit_value(projected.linear_unit()),
                ),
                ParameterKind::Scale => (param.value, Value::String("unity".into())),
            };
            json!({ "name": param.name, "value": value, "unit": unit })
        })
        .collect::<Vec<_>>();

    let mut object = Map::new();
    object.insert("type".into(), Value::String("ProjectedCRS".into()));
    object.insert(
        "name".into(),
        Value::String(nonempty_name(projected.name(), "unnamed projected CRS")),
    );
    object.insert("base_crs".into(), base_value);
    object.insert(
        "conversion".into(),
        json!({
            "name": "unnamed",
            "method": { "name": method_name },
            "parameters": parameters,
        }),
    );
    object.insert(
        "coordinate_system".into(),
        json!({
            "subtype": "Cartesian",
            "axis": [
                {
                    "name": "Easting",
                    "abbreviation": "E",
                    "direction": "east",
                    "unit": linear_unit_value(projected.linear_unit()),
                },
                {
                    "name": "Northing",
                    "abbreviation": "N",
                    "direction": "north",
                    "unit": linear_unit_value(projected.linear_unit()),
                },
            ],
        }),
    );
    insert_id(&mut object, projected.epsg());
    Ok(Value::Object(object))
}

fn vertical_crs_value(vertical: &VerticalCrsDef) -> Result<Value> {
    let VerticalCrsKind::GravityRelatedHeight {
        vertical_datum_epsg,
    } = vertical.kind()
    else {
        return Err(ParseError::UnsupportedSemantics(
            "PROJJSON vertical components must be gravity-related heights".into(),
        ));
    };

    let mut datum = Map::new();
    datum.insert(
        "type".into(),
        Value::String("VerticalReferenceFrame".into()),
    );
    datum.insert(
        "name".into(),
        Value::String(vertical_datum_name(*vertical_datum_epsg).to_string()),
    );
    insert_id(&mut datum, *vertical_datum_epsg);

    let mut object = Map::new();
    object.insert("type".into(), Value::String("VerticalCRS".into()));
    object.insert(
        "name".into(),
        Value::String(nonempty_name(vertical.name(), "unnamed vertical CRS")),
    );
    object.insert("datum".into(), Value::Object(datum));
    object.insert(
        "coordinate_system".into(),
        json!({
            "subtype": "vertical",
            "axis": [{
                "name": "Gravity-related height",
                "abbreviation": "H",
                "direction": "up",
                "unit": linear_unit_value(vertical.linear_unit()),
            }],
        }),
    );
    insert_id(&mut object, vertical.epsg());
    Ok(Value::Object(object))
}

fn datum_value(datum: &Datum, datum_epsg: Option<u32>) -> Result<Value> {
    let info = datum_wkt(datum, datum_epsg)?;
    let mut ellipsoid = Map::new();
    ellipsoid.insert("name".into(), Value::String(info.ellipsoid.name.clone()));
    ellipsoid.insert(
        "semi_major_axis".into(),
        number(info.ellipsoid.semi_major_axis)?,
    );
    ellipsoid.insert(
        "inverse_flattening".into(),
        number(info.ellipsoid.inverse_flattening)?,
    );
    if let Some(code) = info.ellipsoid.epsg {
        insert_id(&mut ellipsoid, code);
    }

    let mut object = Map::new();
    object.insert(
        "type".into(),
        Value::String("GeodeticReferenceFrame".into()),
    );
    object.insert("name".into(), Value::String(info.name.clone()));
    object.insert("ellipsoid".into(), Value::Object(ellipsoid));
    if let Some(code) = info.datum_epsg {
        insert_id(&mut object, code);
    }
    Ok(Value::Object(object))
}

fn linear_unit_value(unit: LinearUnit) -> Value {
    if unit.meters_per_unit() == 1.0 {
        return Value::String("metre".into());
    }
    json!({
        "type": "LinearUnit",
        "name": "unit",
        "conversion_factor": unit.meters_per_unit(),
    })
}

fn number(value: f64) -> Result<Value> {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| {
            ParseError::Parse(format!(
                "PROJJSON serialization requires finite numbers, got {value}"
            ))
        })
}

fn nonempty_name(name: &str, fallback: &str) -> String {
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

fn authority_code(code: u32) -> Option<u32> {
    (code != 0).then_some(code)
}

fn insert_id(object: &mut Map<String, Value>, code: u32) {
    if code != 0 {
        object.insert("id".into(), json!({ "authority": "EPSG", "code": code }));
    }
}

#[cfg(test)]
mod tests {
    use crate::{parse_crs, to_projjson};

    /// Every supported projection method plus geographic, compound-gravity,
    /// and compound-ellipsoidal cases must survive parse→emit→parse.
    #[test]
    fn registry_definitions_roundtrip_through_projjson() {
        let codes = [
            4326,  // geographic
            4258,  // geographic (ETRS89)
            3857,  // Web Mercator
            32618, // transverse Mercator
            3413,  // polar stereographic
            2154,  // Lambert conformal conic
            5070,  // Albers
            3035,  // LAEA
            3408,  // spherical LAEA
            28992, // oblique stereographic
            3078,  // Hotine variant A
            2056,  // Hotine variant B
            30200, // Cassini-Soldner
            3395,  // Mercator
            32662, // equidistant cylindrical
            6247,  // Colombia Urban
            24200, // Lambert conformal conic 1SP
            6201,  // Lambert conformal conic 2SP Michigan
            9549,  // Lambert conformal conic 1SP variant B
            5514,  // Krovak East North
            5516,  // Modified Krovak East North
            7415,  // compound projected + gravity height
            7678,  // compound geographic + ellipsoidal height
        ];
        for code in codes {
            let original = parse_crs(&format!("EPSG:{code}")).unwrap();
            let json = to_projjson(&original)
                .unwrap_or_else(|e| panic!("EPSG:{code}: serialization failed: {e}"));
            let reparsed = parse_crs(&json)
                .unwrap_or_else(|e| panic!("EPSG:{code}: reparse failed: {e}\n{json}"));
            assert_eq!(
                format!("{original:?}"),
                format!("{reparsed:?}"),
                "EPSG:{code}: roundtrip changed the definition\n{json}"
            );
        }
    }

    #[test]
    fn custom_definition_without_ids_roundtrips() {
        // A custom UTM-like CRS with no EPSG identity anywhere must roundtrip
        // through the full-body (non-canonicalized) parse path.
        let wkt = r#"PROJCS["custom",GEOGCS["custom geographic",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
        let original = parse_crs(wkt).unwrap();
        let json = to_projjson(&original).unwrap();
        let reparsed = parse_crs(&json).unwrap_or_else(|e| panic!("reparse failed: {e}\n{json}"));
        assert!(
            original.semantically_equivalent(&reparsed),
            "roundtrip changed the definition\n{json}"
        );
    }
}

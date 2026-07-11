//! WKT2 (ISO 19162) serializer for [`CrsDef`] values.
//!
//! Emission is scoped to what `CrsDef` represents and shares the WKT1
//! serializer's method/parameter/datum mapping so the dialects stay in sync.
//! Everything the serializer emits reparses through [`crate::parse_crs`];
//! the roundtrip is asserted by tests and fuzzing.

use proj_core::{
    CompoundCrsDef, CrsDef, Datum, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    VerticalCrsDef, VerticalCrsKind,
};

use crate::wkt_writer::{
    datum_wkt, format_f64, linear_unit_wkt, projection_parameters, projection_wkt_name, quote,
    vertical_datum_name, ParameterKind,
};
use crate::{ParseError, Result};

const DEGREE_TO_RADIAN: &str = "0.0174532925199433";

pub(crate) fn to_wkt2(crs: &CrsDef) -> Result<String> {
    match crs {
        CrsDef::Geographic(geographic) => geographic_wkt2(geographic, None, None, None),
        CrsDef::Projected(projected) => projected_wkt2(projected),
        CrsDef::Compound(compound) => compound_wkt2(compound),
    }
}

fn compound_wkt2(compound: &CompoundCrsDef) -> Result<String> {
    match compound.vertical_crs().kind() {
        VerticalCrsKind::EllipsoidalHeight { .. } => {
            // A geographic CRS with an ellipsoidal-height component is a
            // three-axis geographic CRS in WKT2 terms.
            let HorizontalCrsDef::Geographic(geographic) = compound.horizontal() else {
                return Err(ParseError::UnsupportedSemantics(
                    "WKT2 serialization of a projected CRS with an ellipsoidal-height component \
                     is not supported"
                        .into(),
                ));
            };
            geographic_wkt2(
                geographic,
                Some(compound.vertical_crs().linear_unit()),
                Some(compound.name()),
                authority_code(compound.epsg()),
            )
        }
        VerticalCrsKind::GravityRelatedHeight { .. } => {
            let horizontal = match compound.horizontal() {
                HorizontalCrsDef::Geographic(geographic) => {
                    geographic_wkt2(geographic, None, None, None)?
                }
                HorizontalCrsDef::Projected(projected) => projected_wkt2(projected)?,
            };
            let vertical = vertical_wkt2(compound.vertical_crs())?;
            let mut fields = vec![quote(nonempty(compound.name(), "unnamed compound CRS"))];
            fields.push(horizontal);
            fields.push(vertical);
            push_id(&mut fields, compound.epsg());
            Ok(format!("COMPOUNDCRS[{}]", fields.join(",")))
        }
    }
}

fn geographic_wkt2(
    geographic: &GeographicCrsDef,
    ellipsoidal_height_unit: Option<LinearUnit>,
    name_override: Option<&str>,
    id_override: Option<u32>,
) -> Result<String> {
    let name = name_override.unwrap_or_else(|| geographic.name());
    let datum_epsg =
        authority_code(geographic.epsg()).and_then(proj_core::lookup_datum_code_for_crs);

    let mut fields = vec![quote(nonempty(name, "unnamed geographic CRS"))];
    fields.push(format_wkt2_datum(geographic.datum(), datum_epsg)?);
    if let Some(height_unit) = ellipsoidal_height_unit {
        fields.push("CS[ellipsoidal,3]".to_string());
        fields.push(format!(
            "AXIS[\"longitude\",east,ORDER[1],{}]",
            angle_unit_wkt2()
        ));
        fields.push(format!(
            "AXIS[\"latitude\",north,ORDER[2],{}]",
            angle_unit_wkt2()
        ));
        fields.push(format!(
            "AXIS[\"ellipsoidal height\",up,ORDER[3],{}]",
            length_unit_wkt2(height_unit)?
        ));
    } else {
        fields.push("CS[ellipsoidal,2]".to_string());
        fields.push(format!(
            "AXIS[\"longitude\",east,ORDER[1],{}]",
            angle_unit_wkt2()
        ));
        fields.push(format!(
            "AXIS[\"latitude\",north,ORDER[2],{}]",
            angle_unit_wkt2()
        ));
    }
    match id_override {
        Some(code) => push_id(&mut fields, code),
        None => push_id(&mut fields, geographic.epsg()),
    }
    Ok(format!("GEOGCRS[{}]", fields.join(",")))
}

fn projected_wkt2(projected: &ProjectedCrsDef) -> Result<String> {
    let base_epsg = authority_code(projected.base_geographic_crs_epsg());
    let base_name = base_epsg
        .and_then(proj_core::lookup_epsg)
        .map(|crs| crs.name().to_string())
        .unwrap_or_else(|| format!("{} base geographic CRS", projected.name()));
    let datum_epsg = base_epsg
        .or_else(|| authority_code(projected.epsg()))
        .and_then(proj_core::lookup_datum_code_for_crs);

    let mut base_fields = vec![quote(nonempty(&base_name, "unnamed base geographic CRS"))];
    base_fields.push(format_wkt2_datum(projected.datum(), datum_epsg)?);
    if let Some(code) = base_epsg {
        base_fields.push(format_id(code));
    }
    let base = format!("BASEGEOGCRS[{}]", base_fields.join(","));

    let method = projected.method();
    let mut conversion_fields = vec![
        quote("unnamed"),
        format!("METHOD[{}]", quote(projection_wkt_name(method))),
    ];
    for param in projection_parameters(method) {
        let (value, unit) = match param.kind {
            ParameterKind::Angle => (param.value, angle_unit_wkt2()),
            ParameterKind::Length => (
                projected.linear_unit().from_meters(param.value),
                length_unit_wkt2(projected.linear_unit())?,
            ),
            ParameterKind::Scale => (param.value, "SCALEUNIT[\"unity\",1]".to_string()),
        };
        conversion_fields.push(format!(
            "PARAMETER[{},{},{}]",
            quote(param.name),
            format_f64(value),
            unit
        ));
    }
    let conversion = format!("CONVERSION[{}]", conversion_fields.join(","));

    let length_unit = length_unit_wkt2(projected.linear_unit())?;
    let mut fields = vec![quote(nonempty(projected.name(), "unnamed projected CRS"))];
    fields.push(base);
    fields.push(conversion);
    fields.push("CS[Cartesian,2]".to_string());
    fields.push(format!("AXIS[\"easting\",east,ORDER[1],{length_unit}]"));
    fields.push(format!("AXIS[\"northing\",north,ORDER[2],{length_unit}]"));
    push_id(&mut fields, projected.epsg());
    Ok(format!("PROJCRS[{}]", fields.join(",")))
}

fn vertical_wkt2(vertical: &VerticalCrsDef) -> Result<String> {
    let VerticalCrsKind::GravityRelatedHeight {
        vertical_datum_epsg,
    } = vertical.kind()
    else {
        return Err(ParseError::UnsupportedSemantics(
            "WKT2 vertical components must be gravity-related heights".into(),
        ));
    };

    let mut datum_fields = vec![quote(vertical_datum_name(*vertical_datum_epsg))];
    if *vertical_datum_epsg != 0 {
        datum_fields.push(format_id(*vertical_datum_epsg));
    }

    let mut fields = vec![quote(nonempty(vertical.name(), "unnamed vertical CRS"))];
    fields.push(format!("VDATUM[{}]", datum_fields.join(",")));
    fields.push("CS[vertical,1]".to_string());
    fields.push(format!(
        "AXIS[\"gravity-related height\",up,{}]",
        length_unit_wkt2(vertical.linear_unit())?
    ));
    push_id(&mut fields, vertical.epsg());
    Ok(format!("VERTCRS[{}]", fields.join(",")))
}

fn format_wkt2_datum(datum: &Datum, datum_epsg: Option<u32>) -> Result<String> {
    let info = datum_wkt(datum, datum_epsg)?;
    let mut ellipsoid_fields = vec![
        quote(&info.ellipsoid.name),
        format_f64(info.ellipsoid.semi_major_axis),
        format_f64(info.ellipsoid.inverse_flattening),
        "LENGTHUNIT[\"metre\",1]".to_string(),
    ];
    if let Some(code) = info.ellipsoid.epsg {
        ellipsoid_fields.push(format_id(code));
    }

    let mut fields = vec![
        quote(&info.name),
        format!("ELLIPSOID[{}]", ellipsoid_fields.join(",")),
    ];
    if let Some(code) = info.datum_epsg {
        fields.push(format_id(code));
    }
    Ok(format!("DATUM[{}]", fields.join(",")))
}

fn angle_unit_wkt2() -> String {
    format!("ANGLEUNIT[\"degree\",{DEGREE_TO_RADIAN}]")
}

fn length_unit_wkt2(unit: LinearUnit) -> Result<String> {
    let info = linear_unit_wkt(unit)?;
    Ok(format!("LENGTHUNIT[{},{}]", quote(info.name), info.factor))
}

fn format_id(code: u32) -> String {
    format!("ID[\"EPSG\",{code}]")
}

fn push_id(fields: &mut Vec<String>, code: u32) {
    if code != 0 {
        fields.push(format_id(code));
    }
}

fn nonempty<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    if name.is_empty() {
        fallback
    } else {
        name
    }
}

fn authority_code(code: u32) -> Option<u32> {
    (code != 0).then_some(code)
}

#[cfg(test)]
mod tests {
    use crate::{parse_crs, to_wkt2};

    /// Every supported projection method plus geographic, compound-gravity,
    /// and compound-ellipsoidal cases must survive parse→emit→parse.
    #[test]
    fn registry_definitions_roundtrip_through_wkt2() {
        let codes = [
            4326, 4258, 3857, 32618, 3413, 2154, 5070, 3035, 3408, 28992, 3078, 2056, 30200, 3395,
            32662, 6247, 24200, 6201, 9549, 5514, 5516, 8857, 5880, 27701, 3295, 3993, 7415, 7678,
        ];
        for code in codes {
            let original = parse_crs(&format!("EPSG:{code}")).unwrap();
            let wkt2 = to_wkt2(&original)
                .unwrap_or_else(|e| panic!("EPSG:{code}: serialization failed: {e}"));
            let reparsed = parse_crs(&wkt2)
                .unwrap_or_else(|e| panic!("EPSG:{code}: reparse failed: {e}\n{wkt2}"));
            assert_eq!(
                format!("{original:?}"),
                format!("{reparsed:?}"),
                "EPSG:{code}: roundtrip changed the definition\n{wkt2}"
            );
        }
    }

    #[test]
    fn custom_definition_without_ids_roundtrips() {
        let wkt = r#"PROJCS["custom",GEOGCS["custom geographic",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",-75],PARAMETER["scale_factor",0.9996],PARAMETER["false_easting",500000],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
        let original = parse_crs(wkt).unwrap();
        let wkt2 = to_wkt2(&original).unwrap();
        let reparsed = parse_crs(&wkt2).unwrap_or_else(|e| panic!("reparse failed: {e}\n{wkt2}"));
        assert!(
            original.semantically_equivalent(&reparsed),
            "roundtrip changed the definition\n{wkt2}"
        );
    }
}

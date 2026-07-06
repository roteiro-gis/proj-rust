use proj_core::{
    CompoundCrsDef, CrsDef, Datum, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    ProjectionMethod, VerticalCrsDef, VerticalCrsKind,
};

use crate::{ParseError, Result};

const DEGREE_TO_RADIAN: &str = "0.0174532925199433";
const EPSG_AUTHORITY: &str = "EPSG";

pub(crate) fn to_wkt(crs: &CrsDef) -> Result<String> {
    match crs {
        CrsDef::Geographic(geographic) => format_geographic_crs(geographic),
        CrsDef::Projected(projected) => format_projected_crs(projected),
        CrsDef::Compound(compound) => format_compound_crs(compound),
    }
}

fn format_compound_crs(compound: &CompoundCrsDef) -> Result<String> {
    let horizontal = format_horizontal_crs(compound.horizontal())?;
    let vertical = format_vertical_crs(compound.vertical_crs())?;
    let mut fields = vec![quote(wkt_name(compound.name(), "unnamed compound CRS"))];
    fields.push(horizontal);
    fields.push(vertical);
    push_authority(&mut fields, compound.epsg());
    Ok(format!("COMPD_CS[{}]", fields.join(",")))
}

fn format_horizontal_crs(horizontal: &HorizontalCrsDef) -> Result<String> {
    match horizontal {
        HorizontalCrsDef::Geographic(geographic) => format_geographic_crs(geographic),
        HorizontalCrsDef::Projected(projected) => format_projected_crs(projected),
    }
}

fn format_geographic_crs(geographic: &GeographicCrsDef) -> Result<String> {
    format_geographic_crs_parts(
        wkt_name(geographic.name(), "unnamed geographic CRS"),
        authority_code(geographic.epsg()),
        geographic.datum(),
        authority_code(geographic.epsg()),
    )
}

fn format_geographic_crs_parts(
    name: &str,
    crs_epsg: Option<u32>,
    datum: &Datum,
    datum_source_crs_epsg: Option<u32>,
) -> Result<String> {
    let datum_epsg = datum_source_crs_epsg.and_then(proj_core::lookup_datum_code_for_crs);
    let mut fields = vec![quote(name)];
    fields.push(format_datum(datum, datum_epsg)?);
    fields.push(format_prime_meridian());
    fields.push(format_angle_unit());
    if let Some(epsg) = crs_epsg {
        fields.push(format_authority(epsg));
    }
    Ok(format!("GEOGCS[{}]", fields.join(",")))
}

fn format_projected_crs(projected: &ProjectedCrsDef) -> Result<String> {
    let linear_unit = linear_unit_wkt(projected.linear_unit())?;
    let mut fields = vec![quote(wkt_name(projected.name(), "unnamed projected CRS"))];
    fields.push(format_base_geographic_crs(projected)?);
    fields.push(format_projection(projected.method())?);
    fields.extend(format_projection_parameters(
        projected.method(),
        projected.linear_unit(),
    )?);
    fields.push(format_linear_unit(&linear_unit));
    push_authority(&mut fields, projected.epsg());
    Ok(format!("PROJCS[{}]", fields.join(",")))
}

fn format_base_geographic_crs(projected: &ProjectedCrsDef) -> Result<String> {
    let base_epsg = authority_code(projected.base_geographic_crs_epsg());
    let base_name = base_epsg
        .and_then(proj_core::lookup_epsg)
        .map(|crs| crs.name().to_string())
        .unwrap_or_else(|| format!("{} base geographic CRS", projected.name()));
    let datum_source_crs_epsg = base_epsg.or_else(|| authority_code(projected.epsg()));

    format_geographic_crs_parts(
        wkt_name(&base_name, "unnamed base geographic CRS"),
        base_epsg,
        projected.datum(),
        datum_source_crs_epsg,
    )
}

fn format_projection(method: ProjectionMethod) -> Result<String> {
    Ok(format!(
        "PROJECTION[{}]",
        quote(projection_wkt_name(method))
    ))
}

fn format_projection_parameters(
    method: ProjectionMethod,
    linear_unit: LinearUnit,
) -> Result<Vec<String>> {
    let method_name = projection_wkt_name(method);
    let mut params = projection_parameters(method);
    for param in &mut params {
        param.method = method_name;
    }
    params
        .into_iter()
        .map(|param| format_parameter(param, linear_unit))
        .collect()
}

fn projection_wkt_name(method: ProjectionMethod) -> &'static str {
    match method {
        ProjectionMethod::WebMercator => "Popular_Visualisation_Pseudo_Mercator",
        ProjectionMethod::TransverseMercator { .. } => "Transverse_Mercator",
        ProjectionMethod::PolarStereographic { .. } => "Polar_Stereographic",
        ProjectionMethod::LambertConformalConic { lat1, lat2, .. } => {
            if approx_eq(lat1, lat2) {
                "Lambert_Conformal_Conic_1SP"
            } else {
                "Lambert_Conformal_Conic_2SP"
            }
        }
        ProjectionMethod::AlbersEqualArea { .. } => "Albers_Equal_Area",
        ProjectionMethod::LambertAzimuthalEqualArea { .. } => "Lambert_Azimuthal_Equal_Area",
        ProjectionMethod::LambertAzimuthalEqualAreaSpherical { .. } => {
            "Lambert_Azimuthal_Equal_Area_Spherical"
        }
        ProjectionMethod::ObliqueStereographic { .. } => "Oblique_Stereographic",
        ProjectionMethod::HotineObliqueMercator {
            variant_b: true, ..
        } => "Hotine_Oblique_Mercator_Variant_B",
        ProjectionMethod::HotineObliqueMercator {
            variant_b: false, ..
        } => "Hotine_Oblique_Mercator",
        ProjectionMethod::CassiniSoldner { .. } => "Cassini_Soldner",
        ProjectionMethod::Mercator { lat_ts, .. } => {
            if approx_eq(lat_ts, 0.0) {
                "Mercator_1SP"
            } else {
                "Mercator_2SP"
            }
        }
        ProjectionMethod::EquidistantCylindrical { .. } => "Equidistant_Cylindrical",
    }
}

fn projection_parameters(method: ProjectionMethod) -> Vec<ProjectionParam> {
    match method {
        ProjectionMethod::WebMercator => vec![
            angle_param("central_meridian", 0.0),
            length_param("false_easting", 0.0),
            length_param("false_northing", 0.0),
        ],
        ProjectionMethod::TransverseMercator {
            lon0,
            lat0,
            k0,
            false_easting,
            false_northing,
        } => vec![
            angle_param("latitude_of_origin", lat0),
            angle_param("central_meridian", lon0),
            scale_param("scale_factor", k0),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::PolarStereographic {
            lon0,
            lat_ts,
            k0,
            false_easting,
            false_northing,
        } => vec![
            angle_param(
                "latitude_of_origin",
                if lat_ts.is_sign_negative() {
                    -90.0
                } else {
                    90.0
                },
            ),
            angle_param("central_meridian", lon0),
            angle_param("standard_parallel", lat_ts),
            scale_param("scale_factor", k0),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::LambertConformalConic {
            lon0,
            lat0,
            lat1,
            lat2,
            false_easting,
            false_northing,
        } => vec![
            angle_param("latitude_of_origin", lat0),
            angle_param("central_meridian", lon0),
            angle_param("standard_parallel_1", lat1),
            angle_param("standard_parallel_2", lat2),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::AlbersEqualArea {
            lon0,
            lat0,
            lat1,
            lat2,
            false_easting,
            false_northing,
        } => vec![
            angle_param("latitude_of_origin", lat0),
            angle_param("central_meridian", lon0),
            angle_param("standard_parallel_1", lat1),
            angle_param("standard_parallel_2", lat2),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::LambertAzimuthalEqualArea {
            lon0,
            lat0,
            false_easting,
            false_northing,
        }
        | ProjectionMethod::LambertAzimuthalEqualAreaSpherical {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => vec![
            angle_param("latitude_of_origin", lat0),
            angle_param("central_meridian", lon0),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::ObliqueStereographic {
            lon0,
            lat0,
            k0,
            false_easting,
            false_northing,
        } => vec![
            angle_param("latitude_of_origin", lat0),
            angle_param("central_meridian", lon0),
            scale_param("scale_factor", k0),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::HotineObliqueMercator {
            latc,
            lonc,
            azimuth,
            rectified_grid_angle,
            k0,
            false_easting,
            false_northing,
            variant_b,
        } => {
            let (lat_name, lon_name, easting_name, northing_name) = if variant_b {
                (
                    "latitude_of_projection_center",
                    "longitude_of_projection_center",
                    "easting_at_projection_center",
                    "northing_at_projection_center",
                )
            } else {
                (
                    "latitude_of_center",
                    "longitude_of_center",
                    "false_easting",
                    "false_northing",
                )
            };
            vec![
                angle_param(lat_name, latc),
                angle_param(lon_name, lonc),
                angle_param("azimuth", azimuth),
                angle_param("rectified_grid_angle", rectified_grid_angle),
                scale_param("scale_factor", k0),
                length_param(easting_name, false_easting),
                length_param(northing_name, false_northing),
            ]
        }
        ProjectionMethod::CassiniSoldner {
            lon0,
            lat0,
            false_easting,
            false_northing,
        } => vec![
            angle_param("latitude_of_origin", lat0),
            angle_param("central_meridian", lon0),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
        ProjectionMethod::Mercator {
            lon0,
            lat_ts,
            k0,
            false_easting,
            false_northing,
        } => {
            let mut params = vec![angle_param("central_meridian", lon0)];
            if !approx_eq(lat_ts, 0.0) {
                params.push(angle_param("standard_parallel_1", lat_ts));
            }
            params.extend([
                scale_param("scale_factor", k0),
                length_param("false_easting", false_easting),
                length_param("false_northing", false_northing),
            ]);
            params
        }
        ProjectionMethod::EquidistantCylindrical {
            lon0,
            lat_ts,
            false_easting,
            false_northing,
        } => vec![
            angle_param("central_meridian", lon0),
            angle_param("standard_parallel_1", lat_ts),
            length_param("false_easting", false_easting),
            length_param("false_northing", false_northing),
        ],
    }
}

fn format_parameter(param: ProjectionParam, linear_unit: LinearUnit) -> Result<String> {
    let method = param.method;
    let value = checked_number(method, param.name, param.value)?;
    let value = match param.kind {
        ParameterKind::Length => {
            checked_number(method, param.name, linear_unit.from_meters(value))?
        }
        ParameterKind::Angle | ParameterKind::Scale => value,
    };
    Ok(format!(
        "PARAMETER[{},{}]",
        quote(param.name),
        format_f64(value)
    ))
}

fn format_vertical_crs(vertical: &VerticalCrsDef) -> Result<String> {
    let linear_unit = linear_unit_wkt(vertical.linear_unit())?;
    let mut fields = vec![quote(wkt_name(vertical.name(), "unnamed vertical CRS"))];
    fields.push(format_vertical_datum(vertical)?);
    fields.push(format_linear_unit(&linear_unit));
    fields.push(
        match vertical.kind() {
            VerticalCrsKind::EllipsoidalHeight { .. } => r#"AXIS["Ellipsoidal height",UP]"#,
            VerticalCrsKind::GravityRelatedHeight { .. } => r#"AXIS["Gravity-related height",UP]"#,
        }
        .to_string(),
    );
    push_authority(&mut fields, vertical.epsg());
    Ok(format!("VERT_CS[{}]", fields.join(",")))
}

fn format_vertical_datum(vertical: &VerticalCrsDef) -> Result<String> {
    match vertical.kind() {
        VerticalCrsKind::GravityRelatedHeight {
            vertical_datum_epsg,
        } => {
            let mut fields = vec![quote(vertical_datum_name(*vertical_datum_epsg))];
            fields.push("2005".to_string());
            push_authority(&mut fields, *vertical_datum_epsg);
            Ok(format!("VERT_DATUM[{}]", fields.join(",")))
        }
        VerticalCrsKind::EllipsoidalHeight { datum } => {
            let info = datum_wkt(datum, None)?;
            let mut fields = vec![quote(&info.name)];
            fields.push("2002".to_string());
            if let Some(code) = info.datum_epsg {
                fields.push(format_authority(code));
            }
            Ok(format!("VERT_DATUM[{}]", fields.join(",")))
        }
    }
}

fn format_datum(datum: &Datum, datum_epsg: Option<u32>) -> Result<String> {
    let info = datum_wkt(datum, datum_epsg)?;
    let mut fields = vec![quote(&info.name), format_spheroid(&info.ellipsoid)?];
    if let Some(code) = info.datum_epsg {
        fields.push(format_authority(code));
    }
    Ok(format!("DATUM[{}]", fields.join(",")))
}

fn format_spheroid(ellipsoid: &EllipsoidWkt) -> Result<String> {
    let mut fields = vec![
        quote(&ellipsoid.name),
        format_f64(checked_number(
            "SPHEROID",
            "semi_major_axis",
            ellipsoid.semi_major_axis,
        )?),
        format_f64(checked_number(
            "SPHEROID",
            "inverse_flattening",
            ellipsoid.inverse_flattening,
        )?),
    ];
    if let Some(code) = ellipsoid.epsg {
        fields.push(format_authority(code));
    }
    Ok(format!("SPHEROID[{}]", fields.join(",")))
}

fn format_prime_meridian() -> String {
    r#"PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]]"#.to_string()
}

fn format_angle_unit() -> String {
    format!(
        r#"UNIT["degree",{DEGREE_TO_RADIAN},{}]"#,
        format_authority(9122)
    )
}

fn format_linear_unit(unit: &LinearUnitWkt) -> String {
    let mut fields = vec![quote(unit.name), unit.factor.clone()];
    if let Some(code) = unit.epsg {
        fields.push(format_authority(code));
    }
    format!("UNIT[{}]", fields.join(","))
}

fn format_authority(code: u32) -> String {
    format!(r#"AUTHORITY["{EPSG_AUTHORITY}","{code}"]"#)
}

fn push_authority(fields: &mut Vec<String>, code: u32) {
    if code != 0 {
        fields.push(format_authority(code));
    }
}

fn authority_code(code: u32) -> Option<u32> {
    (code != 0).then_some(code)
}

fn datum_wkt(datum: &Datum, datum_epsg: Option<u32>) -> Result<DatumWkt> {
    if let Some(code) = datum_epsg {
        if let Some(known) = known_datum(code) {
            return Ok(known.with_datum(datum));
        }

        let ellipsoid_epsg = proj_core::lookup_ellipsoid_code_for_datum(code);
        return Ok(DatumWkt {
            name: format!("EPSG datum {code}"),
            datum_epsg: Some(code),
            ellipsoid: ellipsoid_wkt(datum.ellipsoid(), ellipsoid_epsg),
        });
    }

    for known in KNOWN_DATUMS {
        if datum.same_datum(&known.datum) {
            return Ok(known.with_datum(datum));
        }
    }

    Ok(DatumWkt {
        name: "Unknown datum".to_string(),
        datum_epsg: None,
        ellipsoid: ellipsoid_wkt(datum.ellipsoid(), None),
    })
}

fn ellipsoid_wkt(ellipsoid: proj_core::Ellipsoid, epsg: Option<u32>) -> EllipsoidWkt {
    if let Some(code) = epsg {
        if let Some(known) = known_ellipsoid(code) {
            return known.with_ellipsoid(ellipsoid);
        }
        return EllipsoidWkt {
            name: format!("EPSG ellipsoid {code}"),
            epsg: Some(code),
            semi_major_axis: ellipsoid.semi_major_axis(),
            inverse_flattening: ellipsoid.inverse_flattening(),
        };
    }

    for known in KNOWN_ELLIPSOIDS {
        if ellipsoid_matches(ellipsoid, known.ellipsoid) {
            return known.with_ellipsoid(ellipsoid);
        }
    }

    EllipsoidWkt {
        name: "Unknown ellipsoid".to_string(),
        epsg: None,
        semi_major_axis: ellipsoid.semi_major_axis(),
        inverse_flattening: ellipsoid.inverse_flattening(),
    }
}

fn linear_unit_wkt(unit: LinearUnit) -> Result<LinearUnitWkt> {
    let factor = checked_number("linear unit", "meters_per_unit", unit.meters_per_unit())?;
    if approx_eq(factor, 1.0) {
        return Ok(LinearUnitWkt {
            name: "metre",
            factor: format_f64(factor),
            epsg: Some(9001),
        });
    }
    if approx_eq(factor, 0.3048) {
        return Ok(LinearUnitWkt {
            name: "foot",
            factor: format_f64(factor),
            epsg: Some(9002),
        });
    }
    if approx_eq(factor, 0.3048006096012192) {
        return Ok(LinearUnitWkt {
            name: "US survey foot",
            factor: format_f64(factor),
            epsg: Some(9003),
        });
    }
    if approx_eq(factor, 1000.0) {
        return Ok(LinearUnitWkt {
            name: "kilometre",
            factor: format_f64(factor),
            epsg: Some(9036),
        });
    }

    Ok(LinearUnitWkt {
        name: "unknown",
        factor: format_f64(factor),
        epsg: None,
    })
}

fn vertical_datum_name(code: u32) -> &'static str {
    match code {
        1027 => "EGM2008 geoid",
        5102 => "National Geodetic Vertical Datum 1929",
        5103 => "North American Vertical Datum 1988",
        5109 => "Normaal Amsterdams Peil",
        5171 => "EGM96 geoid",
        _ => "EPSG vertical datum",
    }
}

fn wkt_name<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    if name.trim().is_empty() {
        fallback
    } else {
        name
    }
}

fn quote(value: &str) -> String {
    format!(r#""{}""#, value.replace('"', "\"\""))
}

fn checked_number(method: &str, name: &str, value: f64) -> Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ParseError::UnsupportedSemantics(format!(
            "cannot emit WKT for {method}: parameter {name} must be finite"
        )))
    }
}

fn format_f64(value: f64) -> String {
    if value == 0.0 {
        "0".to_string()
    } else if (value - value.round()).abs() <= 1e-9 {
        format!("{:.0}", value.round())
    } else {
        value.to_string()
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}

fn ellipsoid_matches(left: proj_core::Ellipsoid, right: proj_core::Ellipsoid) -> bool {
    approx_eq(left.semi_major_axis(), right.semi_major_axis())
        && approx_eq(left.flattening(), right.flattening())
}

fn angle_param(name: &'static str, value: f64) -> ProjectionParam {
    ProjectionParam::new(name, value, ParameterKind::Angle)
}

fn length_param(name: &'static str, value: f64) -> ProjectionParam {
    ProjectionParam::new(name, value, ParameterKind::Length)
}

fn scale_param(name: &'static str, value: f64) -> ProjectionParam {
    ProjectionParam::new(name, value, ParameterKind::Scale)
}

#[derive(Clone, Copy)]
struct ProjectionParam {
    method: &'static str,
    name: &'static str,
    value: f64,
    kind: ParameterKind,
}

impl ProjectionParam {
    fn new(name: &'static str, value: f64, kind: ParameterKind) -> Self {
        Self {
            method: "projection",
            name,
            value,
            kind,
        }
    }
}

#[derive(Clone, Copy)]
enum ParameterKind {
    Angle,
    Length,
    Scale,
}

struct DatumWkt {
    name: String,
    datum_epsg: Option<u32>,
    ellipsoid: EllipsoidWkt,
}

#[derive(Clone)]
struct DatumTemplate {
    datum_epsg: u32,
    name: &'static str,
    datum: Datum,
    ellipsoid: EllipsoidTemplate,
}

impl DatumTemplate {
    fn with_datum(&self, datum: &Datum) -> DatumWkt {
        DatumWkt {
            name: self.name.to_string(),
            datum_epsg: Some(self.datum_epsg),
            ellipsoid: self.ellipsoid.with_ellipsoid(datum.ellipsoid()),
        }
    }
}

#[derive(Clone, Copy)]
struct EllipsoidTemplate {
    epsg: u32,
    name: &'static str,
    ellipsoid: proj_core::Ellipsoid,
}

impl EllipsoidTemplate {
    fn with_ellipsoid(self, ellipsoid: proj_core::Ellipsoid) -> EllipsoidWkt {
        EllipsoidWkt {
            name: self.name.to_string(),
            epsg: Some(self.epsg),
            semi_major_axis: ellipsoid.semi_major_axis(),
            inverse_flattening: ellipsoid.inverse_flattening(),
        }
    }
}

struct EllipsoidWkt {
    name: String,
    epsg: Option<u32>,
    semi_major_axis: f64,
    inverse_flattening: f64,
}

struct LinearUnitWkt {
    name: &'static str,
    factor: String,
    epsg: Option<u32>,
}

fn known_datum(code: u32) -> Option<&'static DatumTemplate> {
    KNOWN_DATUMS.iter().find(|datum| datum.datum_epsg == code)
}

fn known_ellipsoid(code: u32) -> Option<&'static EllipsoidTemplate> {
    KNOWN_ELLIPSOIDS
        .iter()
        .find(|ellipsoid| ellipsoid.epsg == code)
}

const WGS84_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7030,
    name: "WGS 84",
    ellipsoid: proj_core::ellipsoid::WGS84,
};
const GRS80_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7019,
    name: "GRS 1980",
    ellipsoid: proj_core::ellipsoid::GRS80,
};
const CLARKE1866_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7008,
    name: "Clarke 1866",
    ellipsoid: proj_core::ellipsoid::CLARKE1866,
};
const AIRY1830_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7001,
    name: "Airy 1830",
    ellipsoid: proj_core::ellipsoid::AIRY1830,
};
const INTERNATIONAL1924_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7022,
    name: "International 1924",
    ellipsoid: proj_core::ellipsoid::INTL1924,
};
const KRASSOWSKY1940_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7024,
    name: "Krassowsky 1940",
    ellipsoid: proj_core::ellipsoid::KRASSOWSKY,
};
const BESSEL1841_ELLIPSOID: EllipsoidTemplate = EllipsoidTemplate {
    epsg: 7004,
    name: "Bessel 1841",
    ellipsoid: proj_core::ellipsoid::BESSEL1841,
};

const KNOWN_ELLIPSOIDS: &[EllipsoidTemplate] = &[
    WGS84_ELLIPSOID,
    GRS80_ELLIPSOID,
    CLARKE1866_ELLIPSOID,
    AIRY1830_ELLIPSOID,
    INTERNATIONAL1924_ELLIPSOID,
    KRASSOWSKY1940_ELLIPSOID,
    BESSEL1841_ELLIPSOID,
];

const KNOWN_DATUMS: &[DatumTemplate] = &[
    DatumTemplate {
        datum_epsg: 6326,
        name: "WGS_1984",
        datum: proj_core::datum::WGS84,
        ellipsoid: WGS84_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6269,
        name: "North_American_Datum_1983",
        datum: proj_core::datum::NAD83,
        ellipsoid: GRS80_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6267,
        name: "North_American_Datum_1927",
        datum: proj_core::datum::NAD27,
        ellipsoid: CLARKE1866_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6258,
        name: "European_Terrestrial_Reference_System_1989",
        datum: proj_core::datum::ETRS89,
        ellipsoid: GRS80_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6277,
        name: "OSGB_1936",
        datum: proj_core::datum::OSGB36,
        ellipsoid: AIRY1830_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6230,
        name: "European_Datum_1950",
        datum: proj_core::datum::ED50,
        ellipsoid: INTERNATIONAL1924_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6284,
        name: "Pulkovo_1942",
        datum: proj_core::datum::PULKOVO1942,
        ellipsoid: KRASSOWSKY1940_ELLIPSOID,
    },
    DatumTemplate {
        datum_epsg: 6301,
        name: "Tokyo",
        datum: proj_core::datum::TOKYO,
        ellipsoid: BESSEL1841_ELLIPSOID,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use proj_core::{
        datum, CompoundCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef, ProjectionMethod,
    };

    #[test]
    fn emits_geographic_wkt_with_authorities() {
        let crs = proj_core::lookup_epsg(4326).expect("EPSG:4326");

        let wkt = to_wkt(&crs).unwrap();

        assert!(wkt.starts_with(r#"GEOGCS["WGS 84""#), "{wkt}");
        assert!(wkt.contains(r#"DATUM["WGS_1984""#), "{wkt}");
        assert!(wkt.contains(r#"AUTHORITY["EPSG","6326"]"#), "{wkt}");
        assert!(
            wkt.contains(r#"SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]]"#),
            "{wkt}"
        );
        assert!(
            wkt.contains(r#"PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]]"#),
            "{wkt}"
        );
        assert!(
            wkt.contains(r#"UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]]"#),
            "{wkt}"
        );
        assert!(wkt.ends_with(r#"AUTHORITY["EPSG","4326"]]"#), "{wkt}");
        assert_round_trips(&crs, &wkt);
    }

    #[test]
    fn emits_web_mercator_as_pseudo_mercator() {
        let crs = proj_core::lookup_epsg(3857).expect("EPSG:3857");

        let wkt = to_wkt(&crs).unwrap();

        assert!(
            wkt.starts_with(r#"PROJCS["WGS 84 / Pseudo-Mercator""#),
            "{wkt}"
        );
        assert!(
            wkt.contains(r#"PROJECTION["Popular_Visualisation_Pseudo_Mercator"]"#),
            "{wkt}"
        );
        assert_round_trips(&crs, &wkt);
    }

    #[test]
    fn emits_us_survey_foot_projected_units() {
        let crs = proj_core::lookup_epsg(2264).expect("EPSG:2264");

        let wkt = to_wkt(&crs).unwrap();

        assert!(
            wkt.contains(r#"UNIT["US survey foot","#)
                && wkt.contains(r#"AUTHORITY["EPSG","9003"]"#),
            "{wkt}"
        );
        assert_round_trips(&crs, &wkt);
    }

    #[test]
    fn emits_international_foot_projected_units() {
        let crs = CrsDef::Projected(ProjectedCrsDef::new_with_base_geographic_crs(
            0,
            4326,
            datum::WGS84,
            ProjectionMethod::TransverseMercator {
                lon0: -75.0,
                lat0: 0.0,
                k0: 0.9996,
                false_easting: 304_800.0,
                false_northing: 0.0,
            },
            LinearUnit::foot(),
            "Custom international foot TM",
        ));

        let wkt = to_wkt(&crs).unwrap();

        assert!(
            wkt.contains(r#"UNIT["foot",0.3048,AUTHORITY["EPSG","9002"]]"#),
            "{wkt}"
        );
        assert!(
            wkt.contains(r#"PARAMETER["false_easting",1000000]"#),
            "{wkt}"
        );
        assert_round_trips(&crs, &wkt);
    }

    #[test]
    fn emits_compound_wkt_with_navd88_vertical_crs() {
        let horizontal = match proj_core::lookup_epsg(4326).expect("EPSG:4326") {
            CrsDef::Geographic(geographic) => HorizontalCrsDef::Geographic(geographic),
            _ => panic!("EPSG:4326 should be geographic"),
        };
        let vertical = proj_core::lookup_vertical_epsg(5703).expect("EPSG:5703");
        let crs = CrsDef::Compound(Box::new(CompoundCrsDef::new(
            0,
            horizontal,
            vertical,
            "WGS 84 + NAVD88 height",
        )));

        let wkt = to_wkt(&crs).unwrap();

        assert!(
            wkt.starts_with(r#"COMPD_CS["WGS 84 + NAVD88 height""#),
            "{wkt}"
        );
        assert!(wkt.contains(r#"VERT_CS["NAVD88 height""#), "{wkt}");
        assert!(
            wkt.contains(
                r#"VERT_DATUM["North American Vertical Datum 1988",2005,AUTHORITY["EPSG","5103"]]"#
            ),
            "{wkt}"
        );
        assert!(wkt.contains(r#"AUTHORITY["EPSG","5703"]"#), "{wkt}");
        assert_round_trips(&crs, &wkt);
    }

    #[test]
    fn emits_compound_wkt_for_ellipsoidal_height() {
        let crs = proj_core::lookup_epsg(4979).expect("EPSG:4979");

        let wkt = to_wkt(&crs).unwrap();

        assert!(wkt.starts_with(r#"COMPD_CS["WGS 84""#), "{wkt}");
        assert!(
            wkt.contains(r#"VERT_DATUM["WGS_1984",2002,AUTHORITY["EPSG","6326"]]"#),
            "{wkt}"
        );
        assert_round_trips(&crs, &wkt);
    }

    #[test]
    fn emits_all_projection_method_variants() {
        for (label, method) in projection_method_examples() {
            let crs = CrsDef::Projected(ProjectedCrsDef::new_with_base_geographic_crs(
                0,
                4326,
                datum::WGS84,
                method,
                LinearUnit::metre(),
                label,
            ));

            let wkt = to_wkt(&crs).unwrap_or_else(|err| panic!("{label}: {err}"));
            assert!(
                wkt.contains("PROJECTION["),
                "{label}: missing projection in {wkt}"
            );
            assert_round_trips(&crs, &wkt);
        }
    }

    fn assert_round_trips(expected: &CrsDef, wkt: &str) {
        let parsed = crate::parse_crs(wkt).unwrap_or_else(|err| panic!("{err}\n{wkt}"));
        assert!(
            expected.semantically_equivalent(&parsed),
            "round-trip mismatch\nexpected: {expected:?}\nparsed: {parsed:?}\nwkt: {wkt}"
        );
    }

    fn projection_method_examples() -> Vec<(&'static str, ProjectionMethod)> {
        vec![
            ("Web Mercator", ProjectionMethod::WebMercator),
            (
                "Transverse Mercator",
                ProjectionMethod::TransverseMercator {
                    lon0: -75.0,
                    lat0: 0.0,
                    k0: 0.9996,
                    false_easting: 500_000.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Polar Stereographic",
                ProjectionMethod::PolarStereographic {
                    lon0: -45.0,
                    lat_ts: 70.0,
                    k0: 1.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Lambert Conformal Conic 1SP",
                ProjectionMethod::LambertConformalConic {
                    lon0: -96.0,
                    lat0: 33.0,
                    lat1: 33.0,
                    lat2: 33.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Lambert Conformal Conic 2SP",
                ProjectionMethod::LambertConformalConic {
                    lon0: -96.0,
                    lat0: 23.0,
                    lat1: 33.0,
                    lat2: 45.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Albers Equal Area",
                ProjectionMethod::AlbersEqualArea {
                    lon0: -96.0,
                    lat0: 23.0,
                    lat1: 29.5,
                    lat2: 45.5,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Lambert Azimuthal Equal Area",
                ProjectionMethod::LambertAzimuthalEqualArea {
                    lon0: 10.0,
                    lat0: 52.0,
                    false_easting: 4_321_000.0,
                    false_northing: 3_210_000.0,
                },
            ),
            (
                "Lambert Azimuthal Equal Area Spherical",
                ProjectionMethod::LambertAzimuthalEqualAreaSpherical {
                    lon0: 0.0,
                    lat0: 0.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Oblique Stereographic",
                ProjectionMethod::ObliqueStereographic {
                    lon0: 5.38763888888889,
                    lat0: 52.1561605555556,
                    k0: 0.9999079,
                    false_easting: 155_000.0,
                    false_northing: 463_000.0,
                },
            ),
            (
                "Hotine Oblique Mercator A",
                ProjectionMethod::HotineObliqueMercator {
                    latc: 4.0,
                    lonc: 115.0,
                    azimuth: 53.3158204722222,
                    rectified_grid_angle: 53.1301023611111,
                    k0: 0.99984,
                    false_easting: 590_476.87,
                    false_northing: 442_857.65,
                    variant_b: false,
                },
            ),
            (
                "Hotine Oblique Mercator B",
                ProjectionMethod::HotineObliqueMercator {
                    latc: 4.0,
                    lonc: 115.0,
                    azimuth: 53.3158204722222,
                    rectified_grid_angle: 53.1301023611111,
                    k0: 0.99984,
                    false_easting: 590_476.87,
                    false_northing: 442_857.65,
                    variant_b: true,
                },
            ),
            (
                "Cassini-Soldner",
                ProjectionMethod::CassiniSoldner {
                    lon0: 103.833333333333,
                    lat0: 1.36666666666667,
                    false_easting: 30_000.0,
                    false_northing: 30_000.0,
                },
            ),
            (
                "Mercator 1SP",
                ProjectionMethod::Mercator {
                    lon0: 0.0,
                    lat_ts: 0.0,
                    k0: 1.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Mercator 2SP",
                ProjectionMethod::Mercator {
                    lon0: 0.0,
                    lat_ts: 42.0,
                    k0: 1.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
            (
                "Equidistant Cylindrical",
                ProjectionMethod::EquidistantCylindrical {
                    lon0: 0.0,
                    lat_ts: 30.0,
                    false_easting: 0.0,
                    false_northing: 0.0,
                },
            ),
        ]
    }
}

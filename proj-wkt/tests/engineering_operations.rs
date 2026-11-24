//! Engineering CRSs and the operations that position them, through WKT2,
//! PROJJSON, and the transform helpers.

use proj_core::{
    lookup_epsg, OperationMethod, RotationConvention, SelectionOptions, Similarity3DParams,
    Transform,
};
use proj_wkt::{
    bound_crs_to_projjson, bound_crs_to_wkt2, operation_to_projjson, operation_to_wkt2,
    parse_bound_crs, parse_crs, parse_operation, to_wkt2, transform_from_crs_strings,
    transform_from_crs_strings_horizontal, OperationDefinition, Proj,
};

const SITE_GRID: &str = r#"ENGCRS["Site grid Example",
    EDATUM["Site grid Example"],
    CS[Cartesian,3],
        AXIS["site east (x)",east,ORDER[1],LENGTHUNIT["metre",1]],
        AXIS["site north (y)",north,ORDER[2],LENGTHUNIT["metre",1]],
        AXIS["height (h)",up,ORDER[3],LENGTHUNIT["metre",1]],
    ID["GEORIZON",12]]"#;

/// The plan's appendix positioning, with RD + NAP written out in full.
fn appendix_operation_wkt() -> String {
    let rd_nap = to_wkt2(&lookup_epsg(7415).unwrap()).unwrap();
    format!(
        r#"COORDINATEOPERATION["Site grid Example to RD + NAP",
    VERSION["3"],
    SOURCECRS[{SITE_GRID}],
    TARGETCRS[{rd_nap}],
    METHOD["Similarity transformation 3D, position vector",ID["GEORIZON",1]],
    PARAMETER["X-axis translation",90512.334,LENGTHUNIT["metre",1]],
    PARAMETER["Y-axis translation",435870.112,LENGTHUNIT["metre",1]],
    PARAMETER["Z-axis translation",-3.418,LENGTHUNIT["metre",1]],
    PARAMETER["X-axis rotation",-12.6,ANGLEUNIT["arc-second",4.84813681109536E-06]],
    PARAMETER["Y-axis rotation",31.4,ANGLEUNIT["arc-second",4.84813681109536E-06]],
    PARAMETER["Z-axis rotation",133200,ANGLEUNIT["arc-second",4.84813681109536E-06]],
    PARAMETER["Scale difference",-41.7,SCALEUNIT["parts per million",1E-06]],
    OPERATIONACCURACY[0.03],
    ID["GEORIZON",1047]]"#
    )
}

fn appendix_params() -> Similarity3DParams {
    Similarity3DParams::new(
        [90512.334, 435870.112, -3.418],
        [-12.6, 31.4, 133200.0],
        -41.7,
        RotationConvention::PositionVector,
    )
    .unwrap()
}

fn params(operation: &OperationDefinition) -> Similarity3DParams {
    match &operation.method {
        OperationMethod::Similarity3D { params } => *params,
        other => panic!("unexpected method {other:?}"),
    }
}

fn assert_params_close(actual: Similarity3DParams, expected: Similarity3DParams) {
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * b.abs().max(1.0);
    assert_eq!(actual.convention(), expected.convention());
    let values = |params: Similarity3DParams| {
        let mut values = params.translation().to_vec();
        values.extend(params.rotation_arcsec());
        values.push(params.scale_ppm());
        values
    };
    for (a, b) in values(actual).into_iter().zip(values(expected)) {
        assert!(close(a, b), "{actual:?} vs {expected:?}");
    }
}

/// `cct +proj=helmert +convention=position_vector +exact` of (1000, 2000, 10).
const CCT_POINT: (f64, f64, f64) = (90107.3578789594, 438069.1069518616, 6.5089042981);

fn assert_cct_point(actual: (f64, f64, f64)) {
    assert!(
        (actual.0 - CCT_POINT.0).abs() < 1e-8
            && (actual.1 - CCT_POINT.1).abs() < 1e-8
            && (actual.2 - CCT_POINT.2).abs() < 1e-8,
        "{actual:?}"
    );
}

#[test]
fn parses_the_appendix_operation() {
    let operation = parse_operation(&appendix_operation_wkt()).unwrap();
    assert_eq!(operation.name, "Site grid Example to RD + NAP");
    assert_eq!(operation.version.as_deref(), Some("3"));
    assert_eq!(
        operation.method_id.as_ref().unwrap().to_string(),
        "GEORIZON:1"
    );
    assert_eq!(operation.id.as_ref().unwrap().to_string(), "GEORIZON:1047");
    assert_eq!(operation.accuracy, Some(0.03));
    assert_eq!(
        operation.source_crs.identifier().unwrap().to_string(),
        "GEORIZON:12"
    );
    assert_eq!(operation.target_crs.epsg(), 7415);
    assert_eq!(params(&operation), appendix_params());

    let coordinate_operation = operation.coordinate_operation().unwrap();
    assert_eq!(coordinate_operation.target_crs_epsg, Some(7415));
    assert_eq!(
        coordinate_operation
            .source_crs_id
            .as_ref()
            .unwrap()
            .to_string(),
        "GEORIZON:12"
    );
    let transform = Transform::from_crs_defs_with_selection_options(
        &operation.source_crs,
        &operation.target_crs,
        SelectionOptions::new().with_coordinate_operation(coordinate_operation),
    )
    .unwrap();
    assert_cct_point(transform.convert_3d((1000.0, 2000.0, 10.0)).unwrap());
}

#[test]
fn operation_roundtrips_through_wkt2_and_projjson() {
    let operation = parse_operation(&appendix_operation_wkt()).unwrap();
    let wkt2 = operation_to_wkt2(&operation).unwrap();
    assert!(wkt2.starts_with(
        "COORDINATEOPERATION[\"Site grid Example to RD + NAP\",VERSION[\"3\"],SOURCECRS[ENGCRS["
    ));
    assert!(wkt2.contains(
        "METHOD[\"Similarity transformation 3D, position vector\",ID[\"GEORIZON\",1]],\
         PARAMETER[\"X-axis translation\",90512.334,LENGTHUNIT[\"metre\",1]]"
    ));
    assert!(wkt2.contains(
        "PARAMETER[\"Z-axis rotation\",133200,ANGLEUNIT[\"arc-second\",4.84813681109536E-06]]"
    ));
    assert!(wkt2.ends_with(
        "PARAMETER[\"Scale difference\",-41.7,SCALEUNIT[\"parts per million\",1E-06]],\
         OPERATIONACCURACY[0.03],ID[\"GEORIZON\",1047]]"
    ));

    let projjson = operation_to_projjson(&operation).unwrap();
    for text in [wkt2, projjson] {
        let reparsed = parse_operation(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(params(&reparsed), appendix_params(), "{text}");
        assert_eq!(reparsed.name, operation.name);
        // PROJJSON has no operation version.
        if text.starts_with("COORDINATEOPERATION") {
            assert_eq!(reparsed.version, operation.version);
        } else {
            assert_eq!(reparsed.version, None);
        }
        assert_eq!(reparsed.method_id, operation.method_id);
        assert_eq!(reparsed.id, operation.id);
        assert_eq!(reparsed.accuracy, operation.accuracy);
        assert!(reparsed
            .source_crs
            .semantically_equivalent(&operation.source_crs));
        assert_eq!(reparsed.target_crs.epsg(), 7415);
    }
}

#[test]
fn coordinate_frame_operations_keep_their_convention() {
    let wkt = appendix_operation_wkt().replace("position vector", "coordinate frame");
    let operation = parse_operation(&wkt).unwrap();
    assert_eq!(
        params(&operation).convention(),
        RotationConvention::CoordinateFrame
    );
    let reparsed = parse_operation(&operation_to_wkt2(&operation).unwrap()).unwrap();
    assert_eq!(params(&reparsed), params(&operation));
}

#[test]
fn reads_parameters_in_other_units() {
    let wkt = appendix_operation_wkt()
        .replace(
            "PARAMETER[\"Z-axis rotation\",133200,ANGLEUNIT[\"arc-second\",4.84813681109536E-06]]",
            "PARAMETER[\"Z-axis rotation\",37,ANGLEUNIT[\"degree\",0.0174532925199433]]",
        )
        .replace(
            "PARAMETER[\"X-axis translation\",90512.334,LENGTHUNIT[\"metre\",1]]",
            "PARAMETER[\"X-axis translation\",90.512334,LENGTHUNIT[\"kilometre\",1000]]",
        )
        .replace(
            "PARAMETER[\"Scale difference\",-41.7,SCALEUNIT[\"parts per million\",1E-06]]",
            "PARAMETER[\"Scale difference\",-0.0000417,SCALEUNIT[\"unity\",1]]",
        );
    assert_params_close(params(&parse_operation(&wkt).unwrap()), appendix_params());
}

#[test]
fn rejects_operations_it_cannot_run() {
    let wkt = appendix_operation_wkt();
    let cases = [
        wkt.replace(
            "Similarity transformation 3D",
            "Affine parametric transformation",
        ),
        wkt.replace(
            "PARAMETER[\"Scale difference\",-41.7,SCALEUNIT[\"parts per million\",1E-06]],",
            "",
        ),
        wkt.replace(
            "PARAMETER[\"X-axis rotation\",-12.6,ANGLEUNIT",
            "PARAMETER[\"X-axis rotation\",-12.6,LENGTHUNIT",
        ),
        wkt.replace(
            "X-axis translation\",90512.334",
            "Easting offset\",90512.334",
        ),
    ];
    for case in cases {
        assert!(parse_operation(&case).is_err(), "{case}");
    }
}

#[test]
fn bound_crs_carries_the_frame_and_its_operation() {
    let operation = parse_operation(&appendix_operation_wkt()).unwrap();
    let wkt2 = bound_crs_to_wkt2(&operation).unwrap();
    assert!(wkt2.starts_with("BOUNDCRS[SOURCECRS[ENGCRS["));
    assert!(wkt2.contains(
        "ABRIDGEDTRANSFORMATION[\"Site grid Example to RD + NAP\",VERSION[\"3\"],\
         METHOD[\"Similarity transformation 3D, position vector\",ID[\"GEORIZON\",1]],\
         PARAMETER[\"X-axis translation\",90512.334]"
    ));
    assert!(wkt2.contains("PARAMETER[\"Scale difference\",0.9999583]"));
    let projjson = bound_crs_to_projjson(&operation).unwrap();

    for text in [wkt2, projjson] {
        let bound = parse_bound_crs(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_params_close(params(&bound), appendix_params());
        assert_eq!(bound.target_crs.epsg(), 7415);

        // parse_crs reads the bound CRS as the frame.
        let frame = parse_crs(&text).unwrap();
        assert!(frame.semantically_equivalent(&operation.source_crs));

        // The transform helpers run the operation it carries.
        let transform = transform_from_crs_strings(&text, "EPSG:7415").unwrap();
        assert_cct_point(transform.convert_3d((1000.0, 2000.0, 10.0)).unwrap());
        let back = transform_from_crs_strings("EPSG:7415", &text).unwrap();
        let roundtrip = back.convert_3d(CCT_POINT).unwrap();
        assert!((roundtrip.0 - 1000.0).abs() < 1e-8 && (roundtrip.2 - 10.0).abs() < 1e-8);

        let to_wgs84 = transform_from_crs_strings_horizontal(&text, "EPSG:4326").unwrap();
        let (lon, lat) = to_wgs84.convert((1000.0, 2000.0)).unwrap();
        assert!(
            (4.0..5.0).contains(&lon) && (51.0..53.0).contains(&lat),
            "{lon} {lat}"
        );

        let source = Proj::new(&text).unwrap();
        let target = Proj::new("EPSG:7415").unwrap();
        let facade = source
            .create_crs_to_crs_from_pj(&target, None, None)
            .unwrap();
        assert_cct_point(facade.convert_3d((1000.0, 2000.0, 10.0)).unwrap());
    }
}

#[test]
fn abridged_parameters_take_no_units() {
    let operation = parse_operation(&appendix_operation_wkt()).unwrap();
    let wkt2 = bound_crs_to_wkt2(&operation).unwrap().replace(
        "PARAMETER[\"X-axis translation\",90512.334]",
        "PARAMETER[\"X-axis translation\",90512.334,LENGTHUNIT[\"metre\",1]]",
    );
    assert!(parse_bound_crs(&wkt2).is_err());
}

#[test]
fn a_frame_without_its_bound_operation_reaches_only_itself() {
    assert!(transform_from_crs_strings(SITE_GRID, "EPSG:7415").is_err());
    let identity = transform_from_crs_strings(SITE_GRID, SITE_GRID).unwrap();
    assert_eq!(
        identity.convert_3d((1.0, 2.0, 3.0)).unwrap(),
        (1.0, 2.0, 3.0)
    );
}

#[test]
fn a_declared_shift_applies_from_the_frames_base_crs() {
    let mut operation = parse_operation(&appendix_operation_wkt()).unwrap();
    operation.target_crs = lookup_epsg(32631).unwrap();
    operation.method = OperationMethod::Similarity3D {
        params: Similarity3DParams::new(
            [500000.0, 5700000.0, 0.0],
            [0.0, 0.0, 3600.0],
            0.0,
            RotationConvention::PositionVector,
        )
        .unwrap(),
    };
    let bound = bound_crs_to_wkt2(&operation).unwrap();
    let intl = "+proj=utm +zone=31 +ellps=intl +towgs84=-87,-98,-121 +units=m";

    let through_frame = transform_from_crs_strings_horizontal(&bound, intl).unwrap();
    let from_base = transform_from_crs_strings_horizontal("EPSG:32631", intl).unwrap();
    let base = params(&operation).forward([100.0, 200.0, 0.0]);
    assert_eq!(
        through_frame.convert((100.0, 200.0)).unwrap(),
        from_base.convert((base[0], base[1])).unwrap()
    );
}

#[test]
fn projjson_parameters_need_units_of_their_kind() {
    let operation = parse_operation(&appendix_operation_wkt()).unwrap();
    let json = operation_to_projjson(&operation).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["parameters"][0]["unit"] = serde_json::json!({
        "type": "AngularUnit",
        "name": "arc-second",
        "conversion_factor": 4.84813681109536e-6,
    });
    assert!(parse_operation(&value.to_string()).is_err());
    value["parameters"][0]["unit"] = serde_json::json!("arc-second");
    assert!(parse_operation(&value.to_string()).is_err());
    value["parameters"][0]["unit"] = serde_json::json!("metre");
    value["parameters"][3]["unit"] = serde_json::json!("metre");
    assert!(parse_operation(&value.to_string()).is_err());
}

#[test]
fn bound_scale_multipliers_are_written_exactly() {
    let mut operation = parse_operation(&appendix_operation_wkt()).unwrap();
    operation.method = OperationMethod::Similarity3D {
        params: Similarity3DParams::new(
            [90512.334, 435870.112, -3.418],
            [-12.6, 31.4, 133200.0],
            0.0004,
            RotationConvention::PositionVector,
        )
        .unwrap(),
    };
    let bound = parse_bound_crs(&bound_crs_to_wkt2(&operation).unwrap()).unwrap();
    assert!((params(&bound).scale_ppm() - 0.0004).abs() < 1e-9);
}

//! Transforms with an engineering CRS at either end.

use proj_core::{
    lookup_epsg, AreaOfInterest, AxisOrder, Bounds, Coord, CoordinateOperation, CrsDef,
    EngineeringCrsDef, Identifier, LinearUnit, OperationDomain, OperationMatchKind,
    OperationMethod, RotationConvention, SelectionOptions, Similarity3DParams, Transform,
};

fn site_grid() -> CrsDef {
    CrsDef::Engineering(Box::new(
        EngineeringCrsDef::new("Site grid Example", "Site grid Example", 3)
            .unwrap()
            .with_identifier(Identifier::new("GEORIZON", "12").unwrap()),
    ))
}

fn other_site_grid() -> CrsDef {
    CrsDef::Engineering(Box::new(
        EngineeringCrsDef::new("Other site", "Other site", 3)
            .unwrap()
            .with_identifier(Identifier::new("GEORIZON", "13").unwrap()),
    ))
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

/// `GEORIZON:12` to RD + NAP, the plan's appendix positioning.
fn positioning(frame: &str, params: Similarity3DParams) -> CoordinateOperation {
    CoordinateOperation {
        id: None,
        name: format!("{frame} to RD + NAP"),
        source_crs_epsg: None,
        target_crs_epsg: Some(7415),
        source_crs_id: Some(Identifier::new("GEORIZON", frame).unwrap()),
        target_crs_id: None,
        source_datum_epsg: None,
        target_datum_epsg: None,
        accuracy: Some(proj_core::OperationAccuracy { meters: 0.03 }),
        areas_of_use: Default::default(),
        deprecated: false,
        preferred: true,
        approximate: false,
        superseded: false,
        domain: OperationDomain::IncludesHeight,
        method: OperationMethod::Similarity3D { params },
    }
}

fn options() -> SelectionOptions {
    SelectionOptions::new().with_coordinate_operation(positioning("12", appendix_params()))
}

fn epsg(code: u32) -> CrsDef {
    lookup_epsg(code).unwrap()
}

fn assert_close3(actual: (f64, f64, f64), expected: (f64, f64, f64), tolerance: f64) {
    let delta = (
        (actual.0 - expected.0).abs(),
        (actual.1 - expected.1).abs(),
        (actual.2 - expected.2).abs(),
    );
    assert!(
        delta.0 < tolerance && delta.1 < tolerance && delta.2 < tolerance,
        "expected {expected:?}, got {actual:?}"
    );
}

#[test]
fn frame_to_its_base_crs_runs_the_similarity() {
    let transform =
        Transform::from_crs_defs_with_selection_options(&site_grid(), &epsg(7415), options())
            .unwrap();

    // cct +proj=helmert +convention=position_vector +exact (appendix values).
    let actual = transform.convert_3d((1000.0, 2000.0, 10.0)).unwrap();
    assert_close3(
        actual,
        (90107.3578789594, 438069.1069518616, 6.5089042981),
        1e-9,
    );

    assert!(transform.source_crs().is_engineering());
    assert_eq!(transform.target_crs().epsg(), 7415);
    let selected = transform.selected_operation();
    assert_eq!(selected.name, "12 to RD + NAP");
    assert_eq!(selected.target_crs_epsg, Some(7415));
    assert_eq!(selected.accuracy.unwrap().meters, 0.03);
    assert_eq!(
        transform.selection_diagnostics().selected_match_kind,
        OperationMatchKind::Custom
    );

    let back = transform.inverse().unwrap();
    assert!(back.target_crs().is_engineering());
    assert_close3(
        back.convert_3d(actual).unwrap(),
        (1000.0, 2000.0, 10.0),
        1e-9,
    );
    assert_eq!(back.selected_operation().name, "12 to RD + NAP");
    assert_eq!(back.selected_operation().source_crs_epsg, Some(7415));
}

#[test]
fn base_crs_to_frame_is_the_inverse_similarity() {
    let transform =
        Transform::from_crs_defs_with_selection_options(&epsg(7415), &site_grid(), options())
            .unwrap();
    let actual = transform.convert_3d((91000.0, 436500.0, 5.0)).unwrap();
    assert_close3(actual, (768.5741526816, 209.5753048870, 8.5310714247), 1e-9);
}

#[test]
fn an_operation_stated_towards_the_frame_positions_it_too() {
    let mut reversed = positioning("12", appendix_params());
    std::mem::swap(&mut reversed.source_crs_epsg, &mut reversed.target_crs_epsg);
    std::mem::swap(&mut reversed.source_crs_id, &mut reversed.target_crs_id);
    let transform = Transform::from_crs_defs_with_selection_options(
        &site_grid(),
        &epsg(7415),
        SelectionOptions::new().with_coordinate_operation(reversed),
    )
    .unwrap();
    // The frame is the operation's target, so frame to RD is its inverse.
    assert_close3(
        transform.convert_3d((91000.0, 436500.0, 5.0)).unwrap(),
        (768.5741526816, 209.5753048870, 8.5310714247),
        1e-8,
    );
}

#[test]
fn frame_to_another_crs_continues_with_the_registry_transform() {
    let frame_to_wgs84 = Transform::from_horizontal_components_with_selection_options(
        &site_grid(),
        &epsg(4326),
        options(),
    )
    .unwrap();
    let rd_to_wgs84 = Transform::new("EPSG:28992", "EPSG:4326").unwrap();

    for (x, y) in [(0.0, 0.0), (1000.0, 2000.0), (-350.25, 812.5)] {
        let rd = appendix_params().forward([x, y, 0.0]);
        let expected = rd_to_wgs84.convert((rd[0], rd[1])).unwrap();
        let actual = frame_to_wgs84.convert((x, y)).unwrap();
        assert_eq!(actual, expected);
    }
    assert!(frame_to_wgs84
        .selected_operation()
        .name
        .starts_with("12 to RD + NAP + "));
    assert_eq!(
        frame_to_wgs84.selected_operation().target_crs_epsg,
        rd_to_wgs84.selected_operation().target_crs_epsg
    );

    // The inverse runs the registry's inverse, then finds the frame point.
    // RD to WGS 84 and back drops heights around its Helmert and is itself
    // 0.65 mm off in 2D here, so that is the bound.
    let back = frame_to_wgs84.inverse().unwrap();
    let wgs84 = frame_to_wgs84.convert((123.0, 456.0)).unwrap();
    let roundtrip = back.convert(wgs84).unwrap();
    assert!(
        (roundtrip.0 - 123.0).abs() < 1e-3 && (roundtrip.1 - 456.0).abs() < 1e-3,
        "{roundtrip:?}"
    );
}

#[test]
fn two_dimensional_conversions_to_a_tilted_frame_invert_exactly() {
    let tilted = Similarity3DParams::new(
        [90000.0, 435000.0, 50.0],
        [7200.0, -5400.0, 40000.0],
        80.0,
        RotationConvention::PositionVector,
    )
    .unwrap();
    for frame_is_source in [true, false] {
        let mut operation = positioning("12", tilted);
        if !frame_is_source {
            std::mem::swap(
                &mut operation.source_crs_epsg,
                &mut operation.target_crs_epsg,
            );
            std::mem::swap(&mut operation.source_crs_id, &mut operation.target_crs_id);
        }
        let options = SelectionOptions::new().with_coordinate_operation(operation);
        let to_frame =
            Transform::from_crs_defs_with_selection_options(&epsg(7415), &site_grid(), options)
                .unwrap();
        let to_rd = to_frame.inverse().unwrap();
        for point in [(90100.0, 435200.0), (91000.0, 436500.0)] {
            let frame = to_frame.convert(point).unwrap();
            let back = to_rd.convert(frame).unwrap();
            assert!(
                (back.0 - point.0).abs() < 1e-8 && (back.1 - point.1).abs() < 1e-8,
                "{point:?} came back as {back:?}"
            );
            // The frame point is on h = 0: in 3D it lands on the same
            // easting and northing at some height.
            let base = to_rd.convert_3d((frame.0, frame.1, 0.0)).unwrap();
            assert!((base.0 - point.0).abs() < 1e-8 && (base.1 - point.1).abs() < 1e-8);
        }
    }
}

#[test]
fn strict_transforms_keep_the_base_crs_rules() {
    // RD + NAP to a 2D CRS is refused without the horizontal-only
    // constructor, so the 3D frame positioned on it is too.
    let strict =
        Transform::from_crs_defs_with_selection_options(&site_grid(), &epsg(4326), options());
    let base = Transform::from_crs_defs(&epsg(7415), &epsg(4326));
    assert_eq!(strict.is_err(), base.is_err());
}

#[test]
fn convert_takes_heights_as_zero() {
    let transform =
        Transform::from_crs_defs_with_selection_options(&site_grid(), &epsg(7415), options())
            .unwrap();
    // RD + NAP to itself runs RD's projection there and back, which costs a
    // nanometre.
    let expected = appendix_params().forward([1000.0, 2000.0, 0.0]);
    let actual = transform.convert((1000.0, 2000.0)).unwrap();
    assert_close3(
        (actual.0, actual.1, 0.0),
        (expected[0], expected[1], 0.0),
        1e-8,
    );
}

#[test]
fn a_2d_frame_passes_z_through() {
    let frame_2d = site_grid().horizontal_crs().unwrap();
    assert_eq!(frame_2d.as_engineering().unwrap().dimension(), 2);
    let transform = Transform::from_horizontal_components_with_selection_options(
        &frame_2d,
        &epsg(28992),
        options(),
    )
    .unwrap();
    let expected = appendix_params().forward([1000.0, 2000.0, 0.0]);
    assert_close3(
        transform.convert_3d((1000.0, 2000.0, 42.0)).unwrap(),
        (expected[0], expected[1], 42.0),
        1e-8,
    );
}

#[test]
fn two_frames_meet_through_their_shared_base_crs() {
    let other_params = Similarity3DParams::new(
        [91000.0, 436000.0, 1.0],
        [0.0, 0.0, -3600.0],
        12.0,
        RotationConvention::CoordinateFrame,
    )
    .unwrap();
    let options = options().with_coordinate_operation(positioning("13", other_params));
    let transform =
        Transform::from_crs_defs_with_selection_options(&site_grid(), &other_site_grid(), options)
            .unwrap();

    let point = [100.0, 200.0, 3.0];
    let expected = other_params.inverse(appendix_params().forward(point));
    assert_close3(
        transform
            .convert_3d((point[0], point[1], point[2]))
            .unwrap(),
        (expected[0], expected[1], expected[2]),
        1e-8,
    );
    assert_eq!(
        transform.selected_operation().name,
        "12 to RD + NAP + Inverse of 13 to RD + NAP"
    );
    assert_eq!(
        transform.selected_operation().accuracy.unwrap().meters,
        0.06
    );
}

#[test]
fn a_frame_reaches_itself_without_an_operation() {
    let metres = site_grid();
    let feet_north_first = CrsDef::Engineering(Box::new(
        metres
            .as_engineering()
            .unwrap()
            .clone()
            .with_linear_unit(LinearUnit::foot())
            .with_axis_order(AxisOrder::NorthEast),
    ));

    let identity = Transform::from_crs_defs(&metres, &metres).unwrap();
    assert_eq!(
        identity.convert_3d((1.5, -2.5, 3.5)).unwrap(),
        (1.5, -2.5, 3.5)
    );

    let to_feet = Transform::from_crs_defs(&metres, &feet_north_first).unwrap();
    assert_close3(
        to_feet.convert_3d((0.3048, 0.6096, 3.048)).unwrap(),
        (2.0, 1.0, 10.0),
        1e-12,
    );
    assert_close3(
        to_feet
            .inverse()
            .unwrap()
            .convert_3d((2.0, 1.0, 10.0))
            .unwrap(),
        (0.3048, 0.6096, 3.048),
        1e-12,
    );
}

#[test]
fn an_unpositioned_frame_transforms_to_nothing_else() {
    let error = Transform::from_crs_defs(&site_grid(), &epsg(7415)).unwrap_err();
    assert!(
        error.to_string().contains("no operation positions"),
        "{error}"
    );
    let error = Transform::from_crs_defs(&site_grid(), &other_site_grid()).unwrap_err();
    assert!(error.to_string().contains("GEORIZON:"), "{error}");
}

#[test]
fn an_operation_for_another_frame_does_not_apply() {
    let options =
        SelectionOptions::new().with_coordinate_operation(positioning("13", appendix_params()));
    let error = Transform::from_crs_defs_with_selection_options(&site_grid(), &epsg(7415), options)
        .unwrap_err();
    assert!(error.to_string().contains("GEORIZON:12"), "{error}");
}

#[test]
fn a_frame_is_positioned_only_on_a_projected_crs() {
    let mut operation = positioning("12", appendix_params());
    operation.target_crs_epsg = Some(4326);
    let error = Transform::from_crs_defs_with_selection_options(
        &site_grid(),
        &epsg(4326),
        SelectionOptions::new().with_coordinate_operation(operation),
    )
    .unwrap_err();
    assert!(error.to_string().contains("projected"), "{error}");
}

#[test]
fn similarities_never_become_pipeline_steps_between_other_crs() {
    // An operation without CRS filters matches any pair; the similarity must
    // still not be run between two projected CRSs.
    let mut operation = positioning("12", appendix_params());
    operation.source_crs_id = None;
    operation.target_crs_epsg = None;
    let transform = Transform::from_crs_defs_with_selection_options(
        &epsg(28992),
        &epsg(28992),
        SelectionOptions::new().with_coordinate_operation(operation),
    )
    .unwrap();
    assert_eq!(
        transform.convert((155000.0, 463000.0)).unwrap(),
        (155000.0, 463000.0)
    );
}

#[test]
fn an_area_of_interest_in_frame_coordinates_selects_like_one_in_the_base_crs() {
    let frame_point = Coord::new(500.0, 500.0);
    let rd = appendix_params().forward([frame_point.x, frame_point.y, 0.0]);
    let in_frame = Transform::from_horizontal_components_with_selection_options(
        &site_grid(),
        &epsg(4326),
        options().with_area_of_interest(AreaOfInterest::source_crs_point(frame_point)),
    )
    .unwrap();
    let in_rd = Transform::with_selection_options(
        "EPSG:28992",
        "EPSG:4326",
        SelectionOptions::new()
            .with_area_of_interest(AreaOfInterest::source_crs_point(Coord::new(rd[0], rd[1]))),
    )
    .unwrap();
    assert_eq!(
        in_frame.selection_diagnostics().selected_reasons.len(),
        in_rd.selection_diagnostics().selected_reasons.len() + 1
    );
    assert_eq!(
        in_frame.convert((frame_point.x, frame_point.y)).unwrap(),
        in_rd.convert((rd[0], rd[1])).unwrap()
    );

    let bounds = Transform::from_horizontal_components_with_selection_options(
        &site_grid(),
        &epsg(4326),
        options().with_area_of_interest(AreaOfInterest::source_crs_bounds(Bounds::new(
            0.0, 0.0, 1000.0, 1000.0,
        ))),
    );
    assert!(bounds.is_ok());
}

#[test]
fn bounds_sample_through_the_frame() {
    let transform = Transform::from_horizontal_components_with_selection_options(
        &site_grid(),
        &epsg(28992),
        options(),
    )
    .unwrap();
    let bounds = transform
        .transform_bounds(Bounds::new(0.0, 0.0, 100.0, 100.0), 0)
        .unwrap();
    let corners = [[0.0, 0.0], [0.0, 100.0], [100.0, 0.0], [100.0, 100.0]]
        .map(|[x, y]| appendix_params().forward([x, y, 0.0]));
    let min_x = corners.iter().map(|c| c[0]).fold(f64::INFINITY, f64::min);
    let max_y = corners
        .iter()
        .map(|c| c[1])
        .fold(f64::NEG_INFINITY, f64::max);
    assert!((bounds.min_x - min_x).abs() < 1e-8);
    assert!((bounds.max_y - max_y).abs() < 1e-8);
}

fn unidentified(name: &str, datum: &str, dimension: usize, unit: LinearUnit) -> CrsDef {
    CrsDef::Engineering(Box::new(
        EngineeringCrsDef::new(name, datum, dimension)
            .unwrap()
            .with_linear_unit(unit),
    ))
}

#[test]
fn unidentified_frames_match_only_on_both_names() {
    let plant_a = unidentified("Plant A", "Local Datum", 2, LinearUnit::metre());
    let plant_b = unidentified("Plant B", "Local Datum", 2, LinearUnit::metre());
    assert!(Transform::from_crs_defs(&plant_a, &plant_b).is_err());
    let again = unidentified("Plant A", "Local Datum", 2, LinearUnit::metre());
    assert_eq!(
        Transform::from_crs_defs(&plant_a, &again)
            .unwrap()
            .convert((5.0, 7.0))
            .unwrap(),
        (5.0, 7.0)
    );
}

#[test]
fn heights_convert_between_forms_of_a_frame_only_when_both_have_them() {
    let feet = LinearUnit::foot();
    let flat = unidentified("Plant", "Plant", 2, feet);
    let solid = unidentified("Plant", "Plant", 3, feet);
    let metres = unidentified("Plant", "Plant", 3, LinearUnit::metre());
    for (from, to) in [(&flat, &solid), (&solid, &flat)] {
        let z = Transform::from_crs_defs(from, to)
            .unwrap()
            .convert_3d((1.0, 2.0, 10.0))
            .unwrap()
            .2;
        assert_eq!(z, 10.0);
    }
    assert_close3(
        Transform::from_crs_defs(&solid, &metres)
            .unwrap()
            .convert_3d((1.0, 2.0, 10.0))
            .unwrap(),
        (0.3048, 0.6096, 3.048),
        1e-12,
    );
}

#[test]
fn unit_changes_alone_are_not_reported_as_an_operation() {
    let feet = unidentified("Plant", "Plant", 3, LinearUnit::foot());
    let metres = unidentified("Plant", "Plant", 3, LinearUnit::metre());
    let transform = Transform::from_crs_defs(&feet, &metres).unwrap();
    assert_ne!(
        transform.selection_diagnostics().selected_match_kind,
        OperationMatchKind::Custom
    );
    assert_eq!(transform.selected_operation().name, "Identity");
}

#[test]
fn a_near_vertical_frame_plane_has_no_2d_point() {
    let upright = Similarity3DParams::new(
        [150000.0, 450000.0, 0.0],
        [324000.0, 0.0, 0.0],
        0.0,
        RotationConvention::PositionVector,
    )
    .unwrap();
    let transform = Transform::from_crs_defs_with_selection_options(
        &epsg(7415),
        &site_grid(),
        SelectionOptions::new().with_coordinate_operation(positioning("12", upright)),
    )
    .unwrap();
    let error = transform.convert((150010.0, 450020.0)).unwrap_err();
    assert!(error.to_string().contains("near vertical"), "{error}");
    // In 3D the point is well defined.
    assert!(transform.convert_3d((150010.0, 450020.0, 5.0)).is_ok());
}

#[test]
fn the_base_crs_a_frame_reaches_is_reported() {
    let base = Transform::engineering_base_crs(&site_grid(), &epsg(4326), &options()).unwrap();
    assert_eq!(base.epsg(), 7415);
    let flat = site_grid().horizontal_crs().unwrap();
    let base = Transform::engineering_base_crs(&flat, &epsg(4326), &options()).unwrap();
    assert_eq!(base.epsg(), 28992);
    assert!(Transform::engineering_base_crs(&site_grid(), &site_grid(), &options()).is_none());
    assert!(Transform::engineering_base_crs(&epsg(4326), &site_grid(), &options()).is_none());
}

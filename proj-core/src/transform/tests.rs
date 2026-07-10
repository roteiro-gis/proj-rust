use super::*;
use crate::crs::{
    CompoundCrsDef, CrsDef, GeographicCrsDef, HorizontalCrsDef, LinearUnit, ProjectedCrsDef,
    ProjectionMethod, VerticalCrsDef,
};
use crate::datum::{self, DatumToWgs84};
use crate::grid::{FilesystemGridProvider, GridDefinition, GridError, GridFormat};
use crate::operation::{
    AreaOfInterest, CoordinateOperation, GridId, GridInterpolation, OperationMatchKind,
    OperationMethod, SelectionPolicy, SelectionReason, SkippedOperationReason,
    VerticalGridOffsetConvention, VerticalGridOperation, VerticalTransformAction,
};
use crate::selector::SelectedOperationKind;
use smallvec::SmallVec;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

const US_FOOT_TO_METER: f64 = 0.3048006096012192;
static TEMP_GRID_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn expect_transform_error(result: Result<Transform>) -> Error {
    match result {
        Ok(_) => panic!("expected transform construction to fail"),
        Err(err) => err,
    }
}

fn custom_nad27_to_wgs84_operation(name: &str) -> CoordinateOperation {
    CoordinateOperation {
        id: None,
        name: name.into(),
        source_crs_epsg: None,
        target_crs_epsg: Some(4326),
        source_datum_epsg: None,
        target_datum_epsg: None,
        accuracy: Some(crate::operation::OperationAccuracy { meters: 5.0 }),
        areas_of_use: SmallVec::new(),
        deprecated: false,
        preferred: true,
        approximate: false,
        superseded: false,
        method: OperationMethod::Helmert {
            params: *datum::NAD27.helmert_to_wgs84().unwrap(),
        },
    }
}

fn write_test_gtx(values: &[f32]) -> PathBuf {
    write_test_gtx_files(&[("test.gtx", -75.0, 40.0, values)])
}

fn write_test_gtx_files(files: &[(&str, f64, f64, &[f32])]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "proj-core-vertical-grid-{}-{}",
        std::process::id(),
        TEMP_GRID_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, west, south, values) in files {
        write_test_gtx_file(&dir, name, *west, *south, values);
    }
    dir
}

fn write_test_gtx_file(dir: &std::path::Path, name: &str, west: f64, south: f64, values: &[f32]) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&south.to_be_bytes());
    bytes.extend_from_slice(&west.to_be_bytes());
    bytes.extend_from_slice(&1.0f64.to_be_bytes());
    bytes.extend_from_slice(&1.0f64.to_be_bytes());
    bytes.extend_from_slice(&2i32.to_be_bytes());
    bytes.extend_from_slice(&2i32.to_be_bytes());
    for value in values {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn write_test_gtx_resource_names(
    resource_names: impl IntoIterator<Item = String>,
    west: f64,
    south: f64,
    values: &[f32],
) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "proj-core-vertical-grid-{}-{}",
        std::process::id(),
        TEMP_GRID_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for name in resource_names {
        write_test_gtx_file(&dir, &name, west, south, values);
    }
    dir
}

fn registry_vertical_grid_resource_names(operations: &[VerticalGridOperation]) -> BTreeSet<String> {
    operations
        .iter()
        .flat_map(|operation| operation.grid.resource_names.iter().cloned())
        .collect()
}

fn nad83_ellipsoidal_to_navd88_pair() -> (CrsDef, CrsDef) {
    let horizontal_crs = registry::lookup_epsg(4269).expect("NAD83 geographic CRS");
    let geographic = horizontal_crs
        .as_geographic()
        .expect("NAD83 is geographic")
        .clone();
    let horizontal = HorizontalCrsDef::Geographic(geographic.clone());
    let source_vertical = VerticalCrsDef::ellipsoidal_height(
        0,
        geographic.datum().clone(),
        LinearUnit::metre(),
        "NAD83 ellipsoidal height",
    );
    let source = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal.clone(),
        source_vertical,
        "NAD83 + ellipsoidal height",
    )));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "NAD83 + NAVD88 height",
    )));
    (source, target)
}

fn nad83_horizontal_wgs84_ellipsoidal_to_navd88_pair() -> (CrsDef, CrsDef) {
    let horizontal_crs = registry::lookup_epsg(4269).expect("NAD83 geographic CRS");
    let geographic = horizontal_crs
        .as_geographic()
        .expect("NAD83 is geographic")
        .clone();
    let horizontal = HorizontalCrsDef::Geographic(geographic);
    let source_vertical = VerticalCrsDef::ellipsoidal_height(
        0,
        datum::WGS84,
        LinearUnit::metre(),
        "WGS 84 ellipsoidal height",
    );
    let source = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal.clone(),
        source_vertical,
        "NAD83 + WGS 84 ellipsoidal height",
    )));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "NAD83 + NAVD88 height",
    )));
    (source, target)
}

fn test_vertical_grid_operation() -> VerticalGridOperation {
    test_vertical_grid_operation_named("Test geoid height to NAVD88", "test.gtx")
}

fn test_vertical_grid_operation_named(name: &str, resource_name: &str) -> VerticalGridOperation {
    VerticalGridOperation {
        name: name.into(),
        grid: GridDefinition {
            id: GridId(900_001),
            name: resource_name.into(),
            format: GridFormat::Gtx,
            interpolation: GridInterpolation::Bilinear,
            area_of_use: Some(crate::operation::AreaOfUse {
                west: -75.0,
                south: 40.0,
                east: -74.0,
                north: 41.0,
                name: "test grid".into(),
            }),
            resource_names: SmallVec::from_vec(vec![resource_name.into()]),
        },
        grid_horizontal_crs_epsg: Some(4326),
        source_vertical_crs_epsg: None,
        target_vertical_crs_epsg: Some(5703),
        source_vertical_datum_epsg: None,
        target_vertical_datum_epsg: Some(5103),
        accuracy: Some(crate::operation::OperationAccuracy { meters: 0.01 }),
        area_of_use: None,
        offset_convention: VerticalGridOffsetConvention::GeoidHeightMeters,
    }
}

#[test]
fn identity_same_crs() {
    let t = Transform::new("EPSG:4326", "EPSG:4326").unwrap();
    let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
    assert_eq!(x, -74.006);
    assert_eq!(y, 40.7128);
}

#[test]
fn identity_transform_rejects_invalid_geographic_coordinates() {
    let identity = Transform::new("EPSG:4326", "EPSG:4326").unwrap();
    for coord in [(f64::NAN, 0.0), (0.0, f64::NAN), (0.0, 91.0)] {
        let err = identity.convert(coord).unwrap_err();
        assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
    }

    let same_datum = Transform::new("EPSG:4269", "EPSG:4326").unwrap();
    let err = same_datum.convert((0.0, f64::INFINITY)).unwrap_err();
    assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
}

#[test]
fn three_dimensional_transform_rejects_non_finite_height() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let err = t.convert_3d((0.0, 0.0, f64::NAN)).unwrap_err();
    assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
}

#[test]
fn helmert_constructor_rejects_non_finite_params() {
    let err = datum::HelmertParams::new(f64::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0).unwrap_err();
    assert!(matches!(err, Error::InvalidDefinition(_)), "got {err}");
    assert!(err.to_string().contains("Helmert parameters"), "{err}");
}

#[test]
fn custom_helmert_datum_does_not_synthesize_operation() {
    let datum = datum::Datum::new(
        datum::WGS84.ellipsoid(),
        DatumToWgs84::Helmert(
            datum::HelmertParams::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, f64::MAX).unwrap(),
        ),
    )
    .unwrap();
    let source = CrsDef::Geographic(GeographicCrsDef::new(0, datum, "Overflowing Helmert datum"));
    let target = registry::lookup_epsg(4326).unwrap();

    let err = expect_transform_error(Transform::from_crs_defs(&source, &target));

    assert!(matches!(err, Error::OperationSelection(_)), "got {err}");
    assert!(
        err.to_string()
            .contains("no compatible registry operation found"),
        "{err}"
    );
}

#[test]
fn custom_wgs84_compatible_datum_does_not_synthesize_identity() {
    let source = CrsDef::Geographic(GeographicCrsDef::new(0, datum::NAD83, "Custom NAD83"));
    let target = registry::lookup_epsg(4326).unwrap();

    let err = expect_transform_error(Transform::from_crs_defs(&source, &target));

    assert!(matches!(err, Error::OperationSelection(_)), "got {err}");
    assert!(
        err.to_string()
            .contains("no compatible registry operation found"),
        "{err}"
    );
}

#[test]
fn custom_grid_datum_does_not_synthesize_helmert_leg() {
    let grid = datum::DatumGridShift::from_vec(vec![datum::DatumGridShiftEntry::Grid {
        definition: GridDefinition {
            id: GridId(900_010),
            name: "test horizontal grid".into(),
            format: GridFormat::Ntv2,
            interpolation: GridInterpolation::Bilinear,
            area_of_use: None,
            resource_names: SmallVec::from_vec(vec!["missing.gsb".into()]),
        },
        optional: false,
    }]);
    let grid_datum = datum::Datum::new(
        datum::WGS84.ellipsoid(),
        DatumToWgs84::GridShift(Box::new(grid)),
    )
    .unwrap();
    let helmert_datum = datum::Datum::new(
        datum::WGS84.ellipsoid(),
        DatumToWgs84::Helmert(
            datum::HelmertParams::new(1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0).unwrap(),
        ),
    )
    .unwrap();
    let source = CrsDef::Geographic(GeographicCrsDef::new(0, grid_datum, "Grid datum"));
    let target = CrsDef::Geographic(GeographicCrsDef::new(0, helmert_datum, "Helmert datum"));

    let err = expect_transform_error(Transform::from_crs_defs(&source, &target));

    assert!(matches!(err, Error::OperationSelection(_)), "got {err}");
    assert!(
        err.to_string()
            .contains("no compatible registry operation found"),
        "{err}"
    );
}

#[test]
fn pipeline_rejects_non_finite_final_output_after_unit_conversion() {
    let source = registry::lookup_epsg(4326).unwrap();
    let tiny_unit = LinearUnit::from_meters_per_unit(f64::MIN_POSITIVE).unwrap();
    let target = CrsDef::Projected(ProjectedCrsDef::new_with_base_geographic_crs(
        0,
        4326,
        datum::WGS84,
        ProjectionMethod::WebMercator,
        tiny_unit,
        "WGS 84 / Pseudo-Mercator in tiny units",
    ));
    let transform = Transform::from_crs_defs(&source, &target).unwrap();

    let err = transform.convert((-74.006, 40.7128)).unwrap_err();

    assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
    assert!(err.to_string().contains("pipeline final output"), "{err}");
}

#[test]
fn wgs84_to_web_mercator() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
    assert!((x - (-8238310.0)).abs() < 100.0, "x = {x}");
    assert!((y - 4970072.0).abs() < 100.0, "y = {y}");
}

#[test]
fn web_mercator_to_wgs84() {
    let t = Transform::new("EPSG:3857", "EPSG:4326").unwrap();
    let (lon, lat) = t.convert((-8238310.0, 4970072.0)).unwrap();
    assert!((lon - (-74.006)).abs() < 0.001, "lon = {lon}");
    assert!((lat - 40.7128).abs() < 0.001, "lat = {lat}");
}

#[test]
fn roundtrip_4326_3857() {
    let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let inv = fwd.inverse().unwrap();

    let original = (-74.0445, 40.6892);
    let projected = fwd.convert(original).unwrap();
    let back = inv.convert(projected).unwrap();

    assert!((back.0 - original.0).abs() < 1e-8);
    assert!((back.1 - original.1).abs() < 1e-8);
}

#[test]
fn wgs84_to_utm_18n() {
    let t = Transform::new("EPSG:4326", "EPSG:32618").unwrap();
    let (x, y) = t.convert((-74.006, 40.7128)).unwrap();
    assert!((x - 583960.0).abs() < 1.0, "easting = {x}");
    assert!(y > 4_500_000.0 && y < 4_510_000.0, "northing = {y}");
}

#[test]
fn equivalent_meter_and_foot_state_plane_crs_match_after_unit_conversion() {
    let coord = (-80.8431, 35.2271);
    let meter_tx = Transform::new("EPSG:4326", "EPSG:32119").unwrap();
    let foot_tx = Transform::new("EPSG:4326", "EPSG:2264").unwrap();

    let (mx, my) = meter_tx.convert(coord).unwrap();
    let (fx, fy) = foot_tx.convert(coord).unwrap();

    assert!((fx * US_FOOT_TO_METER - mx).abs() < 0.02);
    assert!((fy * US_FOOT_TO_METER - my).abs() < 0.02);
}

#[test]
fn inverse_transform_accepts_native_projected_units_for_foot_crs() {
    let coord = (-80.8431, 35.2271);
    let forward = Transform::new("EPSG:4326", "EPSG:2264").unwrap();
    let inverse = Transform::new("EPSG:2264", "EPSG:4326").unwrap();

    let projected = forward.convert(coord).unwrap();
    let roundtrip = inverse.convert(projected).unwrap();

    assert!((roundtrip.0 - coord.0).abs() < 1e-8);
    assert!((roundtrip.1 - coord.1).abs() < 1e-8);
}

#[test]
fn pipeline_compiles_xy_unit_modes() {
    let web_mercator = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    assert!(matches!(
        web_mercator.pipeline.source_xy_units,
        PipelineSourceXyUnits::GeographicDegrees
    ));
    assert!(matches!(
        web_mercator.pipeline.target_xy_units,
        PipelineTargetXyUnits::ProjectedMeters
    ));

    let foot_crs = Transform::new("EPSG:2264", "EPSG:4326").unwrap();
    assert!(matches!(
        foot_crs.pipeline.source_xy_units,
        PipelineSourceXyUnits::ProjectedNativeToMeters(unit)
            if (unit.meters_per_unit() - US_FOOT_TO_METER).abs() < 1e-15
    ));
    assert!(matches!(
        foot_crs.pipeline.target_xy_units,
        PipelineTargetXyUnits::GeographicDegrees
    ));
}

#[test]
fn utm_to_web_mercator() {
    let t = Transform::new("EPSG:32618", "EPSG:3857").unwrap();
    let (x, _y) = t.convert((583960.0, 4507523.0)).unwrap();
    assert!((x - (-8238310.0)).abs() < 200.0, "x = {x}");
}

#[test]
fn wgs84_to_polar_stereo_3413() {
    let t = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
    let (x, y) = t.convert((-45.0, 90.0)).unwrap();
    assert!(x.abs() < 1.0, "x = {x}");
    assert!(y.abs() < 1.0, "y = {y}");
}

#[test]
fn roundtrip_4326_3413() {
    let fwd = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
    let inv = fwd.inverse().unwrap();

    let original = (-45.0, 75.0);
    let projected = fwd.convert(original).unwrap();
    let back = inv.convert(projected).unwrap();

    assert!((back.0 - original.0).abs() < 1e-6);
    assert!((back.1 - original.1).abs() < 1e-6);
}

#[test]
fn nad83_to_wgs84_uses_registry_operation() {
    let t = Transform::new("EPSG:4269", "EPSG:4326").unwrap();
    let (lon, lat) = t.convert((-74.006, 40.7128)).unwrap();
    assert!((lon - (-74.006)).abs() < 1e-9, "lon = {lon}");
    assert!((lat - 40.7128).abs() < 1e-9, "lat = {lat}");
    assert!(t.selected_operation().id.is_some());
    assert!(!matches!(
        t.selected_operation_kind,
        SelectedOperationKind::Identity
    ));
}

#[test]
fn unknown_crs_error() {
    let result = Transform::new("EPSG:99999", "EPSG:4326");
    assert!(result.is_err());
}

#[test]
fn cross_datum_nad27_to_wgs84() {
    let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
    let (lon, lat) = t.convert((-90.0, 45.0)).unwrap();
    assert!((lon - (-90.0)).abs() < 0.01, "lon = {lon}");
    assert!((lat - 45.0).abs() < 0.01, "lat = {lat}");
    assert!(!t.selected_operation().approximate);
}

#[test]
fn custom_coordinate_operation_is_selected_and_compiled() {
    let source = CrsDef::Geographic(GeographicCrsDef::new(0, datum::NAD27, "Custom NAD27"));
    let target = registry::lookup_epsg(4326).unwrap();
    let operation = custom_nad27_to_wgs84_operation("Custom NAD27 to WGS84");

    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions::new().with_coordinate_operation(operation),
    )
    .unwrap();

    assert_eq!(t.selected_operation().name, "Custom NAD27 to WGS84");
    assert_eq!(t.selected_operation().id, None);
    assert_eq!(
        t.selection_diagnostics().selected_match_kind,
        OperationMatchKind::Custom
    );
    assert!(t
        .selection_diagnostics()
        .selected_reasons
        .contains(&SelectionReason::CustomOperation));

    let (lon, lat) = t.convert((-90.0, 45.0)).unwrap();
    assert!((lon - (-90.0)).abs() < 0.01, "lon = {lon}");
    assert!((lat - 45.0).abs() < 0.01, "lat = {lat}");

    let inv = t.inverse().unwrap();
    assert_eq!(
        inv.selected_operation().direction,
        OperationStepDirection::Reverse
    );
    let (back_lon, back_lat) = inv.convert((lon, lat)).unwrap();
    assert!((back_lon - (-90.0)).abs() < 1e-6, "lon = {back_lon}");
    assert!((back_lat - 45.0).abs() < 1e-6, "lat = {back_lat}");
}

#[test]
fn custom_coordinate_operation_is_ranked_above_registry_candidates() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4326",
        SelectionOptions::new()
            .with_coordinate_operation(custom_nad27_to_wgs84_operation("Preferred custom NAD27")),
    )
    .unwrap();

    assert_eq!(t.selected_operation().name, "Preferred custom NAD27");
    assert_eq!(
        t.selection_diagnostics().selected_match_kind,
        OperationMatchKind::Custom
    );
    assert!(t
        .selection_diagnostics()
        .skipped_operations
        .iter()
        .any(|skipped| skipped.metadata.id.is_some()
            && matches!(skipped.reason, SkippedOperationReason::LessPreferred)));
}

#[test]
fn explicit_grid_operation_compiles() {
    let t =
        Transform::from_operation(CoordinateOperationId(1693), "EPSG:4267", "EPSG:4326").unwrap();
    assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1693)));
}

#[test]
fn explicit_operation_rejects_incompatible_crs_pair() {
    let err = match Transform::from_operation(CoordinateOperationId(1693), "EPSG:4326", "EPSG:3857")
    {
        Ok(_) => panic!("incompatible operation should be rejected"),
        Err(err) => err,
    };
    assert!(matches!(err, Error::OperationSelection(_)));
    assert!(err.to_string().contains("not compatible"));
}

#[test]
fn explicit_selection_options_choose_grid_operation() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                -80.5041667,
                44.5458333,
            ))),
            ..SelectionOptions::default()
        },
    )
    .unwrap();
    assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1313)));
    assert!(!t.selection_diagnostics().approximate);
}

#[test]
fn source_crs_area_of_interest_is_normalized_before_selection() {
    let to_projected = Transform::new("EPSG:4267", "EPSG:26717").unwrap();
    let projected = to_projected.convert((-80.5041667, 44.5458333)).unwrap();

    let t = Transform::with_selection_options(
        "EPSG:26717",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::source_crs_point(Coord::new(
                projected.0,
                projected.1,
            ))),
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1313)));
    assert_eq!(
        t.selection_diagnostics().selected_match_kind,
        OperationMatchKind::DerivedGeographic
    );
}

#[test]
fn area_of_interest_rejects_invalid_geographic_bounds() {
    for bounds in [
        Bounds::new(10.0, 5.0, -10.0, 20.0),
        Bounds::new(f64::NAN, 5.0, 10.0, 20.0),
        Bounds::new(-181.0, 5.0, -170.0, 20.0),
        Bounds::new(-80.0, -91.0, -70.0, -80.0),
    ] {
        let err = expect_transform_error(Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(AreaOfInterest::geographic_bounds(bounds)),
                ..SelectionOptions::default()
            },
        ));

        assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
    }
}

#[test]
fn area_of_interest_accepts_wrapped_geographic_bounds() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_wrapped_bounds(Bounds::new(
                170.0, -20.0, -170.0, -10.0,
            ))),
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert_eq!(t.source_crs().epsg(), 4267);
    assert_eq!(t.target_crs().epsg(), 4269);
}

#[test]
fn area_of_interest_rejects_wrapped_geographic_bounds_that_do_not_wrap() {
    let err = expect_transform_error(Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_wrapped_bounds(Bounds::new(
                160.0, -20.0, 170.0, -10.0,
            ))),
            ..SelectionOptions::default()
        },
    ));

    assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
}

#[test]
fn area_of_interest_validates_geographic_source_and_target_bounds() {
    for area_of_interest in [
        AreaOfInterest::source_crs_bounds(Bounds::new(170.0, -20.0, -170.0, -10.0)),
        AreaOfInterest::source_crs_bounds(Bounds::new(-181.0, 40.0, -170.0, 45.0)),
        AreaOfInterest::target_crs_bounds(Bounds::new(-80.0, 40.0, -70.0, 91.0)),
    ] {
        let err = expect_transform_error(Transform::with_selection_options(
            "EPSG:4267",
            "EPSG:4269",
            SelectionOptions {
                area_of_interest: Some(area_of_interest),
                ..SelectionOptions::default()
            },
        ));

        assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
    }
}

#[test]
fn area_of_interest_rejects_invalid_projected_bounds_before_sampling() {
    let err = expect_transform_error(Transform::with_selection_options(
        "EPSG:3857",
        "EPSG:4326",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::source_crs_bounds(Bounds::new(
                10.0, 0.0, -10.0, 10.0,
            ))),
            ..SelectionOptions::default()
        },
    ));

    assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
}

#[test]
fn selection_diagnostics_capture_accuracy_preference() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                -80.5041667,
                44.5458333,
            ))),
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert!(t
        .selection_diagnostics()
        .selected_reasons
        .contains(&SelectionReason::AccuracyPreferred));
}

#[test]
fn selection_diagnostics_capture_policy_filtered_candidates() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4326",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                -80.5041667,
                44.5458333,
            ))),
            policy: SelectionPolicy::RequireGrids,
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert!(t
        .selection_diagnostics()
        .skipped_operations
        .iter()
        .any(|skipped| { matches!(skipped.reason, SkippedOperationReason::PolicyFiltered) }));
}

#[test]
fn selection_diagnostics_capture_area_mismatch_candidates() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                -80.5041667,
                44.5458333,
            ))),
            policy: SelectionPolicy::RequireExactAreaMatch,
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert!(t
        .selection_diagnostics()
        .skipped_operations
        .iter()
        .any(|skipped| { matches!(skipped.reason, SkippedOperationReason::AreaOfUseMismatch) }));
}

#[test]
fn grid_coverage_miss_has_no_approximate_fallback_candidate() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                -80.5041667,
                44.5458333,
            ))),
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert_eq!(t.selected_operation().id, Some(CoordinateOperationId(1313)));
    assert!(t.selection_diagnostics().fallback_operations.is_empty());

    let err = t.convert((0.0, 0.0)).unwrap_err();

    assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
    assert!(t
        .selection_diagnostics()
        .skipped_operations
        .iter()
        .all(|skipped| !skipped.metadata.approximate));
    assert!(t.selection_diagnostics().fallback_operations.is_empty());
}

#[test]
fn grid_coverage_miss_does_not_use_non_grid_fallback_when_grids_are_required() {
    let t = Transform::with_selection_options(
        "EPSG:4267",
        "EPSG:4269",
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(
                -80.5041667,
                44.5458333,
            ))),
            policy: SelectionPolicy::RequireGrids,
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    assert!(t
        .selection_diagnostics()
        .fallback_operations
        .iter()
        .all(|operation| operation.uses_grids));

    let err = t.convert((0.0, 0.0)).unwrap_err();

    assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
}

#[test]
fn cross_datum_roundtrip_nad27() {
    let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
    let inv = fwd.inverse().unwrap();
    let original = (-90.0, 45.0);
    let shifted = fwd.convert(original).unwrap();
    let back = inv.convert(shifted).unwrap();
    assert!((back.0 - original.0).abs() < 1e-6);
    assert!((back.1 - original.1).abs() < 1e-6);
}

#[test]
fn inverse_preserves_explicit_operation_selection() {
    let fwd =
        Transform::from_operation(CoordinateOperationId(1693), "EPSG:4267", "EPSG:4326").unwrap();
    let inv = fwd.inverse().unwrap();

    assert_eq!(
        fwd.selected_operation().id,
        Some(CoordinateOperationId(1693))
    );
    assert_eq!(
        inv.selected_operation().id,
        Some(CoordinateOperationId(1693))
    );
}

#[test]
fn inverse_reorients_selected_metadata_and_diagnostics() {
    let fwd =
        Transform::from_operation(CoordinateOperationId(1693), "EPSG:4267", "EPSG:4326").unwrap();
    let inv = fwd.inverse().unwrap();

    assert_eq!(inv.source_crs().epsg(), 4326);
    assert_eq!(inv.target_crs().epsg(), 4267);
    assert_eq!(
        inv.selected_operation().direction,
        OperationStepDirection::Reverse
    );
    assert_eq!(inv.selected_operation().source_crs_epsg, Some(4326));
    assert_eq!(inv.selected_operation().target_crs_epsg, Some(4267));
    assert_eq!(
        inv.selection_diagnostics()
            .selected_operation
            .source_crs_epsg,
        Some(4326)
    );
    assert_eq!(
        inv.selection_diagnostics()
            .selected_operation
            .target_crs_epsg,
        Some(4267)
    );
}

#[test]
fn cross_datum_osgb36_to_wgs84() {
    let t = Transform::new("EPSG:4277", "EPSG:4326").unwrap();
    let (lon, lat) = t.convert((-0.1278, 51.5074)).unwrap();
    assert!((lon - (-0.1278)).abs() < 0.01, "lon = {lon}");
    assert!((lat - 51.5074).abs() < 0.01, "lat = {lat}");
}

#[test]
fn wgs84_to_web_mercator_3d_preserves_height() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
    assert!((x - (-8238310.0)).abs() < 100.0);
    assert!((y - 4970072.0).abs() < 100.0);
    assert!((z - 123.45).abs() < 1e-12);
}

#[test]
fn equal_compound_vertical_crs_preserves_height() {
    let source = registry::lookup_epsg(4979).unwrap();
    let target_horizontal = ProjectedCrsDef::new_with_base_geographic_crs(
        3857,
        4326,
        datum::WGS84,
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "WGS 84 / Pseudo-Mercator",
    );
    let target_vertical = VerticalCrsDef::ellipsoidal_height(
        0,
        datum::WGS84,
        LinearUnit::metre(),
        "WGS 84 ellipsoidal height",
    );
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        HorizontalCrsDef::Projected(target_horizontal),
        target_vertical,
        "WGS 84 / Pseudo-Mercator + ellipsoidal height",
    )));

    let t = Transform::from_crs_defs(&source, &target).unwrap();
    let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
    assert!((x - (-8238310.0)).abs() < 100.0);
    assert!((y - 4970072.0).abs() < 100.0);
    assert!((z - 123.45).abs() < 1e-12);
    assert_eq!(
        t.vertical_diagnostics().action,
        VerticalTransformAction::Preserved
    );
}

#[test]
fn horizontal_components_allow_compound_xy_preview() {
    let source = registry::lookup_epsg(4979).unwrap();
    let target = registry::lookup_epsg(3857).unwrap();
    let err = expect_transform_error(Transform::from_crs_defs(&source, &target));
    assert!(err.to_string().contains("explicit vertical CRS"));

    let t = Transform::from_horizontal_components(&source, &target).unwrap();
    assert!(t.source_crs().vertical_crs().is_none());
    assert!(t.target_crs().vertical_crs().is_none());
    assert_eq!(
        t.vertical_diagnostics().action,
        VerticalTransformAction::None
    );
    let (x, y, z) = t.convert_3d((-74.006, 40.7128, 123.45)).unwrap();
    assert!((x - (-8238310.0)).abs() < 100.0);
    assert!((y - 4970072.0).abs() < 100.0);
    assert!((z - 123.45).abs() < 1e-12);
}

#[test]
fn same_vertical_reference_converts_z_units() {
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let source_vertical =
        VerticalCrsDef::gravity_related_height(5703, 5103, LinearUnit::metre(), "NAVD88").unwrap();
    let target_vertical =
        VerticalCrsDef::gravity_related_height(0, 5103, LinearUnit::foot(), "NAVD88 foot").unwrap();
    let source = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal.clone(),
        source_vertical,
        "WGS 84 + NAVD88 height",
    )));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        target_vertical,
        "WGS 84 + NAVD88 height (ft)",
    )));

    let t = Transform::from_crs_defs(&source, &target).unwrap();
    let plain = t.convert_3d((-74.006, 40.7128, 1.0)).unwrap();
    assert!((plain.2 - 3.280839895013123).abs() < 1e-12);

    let outcome = t
        .convert_3d_with_diagnostics((-74.006, 40.7128, 1.0))
        .unwrap();
    assert!((outcome.coord.2 - 3.280839895013123).abs() < 1e-12);
    assert_eq!(
        outcome.vertical.action,
        VerticalTransformAction::UnitConverted
    );
    assert_eq!(outcome.vertical.source_vertical_crs_epsg, Some(5703));
    assert_eq!(outcome.vertical.source_vertical_datum_epsg, Some(5103));
    assert_eq!(outcome.vertical.target_vertical_datum_epsg, Some(5103));
}

#[test]
fn vertical_geoid_grid_transforms_ellipsoidal_to_gravity_height() {
    let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
    let source = registry::lookup_epsg(4979).unwrap();
    let target_horizontal = ProjectedCrsDef::new_with_base_geographic_crs(
        3857,
        4326,
        datum::WGS84,
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "WGS 84 / Pseudo-Mercator",
    );
    let target_vertical = registry::lookup_vertical_epsg(5703).unwrap();
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        HorizontalCrsDef::Projected(target_horizontal),
        target_vertical,
        "WGS 84 / Pseudo-Mercator + NAVD88 height",
    )));

    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![test_vertical_grid_operation()],
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    let plain = t.convert_3d((-74.5, 40.5, 100.0)).unwrap();
    assert!((plain.2 - 130.0).abs() < 1e-9);

    let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
    assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
    assert_eq!(
        outcome.vertical.action,
        VerticalTransformAction::Transformed
    );
    assert_eq!(outcome.vertical.target_vertical_crs_epsg, Some(5703));
    assert_eq!(outcome.vertical.grids[0].name, "test.gtx");
    assert!(outcome.vertical.grids[0]
        .checksum
        .as_ref()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(outcome.vertical.area_of_use_match, None);

    let inverse = t.inverse().unwrap();
    let roundtrip = inverse.convert_3d(outcome.coord).unwrap();
    assert!((roundtrip.0 - -74.5).abs() < 1e-6);
    assert!((roundtrip.1 - 40.5).abs() < 1e-6);
    assert!((roundtrip.2 - 100.0).abs() < 1e-9);
}

#[test]
fn registry_vertical_grid_operation_transforms_without_manual_operation() {
    let (source, target) = nad83_ellipsoidal_to_navd88_pair();
    let operations = registry::vertical_grid_operations_between(&source, &target);
    assert!(!operations.is_empty());
    assert!(operations.iter().any(|operation| {
        operation.grid.format == GridFormat::Gtx
            && operation.target_vertical_crs_epsg == Some(5703)
            && operation.target_vertical_datum_epsg == Some(5103)
    }));
    let grid_root = write_test_gtx_resource_names(
        registry_vertical_grid_resource_names(&operations),
        -75.0,
        40.0,
        &[-30.0, -30.0, -30.0, -30.0],
    );

    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions::new()
            .with_area_of_interest(AreaOfInterest::geographic_point(Coord::new(-74.5, 40.5)))
            .with_grid_provider(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
    )
    .unwrap();

    let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
    assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
    assert_eq!(
        outcome.vertical.action,
        VerticalTransformAction::Transformed
    );
    assert_eq!(outcome.vertical.target_vertical_crs_epsg, Some(5703));
    assert_eq!(outcome.vertical.target_vertical_datum_epsg, Some(5103));
    assert!(outcome
        .vertical
        .operation_name
        .as_deref()
        .is_some_and(|name| name.contains("NAVD88")));
    assert!(outcome.vertical.accuracy.is_some());
    assert_eq!(outcome.vertical.area_of_use_match, Some(true));
    assert!(outcome.vertical.area_of_use.is_some());
    assert!(outcome.vertical.grids[0]
        .checksum
        .as_ref()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        outcome.vertical.grids[0].accuracy,
        outcome.vertical.accuracy
    );

    let inverse = t.inverse().unwrap();
    let roundtrip = inverse.convert_3d(outcome.coord).unwrap();
    assert!((roundtrip.0 - -74.5).abs() < 1e-9);
    assert!((roundtrip.1 - 40.5).abs() < 1e-9);
    assert!((roundtrip.2 - 100.0).abs() < 1e-9);
}

#[test]
fn explicit_vertical_grid_operation_falls_back_to_registry_candidate_after_coverage_miss() {
    let (source, target) = nad83_ellipsoidal_to_navd88_pair();
    let operations = registry::vertical_grid_operations_between(&source, &target);
    assert!(!operations.is_empty());
    let grid_root = write_test_gtx_resource_names(
        registry_vertical_grid_resource_names(&operations),
        -75.0,
        40.0,
        &[-30.0, -30.0, -30.0, -30.0],
    );
    write_test_gtx_file(
        &grid_root,
        "custom-miss.gtx",
        10.0,
        10.0,
        &[-10.0, -10.0, -10.0, -10.0],
    );
    let mut explicit =
        test_vertical_grid_operation_named("custom coverage miss", "custom-miss.gtx");
    explicit.accuracy = Some(crate::operation::OperationAccuracy { meters: 0.001 });

    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions::new()
            .with_area_of_interest(AreaOfInterest::geographic_point(Coord::new(-74.5, 40.5)))
            .with_grid_provider(Arc::new(FilesystemGridProvider::new(vec![grid_root])))
            .with_vertical_grid_operation(explicit),
    )
    .unwrap();

    let VerticalTransform::GridShiftList { shifts, .. } = &t.vertical_transform else {
        panic!("expected vertical grid shift list");
    };
    assert!(shifts.len() >= 2);
    assert_eq!(
        shifts[0].diagnostics.operation_name.as_deref(),
        Some("custom coverage miss")
    );

    let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
    assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
    assert!(outcome
        .vertical
        .operation_name
        .as_deref()
        .is_some_and(|name| name.contains("NAVD88")));
    assert_ne!(
        outcome.vertical.operation_name.as_deref(),
        Some("custom coverage miss")
    );
}

#[test]
fn registry_vertical_grid_operation_reports_missing_grid_resource() {
    let (source, target) = nad83_ellipsoidal_to_navd88_pair();

    let err = expect_transform_error(Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions::new()
            .with_area_of_interest(AreaOfInterest::geographic_point(Coord::new(-74.5, 40.5))),
    ));

    let message = err.to_string();
    assert!(message.contains("unavailable"), "{message}");
    assert!(message.contains("g2003u"), "{message}");
}

#[test]
fn registry_vertical_grid_operation_rejects_mismatched_ellipsoidal_vertical_datum() {
    let (valid_source, target) = nad83_ellipsoidal_to_navd88_pair();
    let operations = registry::vertical_grid_operations_between(&valid_source, &target);
    assert!(!operations.is_empty());
    let (mismatched_source, target) = nad83_horizontal_wgs84_ellipsoidal_to_navd88_pair();
    assert!(registry::vertical_grid_operations_between(&mismatched_source, &target).is_empty());
    let grid_root = write_test_gtx_resource_names(
        registry_vertical_grid_resource_names(&operations),
        -75.0,
        40.0,
        &[-30.0, -30.0, -30.0, -30.0],
    );

    let err = expect_transform_error(Transform::from_crs_defs_with_selection_options(
        &mismatched_source,
        &target,
        SelectionOptions::new()
            .with_area_of_interest(AreaOfInterest::geographic_point(Coord::new(-74.5, 40.5)))
            .with_grid_provider(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
    ));

    assert!(
        err.to_string()
            .contains("no supported vertical grid operation"),
        "{err}"
    );
}

#[test]
fn parallel_transform_construction_with_shared_grid_provider_does_not_deadlock() {
    const THREADS: usize = 8;

    let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
    let provider: Arc<dyn crate::grid::GridProvider> =
        Arc::new(FilesystemGridProvider::new(vec![grid_root]));
    let source = registry::lookup_epsg(4979).unwrap();
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 + NAVD88 height",
    )));
    let barrier = Arc::new(Barrier::new(THREADS));

    let results = std::thread::scope(|scope| {
        let mut joins = Vec::new();
        for _ in 0..THREADS {
            let source = source.clone();
            let target = target.clone();
            let provider = Arc::clone(&provider);
            let barrier = Arc::clone(&barrier);
            joins.push(scope.spawn(move || {
                barrier.wait();
                let t = Transform::from_crs_defs_with_selection_options(
                    &source,
                    &target,
                    SelectionOptions {
                        grid_provider: Some(provider),
                        vertical_grid_operations: vec![test_vertical_grid_operation()],
                        ..SelectionOptions::default()
                    },
                )
                .unwrap();
                t.convert_3d((-74.5, 40.5, 100.0)).unwrap()
            }));
        }

        joins
            .into_iter()
            .map(|join| join.join().unwrap())
            .collect::<Vec<_>>()
    });

    for result in results {
        assert!((result.2 - 130.0).abs() < 1e-9);
    }
}

#[test]
fn vertical_grid_selection_prefers_area_of_use_match() {
    let grid_root = write_test_gtx_files(&[
        ("outside.gtx", -75.0, 40.0, &[-10.0, -10.0, -10.0, -10.0]),
        ("inside.gtx", -75.0, 40.0, &[-30.0, -30.0, -30.0, -30.0]),
    ]);
    let source = registry::lookup_epsg(4979).unwrap();
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 + NAVD88 height",
    )));

    let mut outside = test_vertical_grid_operation_named("outside geoid operation", "outside.gtx");
    outside.grid.area_of_use = Some(crate::operation::AreaOfUse {
        west: 0.0,
        south: 0.0,
        east: 1.0,
        north: 1.0,
        name: "outside".into(),
    });
    let inside = test_vertical_grid_operation_named("inside geoid operation", "inside.gtx");

    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            area_of_interest: Some(AreaOfInterest::geographic_point(Coord::new(-74.5, 40.5))),
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![outside, inside],
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
    assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
    assert_eq!(
        outcome.vertical.operation_name.as_deref(),
        Some("inside geoid operation")
    );
    assert_eq!(outcome.vertical.area_of_use_match, Some(true));
    assert_eq!(outcome.vertical.grids[0].area_of_use_match, Some(true));
}

#[test]
fn vertical_grid_runtime_falls_back_after_coverage_miss() {
    let grid_root = write_test_gtx_files(&[
        ("outside.gtx", 10.0, 10.0, &[-10.0, -10.0, -10.0, -10.0]),
        ("inside.gtx", -75.0, 40.0, &[-30.0, -30.0, -30.0, -30.0]),
    ]);
    let source = registry::lookup_epsg(4979).unwrap();
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 + NAVD88 height",
    )));

    let outside = test_vertical_grid_operation_named("outside geoid operation", "outside.gtx");
    let inside = test_vertical_grid_operation_named("inside geoid operation", "inside.gtx");
    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![outside, inside],
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    let outcome = t.convert_3d_with_diagnostics((-74.5, 40.5, 100.0)).unwrap();
    assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
    assert_eq!(
        outcome.vertical.operation_name.as_deref(),
        Some("inside geoid operation")
    );
}

#[test]
fn vertical_grid_rejects_unsupported_sampling_crs() {
    let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
    let source = registry::lookup_epsg(4979).unwrap();
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 + NAVD88 height",
    )));
    let mut operation = test_vertical_grid_operation();
    operation.grid_horizontal_crs_epsg = Some(3857);

    let err = expect_transform_error(Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![operation],
            ..SelectionOptions::default()
        },
    ));

    assert!(err
        .to_string()
        .contains("not a supported geographic sampling CRS"));
}

#[test]
fn vertical_geoid_grid_rejects_outside_coverage() {
    let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 + NAVD88 height",
    )));
    let source = registry::lookup_epsg(4979).unwrap();
    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![test_vertical_grid_operation()],
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    let err = t.convert_3d((-80.0, 40.5, 100.0)).unwrap_err();
    assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
}

#[test]
fn vertical_geoid_grid_rejects_non_finite_and_normalizes_large_longitude() {
    let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
    let horizontal =
        HorizontalCrsDef::Geographic(GeographicCrsDef::new(4326, datum::WGS84, "WGS 84"));
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        horizontal,
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 + NAVD88 height",
    )));
    let source = registry::lookup_epsg(4979).unwrap();
    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![test_vertical_grid_operation()],
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    for coord in [
        (f64::INFINITY, 40.5, 100.0),
        (f64::NEG_INFINITY, 40.5, 100.0),
        (f64::NAN, 40.5, 100.0),
        (-74.5, f64::INFINITY, 100.0),
        (-74.5, f64::NAN, 100.0),
    ] {
        let err = t.convert_3d_with_diagnostics(coord).unwrap_err();
        assert!(matches!(err, Error::OutOfRange(_)), "got {err}");
        let message = err.to_string();
        assert!(message.contains("finite"), "{message}");
    }

    let wrapped_lon = -74.5 + 360.0 * 1_000_000_000_000.0;
    let outcome = t
        .convert_3d_with_diagnostics((wrapped_lon, 40.5, 100.0))
        .unwrap();
    assert!((outcome.coord.2 - 130.0).abs() < 1e-9);
    assert_eq!(
        outcome.vertical.operation_name.as_deref(),
        Some("Test geoid height to NAVD88")
    );
}

#[test]
fn two_dimensional_convert_does_not_sample_vertical_grids() {
    let grid_root = write_test_gtx(&[-30.0, -30.0, -30.0, -30.0]);
    let source = registry::lookup_epsg(4979).unwrap();
    let target_horizontal = ProjectedCrsDef::new_with_base_geographic_crs(
        3857,
        4326,
        datum::WGS84,
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "WGS 84 / Pseudo-Mercator",
    );
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        HorizontalCrsDef::Projected(target_horizontal),
        registry::lookup_vertical_epsg(5703).unwrap(),
        "WGS 84 / Pseudo-Mercator + NAVD88 height",
    )));
    let t = Transform::from_crs_defs_with_selection_options(
        &source,
        &target,
        SelectionOptions {
            grid_provider: Some(Arc::new(FilesystemGridProvider::new(vec![grid_root]))),
            vertical_grid_operations: vec![test_vertical_grid_operation()],
            ..SelectionOptions::default()
        },
    )
    .unwrap();

    let (x, y) = t.convert((-80.0, 40.5)).unwrap();
    assert!(x < -8_900_000.0 && x > -8_910_000.0, "x = {x}");
    assert!(y > 4_930_000.0 && y < 4_940_000.0, "y = {y}");

    let outcome = t.convert_with_diagnostics((-80.0, 40.5)).unwrap();
    assert!((outcome.coord.0 - x).abs() < 1e-9);
    assert!((outcome.coord.1 - y).abs() < 1e-9);
    assert_eq!(outcome.vertical.action, VerticalTransformAction::None);
    assert!(outcome.grid_coverage_misses.is_empty());

    let err = t.convert_3d((-80.0, 40.5, 100.0)).unwrap_err();
    assert!(matches!(err, Error::Grid(GridError::OutsideCoverage(_))));
}

#[test]
fn explicit_vertical_to_horizontal_only_transform_is_rejected() {
    let source = registry::lookup_epsg(4979).unwrap();
    let target = registry::lookup_epsg(4326).unwrap();
    let err = expect_transform_error(Transform::from_crs_defs(&source, &target));
    assert!(err.to_string().contains("explicit vertical CRS"));
}

#[test]
fn mismatched_compound_vertical_crs_is_rejected() {
    let source = registry::lookup_epsg(4979).unwrap();
    let target_horizontal = GeographicCrsDef::new(4326, datum::WGS84, "WGS 84");
    let target_vertical =
        VerticalCrsDef::gravity_related_height(5703, 5103, LinearUnit::metre(), "NAVD88").unwrap();
    let target = CrsDef::Compound(Box::new(CompoundCrsDef::new(
        0,
        HorizontalCrsDef::Geographic(target_horizontal),
        target_vertical,
        "WGS 84 + NAVD88 height",
    )));

    let err = expect_transform_error(Transform::from_crs_defs(&source, &target));
    assert!(err.to_string().contains("vertical CRS transformations"));
    assert!(err.to_string().contains("geoid grid"));
}

#[test]
fn cross_datum_roundtrip_nad27_3d() {
    let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
    let inv = fwd.inverse().unwrap();
    let original = (-90.0, 45.0, 250.0);
    let shifted = fwd.convert_3d(original).unwrap();
    let back = inv.convert_3d(shifted).unwrap();
    assert!((back.0 - original.0).abs() < 1e-6);
    assert!((back.1 - original.1).abs() < 1e-6);
    assert!((back.2 - original.2).abs() < 1e-12);
}

#[test]
fn identical_custom_projected_crs_is_identity() {
    let from = CrsDef::Projected(ProjectedCrsDef::new(
        0,
        datum::WGS84,
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "Custom Web Mercator A",
    ));
    let to = CrsDef::Projected(ProjectedCrsDef::new(
        0,
        datum::WGS84,
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "Custom Web Mercator B",
    ));

    let t = Transform::from_crs_defs(&from, &to).unwrap();
    assert_eq!(t.selected_operation().name, "Identity");
    assert!(matches!(
        t.selected_operation_kind,
        SelectedOperationKind::Identity
    ));
}

#[test]
fn unknown_custom_datums_do_not_collapse_to_identity() {
    let unknown = datum::Datum::new(datum::WGS84.ellipsoid(), DatumToWgs84::Unknown).unwrap();
    let from = CrsDef::Projected(ProjectedCrsDef::new(
        0,
        unknown.clone(),
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "Unknown A",
    ));
    let to = CrsDef::Projected(ProjectedCrsDef::new(
        0,
        unknown,
        ProjectionMethod::WebMercator,
        LinearUnit::metre(),
        "Unknown B",
    ));

    let err = match Transform::from_crs_defs(&from, &to) {
        Ok(_) => panic!("unknown custom datums should not build a transform"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("no compatible registry operation found"),
        "{err}"
    );
}

#[test]
fn custom_helmert_datum_pair_without_registry_operation_fails() {
    let from = CrsDef::Geographic(GeographicCrsDef::new(0, datum::NAD27, "Custom NAD27"));
    let to = CrsDef::Geographic(GeographicCrsDef::new(0, datum::OSGB36, "Custom OSGB36"));

    let err = expect_transform_error(Transform::from_crs_defs(&from, &to));
    let message = err.to_string();
    assert!(message.contains("no compatible registry operation found"));
    assert!(!message.contains("allow_approximate_helmert_fallback"));
}

#[test]
fn selection_diagnostics_do_not_report_disabled_approximate_fallback() {
    let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();

    assert!(!t.selection_diagnostics().approximate);
    assert!(t
        .selection_diagnostics()
        .skipped_operations
        .iter()
        .all(|skipped| !skipped.metadata.approximate));
}

#[test]
fn inverse_exposes_swapped_crs() {
    let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let inv = fwd.inverse().unwrap();

    assert_eq!(fwd.source_crs().epsg(), 4326);
    assert_eq!(fwd.target_crs().epsg(), 3857);
    assert_eq!(inv.source_crs().epsg(), 3857);
    assert_eq!(inv.target_crs().epsg(), 4326);
}

#[test]
fn transform_bounds_web_mercator() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let bounds = Bounds::new(-74.3, 40.45, -73.65, 40.95);

    let result = t.transform_bounds(bounds, 8).unwrap();

    assert!(result.min_x < -8_200_000.0);
    assert!(result.max_x < -8_100_000.0);
    assert!(result.min_y > 4_900_000.0);
    assert!(result.max_y > result.min_y);
}

#[test]
fn transform_bounds_rejects_invalid_input() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let err = t
        .transform_bounds(Bounds::new(10.0, 5.0, -10.0, 20.0), 0)
        .unwrap_err();

    assert!(matches!(err, Error::OutOfRange(_)));
}

#[test]
fn transform_bounds_rejects_excessive_densification() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let err = t
        .transform_bounds(
            Bounds::new(-74.3, 40.45, -73.65, 40.95),
            MAX_BOUNDS_DENSIFY_POINTS + 1,
        )
        .unwrap_err();

    assert!(matches!(err, Error::OutOfRange(_)));
    assert!(err.to_string().contains("exceeds maximum"));
}

#[test]
fn transform_wrapped_geographic_bounds_crossing_antimeridian() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let result = t
        .transform_geographic_wrapped_bounds(Bounds::new(170.0, -20.0, -170.0, -10.0), 8)
        .unwrap();

    assert!(result.min_x < -20_000_000.0, "min_x = {}", result.min_x);
    assert!(result.max_x > 20_000_000.0, "max_x = {}", result.max_x);
    assert!(result.min_y < -1_000_000.0, "min_y = {}", result.min_y);
    assert!(result.max_y < 0.0, "max_y = {}", result.max_y);
}

#[test]
fn transform_wrapped_geographic_bounds_rejects_projected_source() {
    let t = Transform::new("EPSG:3857", "EPSG:4326").unwrap();
    let err = t
        .transform_geographic_wrapped_bounds(Bounds::new(170.0, -20.0, -170.0, -10.0), 0)
        .unwrap_err();

    assert!(matches!(err, Error::InvalidDefinition(_)));
}

#[test]
fn batch_transform() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64)> = (0..10)
        .map(|i| (-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
        .collect();

    let results = t.convert_batch(&coords).unwrap();
    assert_eq!(results.len(), 10);
    for (x, _y) in &results {
        assert!(*x < 0.0);
    }
}

#[test]
fn batch_transform_3d() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64, f64)> = (0..10)
        .map(|i| (-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1, i as f64))
        .collect();

    let results = t.convert_batch_3d(&coords).unwrap();
    assert_eq!(results.len(), 10);
    for (index, (x, _y, z)) in results.iter().enumerate() {
        assert!(*x < 0.0);
        assert!((*z - index as f64).abs() < 1e-12);
    }
}

#[test]
fn batch_transform_coords_in_place_matches_vec_batch() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let input: Vec<Coord> = (0..10)
        .map(|i| Coord::new(-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
        .collect();
    let expected = t.convert_batch(&input).unwrap();
    let mut actual = input.clone();

    t.convert_coords_in_place(&mut actual).unwrap();

    assert_eq!(actual, expected);
}

#[test]
fn batch_transform_coords_into_reuses_output_slice() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let input: Vec<Coord> = (0..10)
        .map(|i| Coord::new(-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1))
        .collect();
    let expected = t.convert_batch(&input).unwrap();
    let mut actual = vec![Coord::new(0.0, 0.0); input.len()];

    t.convert_coords_into(&input, &mut actual).unwrap();

    assert_eq!(actual, expected);
}

#[test]
fn batch_transform_coords_into_rejects_mismatched_output_len() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let input = [Coord::new(-74.0, 40.0), Coord::new(-73.0, 41.0)];
    let mut output = [Coord::new(0.0, 0.0)];

    let err = t.convert_coords_into(&input, &mut output).unwrap_err();

    assert!(matches!(err, Error::OutOfRange(_)));
}

#[test]
fn batch_transform_coords_3d_in_place_and_into_preserve_height() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let input: Vec<Coord3D> = (0..10)
        .map(|i| Coord3D::new(-74.0 + i as f64 * 0.1, 40.0 + i as f64 * 0.1, i as f64))
        .collect();
    let expected = t.convert_batch_3d(&input).unwrap();

    let mut in_place = input.clone();
    t.convert_coords_3d_in_place(&mut in_place).unwrap();
    assert_eq!(in_place, expected);

    let mut output = vec![Coord3D::new(0.0, 0.0, 0.0); input.len()];
    t.convert_coords_3d_into(&input, &mut output).unwrap();
    assert_eq!(output, expected);
}

#[test]
fn coord_type() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let c = Coord::new(-74.006, 40.7128);
    let result = t.convert(c).unwrap();
    assert!((result.x - (-8238310.0)).abs() < 100.0);
}

#[cfg(feature = "geo-types")]
#[test]
fn geo_types_coord() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let c = geo_types::Coord {
        x: -74.006,
        y: 40.7128,
    };
    let result: geo_types::Coord<f64> = t.convert(c).unwrap();
    assert!((result.x - (-8238310.0)).abs() < 100.0);
}

#[cfg(feature = "rayon")]
#[test]
fn parallel_batch_transform() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64)> = (0..100)
        .map(|i| (-74.0 + i as f64 * 0.01, 40.0 + i as f64 * 0.01))
        .collect();

    let results = t.convert_batch_parallel(&coords).unwrap();
    assert_eq!(results.len(), 100);
}

#[cfg(feature = "rayon")]
#[test]
fn parallel_batch_transform_matches_sequential_on_large_input() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let len = rayon::current_num_threads() * PARALLEL_MIN_ITEMS_PER_THREAD;
    let coords: Vec<(f64, f64)> = (0..len)
        .map(|i| (-179.0 + i as f64 * 0.0001, -80.0 + i as f64 * 0.00005))
        .collect();

    let sequential = t.convert_batch(&coords).unwrap();
    let parallel = t.convert_batch_parallel(&coords).unwrap();

    assert_eq!(parallel, sequential);
}

#[cfg(feature = "rayon")]
#[test]
fn parallel_batch_transform_3d() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64, f64)> = (0..100)
        .map(|i| (-74.0 + i as f64 * 0.01, 40.0 + i as f64 * 0.01, i as f64))
        .collect();

    let results = t.convert_batch_parallel_3d(&coords).unwrap();
    assert_eq!(results.len(), 100);
}

#[cfg(feature = "rayon")]
#[test]
fn parallel_batch_transform_3d_matches_sequential_on_large_input() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let len = rayon::current_num_threads() * PARALLEL_MIN_ITEMS_PER_THREAD;
    let coords: Vec<(f64, f64, f64)> = (0..len)
        .map(|i| {
            (
                -179.0 + i as f64 * 0.0001,
                -80.0 + i as f64 * 0.00005,
                i as f64,
            )
        })
        .collect();

    let sequential = t.convert_batch_3d(&coords).unwrap();
    let parallel = t.convert_batch_parallel_3d(&coords).unwrap();

    assert_eq!(parallel, sequential);
}

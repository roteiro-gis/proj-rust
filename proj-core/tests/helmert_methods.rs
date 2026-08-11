#[path = "common/helmert_cases.rs"]
mod helmert_cases;

use helmert_cases::cases;
use proj_core::{CoordinateOperationId, Transform};

#[test]
fn corrected_helmert_families_match_c_proj_reference_values() {
    for case in cases() {
        let operation_id = CoordinateOperationId(case.operation_epsg);
        let transform = Transform::from_operation(
            operation_id,
            &format!("EPSG:{}", case.source_epsg),
            &format!("EPSG:{}", case.target_epsg),
        )
        .unwrap_or_else(|error| panic!("{}: setup failed: {error}", case.description));
        assert_eq!(transform.selected_operation().id, Some(operation_id));

        let actual = transform
            .convert_3d(case.input)
            .unwrap_or_else(|error| panic!("{}: transform failed: {error}", case.description));
        let delta = (
            (actual.0 - case.expected_xy.0).abs(),
            (actual.1 - case.expected_xy.1).abs(),
        );
        assert!(
            delta.0 < 1e-11 && delta.1 < 1e-11,
            "{} (EPSG:{}): expected {:?}, got ({}, {}), delta {delta:?}",
            case.description,
            case.operation_epsg,
            case.expected_xy,
            actual.0,
            actual.1
        );

        let roundtrip = transform
            .inverse()
            .and_then(|inverse| inverse.convert_3d(actual))
            .unwrap_or_else(|error| panic!("{}: inverse failed: {error}", case.description));
        let roundtrip_delta = (
            (roundtrip.0 - case.input.0).abs(),
            (roundtrip.1 - case.input.1).abs(),
            (roundtrip.2 - case.input.2).abs(),
        );
        assert!(
            roundtrip_delta.0 < 1e-11 && roundtrip_delta.1 < 1e-11 && roundtrip_delta.2 < 1e-6,
            "{} (EPSG:{}): inverse roundtrip delta {roundtrip_delta:?}",
            case.description,
            case.operation_epsg
        );
    }
}

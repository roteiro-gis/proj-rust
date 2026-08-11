#![cfg(feature = "c-proj-compat")]

#[path = "common/c_proj_ffi.rs"]
mod c_proj_ffi;
#[path = "common/helmert_cases.rs"]
mod helmert_cases;

use c_proj_ffi::CProjTransform;
use helmert_cases::cases;
use proj_core::{CoordinateOperationId, Transform};

#[test]
fn explicit_helmert_operations_match_c_proj() {
    for case in cases() {
        let rust = Transform::from_operation(
            CoordinateOperationId(case.operation_epsg),
            &format!("EPSG:{}", case.source_epsg),
            &format!("EPSG:{}", case.target_epsg),
        )
        .unwrap_or_else(|error| panic!("{}: proj-core setup failed: {error}", case.description));
        let c_proj = CProjTransform::new_coordinate_operation(case.operation_epsg)
            .unwrap_or_else(|error| panic!("{}: C PROJ setup failed: {error}", case.description));

        let actual = rust
            .convert_3d(case.input)
            .unwrap_or_else(|error| panic!("{}: proj-core failed: {error}", case.description));
        let expected = c_proj
            .convert_3d(case.input)
            .unwrap_or_else(|error| panic!("{}: C PROJ failed: {error}", case.description));
        // These EPSG records have a geographic-2D domain. C PROJ's explicit
        // operation object therefore preserves z, whereas proj-core's 3D API
        // deliberately propagates the geocentric height change. Compare the
        // shared longitude/latitude contract here.
        let delta = ((actual.0 - expected.0).abs(), (actual.1 - expected.1).abs());
        let reference_delta = (
            (case.expected_xy.0 - expected.0).abs(),
            (case.expected_xy.1 - expected.1).abs(),
        );

        eprintln!(
            "EPSG:{} expected={expected:?} actual={actual:?} delta={delta:?}",
            case.operation_epsg
        );
        assert!(
            delta.0 < 1e-10 && delta.1 < 1e-10,
            "{} (EPSG:{}): expected {expected:?}, got {actual:?}, delta {delta:?}",
            case.description,
            case.operation_epsg
        );
        assert!(
            reference_delta.0 < 1e-12 && reference_delta.1 < 1e-12,
            "{} (EPSG:{}): stored reference {:?} drifted from C PROJ {expected:?}",
            case.description,
            case.operation_epsg,
            case.expected_xy
        );
    }
}

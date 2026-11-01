#![cfg(feature = "c-proj-compat")]

#[path = "common/c_proj_ffi.rs"]
mod c_proj_ffi;

use c_proj_ffi::CProjTransform;
use proj_core::{lookup_authority_axis_order, lookup_epsg, AxisOrder, Transform};

/// The registry's authority axis order against C PROJ's own, across every kind
/// of axis definition: east/north, north/east, `X`/`Y` in either order, and
/// polar systems oriented along meridians.
#[test]
fn authority_axis_order_matches_c_proj() {
    for (code, lon, lat, expected) in [
        (3857, 5.0, 52.0, AxisOrder::EastNorth),
        (26918, -75.0, 40.0, AxisOrder::EastNorth),
        (3912, 15.0, 46.0, AxisOrder::EastNorth),
        (4826, -24.0, 15.0, AxisOrder::EastNorth),
        (2193, 174.0, -41.0, AxisOrder::NorthEast),
        (3006, 15.0, 60.0, AxisOrder::NorthEast),
        (3035, 10.0, 50.0, AxisOrder::NorthEast),
        (31467, 9.0, 50.0, AxisOrder::NorthEast),
        (3031, 30.0, -80.0, AxisOrder::EastNorth),
        (3413, -45.0, 75.0, AxisOrder::EastNorth),
        (5041, 10.0, 85.0, AxisOrder::EastNorth),
        (32661, 10.0, 85.0, AxisOrder::NorthEast),
        (32761, 10.0, -85.0, AxisOrder::NorthEast),
    ] {
        assert_eq!(
            lookup_authority_axis_order(code),
            Some(expected),
            "EPSG:{code}"
        );

        let target = lookup_epsg(code)
            .unwrap()
            .with_axis_order(lookup_authority_axis_order(code).unwrap());
        let source = lookup_epsg(4326).unwrap();
        let (x, y) = Transform::from_crs_defs(&source, &target)
            .unwrap()
            .convert((lon, lat))
            .unwrap();

        let (c_x, c_y, _) =
            CProjTransform::new_known_crs_authority_axes("OGC:CRS84", &format!("EPSG:{code}"))
                .unwrap()
                .convert_3d((lon, lat, 0.0))
                .unwrap();

        // Only the order is under test here, so each value just has to be
        // nearer C PROJ's value in the same position than in the other.
        assert!(
            (x - c_x).abs() < (x - c_y).abs() && (y - c_y).abs() < (y - c_x).abs(),
            "EPSG:{code}"
        );
    }
}

#[test]
fn geographic_authority_axis_order_matches_c_proj() {
    assert_eq!(
        lookup_authority_axis_order(4326),
        Some(AxisOrder::NorthEast)
    );
    assert_eq!(
        lookup_authority_axis_order(4979),
        Some(AxisOrder::NorthEast)
    );
    assert_eq!(lookup_authority_axis_order(4978), None);

    let (lat, lon, _) = CProjTransform::new_known_crs_authority_axes("OGC:CRS84", "EPSG:4326")
        .unwrap()
        .convert_3d((5.0, 52.0, 0.0))
        .unwrap();
    assert!((lat - 52.0).abs() < 1e-9 && (lon - 5.0).abs() < 1e-9);
}

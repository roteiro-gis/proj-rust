#![cfg(feature = "c-proj-compat")]

#[path = "common/c_proj_ffi.rs"]
mod c_proj_ffi;

use c_proj_ffi::CProjTransform;
use proj_core::Transform;

struct ReferencePoint3D {
    from_epsg: u32,
    to_epsg: u32,
    input: (f64, f64, f64),
    tolerance_xy: f64,
    tolerance_z: f64,
    description: &'static str,
}

fn cases() -> [ReferencePoint3D; 6] {
    [
        ReferencePoint3D {
            from_epsg: 4326,
            to_epsg: 3857,
            input: (-74.006, 40.7128, 15.0),
            tolerance_xy: 0.001,
            tolerance_z: 1e-9,
            description: "WGS84 3D to Web Mercator",
        },
        ReferencePoint3D {
            from_epsg: 3857,
            to_epsg: 4326,
            input: (-8238310.0, 4970072.0, 15.0),
            tolerance_xy: 1e-7,
            tolerance_z: 1e-9,
            description: "Web Mercator 3D to WGS84",
        },
        ReferencePoint3D {
            from_epsg: 4267,
            to_epsg: 4326,
            input: (-90.0, 45.0, 250.0),
            tolerance_xy: 0.001,
            tolerance_z: 0.01,
            description: "NAD27 3D to WGS84",
        },
        ReferencePoint3D {
            from_epsg: 4326,
            to_epsg: 27700,
            input: (-0.1278, 51.5074, 45.0),
            tolerance_xy: 1.0,
            tolerance_z: 0.05,
            description: "WGS84 3D to British National Grid",
        },
        ReferencePoint3D {
            from_epsg: 4326,
            to_epsg: 27700,
            input: (-0.1278, 51.5074, 10_000.0),
            tolerance_xy: 0.01,
            tolerance_z: 0.05,
            description: "WGS84 3D to British National Grid with high ellipsoidal height",
        },
        ReferencePoint3D {
            from_epsg: 27700,
            to_epsg: 4326,
            input: (530000.0, 180000.0, 45.0),
            tolerance_xy: 1e-6,
            tolerance_z: 0.05,
            description: "British National Grid 3D to WGS84",
        },
    ]
}

#[test]
fn proj_core_matches_live_c_proj_for_3d_cases() {
    let mut failures = Vec::new();

    for case in cases() {
        let transform = Transform::from_epsg(case.from_epsg, case.to_epsg).unwrap_or_else(|e| {
            panic!(
                "{}: failed to create proj-core transform EPSG:{}->EPSG:{}: {e}",
                case.description, case.from_epsg, case.to_epsg
            )
        });
        let c_transform = CProjTransform::new_known_crs(
            &format!("EPSG:{}", case.from_epsg),
            &format!("EPSG:{}", case.to_epsg),
        )
        .unwrap_or_else(|e| {
            panic!(
                "{}: failed to create C PROJ transform EPSG:{}->EPSG:{}: {e}",
                case.description, case.from_epsg, case.to_epsg
            )
        });

        let expected = c_transform.convert_3d(case.input).unwrap_or_else(|e| {
            panic!(
                "{}: C PROJ convert failed for EPSG:{}->EPSG:{}: {e}",
                case.description, case.from_epsg, case.to_epsg
            )
        });
        let actual = transform.convert_3d(case.input).unwrap_or_else(|e| {
            panic!(
                "{}: proj-core convert_3d failed for EPSG:{}->EPSG:{}: {e}",
                case.description, case.from_epsg, case.to_epsg
            )
        });

        let dx = (actual.0 - expected.0).abs();
        let dy = (actual.1 - expected.1).abs();
        let dz = (actual.2 - expected.2).abs();

        if dx > case.tolerance_xy || dy > case.tolerance_xy || dz > case.tolerance_z {
            failures.push(format!(
                "{}: expected ({}, {}, {}), got ({}, {}, {}), delta ({:e}, {:e}, {:e}), tol_xy {:e}, tol_z {:e}",
                case.description,
                expected.0,
                expected.1,
                expected.2,
                actual.0,
                actual.1,
                actual.2,
                dx,
                dy,
                dz,
                case.tolerance_xy,
                case.tolerance_z
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} of {} live C PROJ 3D comparisons failed:\n{}",
            failures.len(),
            cases().len(),
            failures.join("\n")
        );
    }
}

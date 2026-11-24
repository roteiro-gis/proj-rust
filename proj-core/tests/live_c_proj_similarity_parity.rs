//! The 3D similarity and engineering-CRS transforms against the linked
//! C PROJ's `+proj=helmert +exact`.
#![cfg(feature = "c-proj-compat")]

#[path = "common/c_proj_ffi.rs"]
mod c_proj_ffi;

use c_proj_ffi::{assert_reference_version, CProjTransform};
use proj_core::{
    lookup_epsg, CoordinateOperation, CrsDef, EngineeringCrsDef, Identifier, OperationDomain,
    OperationMethod, RotationConvention, SelectionOptions, Similarity3DParams, Transform,
};

struct Case {
    translation: [f64; 3],
    rotation_arcsec: [f64; 3],
    scale_ppm: f64,
}

/// The plan's appendix frame (37° against RD, slightly tilted), a frame
/// tilted by degrees, and one with a tiny datum-like rotation.
const CASES: [Case; 3] = [
    Case {
        translation: [90512.334, 435870.112, -3.418],
        rotation_arcsec: [-12.6, 31.4, 133200.0],
        scale_ppm: -41.7,
    },
    Case {
        translation: [1500.25, -820.5, 12.75],
        rotation_arcsec: [4320.0, -2880.0, -250000.0],
        scale_ppm: 125.5,
    },
    Case {
        translation: [155000.0, 463000.0, 43.0],
        rotation_arcsec: [0.35, -0.12, 1.8],
        scale_ppm: 4.1,
    },
];

const POINTS: [[f64; 3]; 6] = [
    [0.0, 0.0, 0.0],
    [1000.0, 2000.0, 10.0],
    [-350.25, 812.5, 4.75],
    [123.456, -789.012, -2.5],
    [4999.999, -4999.999, 250.0],
    [0.001, 0.002, -0.003],
];

fn proj_helmert(case: &Case, convention: RotationConvention, inverse: bool) -> String {
    let convention = match convention {
        RotationConvention::PositionVector => "position_vector",
        RotationConvention::CoordinateFrame => "coordinate_frame",
    };
    let [x, y, z] = case.translation;
    let [rx, ry, rz] = case.rotation_arcsec;
    format!(
        "+proj=pipeline +step {}+proj=helmert +convention={convention} +exact \
         +x={x} +y={y} +z={z} +rx={rx} +ry={ry} +rz={rz} +s={}",
        if inverse { "+inv " } else { "" },
        case.scale_ppm
    )
}

fn params(case: &Case, convention: RotationConvention) -> Similarity3DParams {
    Similarity3DParams::new(
        case.translation,
        case.rotation_arcsec,
        case.scale_ppm,
        convention,
    )
    .unwrap()
}

fn assert_close(actual: [f64; 3], expected: (f64, f64, f64), tolerance: f64, context: &str) {
    let delta = [
        (actual[0] - expected.0).abs(),
        (actual[1] - expected.1).abs(),
        (actual[2] - expected.2).abs(),
    ];
    assert!(
        delta.iter().all(|delta| *delta < tolerance),
        "{context}: proj-core {actual:?}, C PROJ {expected:?}, delta {delta:?}"
    );
}

const CONVENTIONS: [RotationConvention; 2] = [
    RotationConvention::PositionVector,
    RotationConvention::CoordinateFrame,
];

#[test]
fn similarity_matches_c_proj_exact_helmert() {
    assert_reference_version();
    for (index, case) in CASES.iter().enumerate() {
        for convention in CONVENTIONS {
            let params = params(case, convention);
            let forward =
                CProjTransform::new_proj_string(&proj_helmert(case, convention, false)).unwrap();
            let inverse =
                CProjTransform::new_proj_string(&proj_helmert(case, convention, true)).unwrap();
            for point in POINTS {
                let context = format!("case {index} {convention:?} {point:?}");
                let expected = forward.convert_3d((point[0], point[1], point[2])).unwrap();
                assert_close(params.forward(point), expected, 1e-9, &context);
                let base = params.forward(point);
                let expected = inverse.convert_3d((base[0], base[1], base[2])).unwrap();
                assert_close(params.inverse(base), expected, 1e-9, &context);
            }
        }
    }
}

#[test]
fn frame_transform_matches_c_proj_exact_helmert() {
    assert_reference_version();
    let frame = CrsDef::Engineering(Box::new(
        EngineeringCrsDef::new("Site grid", "Site grid", 3)
            .unwrap()
            .with_identifier(Identifier::new("GEORIZON", "12").unwrap()),
    ));
    let rd_nap = lookup_epsg(7415).unwrap();
    let case = &CASES[0];
    for convention in CONVENTIONS {
        let operation = CoordinateOperation {
            id: None,
            name: "Site grid to RD + NAP".into(),
            source_crs_epsg: None,
            target_crs_epsg: Some(7415),
            source_crs_id: frame.identifier().cloned(),
            target_crs_id: None,
            source_datum_epsg: None,
            target_datum_epsg: None,
            accuracy: None,
            areas_of_use: Default::default(),
            deprecated: false,
            preferred: true,
            approximate: false,
            superseded: false,
            domain: OperationDomain::IncludesHeight,
            method: OperationMethod::Similarity3D {
                params: params(case, convention),
            },
        };
        let transform = Transform::from_crs_defs_with_selection_options(
            &frame,
            &rd_nap,
            SelectionOptions::new().with_coordinate_operation(operation),
        )
        .unwrap();
        let back = transform.inverse().unwrap();
        let forward =
            CProjTransform::new_proj_string(&proj_helmert(case, convention, false)).unwrap();
        let inverse =
            CProjTransform::new_proj_string(&proj_helmert(case, convention, true)).unwrap();
        for point in POINTS {
            let context = format!("{convention:?} {point:?}");
            let expected = forward.convert_3d((point[0], point[1], point[2])).unwrap();
            let actual = transform
                .convert_3d((point[0], point[1], point[2]))
                .unwrap();
            // RD + NAP to itself runs RD's projection there and back.
            assert_close([actual.0, actual.1, actual.2], expected, 1e-8, &context);
            let expected = inverse.convert_3d(actual).unwrap();
            let actual = back.convert_3d(actual).unwrap();
            assert_close([actual.0, actual.1, actual.2], expected, 1e-8, &context);
        }
    }
}

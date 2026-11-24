//! The 3D similarity against C PROJ's `+proj=helmert +exact`.
//!
//! Every expected value comes from `cct -d 10` (PROJ 9.8.1), for example:
//!
//! ```text
//! cct -d 10 +proj=helmert +convention=position_vector +exact \
//!     +x=90512.334 +y=435870.112 +z=-3.418 \
//!     +rx=-12.6 +ry=31.4 +rz=133200 +s=-41.7
//! ```
//!
//! `live_c_proj_similarity_parity` runs the same operations through the
//! linked C PROJ instead of these pinned values.

use proj_core::{RotationConvention, Similarity3DParams};

/// A frame turned 37° against RD, slightly tilted (the plan's appendix).
fn appendix(convention: RotationConvention) -> Similarity3DParams {
    Similarity3DParams::new(
        [90512.334, 435870.112, -3.418],
        [-12.6, 31.4, 133200.0],
        -41.7,
        convention,
    )
    .unwrap()
}

/// Rotations of a degree and more about every axis, to show the matrix is
/// exact rather than small-angle.
fn tilted(convention: RotationConvention) -> Similarity3DParams {
    Similarity3DParams::new(
        [1500.25, -820.5, 12.75],
        [4320.0, -2880.0, -250000.0],
        125.5,
        convention,
    )
    .unwrap()
}

const FRAME_POINTS: [[f64; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [1000.0, 2000.0, 10.0],
    [-350.25, 812.5, 4.75],
    [123.456, -789.012, -2.5],
];

fn assert_close(actual: [f64; 3], expected: [f64; 3], context: &str) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 1e-9,
            "{context}: axis {axis} is {} where cct gives {}",
            actual[axis],
            expected[axis]
        );
    }
}

#[test]
fn position_vector_matches_cct() {
    let expected = [
        [90512.334, 435870.112, -3.418],
        [90107.3578789594, 438069.1069518616, 6.5089042981],
        [89743.6699929273, 436308.1996675288, 1.4220556089],
        [91085.7453211032, 435314.2996957727, -5.9712344060],
    ];
    let params = appendix(RotationConvention::PositionVector);
    for (point, expected) in FRAME_POINTS.iter().zip(expected) {
        assert_close(params.forward(*point), expected, "appendix position vector");
    }

    let expected = [
        [1500.25, -820.5, 12.75],
        [3723.9524488153, -1055.4390793450, 48.8907247035],
        [2137.9932492112, -207.6119022687, 39.2473847320],
        [804.8354126138, -1212.8375464811, -7.6819866164],
    ];
    let params = tilted(RotationConvention::PositionVector);
    for (point, expected) in FRAME_POINTS.iter().zip(expected) {
        assert_close(params.forward(*point), expected, "tilted position vector");
    }
}

#[test]
fn coordinate_frame_matches_cct() {
    let expected = [
        [90512.334, 435870.112, -3.418],
        [92514.5144521890, 436865.5269298885, 6.8559759642],
        [90721.5771372927, 436729.7534195637, 1.3281157336],
        [90136.1071612485, 435165.7105887597, -5.9472985985],
    ];
    let params = appendix(RotationConvention::CoordinateFrame);
    for (point, expected) in FRAME_POINTS.iter().zip(expected) {
        assert_close(
            params.forward(*point),
            expected,
            "appendix coordinate frame",
        );
    }

    let expected = [
        [1500.25, -820.5, 12.75],
        [-21.4657992286, 817.6800075942, -33.1018587372],
        [616.4165732657, -863.3304635737, 5.3737658682],
        [2282.4243063767, -981.7442520500, 25.0508304736],
    ];
    let params = tilted(RotationConvention::CoordinateFrame);
    for (point, expected) in FRAME_POINTS.iter().zip(expected) {
        assert_close(params.forward(*point), expected, "tilted coordinate frame");
    }
}

#[test]
fn inverse_matches_cct() {
    let base = [[90512.334, 435870.112, -3.418], [91000.0, 436500.0, 5.0]];

    let params = appendix(RotationConvention::PositionVector);
    let expected = [
        [0.0, 0.0, 0.0],
        [768.5741526816, 209.5753048870, 8.5310714247],
    ];
    for (point, expected) in base.iter().zip(expected) {
        assert_close(params.inverse(*point), expected, "appendix inverse");
    }

    let params = tilted(RotationConvention::CoordinateFrame);
    let expected = [
        [440043.0861336598, 69831.4656818503, 7592.4973610529],
        [440803.8104138278, 69595.6941467268, 7606.6029973879],
    ];
    for (point, expected) in base.iter().zip(expected) {
        assert_close(params.inverse(*point), expected, "tilted inverse");
    }
}

#[test]
fn small_angle_matrix_would_be_a_hundred_metres_off() {
    // cct without +exact, for the record of why the step must be exact:
    // C PROJ's default small-angle matrix puts this point 120 m away.
    let small_angle = [90220.8040331457, 438515.7741053927, 6.3071898992];
    let exact = appendix(RotationConvention::PositionVector).forward(FRAME_POINTS[1]);
    let miss = ((exact[0] - small_angle[0]).powi(2) + (exact[1] - small_angle[1]).powi(2)).sqrt();
    assert!(miss > 100.0, "small-angle miss is only {miss} m");
}

#[test]
fn conventions_are_transposes_and_roundtrip() {
    for (position_vector, coordinate_frame) in [
        (
            appendix(RotationConvention::PositionVector),
            appendix(RotationConvention::CoordinateFrame),
        ),
        (
            tilted(RotationConvention::PositionVector),
            tilted(RotationConvention::CoordinateFrame),
        ),
    ] {
        let pv = position_vector.rotation_matrix();
        let cf = coordinate_frame.rotation_matrix();
        for row in 0..3 {
            for column in 0..3 {
                assert_eq!(pv[row * 3 + column], cf[column * 3 + row]);
            }
        }
        for params in [position_vector, coordinate_frame] {
            for point in FRAME_POINTS {
                let roundtrip = params.inverse(params.forward(point));
                assert_close(roundtrip, point, "roundtrip");
            }
        }
    }
}

#[test]
fn rejects_non_finite_parameters_and_a_collapsing_scale() {
    let pv = RotationConvention::PositionVector;
    assert!(Similarity3DParams::new([f64::NAN, 0.0, 0.0], [0.0; 3], 0.0, pv).is_err());
    assert!(Similarity3DParams::new([0.0; 3], [f64::INFINITY, 0.0, 0.0], 0.0, pv).is_err());
    assert!(Similarity3DParams::new([0.0; 3], [0.0; 3], -1e6, pv).is_err());
    assert_eq!(
        Similarity3DParams::identity().forward([1.0, 2.0, 3.0]),
        [1.0, 2.0, 3.0]
    );
}

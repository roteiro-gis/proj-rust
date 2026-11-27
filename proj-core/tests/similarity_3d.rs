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

/// Rotation matrix of the unit quaternion (w, x, y, z), row-major: an
/// independent construction, as a Procrustes fit would produce.
fn quaternion_matrix(w: f64, x: f64, y: f64, z: f64) -> [f64; 9] {
    let n = (w * w + x * x + y * y + z * z).sqrt();
    let (w, x, y, z) = (w / n, x / n, y / n, z / n);
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - w * z),
        2.0 * (x * z + w * y),
        2.0 * (x * y + w * z),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - w * x),
        2.0 * (x * z - w * y),
        2.0 * (y * z + w * x),
        1.0 - 2.0 * (x * x + y * y),
    ]
}

const CONVENTIONS: [RotationConvention; 2] = [
    RotationConvention::PositionVector,
    RotationConvention::CoordinateFrame,
];

#[test]
fn from_rotation_matrix_recovers_the_angles_and_matches_cct() {
    for params in CONVENTIONS
        .into_iter()
        .flat_map(|convention| [appendix(convention), tilted(convention)])
    {
        let rebuilt = Similarity3DParams::from_rotation_matrix(
            params.translation(),
            params.rotation_matrix(),
            params.scale_ppm(),
            params.convention(),
        )
        .unwrap();
        for (rebuilt, original) in rebuilt
            .rotation_arcsec()
            .iter()
            .zip(params.rotation_arcsec())
        {
            assert!((rebuilt - original).abs() < 1e-6, "{rebuilt} vs {original}");
        }
        for point in FRAME_POINTS {
            assert_close(rebuilt.forward(point), params.forward(point), "rebuilt");
        }
    }

    // From the matrix alone, the appendix frame still gives cct's values.
    let appendix_pv = appendix(RotationConvention::PositionVector);
    let rebuilt = Similarity3DParams::from_rotation_matrix(
        appendix_pv.translation(),
        appendix_pv.rotation_matrix(),
        appendix_pv.scale_ppm(),
        RotationConvention::PositionVector,
    )
    .unwrap();
    assert_close(
        rebuilt.forward(FRAME_POINTS[1]),
        [90107.3578789594, 438069.1069518616, 6.5089042981],
        "appendix from its matrix",
    );
}

#[test]
fn from_rotation_matrix_applies_a_fitted_matrix_as_given() {
    let translation = [91000.0, 436000.0, 2.5];
    let scale_ppm = 87.5;
    let scale = 1.0 + scale_ppm * 1e-6;
    for rotation in [
        quaternion_matrix(0.95, 0.01, -0.02, 0.31),
        quaternion_matrix(0.2, 0.05, 0.03, -0.98),
        quaternion_matrix(0.7, 0.2, -0.3, 0.6),
    ] {
        for convention in CONVENTIONS {
            let params = Similarity3DParams::from_rotation_matrix(
                translation,
                rotation,
                scale_ppm,
                convention,
            )
            .unwrap();
            for (rebuilt, given) in params.rotation_matrix().iter().zip(rotation) {
                assert!((rebuilt - given).abs() < 1e-12);
            }
            for point in FRAME_POINTS {
                let r = rotation;
                let expected = [0, 1, 2].map(|row| {
                    translation[row]
                        + scale
                            * (r[row * 3] * point[0]
                                + r[row * 3 + 1] * point[1]
                                + r[row * 3 + 2] * point[2])
                });
                assert_close(params.forward(point), expected, "fitted matrix");
            }
        }
    }
}

#[test]
fn from_rotation_matrix_rejects_what_is_not_a_rotation() {
    let pv = RotationConvention::PositionVector;
    let mirror = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
    let error = Similarity3DParams::from_rotation_matrix([0.0; 3], mirror, 0.0, pv).unwrap_err();
    assert!(error.to_string().contains("reflection"), "{error}");
    let stretched = [1.001, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    assert!(Similarity3DParams::from_rotation_matrix([0.0; 3], stretched, 0.0, pv).is_err());
    let skewed = [1.0, 0.01, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    assert!(Similarity3DParams::from_rotation_matrix([0.0; 3], skewed, 0.0, pv).is_err());
}

#[test]
fn a_frame_whose_north_is_clockwise_of_grid_north_has_negative_rz() {
    // North 30° clockwise from grid north: the frame's y axis points at
    // azimuth 30°, that is (sin 30°, cos 30°) in easting and northing.
    let theta = 30.0_f64;
    let azimuth = [theta.to_radians().sin(), theta.to_radians().cos(), 0.0];
    for (convention, rz) in [
        (RotationConvention::PositionVector, -theta * 3600.0),
        (RotationConvention::CoordinateFrame, theta * 3600.0),
    ] {
        let params = Similarity3DParams::new([0.0; 3], [0.0, 0.0, rz], 0.0, convention).unwrap();
        assert_close(params.forward([0.0, 1.0, 0.0]), azimuth, "frame north");
    }
}

#[test]
fn from_rotation_matrix_rebuilds_a_90_degree_y_turn() {
    // At ry = ±90° only rx ± rz is defined; the angles returned must still
    // rebuild the matrix.
    let pv = RotationConvention::PositionVector;
    for angles in [
        [0.0, 324000.0, 0.0],
        [72000.0, 324000.0, 126000.0],
        [72000.0, -324000.0, 126000.0],
    ] {
        let matrix = Similarity3DParams::new([0.0; 3], angles, 0.0, pv)
            .unwrap()
            .rotation_matrix();
        let rebuilt = Similarity3DParams::from_rotation_matrix([0.0; 3], matrix, 0.0, pv)
            .unwrap()
            .rotation_matrix();
        for (rebuilt, given) in rebuilt.iter().zip(matrix) {
            assert!((rebuilt - given).abs() < 1e-9, "{angles:?}");
        }
    }
}

use proj_core::{CoordinateOperationId, Transform};

#[test]
fn coordinate_frame_operation_uses_position_vector_rotation_signs() {
    // RDNAPTRANS control point at the Onze Lieve Vrouwetoren in Amersfoort.
    // EPSG:4833 is defined using the coordinate-frame convention, while the
    // runtime Helmert implementation uses position-vector rotations.
    let transform = Transform::new("EPSG:4326", "EPSG:28992").unwrap();

    assert_eq!(
        transform.selected_operation().id,
        Some(CoordinateOperationId(4833))
    );

    let (x, y) = transform.convert((5.3872036, 52.1551722)).unwrap();
    assert!((x - 155_000.0).abs() < 1.0, "easting: {x}");
    assert!((y - 463_000.0).abs() < 1.0, "northing: {y}");
}

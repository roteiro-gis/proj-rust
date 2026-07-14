use proj_core::{Coord3D, Transform};

#[test]
fn tuple3d_wgs84_to_web_mercator() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let (x, y, z) = t.convert_3d((-74.0445, 40.6892, 15.5)).unwrap();

    assert!((x - (-8242596.0)).abs() < 1000.0);
    assert!((y - 4966606.0).abs() < 1000.0);
    assert!((z - 15.5).abs() < 1e-12);
}

#[test]
fn coord3d_roundtrip() {
    let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let inv = Transform::new("EPSG:3857", "EPSG:4326").unwrap();

    let original = Coord3D::new(-74.0445, 40.6892, 25.0);
    let projected = fwd.convert_3d(original).unwrap();
    let roundtripped = inv.convert_3d(projected).unwrap();

    assert!((roundtripped.x - original.x).abs() < 1e-6);
    assert!((roundtripped.y - original.y).abs() < 1e-6);
    assert!((roundtripped.z - original.z).abs() < 1e-6);
}

#[test]
fn cross_datum_height_roundtrip() {
    let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
    let inv = Transform::new("EPSG:4326", "EPSG:4267").unwrap();

    let original = (-90.0, 45.0, 250.0);
    let shifted = fwd.convert_3d(original).unwrap();
    let roundtripped = inv.convert_3d(shifted).unwrap();

    assert!((roundtripped.0 - original.0).abs() < 1e-6);
    assert!((roundtripped.1 - original.1).abs() < 1e-6);
    assert!((roundtripped.2 - original.2).abs() < 1e-12);
}

#[test]
fn helmert_backed_projected_transform_uses_source_height_for_xy() {
    let t = Transform::new("EPSG:4326", "EPSG:27700").unwrap();

    let ground = t.convert_3d((-0.1278, 51.5074, 0.0)).unwrap();
    let high = t.convert_3d((-0.1278, 51.5074, 10_000.0)).unwrap();
    let de = high.0 - ground.0;
    let dn = high.1 - ground.1;

    assert!(de.abs() > 0.1, "easting delta = {de}");
    assert!(dn.abs() > 0.05, "northing delta = {dn}");

    // Ellipsoidal height is rebased onto the target datum's ellipsoid: the
    // WGS84→OSGB36(Airy) separation near London is about -46 m, and height
    // differences are preserved to within the datum shift's height gradient.
    assert!(
        (-60.0..=-30.0).contains(&ground.2),
        "ground height = {}",
        ground.2
    );
    assert!(
        (high.2 - ground.2 - 10_000.0).abs() < 20.0,
        "height delta = {}",
        high.2 - ground.2
    );
}

#[test]
fn batch_transform_3d_preserves_heights() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64, f64)> = (0..50)
        .map(|i| {
            (
                -74.0 + i as f64 * 0.01,
                40.0 + i as f64 * 0.01,
                i as f64 * 2.0,
            )
        })
        .collect();

    let results = t.convert_batch_3d(&coords).unwrap();
    assert_eq!(results.len(), 50);

    for (index, result) in results.iter().enumerate() {
        assert!(result.0 < 0.0);
        assert!((result.2 - index as f64 * 2.0).abs() < 1e-12);
    }
}

#[cfg(feature = "rayon")]
#[test]
fn parallel_batch_transform_3d_preserves_heights() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64, f64)> = (0..200)
        .map(|i| (-74.0 + i as f64 * 0.001, 40.0 + i as f64 * 0.001, i as f64))
        .collect();

    let results = t.convert_batch_parallel_3d(&coords).unwrap();
    assert_eq!(results.len(), 200);

    for (index, result) in results.iter().enumerate() {
        assert!((result.2 - index as f64).abs() < 1e-12);
    }
}

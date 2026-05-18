//! API compatibility tests exercising common downstream usage patterns.
//!
//! These tests exercise the `Transform` API with `geo_types` geometry values and
//! `(f64, f64)` inputs, batch transforms, error handling, and common CRS pairs.
//! True integration testing happens in downstream repos by swapping the C PROJ
//! dependency for proj-core and running their own test suites.
//!
//! Requires the `geo-types` feature (enabled by default).
#![cfg(feature = "geo-types")]

use proj_core::{Error, Transform};

const LIBERTY_LON: f64 = -74.0445;
const LIBERTY_LAT: f64 = 40.6892;

// ---------------------------------------------------------------------------
// geo_types::Coord<f64> usage patterns
// ---------------------------------------------------------------------------

#[test]
fn geo_coord_wgs84_to_web_mercator() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coord = geo_types::Coord {
        x: LIBERTY_LON,
        y: LIBERTY_LAT,
    };
    let result: geo_types::Coord<f64> = t.convert(coord).unwrap();

    assert!(
        (result.x - (-8_243_000.0)).abs() < 1000.0,
        "expected x ~ -8243000, got {}",
        result.x
    );
    assert!(
        (result.y - 4_966_000.0).abs() < 1000.0,
        "expected y ~ 4966000, got {}",
        result.y
    );
}

#[test]
fn geo_coord_roundtrip() {
    let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let inv = Transform::new("EPSG:3857", "EPSG:4326").unwrap();

    let original = geo_types::Coord {
        x: LIBERTY_LON,
        y: LIBERTY_LAT,
    };
    let projected: geo_types::Coord<f64> = fwd.convert(original).unwrap();
    let roundtripped: geo_types::Coord<f64> = inv.convert(projected).unwrap();

    assert!(
        (roundtripped.x - LIBERTY_LON).abs() < 1e-6,
        "lon: {} vs {}",
        roundtripped.x,
        LIBERTY_LON
    );
    assert!(
        (roundtripped.y - LIBERTY_LAT).abs() < 1e-6,
        "lat: {} vs {}",
        roundtripped.y,
        LIBERTY_LAT
    );
}

#[test]
fn same_crs_is_noop() {
    let t = Transform::new("EPSG:4326", "EPSG:4326").unwrap();
    let coord = geo_types::Coord { x: 1.0, y: 2.0 };
    let result: geo_types::Coord<f64> = t.convert(coord).unwrap();
    assert!((result.x - 1.0).abs() < 1e-12);
    assert!((result.y - 2.0).abs() < 1e-12);
}

#[test]
fn geo_coord_batch() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<geo_types::Coord<f64>> = (0..50)
        .map(|i| geo_types::Coord {
            x: -74.0 + (i as f64) * 0.01,
            y: 40.0 + (i as f64) * 0.01,
        })
        .collect();

    let results: Vec<geo_types::Coord<f64>> = t.convert_batch(&coords).unwrap();
    assert_eq!(results.len(), 50);
    for r in &results {
        assert!(r.x < 0.0, "all points are west of prime meridian");
    }
}

#[test]
fn geo_point_geometry_transform() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let point = geo_types::Point::new(LIBERTY_LON, LIBERTY_LAT);
    let result: geo_types::Point<f64> = t.convert_geometry(point).unwrap();

    assert!(
        (result.x() - (-8_243_000.0)).abs() < 1000.0,
        "expected x ~ -8243000, got {}",
        result.x()
    );
    assert!(
        (result.y() - 4_966_000.0).abs() < 1000.0,
        "expected y ~ 4966000, got {}",
        result.y()
    );
}

#[test]
fn geo_linestring_geometry_transform() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let line = geo_types::LineString::from(vec![
        (LIBERTY_LON, LIBERTY_LAT),
        (-74.0, 40.7),
        (-73.95, 40.72),
    ]);
    let result: geo_types::LineString<f64> = t.convert_geometry(line).unwrap();

    assert_eq!(result.0.len(), 3);
    assert!(result.0.iter().all(|coord| coord.x < 0.0));
    assert!(result.0.iter().all(|coord| coord.y > 4_900_000.0));
}

#[test]
fn geo_polygon_geometry_transform_preserves_holes() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let polygon = geo_types::Polygon::new(
        geo_types::LineString::from(vec![
            (-74.10, 40.60),
            (-73.90, 40.60),
            (-73.90, 40.80),
            (-74.10, 40.80),
            (-74.10, 40.60),
        ]),
        vec![geo_types::LineString::from(vec![
            (-74.05, 40.65),
            (-73.95, 40.65),
            (-73.95, 40.75),
            (-74.05, 40.75),
            (-74.05, 40.65),
        ])],
    );
    let result: geo_types::Polygon<f64> = t.convert_geometry(polygon).unwrap();

    assert_eq!(result.exterior().0.len(), 5);
    assert_eq!(result.interiors().len(), 1);
    assert_eq!(result.interiors()[0].0.len(), 5);
    assert!(result.exterior().0[0].x < -8_200_000.0);
    assert!(result.interiors()[0].0[0].y > 4_900_000.0);
}

#[test]
fn geo_multi_geometry_transforms() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let points = geo_types::MultiPoint(vec![
        geo_types::Point::new(-74.0, 40.0),
        geo_types::Point::new(-73.9, 40.1),
    ]);
    let points: geo_types::MultiPoint<f64> = t.convert_geometry(points).unwrap();
    assert_eq!(points.0.len(), 2);
    assert!(points.0.iter().all(|point| point.x() < 0.0));

    let lines = geo_types::MultiLineString(vec![
        geo_types::LineString::from(vec![(-74.0, 40.0), (-73.9, 40.1)]),
        geo_types::LineString::from(vec![(-73.8, 40.2), (-73.7, 40.3)]),
    ]);
    let lines: geo_types::MultiLineString<f64> = t.convert_geometry(lines).unwrap();
    assert_eq!(lines.0.len(), 2);
    assert!(lines.0.iter().all(|line| line.0.len() == 2));

    let polygons = geo_types::MultiPolygon(vec![geo_types::Polygon::new(
        geo_types::LineString::from(vec![
            (-74.0, 40.0),
            (-73.9, 40.0),
            (-73.9, 40.1),
            (-74.0, 40.1),
            (-74.0, 40.0),
        ]),
        vec![],
    )]);
    let polygons: geo_types::MultiPolygon<f64> = t.convert_geometry(polygons).unwrap();
    assert_eq!(polygons.0.len(), 1);
    assert!(polygons.0[0].exterior().0[0].x < 0.0);
}

#[test]
fn geo_rect_geometry_transform_returns_axis_aligned_bounds() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let rect = geo_types::Rect::new(
        geo_types::Coord { x: -74.1, y: 40.6 },
        geo_types::Coord { x: -73.9, y: 40.8 },
    );
    let result: geo_types::Rect<f64> = t.convert_geometry(rect).unwrap();

    assert!(result.min().x < result.max().x);
    assert!(result.min().y < result.max().y);
    assert!(result.min().x < -8_200_000.0);
    assert!(result.max().y > 4_900_000.0);
}

#[test]
fn geo_geometry_enum_transform() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let geometry = geo_types::Geometry::LineString(geo_types::LineString::from(vec![
        (LIBERTY_LON, LIBERTY_LAT),
        (-74.0, 40.7),
    ]));
    let result: geo_types::Geometry<f64> = t.convert_geometry(geometry).unwrap();

    let geo_types::Geometry::LineString(line) = result else {
        panic!("expected transformed linestring geometry");
    };
    assert_eq!(line.0.len(), 2);
    assert!(line.0[0].x < 0.0);
}

#[test]
fn geo_geometry_transform_fails_on_first_invalid_coordinate() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();

    let line = geo_types::LineString::from(vec![(-74.0, 40.0), (-73.0, 91.0), (-72.0, 41.0)]);
    let err = t.convert_geometry(line).unwrap_err();

    assert!(matches!(err, Error::OutOfRange(_)));
}

#[test]
fn invalid_crs_returns_error() {
    let result = Transform::new("NONSENSE:99999", "EPSG:4326");
    assert!(result.is_err(), "should fail with invalid CRS");
}

// ---------------------------------------------------------------------------
// (f64, f64) tuple usage patterns
// ---------------------------------------------------------------------------

#[test]
fn tuple_transform() {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let (x, y) = t.convert((LIBERTY_LON, LIBERTY_LAT)).unwrap();
    assert!((x - (-8_243_000.0)).abs() < 1000.0);
    assert!((y - 4_966_000.0).abs() < 1000.0);
}

#[test]
fn densified_bounds_transform() {
    // Densify bbox boundary before reprojecting (common pattern for non-linear CRS).
    let t = Transform::new("EPSG:3857", "EPSG:4326").unwrap();

    let corners: Vec<(f64, f64)> = (0..=64)
        .map(|i| {
            let frac = i as f64 / 64.0;
            let x = -8_238_310.0 + frac * 100_000.0;
            let y = 4_970_072.0;
            (x, y)
        })
        .collect();

    let results = t.convert_batch(&corners).unwrap();
    assert_eq!(results.len(), 65);
    for (lon, lat) in &results {
        assert!((-180.0..=180.0).contains(lon), "lon out of range: {lon}");
        assert!((-90.0..=90.0).contains(lat), "lat out of range: {lat}");
    }
}

// ---------------------------------------------------------------------------
// Cross-datum transforms
// ---------------------------------------------------------------------------

#[test]
fn cross_datum_transform() {
    let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
    let (lon, lat) = t.convert((-90.0, 45.0)).unwrap();
    assert!((lon - (-90.0)).abs() < 0.01);
    assert!((lat - 45.0).abs() < 0.01);
}

// ---------------------------------------------------------------------------
// Polar stereographic (EPSG:3413)
// ---------------------------------------------------------------------------

#[test]
fn polar_stereo_3413_roundtrip() {
    let fwd = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
    let inv = Transform::new("EPSG:3413", "EPSG:4326").unwrap();

    let original = (0.0, 75.0);
    let projected = fwd.convert(original).unwrap();
    let back = inv.convert(projected).unwrap();

    assert!((back.0 - original.0).abs() < 1e-6);
    assert!((back.1 - original.1).abs() < 1e-6);
}

//! API compatibility tests exercising common downstream usage patterns.
//!
//! These tests exercise the `Transform` API with `geo_types::Coord<f64>` and `(f64, f64)`
//! inputs, batch transforms, error handling, and common CRS pairs. True integration
//! testing happens in downstream repos by swapping the C PROJ dependency for proj-core
//! and running their own test suites.
//!
//! Requires the `geo-types` feature (enabled by default).
#![cfg(feature = "geo-types")]

use proj_core::Transform;

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

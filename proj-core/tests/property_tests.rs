//! Property-based tests using proptest.

use proj_core::Transform;
use proptest::prelude::*;

// Web Mercator latitude limit (±85.06°)
const WM_LAT_LIMIT: f64 = 85.0;

proptest! {
    /// Roundtrip: WGS84 → Web Mercator → WGS84 should return the original point.
    #[test]
    fn roundtrip_4326_3857(
        lon in -180.0..180.0f64,
        lat in (-WM_LAT_LIMIT)..WM_LAT_LIMIT,
    ) {
        let fwd = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
        let inv = Transform::new("EPSG:3857", "EPSG:4326").unwrap();

        let projected = fwd.convert((lon, lat)).unwrap();
        let (lon2, lat2) = inv.convert(projected).unwrap();

        prop_assert!((lon2 - lon).abs() < 1e-6, "lon: {lon2} vs {lon}");
        prop_assert!((lat2 - lat).abs() < 1e-6, "lat: {lat2} vs {lat}");
    }

    /// Roundtrip: WGS84 → UTM → WGS84 for zone 18N.
    #[test]
    fn roundtrip_4326_utm18n(
        lon in -78.0..-72.0f64,  // UTM zone 18 bounds
        lat in 0.0..84.0f64,
    ) {
        let fwd = Transform::new("EPSG:4326", "EPSG:32618").unwrap();
        let inv = Transform::new("EPSG:32618", "EPSG:4326").unwrap();

        let projected = fwd.convert((lon, lat)).unwrap();
        let (lon2, lat2) = inv.convert(projected).unwrap();

        prop_assert!((lon2 - lon).abs() < 1e-6, "lon: {lon2} vs {lon}");
        prop_assert!((lat2 - lat).abs() < 1e-6, "lat: {lat2} vs {lat}");
    }

    /// Roundtrip: WGS84 → Polar Stereographic North → WGS84.
    #[test]
    fn roundtrip_4326_3413(
        lon in -180.0..180.0f64,
        lat in 60.0..90.0f64,
    ) {
        let fwd = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
        let inv = Transform::new("EPSG:3413", "EPSG:4326").unwrap();

        let projected = fwd.convert((lon, lat)).unwrap();
        let (lon2, lat2) = inv.convert(projected).unwrap();

        // Near the pole, longitude precision degrades
        if lat < 89.9 {
            prop_assert!((lon2 - lon).abs() < 1e-4, "lon: {lon2} vs {lon}");
        }
        prop_assert!((lat2 - lat).abs() < 1e-6, "lat: {lat2} vs {lat}");
    }

    /// Identity: same-CRS transform returns input unchanged.
    #[test]
    fn identity_same_crs(
        lon in -180.0..180.0f64,
        lat in -90.0..90.0f64,
    ) {
        let t = Transform::new("EPSG:4326", "EPSG:4326").unwrap();
        let (lon2, lat2) = t.convert((lon, lat)).unwrap();
        prop_assert_eq!(lon2, lon);
        prop_assert_eq!(lat2, lat);
    }

    /// Cross-datum roundtrip: NAD27 → WGS84 → NAD27.
    #[test]
    fn roundtrip_nad27_wgs84(
        lon in -130.0..-60.0f64,  // continental US
        lat in 25.0..50.0f64,
    ) {
        let fwd = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
        let inv = Transform::new("EPSG:4326", "EPSG:4267").unwrap();

        let shifted = fwd.convert((lon, lat)).unwrap();
        let (lon2, lat2) = inv.convert(shifted).unwrap();

        prop_assert!((lon2 - lon).abs() < 1e-5, "lon: {lon2} vs {lon}");
        prop_assert!((lat2 - lat).abs() < 1e-5, "lat: {lat2} vs {lat}");
    }
}

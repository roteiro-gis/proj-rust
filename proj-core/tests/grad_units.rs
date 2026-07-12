//! Registry regression tests for CRSs whose EPSG conversion parameters are
//! stated in grads (the generator once applied the arc-second factor to
//! grads, leaving these CRSs unusable).
//!
//! The corpus cannot carry NTF (Paris) points because C PROJ keeps that
//! geographic CRS's native grad axis units while we normalize to degrees,
//! so the Lambert zone II pin lives here with the physically identical
//! degree input (43.965° = 48.85 gr) and C PROJ's reference output.

use proj_core::Transform;

#[test]
fn ntf_paris_lambert_zone_ii_matches_c_proj() {
    let t = Transform::new("EPSG:4807", "EPSG:27572").unwrap();
    // On the Paris meridian at 48.85 gr latitude; C PROJ (fed grads) maps
    // this point to exactly the false easting and N 1884835.155.
    let result: geo_types::Coord<f64> = t.convert(geo_types::Coord { x: 0.0, y: 43.965 }).unwrap();
    assert!((result.x - 600_000.0).abs() < 1e-3, "x = {}", result.x);
    assert!((result.y - 1_884_835.155).abs() < 1e-3, "y = {}", result.y);

    let inv = Transform::new("EPSG:27572", "EPSG:4807").unwrap();
    let back: geo_types::Coord<f64> = inv.convert(result).unwrap();
    assert!(back.x.abs() < 1e-9, "lon = {}", back.x);
    assert!((back.y - 43.965).abs() < 1e-9, "lat = {}", back.y);
}

use crate::ellipsoid::Ellipsoid;

/// Convert geodetic coordinates to geocentric (ECEF) coordinates.
///
/// Input: longitude and latitude in **radians**, ellipsoidal height in **meters**.
/// Output: (X, Y, Z) in meters.
pub(crate) fn geodetic_to_geocentric(
    ellipsoid: &Ellipsoid,
    lon: f64,
    lat: f64,
    h: f64,
) -> (f64, f64, f64) {
    let sin_lat = lat.sin();
    let cos_lat = lat.cos();
    let e2 = ellipsoid.e2();

    // Radius of curvature in the prime vertical
    let n = ellipsoid.semi_major_axis() / (1.0 - e2 * sin_lat * sin_lat).sqrt();

    let x = (n + h) * cos_lat * lon.cos();
    let y = (n + h) * cos_lat * lon.sin();
    let z = (n * (1.0 - e2) + h) * sin_lat;

    (x, y, z)
}

/// Height above which Bowring's solution is refined: its latitude error passes
/// 0.1 mm here and reaches about 5 cm at GNSS altitudes.
const BOWRING_HEIGHT_LIMIT: f64 = 100_000.0;

/// Fixed-point steps that bring the refined latitude under a micrometre.
const BOWRING_REFINEMENT_STEPS: usize = 2;

/// Convert geocentric (ECEF) coordinates to geodetic coordinates.
///
/// Input: (X, Y, Z) in meters.
/// Output: longitude and latitude in **radians**, ellipsoidal height in **meters**.
///
/// Uses Bowring's closed-form solution, like C PROJ's `cart`, which needs no
/// iteration near the Earth's surface. Points far above the ellipsoid take a
/// few fixed-point steps to keep sub-millimetre accuracy at orbital heights.
pub(crate) fn geocentric_to_geodetic(
    ellipsoid: &Ellipsoid,
    x: f64,
    y: f64,
    z: f64,
) -> (f64, f64, f64) {
    let a = ellipsoid.semi_major_axis();
    let b_over_a = 1.0 - ellipsoid.flattening();
    let e2 = ellipsoid.e2();
    let ep2 = ellipsoid.ep2();

    // Work on the unit ellipsoid to keep the squares well conditioned.
    let (x_over_a, y_over_a, z_over_a) = (x / a, y / a, z / a);
    let p = (x_over_a * x_over_a + y_over_a * y_over_a).sqrt();

    let p_scaled = p * b_over_a;
    let norm = (z_over_a * z_over_a + p_scaled * p_scaled).sqrt();
    let (cos_theta, sin_theta) = if norm != 0.0 {
        (p_scaled / norm, z_over_a / norm)
    } else {
        (1.0, 0.0)
    };

    let y_phi = z_over_a + ep2 * b_over_a * sin_theta * sin_theta * sin_theta;
    let x_phi = p - e2 * cos_theta * cos_theta * cos_theta;
    let norm_phi = (y_phi * y_phi + x_phi * x_phi).sqrt();
    let (mut cos_lat, mut sin_lat) = if norm_phi != 0.0 {
        (x_phi / norm_phi, y_phi / norm_phi)
    } else {
        (1.0, 0.0)
    };

    // Very close to the centre or the axis there is no single solution; clamp
    // to the pole so the result stays continuous with the near-polar branch.
    let mut lat = if x_phi <= 0.0 {
        cos_lat = 0.0;
        sin_lat = if z >= 0.0 { 1.0 } else { -1.0 };
        std::f64::consts::FRAC_PI_2.copysign(sin_lat)
    } else {
        (y_phi / x_phi).atan()
    };
    let lon = y_over_a.atan2(x_over_a);

    let height = |cos_lat: f64, sin_lat: f64| {
        if cos_lat < 1e-6 {
            // Poleward of 89.99994 degrees, avoid dividing by cos(lat): use the
            // geocentric radius at this latitude instead.
            z.abs() - a * cos_lat.hypot(b_over_a * sin_lat)
        } else {
            a * p / cos_lat - a / (1.0 - e2 * sin_lat * sin_lat).sqrt()
        }
    };

    let mut h = height(cos_lat, sin_lat);
    if h > BOWRING_HEIGHT_LIMIT {
        for _ in 0..BOWRING_REFINEMENT_STEPS {
            let n_over_a = 1.0 / (1.0 - e2 * lat.sin() * lat.sin()).sqrt();
            lat = (z_over_a + e2 * n_over_a * lat.sin()).atan2(p);
        }
        (sin_lat, cos_lat) = lat.sin_cos();
        h = height(cos_lat, sin_lat);
    }

    (lon, lat, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;

    #[test]
    fn roundtrip_equator() {
        let e = &ellipsoid::WGS84;
        let lon = 0.0_f64.to_radians();
        let lat = 0.0_f64.to_radians();
        let h = 0.0;

        let (x, y, z) = geodetic_to_geocentric(e, lon, lat, h);
        assert!((x - 6378137.0).abs() < 0.001, "x = {x}");
        assert!(y.abs() < 0.001, "y = {y}");
        assert!(z.abs() < 0.001, "z = {z}");

        let (lon2, lat2, h2) = geocentric_to_geodetic(e, x, y, z);
        assert!((lon2 - lon).abs() < 1e-12, "lon: {lon2} vs {lon}");
        assert!((lat2 - lat).abs() < 1e-12, "lat: {lat2} vs {lat}");
        assert!((h2 - h).abs() < 0.001, "h: {h2} vs {h}");
    }

    #[test]
    fn roundtrip_nyc() {
        let e = &ellipsoid::WGS84;
        let lon = (-74.006_f64).to_radians();
        let lat = 40.7128_f64.to_radians();
        let h = 10.0;

        let (x, y, z) = geodetic_to_geocentric(e, lon, lat, h);
        let (lon2, lat2, h2) = geocentric_to_geodetic(e, x, y, z);

        assert!(
            (lon2 - lon).abs() < 1e-12,
            "lon: {} vs {}",
            lon2.to_degrees(),
            lon.to_degrees()
        );
        assert!(
            (lat2 - lat).abs() < 1e-12,
            "lat: {} vs {}",
            lat2.to_degrees(),
            lat.to_degrees()
        );
        assert!((h2 - h).abs() < 0.001, "h: {h2} vs {h}");
    }

    #[test]
    fn roundtrip_north_pole() {
        let e = &ellipsoid::WGS84;
        let lon = 0.0;
        let lat = std::f64::consts::FRAC_PI_2;
        let h = 0.0;

        let (x, y, z) = geodetic_to_geocentric(e, lon, lat, h);
        assert!(x.abs() < 0.001);
        assert!(y.abs() < 0.001);
        assert!((z - ellipsoid::WGS84.b()).abs() < 0.001, "z = {z}");

        let (_lon2, lat2, h2) = geocentric_to_geodetic(e, x, y, z);
        assert!((lat2 - lat).abs() < 1e-10, "lat: {lat2}");
        assert!(h2.abs() < 0.001, "h: {h2}");
    }

    #[test]
    fn roundtrip_holds_across_latitudes_and_heights() {
        let e = &ellipsoid::WGS84;
        for lat_deg in [-89.9, -60.0, -1.0, 0.0, 1.0, 45.0, 89.0, 89.9] {
            for h in [
                -10_000.0,
                0.0,
                10_000.0,
                99_000.0,
                101_000.0,
                1_000_000.0,
                20_200_000.0,
                35_786_000.0,
            ] {
                let lat = f64::to_radians(lat_deg);
                let lon = 1.2;
                let (x, y, z) = geodetic_to_geocentric(e, lon, lat, h);
                let (lon2, lat2, h2) = geocentric_to_geodetic(e, x, y, z);
                assert!((lon2 - lon).abs() < 1e-12, "{lat_deg} {h}: {lon2}");
                // About 1 um when refined, and 0.1 mm just below the limit.
                let (lat_tolerance, h_tolerance) = if h > BOWRING_HEIGHT_LIMIT {
                    (2e-13, 1e-5)
                } else {
                    (2e-11, 2e-4)
                };
                assert!((lat2 - lat).abs() < lat_tolerance, "{lat_deg} {h}: {lat2}");
                assert!((h2 - h).abs() < h_tolerance, "{lat_deg} {h}: {h2}");
            }
        }
    }

    #[test]
    fn degenerate_points_clamp_to_the_pole() {
        let e = &ellipsoid::WGS84;
        let (_, lat, h) = geocentric_to_geodetic(e, 0.0, 0.0, 0.0);
        assert_eq!(lat, std::f64::consts::FRAC_PI_2);
        assert!((h + e.b()).abs() < 1e-6, "{h}");

        let (_, lat, _) = geocentric_to_geodetic(e, 0.0, 0.0, -1.0);
        assert_eq!(lat, -std::f64::consts::FRAC_PI_2);

        let (_, lat, h) = geocentric_to_geodetic(e, 1.0, 1.0, -e.b());
        assert!(lat < -1.57, "{lat}");
        assert!(h.abs() < 1e-3, "{h}");
    }

    #[test]
    fn spheres_convert_without_eccentricity() {
        let sphere = crate::Ellipsoid::sphere(6_371_000.0).unwrap();
        let (lon, lat, h) = geocentric_to_geodetic(&sphere, 0.0, 6_371_100.0, 0.0);
        assert!((lon - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(lat.abs() < 1e-12);
        assert!((h - 100.0).abs() < 1e-8);
    }

    #[test]
    fn roundtrip_clarke1866() {
        // Test with a different ellipsoid (NAD27)
        let e = &ellipsoid::CLARKE1866;
        let lon = (-90.0_f64).to_radians();
        let lat = 45.0_f64.to_radians();
        let h = 100.0;

        let (x, y, z) = geodetic_to_geocentric(e, lon, lat, h);
        let (lon2, lat2, h2) = geocentric_to_geodetic(e, x, y, z);

        assert!((lon2 - lon).abs() < 1e-12);
        assert!((lat2 - lat).abs() < 1e-12);
        assert!((h2 - h).abs() < 0.001);
    }
}

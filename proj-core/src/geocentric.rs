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
    let n = ellipsoid.a / (1.0 - e2 * sin_lat * sin_lat).sqrt();

    let x = (n + h) * cos_lat * lon.cos();
    let y = (n + h) * cos_lat * lon.sin();
    let z = (n * (1.0 - e2) + h) * sin_lat;

    (x, y, z)
}

/// Convert geocentric (ECEF) coordinates to geodetic coordinates.
///
/// Input: (X, Y, Z) in meters.
/// Output: longitude and latitude in **radians**, ellipsoidal height in **meters**.
///
/// Uses Bowring's iterative method for sub-millimeter accuracy (typically converges
/// in 2-3 iterations).
pub(crate) fn geocentric_to_geodetic(
    ellipsoid: &Ellipsoid,
    x: f64,
    y: f64,
    z: f64,
) -> (f64, f64, f64) {
    let a = ellipsoid.a;
    let b = ellipsoid.b();
    let e2 = ellipsoid.e2();
    let ep2 = ellipsoid.ep2();

    let lon = y.atan2(x);

    let p = (x * x + y * y).sqrt();

    // Handle pole case
    if p < 1e-10 {
        let lat = if z >= 0.0 {
            std::f64::consts::FRAC_PI_2
        } else {
            -std::f64::consts::FRAC_PI_2
        };
        let h = z.abs() - b;
        return (lon, lat, h);
    }

    // Initial estimate using Bowring's formula
    let theta = (z * a).atan2(p * b);
    let sin_theta = theta.sin();
    let cos_theta = theta.cos();

    let mut lat = (z + ep2 * b * sin_theta * sin_theta * sin_theta)
        .atan2(p - e2 * a * cos_theta * cos_theta * cos_theta);

    // Iterate for convergence
    for _ in 0..10 {
        let sin_lat = lat.sin();
        let n = a / (1.0 - e2 * sin_lat * sin_lat).sqrt();
        let new_lat = (z + e2 * n * sin_lat).atan2(p);

        if (new_lat - lat).abs() < 1e-14 {
            lat = new_lat;
            break;
        }
        lat = new_lat;
    }

    let sin_lat = lat.sin();
    let n = a / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    let h = if lat.cos().abs() > 1e-10 {
        p / lat.cos() - n
    } else {
        z / lat.sin() - n * (1.0 - e2)
    };

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

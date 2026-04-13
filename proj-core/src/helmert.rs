use crate::datum::HelmertParams;

const ARCSEC_TO_RAD: f64 = std::f64::consts::PI / (180.0 * 3600.0);

/// Apply 7-parameter Helmert (Bursa-Wolf) transformation.
///
/// Transforms geocentric coordinates from one datum to WGS84.
/// Input/output are (X, Y, Z) in meters.
///
/// The formula (Position Vector convention, EPSG method 1033):
/// ```text
/// [X']   [dx]              [  1  -rz   ry] [X]
/// [Y'] = [dy] + (1 + ds) * [  rz   1  -rx] [Y]
/// [Z']   [dz]              [ -ry  rx   1 ] [Z]
/// ```
pub(crate) fn helmert_forward(params: &HelmertParams, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let s = 1.0 + params.ds * 1e-6; // ppm to scale factor
    let rx = params.rx * ARCSEC_TO_RAD;
    let ry = params.ry * ARCSEC_TO_RAD;
    let rz = params.rz * ARCSEC_TO_RAD;

    let xo = params.dx + s * (x - rz * y + ry * z);
    let yo = params.dy + s * (rz * x + y - rx * z);
    let zo = params.dz + s * (-ry * x + rx * y + z);

    (xo, yo, zo)
}

/// Apply inverse Helmert transformation (WGS84 to source datum).
///
/// Negates all parameters to reverse the transformation direction.
pub(crate) fn helmert_inverse(params: &HelmertParams, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let inv = params.inverse();
    helmert_forward(&inv, x, y, z)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum;
    use crate::ellipsoid;
    use crate::geocentric::{geocentric_to_geodetic, geodetic_to_geocentric};

    #[test]
    fn identity_with_zero_params() {
        let params = HelmertParams::translation(0.0, 0.0, 0.0);
        let (xo, yo, zo) = helmert_forward(&params, 1000.0, 2000.0, 3000.0);
        assert_eq!(xo, 1000.0);
        assert_eq!(yo, 2000.0);
        assert_eq!(zo, 3000.0);
    }

    #[test]
    fn translation_only() {
        let params = HelmertParams::translation(10.0, 20.0, 30.0);
        let (xo, yo, zo) = helmert_forward(&params, 1000.0, 2000.0, 3000.0);
        assert!((xo - 1010.0).abs() < 1e-6);
        assert!((yo - 2020.0).abs() < 1e-6);
        assert!((zo - 3030.0).abs() < 1e-6);
    }

    #[test]
    fn roundtrip_forward_inverse() {
        let params = datum::OSGB36.helmert_to_wgs84().unwrap();
        let x = 3_980_000.0;
        let y = -100_000.0;
        let z = 4_960_000.0;

        let (x2, y2, z2) = helmert_forward(params, x, y, z);
        let (x3, y3, z3) = helmert_inverse(params, x2, y2, z2);

        assert!((x3 - x).abs() < 0.01, "x: {x3} vs {x}");
        assert!((y3 - y).abs() < 0.01, "y: {y3} vs {y}");
        assert!((z3 - z).abs() < 0.01, "z: {z3} vs {z}");
    }

    #[test]
    fn nad27_to_wgs84_known_point() {
        // Test the full pipeline: geodetic on NAD27 -> geocentric -> Helmert -> geodetic on WGS84
        let nad27_params = datum::NAD27.helmert_to_wgs84().unwrap();

        // A point roughly at 45°N, 90°W on NAD27
        let lon = (-90.0_f64).to_radians();
        let lat = 45.0_f64.to_radians();
        let h = 0.0;

        // Convert to geocentric on Clarke 1866
        let (x, y, z) = geodetic_to_geocentric(&ellipsoid::CLARKE1866, lon, lat, h);

        // Apply Helmert to get WGS84 geocentric
        let (x2, y2, z2) = helmert_forward(nad27_params, x, y, z);

        // Convert back to geodetic on WGS84
        let (lon2, lat2, _h2) = geocentric_to_geodetic(&ellipsoid::WGS84, x2, y2, z2);

        // The shift should be small (tens of meters -> fraction of arcseconds)
        let d_lon = (lon2 - lon).to_degrees() * 3600.0; // arcseconds
        let d_lat = (lat2 - lat).to_degrees() * 3600.0;

        // NAD27→WGS84 shift should be on the order of a few arcseconds
        assert!(d_lon.abs() < 10.0, "lon shift: {d_lon} arcseconds");
        assert!(d_lat.abs() < 10.0, "lat shift: {d_lat} arcseconds");
    }
}

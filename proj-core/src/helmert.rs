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
    let s = 1.0 + params.ds() * 1e-6; // ppm to scale factor
    let rx = params.rx() * ARCSEC_TO_RAD;
    let ry = params.ry() * ARCSEC_TO_RAD;
    let rz = params.rz() * ARCSEC_TO_RAD;

    let xo = params.dx() + s * (x - rz * y + ry * z);
    let yo = params.dy() + s * (rz * x + y - rx * z);
    let zo = params.dz() + s * (-ry * x + rx * y + z);

    (xo, yo, zo)
}

/// Apply inverse Helmert transformation (WGS84 to source datum).
///
/// Exact inverse of `helmert_forward`'s affine map `p' = T + s·M·p` with
/// `M = I + W`: solves `p = M⁻¹·(p' − T)/s` using the closed-form adjugate,
/// `det(M) = 1 + rx² + ry² + rz²`. Parameter negation (the EPSG reversal
/// convention, `HelmertParams::inverse`) is only a first-order approximation
/// and does not roundtrip exactly for large rotations.
pub(crate) fn helmert_inverse(params: &HelmertParams, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let s = 1.0 + params.ds() * 1e-6;
    let rx = params.rx() * ARCSEC_TO_RAD;
    let ry = params.ry() * ARCSEC_TO_RAD;
    let rz = params.rz() * ARCSEC_TO_RAD;

    let ux = (x - params.dx()) / s;
    let uy = (y - params.dy()) / s;
    let uz = (z - params.dz()) / s;

    let det = 1.0 + rx * rx + ry * ry + rz * rz;
    let xo = ((1.0 + rx * rx) * ux + (rz + rx * ry) * uy + (rx * rz - ry) * uz) / det;
    let yo = ((rx * ry - rz) * ux + (1.0 + ry * ry) * uy + (rx + ry * rz) * uz) / det;
    let zo = ((ry + rx * rz) * ux + (ry * rz - rx) * uy + (1.0 + rz * rz) * uz) / det;

    (xo, yo, zo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum;
    use crate::ellipsoid;
    use crate::geocentric::{geocentric_to_geodetic, geodetic_to_geocentric};

    #[test]
    fn identity_with_zero_params() {
        let params = HelmertParams::translation(0.0, 0.0, 0.0).unwrap();
        let (xo, yo, zo) = helmert_forward(&params, 1000.0, 2000.0, 3000.0);
        assert_eq!(xo, 1000.0);
        assert_eq!(yo, 2000.0);
        assert_eq!(zo, 3000.0);
    }

    #[test]
    fn translation_only() {
        let params = HelmertParams::translation(10.0, 20.0, 30.0).unwrap();
        let (xo, yo, zo) = helmert_forward(&params, 1000.0, 2000.0, 3000.0);
        assert!((xo - 1010.0).abs() < 1e-6);
        assert!((yo - 2020.0).abs() < 1e-6);
        assert!((zo - 3030.0).abs() < 1e-6);
    }

    #[test]
    fn roundtrip_forward_inverse() {
        let datum = datum::OSGB36;
        let params = datum.helmert_to_wgs84().unwrap();
        let x = 3_980_000.0;
        let y = -100_000.0;
        let z = 4_960_000.0;

        let (x2, y2, z2) = helmert_forward(params, x, y, z);
        let (x3, y3, z3) = helmert_inverse(params, x2, y2, z2);

        // The inverse is exact, so the roundtrip holds to machine precision.
        assert!((x3 - x).abs() < 1e-8, "x: {x3} vs {x}");
        assert!((y3 - y).abs() < 1e-8, "y: {y3} vs {y}");
        assert!((z3 - z).abs() < 1e-8, "z: {z3} vs {z}");
    }

    #[test]
    fn roundtrip_is_exact_for_large_rotations() {
        // Several-arcminute rotations and a large scale offset: parameter
        // negation would leave meter-scale roundtrip error here, while the
        // exact adjugate inverse stays at machine precision.
        let params = HelmertParams::new(120.0, -75.0, 310.0, 300.0, -450.0, 600.0, 25.0).unwrap();
        let x = 3_980_000.0;
        let y = -100_000.0;
        let z = 4_960_000.0;

        let (x2, y2, z2) = helmert_forward(&params, x, y, z);
        let (x3, y3, z3) = helmert_inverse(&params, x2, y2, z2);

        assert!((x3 - x).abs() < 1e-8, "x: {x3} vs {x}");
        assert!((y3 - y).abs() < 1e-8, "y: {y3} vs {y}");
        assert!((z3 - z).abs() < 1e-8, "z: {z3} vs {z}");
    }

    #[test]
    fn inverse_matches_negation_to_first_order() {
        // The EPSG negation convention is a first-order approximation of the
        // exact inverse. For OSGB36 (sub-arcsecond rotations, ~545 m
        // translation, 20.5 ppm scale) the rotation×translation and
        // rotation×scale cross terms leave ~6 mm of divergence — which is why
        // the old negation-based roundtrip only held to 1 cm.
        let datum = datum::OSGB36;
        let params = datum.helmert_to_wgs84().unwrap();
        let x = 3_980_000.0;
        let y = -100_000.0;
        let z = 4_960_000.0;

        let (ex, ey, ez) = helmert_inverse(params, x, y, z);
        let (nx, ny, nz) = helmert_forward(&params.inverse(), x, y, z);

        assert!((ex - nx).abs() < 0.01, "x: {ex} vs {nx}");
        assert!((ey - ny).abs() < 0.01, "y: {ey} vs {ny}");
        assert!((ez - nz).abs() < 0.01, "z: {ez} vs {nz}");
    }

    #[test]
    fn nad27_to_wgs84_known_point() {
        // Test the full pipeline: geodetic on NAD27 -> geocentric -> Helmert -> geodetic on WGS84
        let nad27 = datum::NAD27;
        let nad27_params = nad27.helmert_to_wgs84().unwrap();

        // A point roughly at 45°N, 90°W on NAD27
        let lon = (-90.0_f64).to_radians();
        let lat = 45.0_f64.to_radians();
        let h = 0.0;

        // Convert to geocentric on Clarke 1866
        let (x, y, z) = geodetic_to_geocentric(&ellipsoid::CLARKE1866, lon, lat, h);

        // Apply Helmert to get WGS84 geocentric
        let (x2, y2, z2) = helmert_forward(nad27_params, x, y, z);

        // Convert back to geodetic on WGS84
        let (lon2, lat2, _h2) = geocentric_to_geodetic(&ellipsoid::WGS84, x2, y2, z2).unwrap();

        // The shift should be small (tens of meters -> fraction of arcseconds)
        let d_lon = (lon2 - lon).to_degrees() * 3600.0; // arcseconds
        let d_lat = (lat2 - lat).to_degrees() * 3600.0;

        // NAD27→WGS84 shift should be on the order of a few arcseconds
        assert!(d_lon.abs() < 10.0, "lon shift: {d_lon} arcseconds");
        assert!(d_lat.abs() < 10.0, "lat shift: {d_lat} arcseconds");
    }
}

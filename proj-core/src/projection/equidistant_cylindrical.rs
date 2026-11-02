use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, inverse_meridian_arc, meridian_arc,
    meridian_arc_coefficients, normalize_longitude, validate_angle, validate_latitude_param,
    validate_lon_lat, validate_offset, validate_projected,
};

/// Equidistant Cylindrical (Plate Carree) projection.
///
/// EPSG method 1028 on an ellipsoid: x uses the prime-vertical radius at
/// lat_ts, and y is the meridional arc from the equator. On a sphere these
/// reduce to x = a * cos(lat_ts) * normalized(lon - lon0), y = a * lat.
#[derive(Clone)]
pub(crate) struct EquidistantCylindrical {
    nu_cos_lat_ts: f64,
    a: f64,
    en: [f64; 13],
    lon0: f64,
    false_easting: f64,
    false_northing: f64,
}

impl EquidistantCylindrical {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat_ts: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("central meridian", lon0)?;
        validate_latitude_param("latitude of true scale", lat_ts)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let cos_lat_ts = lat_ts.cos();
        if cos_lat_ts.abs() < 1e-12 {
            return Err(Error::InvalidDefinition(
                "Equidistant Cylindrical latitude of true scale cannot be at the poles".into(),
            ));
        }
        let e2 = ellipsoid.e2();
        let sin_lat_ts = lat_ts.sin();
        let nu_cos_lat_ts =
            ellipsoid.semi_major_axis() * cos_lat_ts / (1.0 - e2 * sin_lat_ts * sin_lat_ts).sqrt();

        Ok(Self {
            nu_cos_lat_ts,
            a: ellipsoid.semi_major_axis(),
            en: meridian_arc_coefficients(e2),
            lon0,
            false_easting,
            false_northing,
        })
    }
}

impl super::ProjectionImpl for EquidistantCylindrical {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let x = self.false_easting + self.nu_cos_lat_ts * normalize_longitude(lon - self.lon0);
        let y = self.false_northing + self.a * meridian_arc(lat, lat.sin(), lat.cos(), &self.en);
        ensure_finite_xy("Equidistant Cylindrical", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let lon = self.lon0 + (x - self.false_easting) / self.nu_cos_lat_ts;
        let lat = inverse_meridian_arc((y - self.false_northing) / self.a, &self.en);
        ensure_finite_lon_lat("Equidistant Cylindrical", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn plate_carree_roundtrip() {
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0).unwrap();

        let lon = (-74.006_f64).to_radians();
        let lat = 40.7128_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-10);
        assert!((lat2 - lat).abs() < 1e-10);
    }

    #[test]
    fn ellipsoidal_matches_c_proj() {
        // PROJ 9.8.1, EPSG:4326 -> EPSG:4087, rather than the spherical
        // northing of 5_565_974.539_664 returned by older PROJ releases.
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0).unwrap();
        let expected = (1_113_194.907_932_736, 5_540_847.041_684_15);
        let (x, y) = proj
            .forward(10.0_f64.to_radians(), 50.0_f64.to_radians())
            .unwrap();
        assert!(
            (x - expected.0).abs() < 1e-6,
            "ellipsoidal easting differs from the PROJ reference"
        );
        assert!(
            (y - expected.1).abs() < 1e-6,
            "ellipsoidal northing differs from the PROJ reference"
        );

        let (lon, lat) = proj.inverse(expected.0, expected.1).unwrap();
        assert!((lon.to_degrees() - 10.0).abs() < 1e-10);
        assert!((lat.to_degrees() - 50.0).abs() < 1e-10);
    }

    #[test]
    fn nonzero_standard_parallel_and_offsets_match_c_proj() {
        // PROJ 9.8.1: +proj=eqc +ellps=WGS84 +lat_ts=30 +lon_0=-90
        // +x_0=500000 +y_0=-200000. The easting also needs the ellipsoid.
        let proj = EquidistantCylindrical::new(
            ellipsoid::WGS84,
            (-90.0_f64).to_radians(),
            30.0_f64.to_radians(),
            500_000.0,
            -200_000.0,
        )
        .unwrap();
        for (lon, lat, expected_x, expected_y) in [
            (
                -88.0_f64,
                30.0_f64,
                692_972.560_501_793,
                3_120_113.397_940_382,
            ),
            (-92.0, -50.0, 307_027.439_498_207, -5_740_847.041_684_148),
        ] {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            assert!(
                (x - expected_x).abs() < 1e-6,
                "offset easting differs from the PROJ reference"
            );
            assert!(
                (y - expected_y).abs() < 1e-6,
                "offset northing differs from the PROJ reference"
            );
            let (lon2, lat2) = proj.inverse(expected_x, expected_y).unwrap();
            assert!((lon2.to_degrees() - lon).abs() < 1e-10);
            assert!((lat2.to_degrees() - lat).abs() < 1e-10);
        }
    }

    #[test]
    fn sphere_matches_c_proj() {
        // Same parameters as above, with +R=6371000 instead of +ellps=WGS84.
        let proj = EquidistantCylindrical::new(
            Ellipsoid::sphere(6_371_000.0).unwrap(),
            (-90.0_f64).to_radians(),
            30.0_f64.to_radians(),
            500_000.0,
            -200_000.0,
        )
        .unwrap();
        let expected = (692_595.262_492_270, 3_135_847.799_336_762);
        let (x, y) = proj
            .forward((-88.0_f64).to_radians(), 30.0_f64.to_radians())
            .unwrap();
        assert!(
            (x - expected.0).abs() < 1e-6,
            "spherical easting differs from the PROJ reference"
        );
        assert!(
            (y - expected.1).abs() < 1e-6,
            "spherical northing differs from the PROJ reference"
        );
        let (lon, lat) = proj.inverse(expected.0, expected.1).unwrap();
        assert!((lon.to_degrees() + 88.0).abs() < 1e-10);
        assert!((lat.to_degrees() - 30.0).abs() < 1e-10);
    }

    #[test]
    fn poles_use_the_ellipsoidal_quarter_meridian() {
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0).unwrap();
        for sign in [-1.0, 1.0] {
            let pole = sign * std::f64::consts::FRAC_PI_2;
            let northing = sign * 10_001_965.729_312_722;
            let (_, y) = proj.forward(0.0, pole).unwrap();
            assert!((y - northing).abs() < 1e-6);
            let (_, lat) = proj.inverse(0.0, northing).unwrap();
            assert!((lat - pole).abs() < 1e-12);
            assert!(proj.inverse(0.0, northing + sign).is_err());
        }
    }

    #[test]
    fn origin_at_zero() {
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0).unwrap();
        let (x, y) = proj.forward(0.0, 0.0).unwrap();
        assert!(x.abs() < 0.01);
        assert!(y.abs() < 0.01);
    }

    #[test]
    fn forward_wraps_longitude_delta() {
        let proj =
            EquidistantCylindrical::new(ellipsoid::WGS84, 179.0_f64.to_radians(), 0.0, 0.0, 0.0)
                .unwrap();
        let lat = 40.0_f64.to_radians();

        let wrapped = proj.forward((-181.0_f64).to_radians(), lat).unwrap();
        let canonical = proj.forward(179.0_f64.to_radians(), lat).unwrap();

        assert!((wrapped.0 - canonical.0).abs() < 1e-8);
        assert!((wrapped.1 - canonical.1).abs() < 1e-8);
    }

    #[test]
    fn rejects_polar_true_scale() {
        let result = EquidistantCylindrical::new(
            ellipsoid::WGS84,
            0.0,
            std::f64::consts::FRAC_PI_2,
            0.0,
            0.0,
        );
        assert!(result.is_err());
    }
}

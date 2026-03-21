use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, validate_angle, validate_latitude_param,
    validate_lon_lat, validate_offset, validate_projected,
};

/// Equidistant Cylindrical (Plate Carree) projection.
///
/// The simplest projection: x = a * cos(lat_ts) * (lon - lon0), y = a * (lat - lat0).
/// When lat_ts = 0, this is the standard Plate Carree.
pub(crate) struct EquidistantCylindrical {
    a_cos_lat_ts: f64,
    a: f64,
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
        let a_cos_lat_ts = ellipsoid.a * cos_lat_ts;

        Ok(Self {
            a_cos_lat_ts,
            a: ellipsoid.a,
            lon0,
            false_easting,
            false_northing,
        })
    }
}

impl super::ProjectionImpl for EquidistantCylindrical {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let x = self.false_easting + self.a_cos_lat_ts * (lon - self.lon0);
        let y = self.false_northing + self.a * lat;
        ensure_finite_xy("Equidistant Cylindrical", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let lon = self.lon0 + (x - self.false_easting) / self.a_cos_lat_ts;
        let lat = (y - self.false_northing) / self.a;
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
    fn origin_at_zero() {
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0).unwrap();
        let (x, y) = proj.forward(0.0, 0.0).unwrap();
        assert!(x.abs() < 0.01);
        assert!(y.abs() < 0.01);
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

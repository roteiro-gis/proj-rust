use crate::ellipsoid::Ellipsoid;
use crate::error::Result;

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
    ) -> Self {
        Self {
            a_cos_lat_ts: ellipsoid.a * lat_ts.cos(),
            a: ellipsoid.a,
            lon0,
            false_easting,
            false_northing,
        }
    }
}

impl super::ProjectionImpl for EquidistantCylindrical {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let x = self.false_easting + self.a_cos_lat_ts * (lon - self.lon0);
        let y = self.false_northing + self.a * lat;
        Ok((x, y))
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let lon = self.lon0 + (x - self.false_easting) / self.a_cos_lat_ts;
        let lat = (y - self.false_northing) / self.a;
        Ok((lon, lat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn plate_carree_roundtrip() {
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0);

        let lon = (-74.006_f64).to_radians();
        let lat = 40.7128_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-10);
        assert!((lat2 - lat).abs() < 1e-10);
    }

    #[test]
    fn origin_at_zero() {
        let proj = EquidistantCylindrical::new(ellipsoid::WGS84, 0.0, 0.0, 0.0, 0.0);
        let (x, y) = proj.forward(0.0, 0.0).unwrap();
        assert!(x.abs() < 0.01);
        assert!(y.abs() < 0.01);
    }
}

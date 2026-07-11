use crate::ellipsoid::Ellipsoid;
use crate::error::Result;
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected,
};

/// Colombia Urban projection (EPSG method 1052).
///
/// Maps onto a plane at the elevation of the city being mapped (the
/// "projection plane origin height"), so distances on the projection match
/// ground distances at that elevation. Formulas from IOGP Publication 373-7-2
/// (EPSG Guidance Note 7 part 2), matching C PROJ's `col_urban`.
#[derive(Clone)]
pub(crate) struct ColombiaUrban {
    a: f64,
    e2: f64,
    lon0: f64,
    lat0: f64,
    false_easting: f64,
    false_northing: f64,
    /// Projection plane origin height divided by the semi-major axis.
    h0: f64,
    /// ρ₀/a: adimensional meridian radius of curvature at the origin.
    rho0: f64,
    a_coeff: f64,
    b_coeff: f64,
    c_coeff: f64,
    d_coeff: f64,
}

impl ColombiaUrban {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        h0_meters: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_latitude_param("latitude of natural origin", lat0)?;
        validate_offset("projection plane origin height", h0_meters)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let a = ellipsoid.semi_major_axis();
        let e2 = ellipsoid.e2();
        let h0 = h0_meters / a;
        let sin_lat0 = lat0.sin();
        let nu0 = 1.0 / (1.0 - e2 * sin_lat0 * sin_lat0).sqrt();
        let rho0 = (1.0 - e2) / (1.0 - e2 * sin_lat0 * sin_lat0).powf(1.5);

        Ok(Self {
            a,
            e2,
            lon0,
            lat0,
            false_easting,
            false_northing,
            h0,
            rho0,
            a_coeff: 1.0 + h0 / nu0,
            b_coeff: lat0.tan() / (2.0 * rho0 * nu0),
            c_coeff: 1.0 + h0,
            d_coeff: rho0 * (1.0 + h0 / (1.0 - e2)),
        })
    }
}

impl super::ProjectionImpl for ColombiaUrban {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);

        let cos_lat = lat.cos();
        let sin_lat = lat.sin();
        let nu = 1.0 / (1.0 - self.e2 * sin_lat * sin_lat).sqrt();
        let lam_nu_cos_lat = lam * nu * cos_lat;

        let sin_lat_m = (0.5 * (lat + self.lat0)).sin();
        let rho_m = (1.0 - self.e2) / (1.0 - self.e2 * sin_lat_m * sin_lat_m).powf(1.5);
        let g = 1.0 + self.h0 / rho_m;

        let x = self.false_easting + self.a * self.a_coeff * lam_nu_cos_lat;
        let y = self.false_northing
            + self.a
                * g
                * self.rho0
                * ((lat - self.lat0) + self.b_coeff * lam_nu_cos_lat * lam_nu_cos_lat);

        ensure_finite_xy("Colombia Urban", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let dx = (x - self.false_easting) / self.a;
        let dy = (y - self.false_northing) / self.a;

        let lat = self.lat0 + dy / self.d_coeff
            - self.b_coeff * (dx / self.c_coeff) * (dx / self.c_coeff);
        let sin_lat = lat.sin();
        let nu = 1.0 / (1.0 - self.e2 * sin_lat * sin_lat).sqrt();
        let lon = self.lon0 + dx / (self.c_coeff * nu * lat.cos());

        ensure_finite_lon_lat("Colombia Urban", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    /// MAGNA-SIRGAS / Bogota urban grid (EPSG:6247) parameters.
    fn bogota() -> ColombiaUrban {
        ColombiaUrban::new(
            ellipsoid::GRS80,
            (-(74.0_f64 + 8.0 / 60.0 + 47.73 / 3600.0)).to_radians(),
            (4.0_f64 + 40.0 / 60.0 + 49.75 / 3600.0).to_radians(),
            2550.0,
            92_334.879,
            109_320.965,
        )
        .unwrap()
    }

    #[test]
    fn roundtrip_bogota() {
        let proj = bogota();
        let lon = (-74.25_f64).to_radians();
        let lat = 4.8_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-9, "lon: {} vs {}", lon2, lon);
        assert!((lat2 - lat).abs() < 1e-9, "lat: {} vs {}", lat2, lat);
    }

    #[test]
    fn origin_maps_to_false_offsets() {
        let proj = bogota();
        let (x, y) = proj
            .forward(
                (-(74.0_f64 + 8.0 / 60.0 + 47.73 / 3600.0)).to_radians(),
                (4.0_f64 + 40.0 / 60.0 + 49.75 / 3600.0).to_radians(),
            )
            .unwrap();
        assert!((x - 92_334.879).abs() < 1e-6, "x = {x}");
        assert!((y - 109_320.965).abs() < 1e-6, "y = {y}");
    }

    #[test]
    fn forward_wraps_longitude_delta() {
        let proj = ColombiaUrban::new(
            ellipsoid::GRS80,
            179.0_f64.to_radians(),
            4.5_f64.to_radians(),
            2000.0,
            0.0,
            0.0,
        )
        .unwrap();
        let lat = 4.0_f64.to_radians();

        let wrapped = proj.forward((-181.0_f64).to_radians(), lat).unwrap();
        let canonical = proj.forward(179.0_f64.to_radians(), lat).unwrap();

        assert!((wrapped.0 - canonical.0).abs() < 1e-8);
        assert!((wrapped.1 - canonical.1).abs() < 1e-8);
    }
}

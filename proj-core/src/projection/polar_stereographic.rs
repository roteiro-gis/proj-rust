use std::f64::consts::FRAC_PI_2;

use crate::ellipsoid::Ellipsoid;
use crate::error::Result;

/// Polar Stereographic projection.
///
/// Supports both Variant A (specified by `lat_ts` = latitude of true scale)
/// and Variant B (specified by `k0` = scale factor at the pole).
///
/// Used by EPSG:3413 (NSIDC Sea Ice Polar Stereographic North),
/// EPSG:3031 (Antarctic Polar Stereographic), and others.
pub(crate) struct PolarStereographic {
    /// Central meridian / straight vertical longitude (radians).
    lon0: f64,
    /// True if the projection is for the north pole, false for south.
    is_north: bool,
    /// False easting (meters).
    false_easting: f64,
    /// False northing (meters).
    false_northing: f64,
    // Precomputed constants
    e: f64,
    two_a_k0: f64,
}

impl PolarStereographic {
    /// Create from parameters. `lat_ts` in radians determines hemisphere.
    /// If `|lat_ts| < 90°`, k0 is computed from lat_ts (Variant A).
    /// If `|lat_ts| ≈ 90°`, the provided k0 is used directly (Variant B).
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat_ts: f64,
        k0_input: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Self {
        let is_north = lat_ts >= 0.0;
        let e = ellipsoid.e();
        let e2 = ellipsoid.e2();
        let a = ellipsoid.a;

        // Determine k0:
        // If lat_ts is at the pole (±90°), use provided k0.
        // Otherwise compute k0 from lat_ts (Variant A).
        let k0 = if (lat_ts.abs() - FRAC_PI_2).abs() < 1e-10 {
            k0_input
        } else {
            // Compute scale factor from latitude of true scale
            let abs_lat_ts = lat_ts.abs();
            let sin_ts = abs_lat_ts.sin();
            let cos_ts = abs_lat_ts.cos();
            let e_sin_ts = e * sin_ts;

            let m_ts = cos_ts / (1.0 - e2 * sin_ts * sin_ts).sqrt();
            let t_ts = ((FRAC_PI_2 - abs_lat_ts) / 2.0).tan()
                / ((1.0 - e_sin_ts) / (1.0 + e_sin_ts)).powf(e / 2.0);
            let t_90 = compute_t_polar(e);

            m_ts / (2.0 * t_ts / t_90)
        };

        let two_a_k0 = 2.0 * a * k0;

        Self {
            lon0,
            is_north,
            false_easting,
            false_northing,
            e,
            two_a_k0,
        }
    }
}

/// Compute t at the pole: t(90°) = exp(-e * atanh(e)) = ((1-e)/(1+e))^(e/2)
fn compute_t_polar(e: f64) -> f64 {
    ((1.0 - e) / (1.0 + e)).powf(e / 2.0)
}

/// Compute the conformal latitude parameter t for a given latitude.
fn compute_t(lat: f64, e: f64) -> f64 {
    let sin_lat = lat.sin();
    let e_sin = e * sin_lat;
    ((FRAC_PI_2 - lat.abs()) / 2.0).tan() / ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)
}

/// Iterative computation of latitude from t (isometric latitude → geodetic latitude).
fn lat_from_t(t: f64, e: f64) -> f64 {
    let mut lat = FRAC_PI_2 - 2.0 * t.atan();
    for _ in 0..15 {
        let e_sin = e * lat.sin();
        let new_lat = FRAC_PI_2 - 2.0 * (t * ((1.0 - e_sin) / (1.0 + e_sin)).powf(e / 2.0)).atan();
        if (new_lat - lat).abs() < 1e-14 {
            return new_lat;
        }
        lat = new_lat;
    }
    lat
}

impl super::ProjectionImpl for PolarStereographic {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let e = self.e;
        let t_polar = compute_t_polar(e);

        // For north polar: use latitude directly.
        // For south polar: negate latitude to work with formulas designed for north.
        let (effective_lat, sign) = if self.is_north {
            (lat, 1.0)
        } else {
            (-lat, -1.0)
        };

        let t = compute_t(effective_lat, e);
        let rho = self.two_a_k0 * t / t_polar;

        let d_lon = lon - self.lon0;

        let x = self.false_easting + rho * d_lon.sin();
        let y = self.false_northing - sign * rho * d_lon.cos();

        Ok((x, y))
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let e = self.e;
        let t_polar = compute_t_polar(e);

        let dx = x - self.false_easting;
        let dy = y - self.false_northing;

        let (dy_eff, sign) = if self.is_north {
            (-dy, 1.0)
        } else {
            (dy, -1.0)
        };

        let rho = (dx * dx + dy_eff * dy_eff).sqrt();

        if rho < 1e-10 {
            // At the pole
            let lat = if self.is_north { FRAC_PI_2 } else { -FRAC_PI_2 };
            return Ok((self.lon0, lat));
        }

        let t = rho * t_polar / self.two_a_k0;
        let lat_unsigned = lat_from_t(t, e);
        let lat = sign * lat_unsigned;

        let mut lon = self.lon0 + dx.atan2(dy_eff);
        // Normalize longitude to [-PI, PI]
        while lon > std::f64::consts::PI {
            lon -= 2.0 * std::f64::consts::PI;
        }
        while lon < -std::f64::consts::PI {
            lon += 2.0 * std::f64::consts::PI;
        }

        Ok((lon, lat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    /// EPSG:3413 — WGS 84 / NSIDC Sea Ice Polar Stereographic North
    /// lat_ts = 70°N, lon0 = -45°, k0 derived from lat_ts
    fn epsg_3413() -> PolarStereographic {
        PolarStereographic::new(
            ellipsoid::WGS84,
            (-45.0_f64).to_radians(),
            70.0_f64.to_radians(),
            1.0,
            0.0,
            0.0,
        )
    }

    /// EPSG:3031 — WGS 84 / Antarctic Polar Stereographic
    /// lat_ts = -71°S, lon0 = 0°
    fn epsg_3031() -> PolarStereographic {
        PolarStereographic::new(
            ellipsoid::WGS84,
            0.0_f64.to_radians(),
            (-71.0_f64).to_radians(),
            1.0,
            0.0,
            0.0,
        )
    }

    #[test]
    fn north_pole_at_origin() {
        let proj = epsg_3413();
        let (x, y) = proj
            .forward((-45.0_f64).to_radians(), 90.0_f64.to_radians())
            .unwrap();
        assert!(x.abs() < 0.01, "x = {x}");
        assert!(y.abs() < 0.01, "y = {y}");
    }

    #[test]
    fn roundtrip_north() {
        let proj = epsg_3413();
        let lon = (-45.0_f64).to_radians();
        let lat = 75.0_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon_back, lat_back) = proj.inverse(x, y).unwrap();
        assert!((lon_back - lon).abs() < 1e-8, "lon: {lon_back} vs {lon}");
        assert!((lat_back - lat).abs() < 1e-8, "lat: {lat_back} vs {lat}");
    }

    #[test]
    fn roundtrip_south() {
        let proj = epsg_3031();
        let lon = 45.0_f64.to_radians();
        let lat = (-75.0_f64).to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon_back, lat_back) = proj.inverse(x, y).unwrap();
        assert!((lon_back - lon).abs() < 1e-8, "lon: {lon_back} vs {lon}");
        assert!((lat_back - lat).abs() < 1e-8, "lat: {lat_back} vs {lat}");
    }

    #[test]
    fn known_value_3413() {
        // Point: 75°N, 0°E in EPSG:3413
        // Verified by roundtrip; exact C PROJ values to be added in reference test suite
        let proj = epsg_3413();
        let lon = 0.0_f64.to_radians();
        let lat = 75.0_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();

        // Roundtrip verification
        let (lon_back, lat_back) = proj.inverse(x, y).unwrap();
        assert!(
            (lon_back - lon).abs() < 1e-8,
            "lon roundtrip: {lon_back} vs {lon}"
        );
        assert!(
            (lat_back - lat).abs() < 1e-8,
            "lat roundtrip: {lat_back} vs {lat}"
        );
    }
}

use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected, validate_scale,
};

const ECCENTRICITY_EPSILON: f64 = 1e-15;
const ORIGIN_EPSILON: f64 = 1e-12;
const RHO_EPSILON: f64 = 1e-9;

/// EPSG Oblique Stereographic projection.
///
/// This is the EPSG 9809 conformal-sphere method used by systems such as
/// RD New, not Snyder's single-stage ellipsoidal stereographic.
#[derive(Clone)]
pub(crate) struct ObliqueStereographic {
    e: f64,
    e2: f64,
    lon0: f64,
    n: f64,
    c: f64,
    sin_chi0: f64,
    cos_chi0: f64,
    two_r_k0: f64,
    false_easting: f64,
    false_northing: f64,
}

impl ObliqueStereographic {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_latitude_param("latitude of natural origin", lat0)?;
        validate_scale("scale factor", k0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;
        if (lat0.abs() - std::f64::consts::FRAC_PI_2).abs() < ORIGIN_EPSILON {
            return Err(Error::InvalidDefinition(
                "Oblique Stereographic is indeterminate at a polar origin; use Polar Stereographic"
                    .into(),
            ));
        }

        let e = ellipsoid.e();
        let e2 = ellipsoid.e2();
        let sin_lat0 = lat0.sin();
        let cos_lat0 = lat0.cos();
        let rho0 = ellipsoid.m_radius(lat0);
        let nu0 = ellipsoid.n_radius(lat0);
        let r = (rho0 * nu0).sqrt();
        let n = (1.0 + e2 * cos_lat0.powi(4) / (1.0 - e2)).sqrt();

        let psi0 = isometric_latitude(lat0, e);
        let sin_chi00 = (n * psi0).tanh();
        let denom = (n - sin_lat0) * (1.0 + sin_chi00);
        if denom.abs() < ORIGIN_EPSILON {
            return Err(Error::InvalidDefinition(
                "Oblique Stereographic origin constants are singular".into(),
            ));
        }
        let c = ((n + sin_lat0) * (1.0 - sin_chi00)) / denom;
        if !c.is_finite() || c <= 0.0 {
            return Err(Error::InvalidDefinition(
                "Oblique Stereographic conformal-sphere constant is invalid".into(),
            ));
        }

        let chi0 = conformal_latitude_from_isometric(psi0, n, c);
        let sin_chi0 = chi0.sin();
        let cos_chi0 = chi0.cos();

        Ok(Self {
            e,
            e2,
            lon0,
            n,
            c,
            sin_chi0,
            cos_chi0,
            two_r_k0: 2.0 * r * k0,
            false_easting,
            false_northing,
        })
    }
}

fn isometric_latitude(lat: f64, e: f64) -> f64 {
    if e.abs() < ECCENTRICITY_EPSILON {
        return (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln();
    }

    let sin_lat = lat.sin();
    let e_sin = e * sin_lat;
    (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln()
        + (e / 2.0) * ((1.0 - e_sin) / (1.0 + e_sin)).ln()
}

fn conformal_latitude_from_isometric(psi: f64, n: f64, c: f64) -> f64 {
    (n * psi + 0.5 * c.ln()).tanh().clamp(-1.0, 1.0).asin()
}

fn geodetic_from_isometric(psi: f64, e: f64, e2: f64) -> Result<f64> {
    let initial = 2.0 * psi.exp().atan() - std::f64::consts::FRAC_PI_2;
    if e.abs() < ECCENTRICITY_EPSILON {
        return Ok(initial);
    }

    super::converge(
        "Oblique Stereographic inverse latitude",
        initial,
        15,
        1e-14,
        |lat| {
            let psi_i = isometric_latitude(lat, e);
            let sin_lat = lat.sin();
            lat - (psi_i - psi) * lat.cos() * (1.0 - e2 * sin_lat * sin_lat) / (1.0 - e2)
        },
    )
}

impl super::ProjectionImpl for ObliqueStereographic {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let psi = isometric_latitude(lat, self.e);
        let chi = conformal_latitude_from_isometric(psi, self.n, self.c);
        let d_lambda = self.n * normalize_longitude(lon - self.lon0);

        let sin_chi = chi.sin();
        let cos_chi = chi.cos();
        let denom = 1.0 + sin_chi * self.sin_chi0 + cos_chi * self.cos_chi0 * d_lambda.cos();
        if denom <= 0.0 {
            return Err(Error::OutOfRange(
                "Oblique Stereographic is undefined at the antipode".into(),
            ));
        }

        let x = self.false_easting + self.two_r_k0 * cos_chi * d_lambda.sin() / denom;
        let y = self.false_northing
            + self.two_r_k0 * (sin_chi * self.cos_chi0 - cos_chi * self.sin_chi0 * d_lambda.cos())
                / denom;

        ensure_finite_xy("Oblique Stereographic", x, y)
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let dx = x - self.false_easting;
        let dy = y - self.false_northing;
        let rho = (dx * dx + dy * dy).sqrt();

        if rho < RHO_EPSILON {
            let psi =
                (0.5 * ((1.0 + self.sin_chi0) / (self.c * (1.0 - self.sin_chi0))).ln()) / self.n;
            let lat = geodetic_from_isometric(psi, self.e, self.e2)?;
            return ensure_finite_lon_lat("Oblique Stereographic", self.lon0, lat);
        }

        let c_angle = 2.0 * (rho / self.two_r_k0).atan();
        let sin_c = c_angle.sin();
        let cos_c = c_angle.cos();
        let sin_chi = (cos_c * self.sin_chi0 + dy * sin_c * self.cos_chi0 / rho).clamp(-1.0, 1.0);
        let chi = sin_chi.asin();
        let d_lambda = (dx * sin_c).atan2(rho * self.cos_chi0 * cos_c - dy * self.sin_chi0 * sin_c);

        let psi = (0.5 * ((1.0 + chi.sin()) / (self.c * (1.0 - chi.sin()))).ln()) / self.n;
        let lat = geodetic_from_isometric(psi, self.e, self.e2)?;
        let lon = self.lon0 + d_lambda / self.n;

        ensure_finite_lon_lat("Oblique Stereographic", lon, lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn epsg_rd_new_example() {
        let proj = ObliqueStereographic::new(
            ellipsoid::BESSEL1841,
            5.0_f64.to_radians() + 23.0_f64.to_radians() / 60.0 + 15.5_f64.to_radians() / 3600.0,
            52.0_f64.to_radians() + 9.0_f64.to_radians() / 60.0 + 22.178_f64.to_radians() / 3600.0,
            0.999_907_9,
            155_000.0,
            463_000.0,
        )
        .unwrap();

        let (x, y) = proj
            .forward(6.0_f64.to_radians(), 53.0_f64.to_radians())
            .unwrap();

        assert!((x - 196_105.283).abs() < 0.01, "x = {x}");
        assert!((y - 557_057.739).abs() < 0.01, "y = {y}");

        let (lon, lat) = proj.inverse(x, y).unwrap();
        assert!((lon.to_degrees() - 6.0).abs() < 1e-8);
        assert!((lat.to_degrees() - 53.0).abs() < 1e-8);
    }

    #[test]
    fn roundtrip_southern_origin() {
        let proj = ObliqueStereographic::new(
            ellipsoid::WGS84,
            20.0_f64.to_radians(),
            (-30.0_f64).to_radians(),
            0.9999,
            500_000.0,
            1_000_000.0,
        )
        .unwrap();

        let lon = 22.0_f64.to_radians();
        let lat = (-29.0_f64).to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }
}

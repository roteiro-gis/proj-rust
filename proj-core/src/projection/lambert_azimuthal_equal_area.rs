use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    authalic_latitude, authalic_q, ensure_finite_lon_lat, ensure_finite_xy, geodetic_from_authalic,
    normalize_longitude, validate_angle, validate_latitude_param, validate_lon_lat,
    validate_offset, validate_projected,
};

const POLAR_EPSILON: f64 = 1e-12;
const RHO_EPSILON: f64 = 1e-9;

#[derive(Clone, Copy)]
enum Aspect {
    Oblique,
    NorthPolar,
    SouthPolar,
}

/// Lambert Azimuthal Equal Area projection.
///
/// This implements EPSG method 9820, including oblique/equatorial and polar
/// aspects on ellipsoids and spheres.
#[derive(Clone)]
pub(crate) struct LambertAzimuthalEqualArea {
    a: f64,
    e2: f64,
    spherical: bool,
    spherical_radius: f64,
    lon0: f64,
    lat0: f64,
    q_p: f64,
    rq: f64,
    sin_lat0: f64,
    cos_lat0: f64,
    sin_beta0: f64,
    cos_beta0: f64,
    d: f64,
    false_easting: f64,
    false_northing: f64,
    aspect: Aspect,
}

impl LambertAzimuthalEqualArea {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        Self::new_internal(ellipsoid, lon0, lat0, false_easting, false_northing, false)
    }

    pub(crate) fn new_spherical(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        Self::new_internal(ellipsoid, lon0, lat0, false_easting, false_northing, true)
    }

    fn new_internal(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        false_easting: f64,
        false_northing: f64,
        spherical: bool,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_latitude_param("latitude of natural origin", lat0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let e2 = ellipsoid.e2();
        let q_p = authalic_q(std::f64::consts::FRAC_PI_2, e2);
        if q_p <= 0.0 || !q_p.is_finite() {
            return Err(Error::InvalidDefinition(
                "Lambert Azimuthal Equal Area authalic radius is invalid".into(),
            ));
        }
        let rq = ellipsoid.semi_major_axis() * (q_p / 2.0).sqrt();
        let q0 = authalic_q(lat0, e2);
        let beta0 = authalic_latitude(q0, q_p);
        let sin_lat0 = lat0.sin();
        let cos_lat0 = lat0.cos();
        let sin_beta0 = beta0.sin();
        let cos_beta0 = beta0.cos();

        let aspect = if (lat0 - std::f64::consts::FRAC_PI_2).abs() < POLAR_EPSILON {
            Aspect::NorthPolar
        } else if (lat0 + std::f64::consts::FRAC_PI_2).abs() < POLAR_EPSILON {
            Aspect::SouthPolar
        } else {
            Aspect::Oblique
        };

        let d = match aspect {
            Aspect::Oblique => {
                if cos_beta0.abs() < POLAR_EPSILON {
                    return Err(Error::InvalidDefinition(
                        "Lambert Azimuthal Equal Area polar origin must use the polar aspect"
                            .into(),
                    ));
                }
                let sin_lat0 = lat0.sin();
                ellipsoid.semi_major_axis() * lat0.cos()
                    / ((1.0 - e2 * sin_lat0 * sin_lat0).sqrt() * rq * cos_beta0)
            }
            Aspect::NorthPolar | Aspect::SouthPolar => 1.0,
        };
        if !d.is_finite() || d <= 0.0 {
            return Err(Error::InvalidDefinition(
                "Lambert Azimuthal Equal Area origin scale is invalid".into(),
            ));
        }

        Ok(Self {
            a: ellipsoid.semi_major_axis(),
            e2,
            spherical,
            spherical_radius: rq,
            lon0,
            lat0,
            q_p,
            rq,
            sin_lat0,
            cos_lat0,
            sin_beta0,
            cos_beta0,
            d,
            false_easting,
            false_northing,
            aspect,
        })
    }
}

impl super::ProjectionImpl for LambertAzimuthalEqualArea {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let d_lon = normalize_longitude(lon - self.lon0);

        if self.spherical {
            let sin_lat = lat.sin();
            let cos_lat = lat.cos();
            let cos_d_lon = d_lon.cos();
            let denom = 1.0 + self.sin_lat0 * sin_lat + self.cos_lat0 * cos_lat * cos_d_lon;
            if denom <= 0.0 {
                return Err(Error::OutOfRange(
                    "Lambert Azimuthal Equal Area (spherical) is undefined at the antipode".into(),
                ));
            }
            let k = (2.0 / denom).sqrt();
            let x = self.false_easting + self.spherical_radius * k * cos_lat * d_lon.sin();
            let y = self.false_northing
                + self.spherical_radius
                    * k
                    * (self.cos_lat0 * sin_lat - self.sin_lat0 * cos_lat * cos_d_lon);
            return ensure_finite_xy("Lambert Azimuthal Equal Area (spherical)", x, y);
        }

        let q = authalic_q(lat, self.e2);

        match self.aspect {
            Aspect::NorthPolar => {
                let rho = self.a * (self.q_p - q).max(0.0).sqrt();
                let x = self.false_easting + rho * d_lon.sin();
                let y = self.false_northing - rho * d_lon.cos();
                ensure_finite_xy("Lambert Azimuthal Equal Area", x, y)
            }
            Aspect::SouthPolar => {
                let rho = self.a * (self.q_p + q).max(0.0).sqrt();
                let x = self.false_easting + rho * d_lon.sin();
                let y = self.false_northing + rho * d_lon.cos();
                ensure_finite_xy("Lambert Azimuthal Equal Area", x, y)
            }
            Aspect::Oblique => {
                let beta = authalic_latitude(q, self.q_p);
                let sin_beta = beta.sin();
                let cos_beta = beta.cos();
                let cos_d_lon = d_lon.cos();
                let denom = 1.0 + self.sin_beta0 * sin_beta + self.cos_beta0 * cos_beta * cos_d_lon;
                if denom <= 0.0 {
                    return Err(Error::OutOfRange(
                        "Lambert Azimuthal Equal Area is undefined at the antipode".into(),
                    ));
                }

                let b = self.rq * (2.0 / denom).sqrt();
                let x = self.false_easting + b * self.d * cos_beta * d_lon.sin();
                let y = self.false_northing
                    + (b / self.d)
                        * (self.cos_beta0 * sin_beta - self.sin_beta0 * cos_beta * cos_d_lon);
                ensure_finite_xy("Lambert Azimuthal Equal Area", x, y)
            }
        }
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let dx = x - self.false_easting;
        let dy = y - self.false_northing;

        if self.spherical {
            let rho = (dx * dx + dy * dy).sqrt();
            if rho < RHO_EPSILON {
                return ensure_finite_lon_lat(
                    "Lambert Azimuthal Equal Area (spherical)",
                    self.lon0,
                    self.lat0,
                );
            }

            let c = 2.0
                * (rho / (2.0 * self.spherical_radius))
                    .clamp(-1.0, 1.0)
                    .asin();
            let sin_c = c.sin();
            let cos_c = c.cos();
            let lat = (cos_c * self.sin_lat0 + dy * sin_c * self.cos_lat0 / rho)
                .clamp(-1.0, 1.0)
                .asin();
            let lon = self.lon0
                + (dx * sin_c).atan2(rho * self.cos_lat0 * cos_c - dy * self.sin_lat0 * sin_c);
            return ensure_finite_lon_lat("Lambert Azimuthal Equal Area (spherical)", lon, lat);
        }

        match self.aspect {
            Aspect::NorthPolar | Aspect::SouthPolar => {
                let rho = (dx * dx + dy * dy).sqrt();
                let beta = if rho < RHO_EPSILON {
                    self.lat0
                } else {
                    let q_over_qp = match self.aspect {
                        Aspect::NorthPolar => 1.0 - rho * rho / (self.a * self.a * self.q_p),
                        Aspect::SouthPolar => rho * rho / (self.a * self.a * self.q_p) - 1.0,
                        Aspect::Oblique => unreachable!(),
                    };
                    q_over_qp.clamp(-1.0, 1.0).asin()
                };
                let lat = geodetic_from_authalic(beta, self.e2);
                let lon = if rho < RHO_EPSILON {
                    self.lon0
                } else {
                    self.lon0
                        + match self.aspect {
                            Aspect::NorthPolar => dx.atan2(-dy),
                            Aspect::SouthPolar => dx.atan2(dy),
                            Aspect::Oblique => unreachable!(),
                        }
                };
                ensure_finite_lon_lat("Lambert Azimuthal Equal Area", lon, lat)
            }
            Aspect::Oblique => {
                let x_scaled = dx / self.d;
                let y_scaled = self.d * dy;
                let rho = (x_scaled * x_scaled + y_scaled * y_scaled).sqrt();
                if rho < RHO_EPSILON {
                    return ensure_finite_lon_lat(
                        "Lambert Azimuthal Equal Area",
                        self.lon0,
                        self.lat0,
                    );
                }

                let c = 2.0 * (rho / (2.0 * self.rq)).clamp(-1.0, 1.0).asin();
                let sin_c = c.sin();
                let cos_c = c.cos();
                let beta = (cos_c * self.sin_beta0 + (self.d * dy * sin_c * self.cos_beta0) / rho)
                    .clamp(-1.0, 1.0)
                    .asin();
                let lat = geodetic_from_authalic(beta, self.e2);
                let lon = self.lon0
                    + (dx * sin_c).atan2(
                        self.d * rho * self.cos_beta0 * cos_c
                            - self.d * self.d * dy * self.sin_beta0 * sin_c,
                    );
                ensure_finite_lon_lat("Lambert Azimuthal Equal Area", lon, lat)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    #[test]
    fn epsg_3035_example() {
        let proj = LambertAzimuthalEqualArea::new(
            ellipsoid::GRS80,
            10.0_f64.to_radians(),
            52.0_f64.to_radians(),
            4_321_000.0,
            3_210_000.0,
        )
        .unwrap();

        let (x, y) = proj
            .forward(5.0_f64.to_radians(), 50.0_f64.to_radians())
            .unwrap();

        assert!((x - 3_962_799.45).abs() < 0.02, "x = {x}");
        assert!((y - 2_999_718.85).abs() < 0.02, "y = {y}");

        let (lon, lat) = proj.inverse(x, y).unwrap();
        assert!((lon.to_degrees() - 5.0).abs() < 1e-8);
        assert!((lat.to_degrees() - 50.0).abs() < 1e-8);
    }

    #[test]
    fn polar_roundtrip() {
        let proj =
            LambertAzimuthalEqualArea::new(ellipsoid::WGS84, 0.0, 90.0_f64.to_radians(), 0.0, 0.0)
                .unwrap();

        let lon = 40.0_f64.to_radians();
        let lat = 75.0_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }

    #[test]
    fn spherical_roundtrip_on_ellipsoid_authalic_radius() {
        let proj = LambertAzimuthalEqualArea::new_spherical(
            ellipsoid::CLARKE1866,
            (-100.0_f64).to_radians(),
            45.0_f64.to_radians(),
            0.0,
            0.0,
        )
        .unwrap();

        let lon = (-96.0_f64).to_radians();
        let lat = 37.0_f64.to_radians();
        let (x, y) = proj.forward(lon, lat).unwrap();
        let (lon2, lat2) = proj.inverse(x, y).unwrap();

        assert!(x.is_finite());
        assert!(y.is_finite());
        assert!((lon2 - lon).abs() < 1e-8);
        assert!((lat2 - lat).abs() < 1e-8);
    }
}

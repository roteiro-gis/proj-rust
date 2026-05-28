//! Reference ellipsoid definitions.

use crate::error::{Error, Result};
use std::f64::consts::PI;

/// A reference ellipsoid.
#[derive(Debug, Clone, Copy)]
pub struct Ellipsoid {
    /// Semi-major axis (equatorial radius) in meters.
    a: f64,
    /// Flattening (f = (a - b) / a).
    f: f64,
}

impl Ellipsoid {
    /// Create an ellipsoid from semi-major axis and inverse flattening.
    pub fn from_a_rf(a: f64, rf: f64) -> Result<Self> {
        if !rf.is_finite() || rf <= 1.0 {
            return Err(Error::InvalidDefinition(
                "inverse flattening must be a finite number greater than 1".into(),
            ));
        }
        Self::from_a_f(a, 1.0 / rf)
    }

    /// Create an ellipsoid from semi-major axis and flattening.
    pub fn from_a_f(a: f64, f: f64) -> Result<Self> {
        if !a.is_finite() || a <= 0.0 {
            return Err(Error::InvalidDefinition(
                "semi-major axis must be a finite positive number".into(),
            ));
        }
        if !f.is_finite() || !(0.0..1.0).contains(&f) {
            return Err(Error::InvalidDefinition(
                "flattening must be finite and in the range [0, 1)".into(),
            ));
        }
        Ok(Self { a, f })
    }

    /// Create a sphere (flattening = 0).
    pub fn sphere(radius: f64) -> Result<Self> {
        Self::from_a_f(radius, 0.0)
    }

    const fn from_a_rf_unchecked(a: f64, rf: f64) -> Self {
        Self { a, f: 1.0 / rf }
    }

    const fn sphere_unchecked(radius: f64) -> Self {
        Self { a: radius, f: 0.0 }
    }

    /// Semi-major axis (equatorial radius) in meters.
    pub const fn semi_major_axis(&self) -> f64 {
        self.a
    }

    /// Flattening (f = (a - b) / a).
    pub const fn flattening(&self) -> f64 {
        self.f
    }

    /// Inverse flattening, or 0 for a sphere.
    pub fn inverse_flattening(&self) -> f64 {
        if self.f == 0.0 {
            0.0
        } else {
            1.0 / self.f
        }
    }

    /// Semi-minor axis (polar radius) in meters.
    pub fn b(&self) -> f64 {
        self.a * (1.0 - self.f)
    }

    /// First eccentricity squared: e² = 2f - f².
    pub fn e2(&self) -> f64 {
        2.0 * self.f - self.f * self.f
    }

    /// First eccentricity.
    pub fn e(&self) -> f64 {
        self.e2().sqrt()
    }

    /// Third flattening: n = f / (2 - f) = (a - b) / (a + b).
    pub fn n(&self) -> f64 {
        self.f / (2.0 - self.f)
    }

    /// Second eccentricity squared: e'² = e² / (1 - e²).
    pub fn ep2(&self) -> f64 {
        let e2 = self.e2();
        e2 / (1.0 - e2)
    }

    /// Radius of curvature in the prime vertical at latitude phi (radians).
    pub fn n_radius(&self, phi: f64) -> f64 {
        let sin_phi = phi.sin();
        self.a / (1.0 - self.e2() * sin_phi * sin_phi).sqrt()
    }

    /// Radius of curvature in the meridian at latitude phi (radians).
    pub fn m_radius(&self, phi: f64) -> f64 {
        let e2 = self.e2();
        let sin_phi = phi.sin();
        let denom = 1.0 - e2 * sin_phi * sin_phi;
        self.a * (1.0 - e2) / denom.powf(1.5)
    }
}

// Well-known ellipsoids.

/// WGS 84 ellipsoid.
pub const WGS84: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6378137.0, 298.257223563);

/// GRS 1980 ellipsoid (used by NAD83).
pub const GRS80: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6378137.0, 298.257222101);

/// Clarke 1866 ellipsoid (used by NAD27).
pub const CLARKE1866: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6378206.4, 294.978698214);

/// International 1924 (Hayford) ellipsoid.
pub const INTL1924: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6378388.0, 297.0);

/// Bessel 1841 ellipsoid.
pub const BESSEL1841: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6377397.155, 299.1528128);

/// Krassowsky 1940 ellipsoid.
pub const KRASSOWSKY: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6378245.0, 298.3);

/// Airy 1830 ellipsoid (used by OSGB36).
pub const AIRY1830: Ellipsoid = Ellipsoid::from_a_rf_unchecked(6377563.396, 299.3249646);

/// Unit sphere (radius = 1).
pub const UNIT_SPHERE: Ellipsoid = Ellipsoid::sphere_unchecked(1.0);

/// Authalic sphere matching WGS84 surface area.
pub const WGS84_SPHERE: Ellipsoid = Ellipsoid::sphere_unchecked(6371007.181);

/// Convert degrees to radians.
pub fn deg_to_rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

/// Convert radians to degrees.
pub fn rad_to_deg(rad: f64) -> f64 {
    rad * 180.0 / PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgs84_basics() {
        let e = WGS84;
        assert!((e.semi_major_axis() - 6378137.0).abs() < 1e-6);
        assert!((e.b() - 6356752.314245).abs() < 1.0);
        assert!((e.e2() - 0.00669437999014).abs() < 1e-11);
    }

    #[test]
    fn sphere_has_zero_eccentricity() {
        let s = UNIT_SPHERE;
        assert_eq!(s.e2(), 0.0);
        assert_eq!(s.b(), 1.0);
    }

    #[test]
    fn deg_rad_roundtrip() {
        let deg = 45.0;
        assert!((rad_to_deg(deg_to_rad(deg)) - deg).abs() < 1e-12);
    }

    #[test]
    fn reject_invalid_ellipsoid_dimensions() {
        assert!(Ellipsoid::from_a_rf(f64::NAN, 298.257223563).is_err());
        assert!(Ellipsoid::from_a_rf(6378137.0, 0.0).is_err());
        assert!(Ellipsoid::from_a_f(6378137.0, 1.0).is_err());
        assert!(Ellipsoid::sphere(-1.0).is_err());
    }
}

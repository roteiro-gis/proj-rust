//! Reference ellipsoid definitions.

use std::f64::consts::PI;

/// A reference ellipsoid.
#[derive(Debug, Clone, Copy)]
pub struct Ellipsoid {
    /// Semi-major axis (equatorial radius) in meters.
    pub a: f64,
    /// Flattening (f = (a - b) / a).
    pub f: f64,
}

impl Ellipsoid {
    /// Create an ellipsoid from semi-major axis and inverse flattening.
    pub const fn from_a_rf(a: f64, rf: f64) -> Self {
        Self { a, f: 1.0 / rf }
    }

    /// Create a sphere (flattening = 0).
    pub const fn sphere(radius: f64) -> Self {
        Self { a: radius, f: 0.0 }
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
pub const WGS84: Ellipsoid = Ellipsoid::from_a_rf(6378137.0, 298.257223563);

/// GRS 1980 ellipsoid (used by NAD83).
pub const GRS80: Ellipsoid = Ellipsoid::from_a_rf(6378137.0, 298.257222101);

/// Clarke 1866 ellipsoid (used by NAD27).
pub const CLARKE1866: Ellipsoid = Ellipsoid::from_a_rf(6378206.4, 294.978698214);

/// International 1924 (Hayford) ellipsoid.
pub const INTL1924: Ellipsoid = Ellipsoid::from_a_rf(6378388.0, 297.0);

/// Bessel 1841 ellipsoid.
pub const BESSEL1841: Ellipsoid = Ellipsoid::from_a_rf(6377397.155, 299.1528128);

/// Krassowsky 1940 ellipsoid.
pub const KRASSOWSKY: Ellipsoid = Ellipsoid::from_a_rf(6378245.0, 298.3);

/// Airy 1830 ellipsoid (used by OSGB36).
pub const AIRY1830: Ellipsoid = Ellipsoid::from_a_rf(6377563.396, 299.3249646);

/// Unit sphere (radius = 1).
pub const UNIT_SPHERE: Ellipsoid = Ellipsoid::sphere(1.0);

/// Authalic sphere matching WGS84 surface area.
pub const WGS84_SPHERE: Ellipsoid = Ellipsoid::sphere(6371007.181);

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
        assert!((e.a - 6378137.0).abs() < 1e-6);
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
}

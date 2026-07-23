use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, inverse_meridian_arc, meridian_arc,
    meridian_arc_coefficients, normalize_longitude, validate_angle, validate_latitude_param,
    validate_lon_lat, validate_offset, validate_projected,
};
use geographiclib_rs::{DirectGeodesic, Geodesic, InverseGeodesic};
use std::f64::consts::{FRAC_PI_2, PI};

const EPS10: f64 = 1e-10;
const NEAR_UNIT_TOL: f64 = 1e-14;
/// Fixed iteration count of C PROJ's Guam inverse.
const GUAM_INVERSE_ITERATIONS: usize = 3;

#[derive(Clone, Copy, PartialEq)]
enum Aspect {
    NorthPolar,
    SouthPolar,
    Equatorial,
    Oblique,
}

/// Azimuthal Equidistant (EPSG methods 1125 and 9832), matching C PROJ's
/// `aeqd`.
///
/// True distance and azimuth from the projection centre to every point. The
/// oblique/equatorial ellipsoidal aspects are defined through geodesic
/// distance and azimuth (Karney's algorithms, as in C PROJ); the polar
/// aspects use the meridional arc. C PROJ also uses this construction for
/// EPSG method 9832 (Modified Azimuthal Equidistant), whose closed-form
/// approximation it matches to well under a centimetre over the method's
/// island-scale domains.
#[derive(Clone)]
pub(crate) struct AzimuthalEquidistant {
    a: f64,
    e2: f64,
    lon0: f64,
    lat0: f64,
    false_easting: f64,
    false_northing: f64,
    sin_lat0: f64,
    cos_lat0: f64,
    aspect: Aspect,
    en: [f64; 13],
    /// Meridional arc from the equator to the pole (polar aspects).
    mp: f64,
    /// Geodesic on the unit-semi-major-axis ellipsoid; boxed because its
    /// precomputed coefficient tables would otherwise dominate the size of
    /// the `Projection` enum.
    geod: Box<Geodesic>,
}

impl AzimuthalEquidistant {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_latitude_param("latitude of natural origin", lat0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let e2 = ellipsoid.e2();
        let en = meridian_arc_coefficients(e2);
        let f = 1.0 - (1.0 - e2).sqrt();

        let (aspect, sin_lat0, cos_lat0) = if (lat0.abs() - FRAC_PI_2).abs() < EPS10 {
            if lat0 < 0.0 {
                (Aspect::SouthPolar, -1.0, 0.0)
            } else {
                (Aspect::NorthPolar, 1.0, 0.0)
            }
        } else if lat0.abs() < EPS10 {
            (Aspect::Equatorial, 0.0, 1.0)
        } else {
            (Aspect::Oblique, lat0.sin(), lat0.cos())
        };

        let mp = match aspect {
            Aspect::NorthPolar => meridian_arc(FRAC_PI_2, 1.0, 0.0, &en),
            Aspect::SouthPolar => meridian_arc(-FRAC_PI_2, -1.0, 0.0, &en),
            Aspect::Equatorial | Aspect::Oblique => 0.0,
        };

        Ok(Self {
            a: ellipsoid.semi_major_axis(),
            e2,
            lon0,
            lat0,
            false_easting,
            false_northing,
            sin_lat0,
            cos_lat0,
            aspect,
            en,
            mp,
            geod: Box::new(Geodesic::new(1.0, f)),
        })
    }

    /// Oblique/equatorial forward through the geodesic between the centre
    /// and the point, in semi-major-axis units.
    fn geodesic_forward(&self, lam: f64, phi: f64) -> (f64, f64) {
        if lam.abs() < EPS10 && (phi - self.lat0).abs() < EPS10 {
            return (0.0, 0.0);
        }
        let (s12, azi1, _azi2, _a12): (f64, f64, f64, f64) = self.geod.inverse(
            self.lat0.to_degrees(),
            0.0,
            phi.to_degrees(),
            lam.to_degrees(),
        );
        let azi1 = azi1.to_radians();
        (s12 * azi1.sin(), s12 * azi1.cos())
    }

    fn forward_ellipsoidal(&self, lam: f64, phi: f64) -> Result<(f64, f64)> {
        match self.aspect {
            Aspect::NorthPolar | Aspect::SouthPolar => {
                let coslam = if self.aspect == Aspect::NorthPolar {
                    -lam.cos()
                } else {
                    lam.cos()
                };
                let rho = (self.mp - meridian_arc(phi, phi.sin(), phi.cos(), &self.en)).abs();
                Ok((rho * lam.sin(), rho * coslam))
            }
            Aspect::Equatorial | Aspect::Oblique => Ok(self.geodesic_forward(lam, phi)),
        }
    }

    fn forward_spherical(&self, lam: f64, phi: f64) -> Result<(f64, f64)> {
        let (sinphi, cosphi) = (phi.sin(), phi.cos());
        let (sinlam, coslam) = (lam.sin(), lam.cos());
        match self.aspect {
            Aspect::Equatorial | Aspect::Oblique => {
                // Cosine of the angular distance from the centre.
                let cos_c = if self.aspect == Aspect::Equatorial {
                    cosphi * coslam
                } else {
                    self.sin_lat0 * sinphi + self.cos_lat0 * cosphi * coslam
                };
                if (cos_c.abs() - 1.0).abs() < NEAR_UNIT_TOL {
                    if cos_c < 0.0 {
                        return Err(Error::OutOfRange(
                            "Azimuthal Equidistant is undefined at the centre's antipode".into(),
                        ));
                    }
                    // So close to the centre that the closed form loses
                    // precision; the geodesic path is exact there.
                    return Ok(self.geodesic_forward(lam, phi));
                }
                let c = cos_c.acos();
                let k = c / c.sin();
                let x = k * cosphi * sinlam;
                let y = k * if self.aspect == Aspect::Equatorial {
                    sinphi
                } else {
                    self.cos_lat0 * sinphi - self.sin_lat0 * cosphi * coslam
                };
                Ok((x, y))
            }
            Aspect::NorthPolar | Aspect::SouthPolar => {
                let (phi, coslam) = if self.aspect == Aspect::NorthPolar {
                    (-phi, -coslam)
                } else {
                    (phi, coslam)
                };
                if (phi - FRAC_PI_2).abs() < EPS10 {
                    return Err(Error::OutOfRange(
                        "Azimuthal Equidistant is undefined at the centre's antipode".into(),
                    ));
                }
                let rho = FRAC_PI_2 + phi;
                Ok((rho * sinlam, rho * coslam))
            }
        }
    }

    fn inverse_ellipsoidal(&self, xs: f64, ys: f64) -> Result<(f64, f64)> {
        let s12 = xs.hypot(ys);
        if s12 < EPS10 {
            return Ok((0.0, self.lat0));
        }
        match self.aspect {
            Aspect::Equatorial | Aspect::Oblique => {
                let azi1 = xs.atan2(ys).to_degrees();
                let (lat2, lon2): (f64, f64) =
                    self.geod.direct(self.lat0.to_degrees(), 0.0, azi1, s12);
                Ok((lon2.to_radians(), lat2.to_radians()))
            }
            Aspect::NorthPolar => {
                Ok((xs.atan2(-ys), inverse_meridian_arc(self.mp - s12, &self.en)))
            }
            Aspect::SouthPolar => Ok((xs.atan2(ys), inverse_meridian_arc(self.mp + s12, &self.en))),
        }
    }

    fn inverse_spherical(&self, xs: f64, ys: f64) -> Result<(f64, f64)> {
        let mut x = xs;
        let mut y = ys;
        let mut c_rh = x.hypot(y);
        if c_rh > PI {
            if c_rh - EPS10 > PI {
                return Err(Error::OutOfRange(
                    "Azimuthal Equidistant coordinate lies outside the projection domain".into(),
                ));
            }
            c_rh = PI;
        } else if c_rh < EPS10 {
            return Ok((0.0, self.lat0));
        }
        match self.aspect {
            Aspect::Equatorial | Aspect::Oblique => {
                let sinc = c_rh.sin();
                let cosc = c_rh.cos();
                let phi;
                if self.aspect == Aspect::Equatorial {
                    phi = (y * sinc / c_rh).clamp(-1.0, 1.0).asin();
                    x *= sinc;
                    y = cosc * c_rh;
                } else {
                    phi = (cosc * self.sin_lat0 + y * sinc * self.cos_lat0 / c_rh)
                        .clamp(-1.0, 1.0)
                        .asin();
                    y = (cosc - self.sin_lat0 * phi.sin()) * c_rh;
                    x *= sinc * self.cos_lat0;
                }
                let lam = if y == 0.0 { 0.0 } else { x.atan2(y) };
                Ok((lam, phi))
            }
            Aspect::NorthPolar => Ok((x.atan2(-y), FRAC_PI_2 - c_rh)),
            Aspect::SouthPolar => Ok((x.atan2(y), c_rh - FRAC_PI_2)),
        }
    }
}

impl super::ProjectionImpl for AzimuthalEquidistant {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);
        let (x, y) = if self.e2 == 0.0 {
            self.forward_spherical(lam, lat)?
        } else {
            self.forward_ellipsoidal(lam, lat)?
        };
        ensure_finite_xy(
            "Azimuthal Equidistant",
            self.false_easting + self.a * x,
            self.false_northing + self.a * y,
        )
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let xs = (x - self.false_easting) / self.a;
        let ys = (y - self.false_northing) / self.a;
        let (lam, phi) = if self.e2 == 0.0 {
            self.inverse_spherical(xs, ys)?
        } else {
            self.inverse_ellipsoidal(xs, ys)?
        };
        ensure_finite_lon_lat("Azimuthal Equidistant", self.lon0 + lam, phi)
    }
}

/// Guam Projection (EPSG method 9831), matching C PROJ's `aeqd +guam`:
/// a simplified azimuthal equidistant for the island's extent.
#[derive(Clone)]
pub(crate) struct GuamProjection {
    a: f64,
    e2: f64,
    e: f64,
    lon0: f64,
    lat0: f64,
    false_easting: f64,
    false_northing: f64,
    en: [f64; 13],
    /// Meridional arc at the latitude of origin.
    m1: f64,
}

impl GuamProjection {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of natural origin", lon0)?;
        validate_latitude_param("latitude of natural origin", lat0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;

        let e2 = ellipsoid.e2();
        if e2 == 0.0 {
            return Err(Error::InvalidDefinition(
                "the Guam projection is defined only for ellipsoids".into(),
            ));
        }
        let en = meridian_arc_coefficients(e2);
        Ok(Self {
            a: ellipsoid.semi_major_axis(),
            e2,
            e: e2.sqrt(),
            lon0,
            lat0,
            false_easting,
            false_northing,
            en,
            m1: meridian_arc(lat0, lat0.sin(), lat0.cos(), &en),
        })
    }
}

impl super::ProjectionImpl for GuamProjection {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);
        let (sinphi, cosphi) = (lat.sin(), lat.cos());
        let t = 1.0 / (1.0 - self.e2 * sinphi * sinphi).sqrt();
        let x = lam * cosphi * t;
        let y = meridian_arc(lat, sinphi, cosphi, &self.en) - self.m1
            + 0.5 * lam * lam * cosphi * sinphi * t;
        ensure_finite_xy(
            "Guam",
            self.false_easting + self.a * x,
            self.false_northing + self.a * y,
        )
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let xs = (x - self.false_easting) / self.a;
        let ys = (y - self.false_northing) / self.a;
        let x2 = 0.5 * xs * xs;
        let mut phi = self.lat0;
        let mut t = 0.0;
        for _ in 0..GUAM_INVERSE_ITERATIONS {
            t = self.e * phi.sin();
            t = (1.0 - t * t).sqrt();
            phi = inverse_meridian_arc(self.m1 + ys - x2 * phi.tan() * t, &self.en);
        }
        ensure_finite_lon_lat("Guam", self.lon0 + xs * t / phi.cos(), phi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    /// Expectations from C PROJ's gie suite (`+proj=aeqd +ellps=GRS80
    /// +lat_0=0`, Snyder pp. 196-197, 0.1 mm tolerance).
    #[test]
    fn equatorial_matches_c_proj_gie_vectors() {
        let proj = AzimuthalEquidistant::new(ellipsoid::GRS80, 0.0, 0.0, 0.0, 0.0).unwrap();
        let cases: [((f64, f64), (f64, f64)); 4] = [
            ((0.0, 90.0), (0.0, 10001965.7292)),
            ((0.0, 0.0), (0.0, 0.0)),
            ((90.0, 0.0), (10018754.1714, 0.0)),
            ((45.0, 45.0), (3860398.3783, 5430089.0490)),
        ];
        for ((lon, lat), (ex, ey)) in cases {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            assert!((x - ex).abs() < 1e-4, "({lon},{lat}): x = {x} vs {ex}");
            assert!((y - ey).abs() < 1e-4, "({lon},{lat}): y = {y} vs {ey}");
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            // Longitude is indeterminate at the pole itself.
            assert!(
                (lon2.to_degrees() - lon).abs() < 1e-9 || lat == 90.0,
                "({lon},{lat}): lon = {}",
                lon2.to_degrees()
            );
            assert!(
                (lat2.to_degrees() - lat).abs() < 2e-9,
                "({lon},{lat}): lat = {}",
                lat2.to_degrees()
            );
        }
    }

    /// North-polar aspect (`+proj=aeqd +ellps=intl +lat_0=90`, Snyder
    /// p. 198, 0.1 m tolerance).
    #[test]
    fn north_polar_matches_c_proj_gie_vectors() {
        let proj =
            AzimuthalEquidistant::new(ellipsoid::INTL1924, 0.0, 90.0_f64.to_radians(), 0.0, 0.0)
                .unwrap();
        let cases: [(f64, f64); 4] = [
            (90.0, 0.0),
            (85.0, -558_485.4),
            (80.0, -1_116_885.2),
            (70.0, -2_233_100.9),
        ];
        for (lat, ey) in cases {
            let (x, y) = proj.forward(0.0, lat.to_radians()).unwrap();
            assert!(x.abs() < 1e-7, "lat {lat}: x = {x}");
            assert!((y - ey).abs() < 0.1, "lat {lat}: y = {y} vs {ey}");
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            assert!(lon2.abs() < 1e-9);
            assert!(
                (lat2.to_degrees() - lat).abs() < 1e-9,
                "{}",
                lat2.to_degrees()
            );
        }
    }

    /// The EPSG Guidance Note 7-2 worked example for the Modified Azimuthal
    /// Equidistant (EPSG:3295, Yap Islands), which C PROJ's gie suite pins
    /// on plain `aeqd` at 1 cm.
    #[test]
    fn yap_islands_matches_epsg_worked_example() {
        let proj = AzimuthalEquidistant::new(
            ellipsoid::CLARKE1866,
            138.168_744_450_049_2_f64.to_radians(),
            9.546_708_325_068_591_f64.to_radians(),
            40_000.0,
            60_000.0,
        )
        .unwrap();
        let (lon, lat): (f64, f64) = (138.193_030_011_040_92, 9.596_525_859_439_623);
        let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
        assert!((x - 42_665.90).abs() < 1e-2, "x = {x}");
        assert!((y - 65_509.82).abs() < 1e-2, "y = {y}");
        let (lon2, lat2) = proj.inverse(42_665.90, 65_509.82).unwrap();
        assert!((lon2.to_degrees() - lon).abs() < 1e-7);
        assert!((lat2.to_degrees() - lat).abs() < 1e-7);
    }

    /// Spherical unit-sphere vectors from C PROJ's gie suite
    /// (`+proj=aeqd +R=1 +lat_0=0`).
    #[test]
    #[allow(clippy::approx_constant)] // 1.57080 is gie's published rounding of π/2
    fn spherical_matches_c_proj_gie_vectors() {
        let proj =
            AzimuthalEquidistant::new(Ellipsoid::sphere(1.0).unwrap(), 0.0, 0.0, 0.0, 0.0).unwrap();
        let cases: [((f64, f64), (f64, f64)); 4] = [
            ((0.0, 90.0), (0.0, 1.57080)),
            ((10.0, 80.0), (0.04281, 1.39829)),
            ((40.0, 30.0), (0.62896, 0.56493)),
            ((90.0, 0.0), (1.57080, 0.0)),
        ];
        for ((lon, lat), (ex, ey)) in cases {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            assert!((x - ex).abs() < 2e-5, "({lon},{lat}): x = {x} vs {ex}");
            assert!((y - ey).abs() < 2e-5, "({lon},{lat}): y = {y} vs {ey}");
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            let dlon = (lon2.to_degrees() - lon).abs();
            assert!(dlon < 1e-9 || lat == 90.0, "lon = {}", lon2.to_degrees());
            assert!((lat2.to_degrees() - lat).abs() < 1e-9);
        }
        // The centre's antipode is outside the projection domain.
        assert!(proj.forward(PI, 0.0).is_err());
    }

    /// Near-centre points delegate to the geodesic path (C PROJ gie: sphere
    /// and near-sphere at lat_0=30.2345, 1 mm tolerance).
    #[test]
    fn near_center_stays_precise_on_sphere_and_near_sphere() {
        for (a, b) in [
            (6_371_008.771_415, 6_371_008.771_415),
            (6_371_008.771_415, 6_371_008.771_414),
        ] {
            let ellipsoid = if a == b {
                Ellipsoid::sphere(a).unwrap()
            } else {
                Ellipsoid::from_a_rf(a, a / (a - b)).unwrap()
            };
            let proj = AzimuthalEquidistant::new(
                ellipsoid,
                (-120.2345_f64).to_radians(),
                30.2345_f64.to_radians(),
                0.0,
                0.0,
            )
            .unwrap();
            let (x, y) = proj
                .forward((-120.234_501_f64).to_radians(), 30.234_501_f64.to_radians())
                .unwrap();
            assert!((x - -0.096).abs() < 1e-3, "a={a}: x = {x}");
            assert!((y - 0.111).abs() < 1e-3, "a={a}: y = {y}");
            let (x, y) = proj
                .forward((-120.2345_f64).to_radians(), 30.2345_f64.to_radians())
                .unwrap();
            assert!(x.abs() < 1e-3 && y.abs() < 1e-3);
        }
    }

    /// The EPSG Guidance Note 7-2 worked example for the Guam projection
    /// (EPSG:3993), pinned by C PROJ's gie suite at 1 cm.
    #[test]
    fn guam_matches_epsg_worked_example() {
        let proj = GuamProjection::new(
            ellipsoid::CLARKE1866,
            144.748_750_694_444_45_f64.to_radians(),
            13.472_466_333_333_33_f64.to_radians(),
            50_000.0,
            50_000.0,
        )
        .unwrap();
        let (lon, lat): (f64, f64) = (144.635_331_291_666_66, 13.339_038_461_111_11);
        let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
        assert!((x - 37_712.48).abs() < 1e-2, "x = {x}");
        assert!((y - 35_242.00).abs() < 1e-2, "y = {y}");
        let (lon2, lat2) = proj.inverse(37_712.48, 35_242.00).unwrap();
        assert!((lon2.to_degrees() - lon).abs() < 1e-7);
        assert!((lat2.to_degrees() - lat).abs() < 1e-7);
    }

    #[test]
    fn guam_requires_an_ellipsoid() {
        assert!(
            GuamProjection::new(Ellipsoid::sphere(6_378_137.0).unwrap(), 0.0, 0.0, 0.0, 0.0)
                .is_err()
        );
    }

    /// Oblique ellipsoidal roundtrip with the Equi7 Africa parameters
    /// (EPSG:27701).
    #[test]
    fn roundtrip_equi7_africa() {
        let proj = AzimuthalEquidistant::new(
            ellipsoid::WGS84,
            21.5_f64.to_radians(),
            8.5_f64.to_radians(),
            5_621_452.02,
            5_990_638.423,
        )
        .unwrap();
        for (lon, lat) in [
            (21.5_f64, 8.5_f64),
            (3.0, 36.8),
            (31.2, -29.9),
            (-17.5, 14.7),
            (51.3, 11.8),
        ] {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            assert!(
                (lon2.to_degrees() - lon).abs() < 1e-8,
                "lon {lon}: {}",
                lon2.to_degrees()
            );
            assert!(
                (lat2.to_degrees() - lat).abs() < 1e-8,
                "lat {lat}: {}",
                lat2.to_degrees()
            );
        }
    }
}

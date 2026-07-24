use crate::ellipsoid::Ellipsoid;
use crate::error::{Error, Result};
use crate::projection::{
    ensure_finite_lon_lat, ensure_finite_xy, normalize_longitude, validate_angle,
    validate_latitude_param, validate_lon_lat, validate_offset, validate_projected, validate_scale,
};
use std::f64::consts::FRAC_PI_4;

const INVERSE_TOL: f64 = 1e-10;
const INVERSE_ITERATIONS: usize = 20;

/// Laborde Oblique Mercator (EPSG method 9813), matching C PROJ's `labrd`:
/// the Madagascar grid's conformal sphere construction with a cubic
/// rotation correction for the grid azimuth.
#[derive(Clone)]
pub(crate) struct Laborde {
    a: f64,
    e: f64,
    one_es: f64,
    lon0: f64,
    lat0: f64,
    k0: f64,
    false_easting: f64,
    false_northing: f64,
    /// k0·√(N·R): the scaled Gaussian sphere radius.
    k_rg: f64,
    /// Latitude of the origin on the conformal sphere.
    p0s: f64,
    /// Conformal sphere exponent sin φ0 / sin ψ0.
    a_const: f64,
    /// Integration constant of the conformal mapping.
    c_const: f64,
    ca: f64,
    cb: f64,
    cc: f64,
    cd: f64,
}

impl Laborde {
    pub(crate) fn new(
        ellipsoid: Ellipsoid,
        lon0: f64,
        lat0: f64,
        azimuth: f64,
        k0: f64,
        false_easting: f64,
        false_northing: f64,
    ) -> Result<Self> {
        validate_angle("longitude of projection centre", lon0)?;
        validate_latitude_param("latitude of projection centre", lat0)?;
        validate_angle("azimuth at projection centre", azimuth)?;
        validate_scale("scale factor at projection centre", k0)?;
        validate_offset("false easting", false_easting)?;
        validate_offset("false northing", false_northing)?;
        if lat0 == 0.0 {
            return Err(Error::InvalidDefinition(
                "Laborde requires a non-equatorial latitude of projection centre".into(),
            ));
        }

        let e2 = ellipsoid.e2();
        let e = e2.sqrt();
        let one_es = 1.0 - e2;
        let sinp = lat0.sin();
        let t = 1.0 - e2 * sinp * sinp;
        let n = 1.0 / t.sqrt();
        let r = one_es * n / t;
        let k_rg = k0 * (n * r).sqrt();
        let p0s = ((r / n).sqrt() * lat0.tan()).atan();
        let a_const = sinp / p0s.sin();
        let es = e * sinp;
        let c_const = 0.5 * e * a_const * ((1.0 + es) / (1.0 - es)).ln()
            - a_const * (FRAC_PI_4 + 0.5 * lat0).tan().ln()
            + (FRAC_PI_4 + 0.5 * p0s).tan().ln();

        let two_az = azimuth + azimuth;
        let cb0 = 1.0 / (12.0 * k_rg * k_rg);
        let ca = (1.0 - two_az.cos()) * cb0;
        let cb = two_az.sin() * cb0;

        Ok(Self {
            a: ellipsoid.semi_major_axis(),
            e,
            one_es,
            lon0,
            lat0,
            k0,
            false_easting,
            false_northing,
            k_rg,
            p0s,
            a_const,
            c_const,
            ca,
            cb,
            cc: 3.0 * (ca * ca - cb * cb),
            cd: 6.0 * ca * cb,
        })
    }

    /// Conformal-sphere latitude ψ for geodetic latitude φ.
    fn conformal_sphere_lat(&self, phi: f64) -> f64 {
        let v1 = self.a_const * (FRAC_PI_4 + 0.5 * phi).tan().ln();
        let t = self.e * phi.sin();
        let v2 = 0.5 * self.e * self.a_const * ((1.0 + t) / (1.0 - t)).ln();
        2.0 * ((v1 - v2 + self.c_const).exp().atan() - FRAC_PI_4)
    }
}

impl super::ProjectionImpl for Laborde {
    fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        validate_lon_lat(lon, lat)?;
        let lam = normalize_longitude(lon - self.lon0);

        let ps = self.conformal_sphere_lat(lat);
        let i1 = ps - self.p0s;
        let cosps = ps.cos();
        let cosps2 = cosps * cosps;
        let sinps = ps.sin();
        let sinps2 = sinps * sinps;
        let a2 = self.a_const * self.a_const;
        let i4 = self.a_const * cosps;
        let i2 = 0.5 * self.a_const * i4 * sinps;
        let i3 = i2 * a2 * (5.0 * cosps2 - sinps2) / 12.0;
        let mut i6 = i4 * a2;
        let i5 = i6 * (cosps2 - sinps2) / 6.0;
        i6 *= a2 * (5.0 * cosps2 * cosps2 + sinps2 * (sinps2 - 18.0 * cosps2)) / 120.0;

        let t = lam * lam;
        let x = self.k_rg * lam * (i4 + t * (i5 + t * i6));
        let y = self.k_rg * (i1 + t * (i2 + t * i3));
        let x2 = x * x;
        let y2 = y * y;
        let v1 = 3.0 * x * y2 - x * x2;
        let v2 = y * y2 - 3.0 * x2 * y;
        let x = x + self.ca * v1 + self.cb * v2;
        let y = y + self.ca * v2 - self.cb * v1;

        ensure_finite_xy(
            "Laborde",
            self.false_easting + self.a * x,
            self.false_northing + self.a * y,
        )
    }

    fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        validate_projected(x, y)?;
        let mut xs = (x - self.false_easting) / self.a;
        let mut ys = (y - self.false_northing) / self.a;

        let x2 = xs * xs;
        let y2 = ys * ys;
        let v1 = 3.0 * xs * y2 - xs * x2;
        let v2 = ys * y2 - 3.0 * x2 * ys;
        let v3 = xs * (5.0 * y2 * y2 + x2 * (-10.0 * y2 + x2));
        let v4 = ys * (5.0 * x2 * x2 + y2 * (-10.0 * x2 + y2));
        xs += -self.ca * v1 - self.cb * v2 + self.cc * v3 + self.cd * v4;
        ys += self.cb * v1 - self.ca * v2 - self.cd * v3 + self.cc * v4;

        let ps = self.p0s + ys / self.k_rg;
        let mut pe = ps + self.lat0 - self.p0s;
        let mut converged = false;
        for _ in 0..INVERSE_ITERATIONS {
            let t = ps - self.conformal_sphere_lat(pe);
            pe += t;
            if t.abs() < INVERSE_TOL {
                converged = true;
                break;
            }
        }
        if !converged {
            return Err(Error::NonConvergence {
                context: "Laborde inverse latitude",
                iterations: INVERSE_ITERATIONS,
            });
        }

        let t = self.e * pe.sin();
        let t = 1.0 - t * t;
        let re = self.one_es / (t * t.sqrt());
        let tanps = ps.tan();
        let t2 = tanps * tanps;
        let s = self.k_rg * self.k_rg;
        let d = re * self.k0 * self.k_rg;
        let i7 = tanps / (2.0 * d);
        let i8 = tanps * (5.0 + 3.0 * t2) / (24.0 * d * s);
        let d = ps.cos() * self.k_rg * self.a_const;
        let i9 = 1.0 / d;
        let d = d * s;
        let i10 = (1.0 + 2.0 * t2) / (6.0 * d);
        let i11 = (5.0 + t2 * (28.0 + 24.0 * t2)) / (120.0 * d * s);

        let x2 = xs * xs;
        let phi = pe + x2 * (-i7 + i8 * x2);
        let lam = xs * (i9 + x2 * (-i10 + x2 * i11));
        ensure_finite_lon_lat("Laborde", self.lon0 + lam, phi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid;
    use crate::projection::ProjectionImpl;

    /// Expectations from C PROJ's gie suite
    /// (`+proj=labrd +ellps=GRS80 +lon_0=0.5 +lat_0=2`, 0.1 mm tolerance).
    #[test]
    fn matches_c_proj_gie_vectors() {
        let proj = Laborde::new(
            ellipsoid::GRS80,
            0.5_f64.to_radians(),
            2.0_f64.to_radians(),
            0.0,
            1.0,
            0.0,
            0.0,
        )
        .unwrap();
        let cases: [((f64, f64), (f64, f64)); 4] = [
            ((2.0, 1.0), (166973.166090228, -110536.912730266)),
            ((2.0, -1.0), (166973.168287157, -331761.993650884)),
            ((-2.0, 1.0), (-278345.500519976, -110469.032642032)),
            ((-2.0, -1.0), (-278345.504185270, -331829.870790275)),
        ];
        for ((lon, lat), (ex, ey)) in cases {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            assert!((x - ex).abs() < 1e-4, "({lon},{lat}): x = {x} vs {ex}");
            assert!((y - ey).abs() < 1e-4, "({lon},{lat}): y = {y} vs {ey}");
        }
    }

    #[test]
    fn inverse_matches_c_proj_gie_vectors() {
        let proj = Laborde::new(
            ellipsoid::GRS80,
            0.5_f64.to_radians(),
            2.0_f64.to_radians(),
            0.0,
            1.0,
            0.0,
            0.0,
        )
        .unwrap();
        let cases: [((f64, f64), (f64, f64)); 4] = [
            ((200.0, 100.0), (0.501797719, 2.000904357)),
            ((200.0, -100.0), (0.501797717, 1.999095641)),
            ((-200.0, 100.0), (0.498202281, 2.000904357)),
            ((-200.0, -100.0), (0.498202283, 1.999095641)),
        ];
        for ((x, y), (elon, elat)) in cases {
            let (lon, lat) = proj.inverse(x, y).unwrap();
            assert!(
                (lon.to_degrees() - elon).abs() < 1e-9,
                "lon = {}",
                lon.to_degrees()
            );
            assert!(
                (lat.to_degrees() - elat).abs() < 1e-9,
                "lat = {}",
                lat.to_degrees()
            );
        }
    }

    /// Madagascar parameters (EPSG:8441, Tananarive / Laborde Grid):
    /// lat0 = 18°54'S, azimuth = 18°54', k0 = 0.9995.
    #[test]
    fn roundtrip_madagascar() {
        let proj = Laborde::new(
            ellipsoid::INTL1924,
            46.437_229_166_666_67_f64.to_radians(),
            (-18.9_f64).to_radians(),
            18.9_f64.to_radians(),
            0.9995,
            400_000.0,
            800_000.0,
        )
        .unwrap();
        for (lon, lat) in [
            (47.5_f64, -18.9_f64),
            (44.5, -16.2),
            (48.8, -22.3),
            (46.43722916666667, -18.9),
        ] {
            let (x, y) = proj.forward(lon.to_radians(), lat.to_radians()).unwrap();
            let (lon2, lat2) = proj.inverse(x, y).unwrap();
            // The inverse uses C PROJ's truncated polynomial series, which
            // bounds far-field roundtrips near 1e-8° over Madagascar.
            assert!(
                (lon2.to_degrees() - lon).abs() < 1e-7,
                "lon {lon}: {}",
                lon2.to_degrees()
            );
            assert!(
                (lat2.to_degrees() - lat).abs() < 1e-7,
                "lat {lat}: {}",
                lat2.to_degrees()
            );
        }
        // The projection centre maps to the false origin.
        let (x, y) = proj
            .forward(
                46.437_229_166_666_67_f64.to_radians(),
                (-18.9_f64).to_radians(),
            )
            .unwrap();
        assert!((x - 400_000.0).abs() < 1e-6, "x = {x}");
        assert!((y - 800_000.0).abs() < 1e-6, "y = {y}");
    }

    #[test]
    fn rejects_equatorial_centre() {
        assert!(Laborde::new(ellipsoid::GRS80, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0).is_err());
    }
}

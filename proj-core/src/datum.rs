use crate::ellipsoid::{self, Ellipsoid};

/// A geodetic datum, defined by a reference ellipsoid and its relationship to WGS84.
#[derive(Debug, Clone, Copy)]
pub struct Datum {
    /// The reference ellipsoid.
    pub ellipsoid: Ellipsoid,
    /// 7-parameter Helmert transformation from this datum to WGS84.
    /// `None` means this datum is WGS84 (or is functionally identical, e.g. NAD83).
    pub to_wgs84: Option<HelmertParams>,
}

impl Datum {
    /// Returns true if this datum is WGS84 or functionally identical (no Helmert shift needed).
    pub fn is_wgs84_compatible(&self) -> bool {
        self.to_wgs84.is_none()
    }

    /// Returns true if two datums are the same (same ellipsoid, same Helmert parameters).
    pub fn same_datum(&self, other: &Datum) -> bool {
        // If both are WGS84-compatible with the same ellipsoid, they're the same.
        // If both have Helmert params, compare them.
        let same_ellipsoid = (self.ellipsoid.a - other.ellipsoid.a).abs() < 1e-6
            && (self.ellipsoid.f - other.ellipsoid.f).abs() < 1e-12;

        match (&self.to_wgs84, &other.to_wgs84) {
            (None, None) => same_ellipsoid,
            (Some(a), Some(b)) => same_ellipsoid && a.approx_eq(b),
            _ => false,
        }
    }
}

/// 7-parameter Helmert (Bursa-Wolf) transformation parameters.
///
/// Defines the transformation from one datum to WGS84 geocentric coordinates:
/// ```text
/// [X']   [dx]         [  1  -rz   ry] [X]
/// [Y'] = [dy] + (1+ds)[  rz   1  -rx] [Y]
/// [Z']   [dz]         [ -ry  rx   1 ] [Z]
/// ```
#[derive(Debug, Clone, Copy)]
pub struct HelmertParams {
    /// X-axis translation in meters.
    pub dx: f64,
    /// Y-axis translation in meters.
    pub dy: f64,
    /// Z-axis translation in meters.
    pub dz: f64,
    /// X-axis rotation in arc-seconds.
    pub rx: f64,
    /// Y-axis rotation in arc-seconds.
    pub ry: f64,
    /// Z-axis rotation in arc-seconds.
    pub rz: f64,
    /// Scale difference in parts-per-million (ppm).
    pub ds: f64,
}

impl HelmertParams {
    /// Create a translation-only (3-parameter) transformation.
    pub const fn translation(dx: f64, dy: f64, dz: f64) -> Self {
        Self {
            dx,
            dy,
            dz,
            rx: 0.0,
            ry: 0.0,
            rz: 0.0,
            ds: 0.0,
        }
    }

    /// Return the inverse parameters (WGS84 → this datum).
    pub fn inverse(&self) -> Self {
        Self {
            dx: -self.dx,
            dy: -self.dy,
            dz: -self.dz,
            rx: -self.rx,
            ry: -self.ry,
            rz: -self.rz,
            ds: -self.ds,
        }
    }

    fn approx_eq(&self, other: &Self) -> bool {
        (self.dx - other.dx).abs() < 1e-6
            && (self.dy - other.dy).abs() < 1e-6
            && (self.dz - other.dz).abs() < 1e-6
            && (self.rx - other.rx).abs() < 1e-9
            && (self.ry - other.ry).abs() < 1e-9
            && (self.rz - other.rz).abs() < 1e-9
            && (self.ds - other.ds).abs() < 1e-9
    }
}

// ---------------------------------------------------------------------------
// Well-known datums
// ---------------------------------------------------------------------------

/// WGS 84 datum.
pub const WGS84: Datum = Datum {
    ellipsoid: ellipsoid::WGS84,
    to_wgs84: None,
};

/// NAD83 datum (functionally identical to WGS84 for sub-meter work).
pub const NAD83: Datum = Datum {
    ellipsoid: ellipsoid::GRS80,
    to_wgs84: None,
};

/// NAD27 datum (Clarke 1866 ellipsoid).
/// Helmert parameters from EPSG dataset (approximate continental US average).
pub const NAD27: Datum = Datum {
    ellipsoid: ellipsoid::CLARKE1866,
    to_wgs84: Some(HelmertParams::translation(-8.0, 160.0, 176.0)),
};

/// ETRS89 datum (European Terrestrial Reference System 1989).
/// Functionally identical to WGS84 for most purposes.
pub const ETRS89: Datum = Datum {
    ellipsoid: ellipsoid::GRS80,
    to_wgs84: None,
};

/// OSGB36 datum (Ordnance Survey Great Britain 1936).
pub const OSGB36: Datum = Datum {
    ellipsoid: ellipsoid::AIRY1830,
    to_wgs84: Some(HelmertParams {
        dx: 446.448,
        dy: -125.157,
        dz: 542.060,
        rx: 0.1502,
        ry: 0.2470,
        rz: 0.8421,
        ds: -20.4894,
    }),
};

/// Pulkovo 1942 datum (used in Russia and former Soviet states).
pub const PULKOVO1942: Datum = Datum {
    ellipsoid: ellipsoid::KRASSOWSKY,
    to_wgs84: Some(HelmertParams::translation(23.92, -141.27, -80.9)),
};

/// ED50 datum (European Datum 1950).
pub const ED50: Datum = Datum {
    ellipsoid: ellipsoid::INTL1924,
    to_wgs84: Some(HelmertParams::translation(-87.0, -98.0, -121.0)),
};

/// Tokyo datum (used in Japan).
pub const TOKYO: Datum = Datum {
    ellipsoid: ellipsoid::BESSEL1841,
    to_wgs84: Some(HelmertParams::translation(-146.414, 507.337, 680.507)),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgs84_is_wgs84_compatible() {
        assert!(WGS84.is_wgs84_compatible());
        assert!(NAD83.is_wgs84_compatible());
        assert!(ETRS89.is_wgs84_compatible());
    }

    #[test]
    fn nad27_is_not_wgs84_compatible() {
        assert!(!NAD27.is_wgs84_compatible());
        assert!(!OSGB36.is_wgs84_compatible());
    }

    #[test]
    fn same_datum_identity() {
        assert!(WGS84.same_datum(&WGS84));
        assert!(NAD27.same_datum(&NAD27));
    }

    #[test]
    fn different_datums() {
        assert!(!WGS84.same_datum(&NAD27));
        assert!(!NAD27.same_datum(&OSGB36));
    }

    #[test]
    fn helmert_inverse_negates() {
        let h = HelmertParams {
            dx: 1.0,
            dy: 2.0,
            dz: 3.0,
            rx: 0.1,
            ry: 0.2,
            rz: 0.3,
            ds: 0.5,
        };
        let inv = h.inverse();
        assert_eq!(inv.dx, -1.0);
        assert_eq!(inv.rx, -0.1);
        assert_eq!(inv.ds, -0.5);
    }
}

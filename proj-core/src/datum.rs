use crate::ellipsoid::{self, Ellipsoid};
use crate::error::{Error, Result};
use crate::grid::GridDefinition;
use smallvec::SmallVec;

/// A geodetic datum, defined by a reference ellipsoid and its relationship to WGS84.
#[derive(Debug, Clone)]
pub struct Datum {
    /// The reference ellipsoid.
    ellipsoid: Ellipsoid,
    /// Explicit relationship from this datum to WGS84.
    to_wgs84: DatumToWgs84,
}

impl Datum {
    /// Create a datum from an ellipsoid and an explicit path to WGS84.
    pub fn new(ellipsoid: Ellipsoid, to_wgs84: DatumToWgs84) -> Result<Self> {
        to_wgs84.validate()?;
        Ok(Self {
            ellipsoid,
            to_wgs84,
        })
    }

    const fn new_unchecked(ellipsoid: Ellipsoid, to_wgs84: DatumToWgs84) -> Self {
        Self {
            ellipsoid,
            to_wgs84,
        }
    }

    /// Return the reference ellipsoid.
    pub const fn ellipsoid(&self) -> Ellipsoid {
        self.ellipsoid
    }

    /// Return the explicit relationship from this datum to WGS84.
    pub const fn to_wgs84(&self) -> &DatumToWgs84 {
        &self.to_wgs84
    }

    /// Returns true if this datum is WGS84 or functionally identical (no Helmert shift needed).
    pub fn is_wgs84_compatible(&self) -> bool {
        matches!(self.to_wgs84, DatumToWgs84::Identity)
    }

    /// Returns true if this datum has a known path to WGS84.
    pub fn has_known_wgs84_transform(&self) -> bool {
        !matches!(self.to_wgs84, DatumToWgs84::Unknown)
    }

    /// Returns true if this datum's WGS84 path uses one or more horizontal grids.
    pub fn uses_grid_shift(&self) -> bool {
        self.to_wgs84.uses_grid_shift()
    }

    /// Return the Helmert parameters for this datum's path to WGS84, when available.
    pub fn helmert_to_wgs84(&self) -> Option<&HelmertParams> {
        match &self.to_wgs84 {
            DatumToWgs84::Helmert(params) => Some(params),
            DatumToWgs84::Identity | DatumToWgs84::GridShift(_) | DatumToWgs84::Unknown => None,
        }
    }

    /// Returns true if two datums are the same (same ellipsoid, same Helmert parameters).
    pub fn same_datum(&self, other: &Datum) -> bool {
        let same_ellipsoid =
            (self.ellipsoid.semi_major_axis() - other.ellipsoid.semi_major_axis()).abs() < 1e-6
                && (self.ellipsoid.flattening() - other.ellipsoid.flattening()).abs() < 1e-12;

        match (&self.to_wgs84, &other.to_wgs84) {
            (DatumToWgs84::Identity, DatumToWgs84::Identity) => same_ellipsoid,
            (DatumToWgs84::Helmert(a), DatumToWgs84::Helmert(b)) => {
                same_ellipsoid && a.approx_eq(b)
            }
            (DatumToWgs84::GridShift(a), DatumToWgs84::GridShift(b)) => same_ellipsoid && a == b,
            (DatumToWgs84::Unknown, DatumToWgs84::Unknown) => false,
            _ => false,
        }
    }
}

/// WGS84 relationship metadata for a datum definition.
///
/// Operation selection treats this as CRS definition metadata. Registry
/// operations or explicit custom operations are the authority for transforms.
#[derive(Debug, Clone, PartialEq)]
pub enum DatumToWgs84 {
    /// The datum can be treated as WGS84-compatible in the current model.
    Identity,
    /// The datum requires the provided Helmert transform to reach WGS84.
    Helmert(HelmertParams),
    /// The datum requires horizontal grid interpolation to reach WGS84.
    GridShift(Box<DatumGridShift>),
    /// The datum's path to WGS84 is not known.
    Unknown,
}

impl DatumToWgs84 {
    pub fn uses_grid_shift(&self) -> bool {
        matches!(self, DatumToWgs84::GridShift(shift) if shift.uses_grid_shift())
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Helmert(params) => params.validate(),
            Self::Identity | Self::GridShift(_) | Self::Unknown => Ok(()),
        }
    }
}

/// Ordered PROJ-style datum grid list.
#[derive(Debug, Clone, PartialEq)]
pub struct DatumGridShift {
    entries: SmallVec<[DatumGridShiftEntry; 4]>,
}

impl DatumGridShift {
    pub fn new(entries: SmallVec<[DatumGridShiftEntry; 4]>) -> Self {
        Self { entries }
    }

    pub fn from_vec(entries: Vec<DatumGridShiftEntry>) -> Self {
        Self {
            entries: SmallVec::from_vec(entries),
        }
    }

    pub fn entries(&self) -> &[DatumGridShiftEntry] {
        &self.entries
    }

    pub fn uses_grid_shift(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, DatumGridShiftEntry::Grid { .. }))
    }
}

/// One entry from a datum grid list.
#[derive(Debug, Clone, PartialEq)]
pub enum DatumGridShiftEntry {
    /// Try this horizontal grid. Optional grids may be missing from providers.
    Grid {
        definition: GridDefinition,
        optional: bool,
    },
    /// PROJ's `null` grid: stop grid lookup and apply no shift.
    Null,
}

/// 7-parameter Helmert (Bursa-Wolf) transformation parameters.
///
/// Defines the transformation from one datum to WGS84 geocentric coordinates:
/// ```text
/// [X']   [dx]         [  1  -rz   ry] [X]
/// [Y'] = [dy] + (1+ds)[  rz   1  -rx] [Y]
/// [Z']   [dz]         [ -ry  rx   1 ] [Z]
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HelmertParams {
    /// X-axis translation in meters.
    dx: f64,
    /// Y-axis translation in meters.
    dy: f64,
    /// Z-axis translation in meters.
    dz: f64,
    /// X-axis rotation in arc-seconds.
    rx: f64,
    /// Y-axis rotation in arc-seconds.
    ry: f64,
    /// Z-axis rotation in arc-seconds.
    rz: f64,
    /// Scale difference in parts-per-million (ppm).
    ds: f64,
}

impl HelmertParams {
    /// Create a 7-parameter transformation.
    pub fn new(dx: f64, dy: f64, dz: f64, rx: f64, ry: f64, rz: f64, ds: f64) -> Result<Self> {
        let params = Self::new_unchecked(dx, dy, dz, rx, ry, rz, ds);
        params.validate()?;
        Ok(params)
    }

    /// Create a translation-only (3-parameter) transformation.
    pub fn translation(dx: f64, dy: f64, dz: f64) -> Result<Self> {
        Self::new(dx, dy, dz, 0.0, 0.0, 0.0, 0.0)
    }

    /// Identity transformation.
    pub const fn identity() -> Self {
        Self::new_unchecked(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    }

    const fn translation_unchecked(dx: f64, dy: f64, dz: f64) -> Self {
        Self::new_unchecked(dx, dy, dz, 0.0, 0.0, 0.0, 0.0)
    }

    const fn new_unchecked(dx: f64, dy: f64, dz: f64, rx: f64, ry: f64, rz: f64, ds: f64) -> Self {
        Self {
            dx,
            dy,
            dz,
            rx,
            ry,
            rz,
            ds,
        }
    }

    pub const fn dx(&self) -> f64 {
        self.dx
    }

    pub const fn dy(&self) -> f64 {
        self.dy
    }

    pub const fn dz(&self) -> f64 {
        self.dz
    }

    pub const fn rx(&self) -> f64 {
        self.rx
    }

    pub const fn ry(&self) -> f64 {
        self.ry
    }

    pub const fn rz(&self) -> f64 {
        self.rz
    }

    pub const fn ds(&self) -> f64 {
        self.ds
    }

    pub fn validate(&self) -> Result<()> {
        if self.dx.is_finite()
            && self.dy.is_finite()
            && self.dz.is_finite()
            && self.rx.is_finite()
            && self.ry.is_finite()
            && self.rz.is_finite()
            && self.ds.is_finite()
        {
            return Ok(());
        }

        Err(Error::InvalidDefinition(
            "Helmert parameters must be finite".into(),
        ))
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

    pub fn compose_approx(&self, next: &Self) -> Result<Self> {
        Self::new(
            self.dx + next.dx,
            self.dy + next.dy,
            self.dz + next.dz,
            self.rx + next.rx,
            self.ry + next.ry,
            self.rz + next.rz,
            self.ds + next.ds,
        )
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
pub const WGS84: Datum = Datum::new_unchecked(ellipsoid::WGS84, DatumToWgs84::Identity);

/// NAD83 datum (functionally identical to WGS84 for sub-meter work).
pub const NAD83: Datum = Datum::new_unchecked(ellipsoid::GRS80, DatumToWgs84::Identity);

/// NAD27 datum (Clarke 1866 ellipsoid).
/// Helmert parameters from EPSG dataset (approximate continental US average).
pub const NAD27: Datum = Datum::new_unchecked(
    ellipsoid::CLARKE1866,
    DatumToWgs84::Helmert(HelmertParams::translation_unchecked(-8.0, 160.0, 176.0)),
);

/// ETRS89 datum (European Terrestrial Reference System 1989).
/// Functionally identical to WGS84 for most purposes.
pub const ETRS89: Datum = Datum::new_unchecked(ellipsoid::GRS80, DatumToWgs84::Identity);

/// OSGB36 datum (Ordnance Survey Great Britain 1936).
pub const OSGB36: Datum = Datum::new_unchecked(
    ellipsoid::AIRY1830,
    DatumToWgs84::Helmert(HelmertParams::new_unchecked(
        446.448, -125.157, 542.060, 0.1502, 0.2470, 0.8421, -20.4894,
    )),
);

/// Pulkovo 1942 datum (used in Russia and former Soviet states).
pub const PULKOVO1942: Datum = Datum::new_unchecked(
    ellipsoid::KRASSOWSKY,
    DatumToWgs84::Helmert(HelmertParams::translation_unchecked(23.92, -141.27, -80.9)),
);

/// ED50 datum (European Datum 1950).
pub const ED50: Datum = Datum::new_unchecked(
    ellipsoid::INTL1924,
    DatumToWgs84::Helmert(HelmertParams::translation_unchecked(-87.0, -98.0, -121.0)),
);

/// Tokyo datum (used in Japan).
pub const TOKYO: Datum = Datum::new_unchecked(
    ellipsoid::BESSEL1841,
    DatumToWgs84::Helmert(HelmertParams::translation_unchecked(
        -146.414, 507.337, 680.507,
    )),
);

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
    fn unknown_datums_are_not_collapsed_by_ellipsoid() {
        let a = Datum::new(ellipsoid::WGS84, DatumToWgs84::Unknown).unwrap();
        let b = Datum::new(ellipsoid::WGS84, DatumToWgs84::Unknown).unwrap();

        assert!(!a.same_datum(&b));
    }

    #[test]
    fn helmert_inverse_negates() {
        let h = HelmertParams::new(1.0, 2.0, 3.0, 0.1, 0.2, 0.3, 0.5).unwrap();
        let inv = h.inverse();
        assert_eq!(inv.dx(), -1.0);
        assert_eq!(inv.rx(), -0.1);
        assert_eq!(inv.ds(), -0.5);
    }

    #[test]
    fn helmert_params_reject_non_finite_values() {
        let err = HelmertParams::new(f64::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0).unwrap_err();
        assert!(matches!(err, Error::InvalidDefinition(_)), "got {err}");

        let err = HelmertParams::new(0.0, 0.0, 0.0, 0.0, 0.0, f64::INFINITY, 0.0).unwrap_err();
        assert!(err.to_string().contains("Helmert parameters"), "{err}");
    }
}

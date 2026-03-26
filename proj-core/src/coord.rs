/// A 2D coordinate.
///
/// At the public API boundary, units match the CRS:
/// - **Geographic CRS**: degrees (x = longitude, y = latitude)
/// - **Projected CRS**: meters (x = easting, y = northing)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord {
    pub x: f64,
    pub y: f64,
}

impl Coord {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// A 3D coordinate.
///
/// At the public API boundary:
/// - **Geographic CRS**: x/y are longitude/latitude in degrees
/// - **Projected CRS**: x/y are easting/northing in meters
/// - `z` is preserved unchanged by the current horizontal-only transform pipeline
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Coord3D {
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
}

/// A 2D axis-aligned bounding box in CRS-native units.
///
/// At the public API boundary, units match the CRS:
/// - **Geographic CRS**: degrees
/// - **Projected CRS**: meters
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bounds {
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.min_x.is_finite()
            && self.min_y.is_finite()
            && self.max_x.is_finite()
            && self.max_y.is_finite()
            && self.min_x <= self.max_x
            && self.min_y <= self.max_y
    }

    pub(crate) fn expand_to_include(&mut self, coord: Coord) {
        self.min_x = self.min_x.min(coord.x);
        self.min_y = self.min_y.min(coord.y);
        self.max_x = self.max_x.max(coord.x);
        self.max_y = self.max_y.max(coord.y);
    }
}

impl From<(f64, f64)> for Coord {
    fn from((x, y): (f64, f64)) -> Self {
        Self { x, y }
    }
}

impl From<Coord> for (f64, f64) {
    fn from(c: Coord) -> Self {
        (c.x, c.y)
    }
}

impl From<(f64, f64, f64)> for Coord3D {
    fn from((x, y, z): (f64, f64, f64)) -> Self {
        Self { x, y, z }
    }
}

impl From<Coord3D> for (f64, f64, f64) {
    fn from(c: Coord3D) -> Self {
        (c.x, c.y, c.z)
    }
}

#[cfg(feature = "geo-types")]
impl From<geo_types::Coord<f64>> for Coord {
    fn from(c: geo_types::Coord<f64>) -> Self {
        Self { x: c.x, y: c.y }
    }
}

#[cfg(feature = "geo-types")]
impl From<Coord> for geo_types::Coord<f64> {
    fn from(c: Coord) -> Self {
        geo_types::Coord { x: c.x, y: c.y }
    }
}

/// Trait for types that can be transformed through a [`Transform`](crate::Transform).
///
/// The transform returns the same type as the input, so `geo_types::Coord<f64>` in
/// gives `geo_types::Coord<f64>` out, and `(f64, f64)` in gives `(f64, f64)` out.
pub trait Transformable: Sized {
    fn into_coord(self) -> Coord;
    fn from_coord(c: Coord) -> Self;
}

/// Trait for types that can be transformed through a [`Transform`](crate::Transform)
/// while preserving an ellipsoidal height component.
///
/// The transform returns the same type as the input, so `(f64, f64, f64)` in gives
/// `(f64, f64, f64)` out and [`Coord3D`] in gives [`Coord3D`] out.
pub trait Transformable3D: Sized {
    fn into_coord3d(self) -> Coord3D;
    fn from_coord3d(c: Coord3D) -> Self;
}

impl Transformable for Coord {
    fn into_coord(self) -> Coord {
        self
    }
    fn from_coord(c: Coord) -> Self {
        c
    }
}

impl Transformable for (f64, f64) {
    fn into_coord(self) -> Coord {
        Coord {
            x: self.0,
            y: self.1,
        }
    }
    fn from_coord(c: Coord) -> Self {
        (c.x, c.y)
    }
}

impl Transformable3D for Coord3D {
    fn into_coord3d(self) -> Coord3D {
        self
    }

    fn from_coord3d(c: Coord3D) -> Self {
        c
    }
}

impl Transformable3D for (f64, f64, f64) {
    fn into_coord3d(self) -> Coord3D {
        Coord3D {
            x: self.0,
            y: self.1,
            z: self.2,
        }
    }

    fn from_coord3d(c: Coord3D) -> Self {
        (c.x, c.y, c.z)
    }
}

#[cfg(feature = "geo-types")]
impl Transformable for geo_types::Coord<f64> {
    fn into_coord(self) -> Coord {
        Coord {
            x: self.x,
            y: self.y,
        }
    }
    fn from_coord(c: Coord) -> Self {
        geo_types::Coord { x: c.x, y: c.y }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_from_tuple() {
        let c: Coord = (1.0, 2.0).into();
        assert_eq!(c.x, 1.0);
        assert_eq!(c.y, 2.0);
    }

    #[test]
    fn tuple_from_coord() {
        let t: (f64, f64) = Coord::new(3.0, 4.0).into();
        assert_eq!(t, (3.0, 4.0));
    }

    #[test]
    fn coord3d_from_tuple() {
        let c: Coord3D = (1.0, 2.0, 3.0).into();
        assert_eq!(c.x, 1.0);
        assert_eq!(c.y, 2.0);
        assert_eq!(c.z, 3.0);
    }

    #[test]
    fn tuple_from_coord3d() {
        let t: (f64, f64, f64) = Coord3D::new(3.0, 4.0, 5.0).into();
        assert_eq!(t, (3.0, 4.0, 5.0));
    }

    #[test]
    fn transformable_roundtrip_tuple() {
        let original = (10.0, 20.0);
        let coord = original.into_coord();
        let back = <(f64, f64)>::from_coord(coord);
        assert_eq!(original, back);
    }

    #[test]
    fn transformable3d_roundtrip_tuple() {
        let original = (10.0, 20.0, 30.0);
        let coord = original.into_coord3d();
        let back = <(f64, f64, f64)>::from_coord3d(coord);
        assert_eq!(original, back);
    }

    #[test]
    fn bounds_basics() {
        let bounds = Bounds::new(-10.0, 20.0, 30.0, 40.0);
        assert_eq!(bounds.width(), 40.0);
        assert_eq!(bounds.height(), 20.0);
        assert!(bounds.is_valid());
    }
}

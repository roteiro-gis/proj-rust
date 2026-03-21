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
    fn transformable_roundtrip_tuple() {
        let original = (10.0, 20.0);
        let coord = original.into_coord();
        let back = <(f64, f64)>::from_coord(coord);
        assert_eq!(original, back);
    }
}

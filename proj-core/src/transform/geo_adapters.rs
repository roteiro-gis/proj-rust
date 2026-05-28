use super::{Transform, TransformableGeometry};
use crate::error::Result;

#[cfg(feature = "geo-types")]
fn transform_geo_coord(
    transform: &Transform,
    coord: geo_types::Coord<f64>,
) -> Result<geo_types::Coord<f64>> {
    transform.convert(coord)
}

#[cfg(feature = "geo-types")]
fn transform_geo_coords(
    transform: &Transform,
    coords: Vec<geo_types::Coord<f64>>,
) -> Result<Vec<geo_types::Coord<f64>>> {
    coords
        .into_iter()
        .map(|coord| transform_geo_coord(transform, coord))
        .collect()
}

#[cfg(feature = "geo-types")]
fn transform_geo_rect(
    transform: &Transform,
    rect: geo_types::Rect<f64>,
) -> Result<geo_types::Rect<f64>> {
    let min = rect.min();
    let max = rect.max();
    let corners = [
        geo_types::Coord { x: min.x, y: min.y },
        geo_types::Coord { x: max.x, y: min.y },
        geo_types::Coord { x: max.x, y: max.y },
        geo_types::Coord { x: min.x, y: max.y },
    ];

    let mut transformed = corners
        .into_iter()
        .map(|coord| transform_geo_coord(transform, coord));
    let first = transformed.next().expect("rect has four corners")?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x;
    let mut max_y = first.y;
    for coord in transformed {
        let coord = coord?;
        min_x = min_x.min(coord.x);
        min_y = min_y.min(coord.y);
        max_x = max_x.max(coord.x);
        max_y = max_y.max(coord.y);
    }

    Ok(geo_types::Rect::new(
        geo_types::Coord { x: min_x, y: min_y },
        geo_types::Coord { x: max_x, y: max_y },
    ))
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Coord<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        transform_geo_coord(transform, self)
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Point<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::Point::from(transform_geo_coord(
            transform, self.0,
        )?))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Line<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::Line::new(
            transform_geo_coord(transform, self.start)?,
            transform_geo_coord(transform, self.end)?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::LineString<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::LineString::new(transform_geo_coords(
            transform,
            self.into_inner(),
        )?))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Polygon<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        let (exterior, interiors) = self.into_inner();
        let exterior = exterior.transform_geometry(transform)?;
        let interiors = interiors
            .into_iter()
            .map(|ring| ring.transform_geometry(transform))
            .collect::<Result<Vec<_>>>()?;
        Ok(geo_types::Polygon::new(exterior, interiors))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::MultiPoint<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::MultiPoint(
            self.0
                .into_iter()
                .map(|point| point.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::MultiLineString<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::MultiLineString(
            self.0
                .into_iter()
                .map(|line| line.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::MultiPolygon<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::MultiPolygon(
            self.0
                .into_iter()
                .map(|polygon| polygon.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::GeometryCollection<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(geo_types::GeometryCollection(
            self.0
                .into_iter()
                .map(|geometry| geometry.transform_geometry(transform))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Rect<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        transform_geo_rect(transform, self)
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Triangle<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        let [v1, v2, v3] = self.to_array();
        Ok(geo_types::Triangle(
            transform_geo_coord(transform, v1)?,
            transform_geo_coord(transform, v2)?,
            transform_geo_coord(transform, v3)?,
        ))
    }
}

#[cfg(feature = "geo-types")]
impl TransformableGeometry for geo_types::Geometry<f64> {
    fn transform_geometry(self, transform: &Transform) -> Result<Self> {
        Ok(match self {
            geo_types::Geometry::Point(geometry) => {
                geo_types::Geometry::Point(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Line(geometry) => {
                geo_types::Geometry::Line(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::LineString(geometry) => {
                geo_types::Geometry::LineString(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Polygon(geometry) => {
                geo_types::Geometry::Polygon(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::MultiPoint(geometry) => {
                geo_types::Geometry::MultiPoint(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::MultiLineString(geometry) => {
                geo_types::Geometry::MultiLineString(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::MultiPolygon(geometry) => {
                geo_types::Geometry::MultiPolygon(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::GeometryCollection(geometry) => {
                geo_types::Geometry::GeometryCollection(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Rect(geometry) => {
                geo_types::Geometry::Rect(geometry.transform_geometry(transform)?)
            }
            geo_types::Geometry::Triangle(geometry) => {
                geo_types::Geometry::Triangle(geometry.transform_geometry(transform)?)
            }
        })
    }
}

//! Conversions between this crate's IR and the `geo-types` crate.
//!
//! **Lossy:** `geo-types` is strictly 2D — Z and M ordinates are silently
//! dropped. `Geometry::Empty(_)` becomes a typed empty `geo_types` value.
//! `Geometry::GeometryCollection` round-trips.

use geo_types::{
    Coord as GtCoord, Geometry as GtGeometry, GeometryCollection as GtGc,
    LineString as GtLineString, MultiLineString as GtMls, MultiPoint as GtMp,
    MultiPolygon as GtMpoly, Point as GtPoint, Polygon as GtPolygon,
};

use crate::{Coord, Geometry, LineString, Polygon};

impl From<Coord> for GtCoord<f64> {
    fn from(c: Coord) -> Self {
        GtCoord { x: c.x, y: c.y }
    }
}

impl From<GtCoord<f64>> for Coord {
    fn from(c: GtCoord<f64>) -> Self {
        Coord::xy(c.x, c.y)
    }
}

impl From<&LineString> for GtLineString<f64> {
    fn from(ls: &LineString) -> Self {
        GtLineString::new(ls.coords.iter().copied().map(Into::into).collect())
    }
}

impl From<&Polygon> for GtPolygon<f64> {
    fn from(p: &Polygon) -> Self {
        GtPolygon::new(
            GtLineString::from(&p.exterior),
            p.holes.iter().map(GtLineString::from).collect(),
        )
    }
}

impl From<&Geometry> for GtGeometry<f64> {
    fn from(g: &Geometry) -> Self {
        match g {
            Geometry::Point(c) => GtGeometry::Point(GtPoint::new(c.x, c.y)),
            Geometry::LineString(ls) => GtGeometry::LineString(GtLineString::from(ls)),
            Geometry::Polygon(p) => GtGeometry::Polygon(GtPolygon::from(p)),
            Geometry::MultiPoint(v) => GtGeometry::MultiPoint(GtMp(
                v.iter().map(|c| GtPoint::new(c.x, c.y)).collect(),
            )),
            Geometry::MultiLineString(v) => {
                GtGeometry::MultiLineString(GtMls(v.iter().map(GtLineString::from).collect()))
            }
            Geometry::MultiPolygon(v) => {
                GtGeometry::MultiPolygon(GtMpoly(v.iter().map(GtPolygon::from).collect()))
            }
            Geometry::GeometryCollection(v) => {
                GtGeometry::GeometryCollection(GtGc(v.iter().map(Into::into).collect()))
            }
            Geometry::Empty(t) => empty_of(*t),
        }
    }
}

fn empty_of(t: crate::GeometryType) -> GtGeometry<f64> {
    use crate::GeometryType::*;
    match t {
        Point => GtGeometry::Point(GtPoint::new(f64::NAN, f64::NAN)),
        LineString => GtGeometry::LineString(GtLineString::new(vec![])),
        Polygon => GtGeometry::Polygon(GtPolygon::new(GtLineString::new(vec![]), vec![])),
        MultiPoint => GtGeometry::MultiPoint(GtMp(vec![])),
        MultiLineString => GtGeometry::MultiLineString(GtMls(vec![])),
        MultiPolygon => GtGeometry::MultiPolygon(GtMpoly(vec![])),
        GeometryCollection => GtGeometry::GeometryCollection(GtGc(vec![])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_roundtrip_drops_z_m() {
        let our = Coord::xyzm(1.0, 2.0, 3.0, 4.0);
        let theirs: GtCoord<f64> = our.into();
        assert_eq!(theirs.x, 1.0);
        assert_eq!(theirs.y, 2.0);
        let back: Coord = theirs.into();
        assert_eq!(back, Coord::xy(1.0, 2.0)); // z/m dropped
    }

    #[test]
    fn polygon_with_hole_converts() {
        let p = Polygon::new(
            LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(10.0, 0.0),
                Coord::xy(10.0, 10.0),
                Coord::xy(0.0, 10.0),
                Coord::xy(0.0, 0.0),
            ]),
            vec![LineString::new(vec![
                Coord::xy(2.0, 2.0),
                Coord::xy(4.0, 2.0),
                Coord::xy(4.0, 4.0),
                Coord::xy(2.0, 4.0),
                Coord::xy(2.0, 2.0),
            ])],
        );
        let gt: GtPolygon<f64> = (&p).into();
        assert_eq!(gt.exterior().0.len(), 5);
        assert_eq!(gt.interiors().len(), 1);
        assert_eq!(gt.interiors()[0].0.len(), 5);
    }

    #[test]
    fn typed_empty_preserves_variant() {
        use crate::GeometryType;
        let e = Geometry::Empty(GeometryType::Polygon);
        let gt: GtGeometry<f64> = (&e).into();
        assert!(matches!(gt, GtGeometry::Polygon(p) if p.exterior().0.is_empty()));
    }
}

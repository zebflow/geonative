//! Simple-Features geometry tree with optional Z/M ordinates.
//!
//! The shape is deliberately close to OGC Simple Features + WKB so writers
//! (WKB, GeoJSON, Shapefile, GeoParquet) reduce to direct tree walks.

/// A single coordinate. `z` and `m` are `Option`s so we can distinguish
/// "this geometry has no Z dimension" from "Z is invalid". We do **not** use
/// NaN as a sentinel — that's a GDB-specific quirk handled inside its reader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord {
    pub x: f64,
    pub y: f64,
    pub z: Option<f64>,
    pub m: Option<f64>,
}

impl Coord {
    pub const fn xy(x: f64, y: f64) -> Self {
        Self { x, y, z: None, m: None }
    }

    pub const fn xyz(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z: Some(z), m: None }
    }

    pub const fn xym(x: f64, y: f64, m: f64) -> Self {
        Self { x, y, z: None, m: Some(m) }
    }

    pub const fn xyzm(x: f64, y: f64, z: f64, m: f64) -> Self {
        Self { x, y, z: Some(z), m: Some(m) }
    }

    pub const fn has_z(&self) -> bool {
        self.z.is_some()
    }

    pub const fn has_m(&self) -> bool {
        self.m.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LineString {
    pub coords: Vec<Coord>,
}

impl LineString {
    pub fn new(coords: Vec<Coord>) -> Self {
        Self { coords }
    }

    pub fn is_empty(&self) -> bool {
        self.coords.is_empty()
    }
}

/// A polygon: one exterior ring plus zero or more interior rings (holes).
///
/// Ring orientation in this IR is **OGC-standard**: exterior counter-clockwise,
/// interior clockwise. Drivers that read formats with a different convention
/// (Esri Shapefile/GDB: exterior CW, hole CCW) re-orient at read time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Polygon {
    pub exterior: LineString,
    pub holes: Vec<LineString>,
}

impl Polygon {
    pub fn new(exterior: LineString, holes: Vec<LineString>) -> Self {
        Self { exterior, holes }
    }

    pub fn is_empty(&self) -> bool {
        self.exterior.is_empty()
    }
}

/// Discriminant of [`Geometry`]. Used in `Geometry::Empty(_)` to preserve
/// the type of a typed-empty geometry (WKB requires this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryType {
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    Point(Coord),
    LineString(LineString),
    Polygon(Polygon),
    MultiPoint(Vec<Coord>),
    MultiLineString(Vec<LineString>),
    MultiPolygon(Vec<Polygon>),
    GeometryCollection(Vec<Geometry>),
    /// A typed-empty geometry (e.g. `POINT EMPTY`, `POLYGON EMPTY`). The tag
    /// is preserved so WKB serialization round-trips correctly.
    Empty(GeometryType),
}

impl Geometry {
    pub fn type_of(&self) -> GeometryType {
        match self {
            Geometry::Point(_) => GeometryType::Point,
            Geometry::LineString(_) => GeometryType::LineString,
            Geometry::Polygon(_) => GeometryType::Polygon,
            Geometry::MultiPoint(_) => GeometryType::MultiPoint,
            Geometry::MultiLineString(_) => GeometryType::MultiLineString,
            Geometry::MultiPolygon(_) => GeometryType::MultiPolygon,
            Geometry::GeometryCollection(_) => GeometryType::GeometryCollection,
            Geometry::Empty(t) => *t,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Geometry::Empty(_) => true,
            Geometry::Point(_) => false,
            Geometry::LineString(ls) => ls.is_empty(),
            Geometry::Polygon(p) => p.is_empty(),
            Geometry::MultiPoint(v) => v.is_empty(),
            Geometry::MultiLineString(v) => v.iter().all(LineString::is_empty),
            Geometry::MultiPolygon(v) => v.iter().all(Polygon::is_empty),
            Geometry::GeometryCollection(v) => v.iter().all(Geometry::is_empty),
        }
    }

    /// True if any coordinate in the tree carries a Z ordinate.
    pub fn has_z(&self) -> bool {
        match self {
            Geometry::Empty(_) => false,
            Geometry::Point(c) => c.has_z(),
            Geometry::LineString(ls) => ls.coords.iter().any(Coord::has_z),
            Geometry::Polygon(p) => {
                p.exterior.coords.iter().any(Coord::has_z)
                    || p.holes.iter().any(|h| h.coords.iter().any(Coord::has_z))
            }
            Geometry::MultiPoint(v) => v.iter().any(Coord::has_z),
            Geometry::MultiLineString(v) => {
                v.iter().any(|ls| ls.coords.iter().any(Coord::has_z))
            }
            Geometry::MultiPolygon(v) => v.iter().any(|p| {
                p.exterior.coords.iter().any(Coord::has_z)
                    || p.holes.iter().any(|h| h.coords.iter().any(Coord::has_z))
            }),
            Geometry::GeometryCollection(v) => v.iter().any(Geometry::has_z),
        }
    }

    /// Compute the 2D bounding box `[xmin, ymin, xmax, ymax]`.
    ///
    /// Returns `None` for empty geometries (no coordinates to bound). NaN
    /// coordinates are skipped, so `POINT EMPTY` (stored as NaN/NaN) also
    /// yields `None`.
    pub fn bbox(&self) -> Option<[f64; 4]> {
        let mut acc: Option<[f64; 4]> = None;
        for_each_xy(self, &mut |x, y| {
            if !x.is_finite() || !y.is_finite() {
                return;
            }
            match &mut acc {
                None => acc = Some([x, y, x, y]),
                Some(b) => {
                    if x < b[0] { b[0] = x; }
                    if y < b[1] { b[1] = y; }
                    if x > b[2] { b[2] = x; }
                    if y > b[3] { b[3] = y; }
                }
            }
        });
        acc
    }

    /// True if any coordinate in the tree carries an M ordinate.
    pub fn has_m(&self) -> bool {
        match self {
            Geometry::Empty(_) => false,
            Geometry::Point(c) => c.has_m(),
            Geometry::LineString(ls) => ls.coords.iter().any(Coord::has_m),
            Geometry::Polygon(p) => {
                p.exterior.coords.iter().any(Coord::has_m)
                    || p.holes.iter().any(|h| h.coords.iter().any(Coord::has_m))
            }
            Geometry::MultiPoint(v) => v.iter().any(Coord::has_m),
            Geometry::MultiLineString(v) => {
                v.iter().any(|ls| ls.coords.iter().any(Coord::has_m))
            }
            Geometry::MultiPolygon(v) => v.iter().any(|p| {
                p.exterior.coords.iter().any(Coord::has_m)
                    || p.holes.iter().any(|h| h.coords.iter().any(Coord::has_m))
            }),
            Geometry::GeometryCollection(v) => v.iter().any(Geometry::has_m),
        }
    }
}

/// Walk every (x, y) coordinate in the tree and invoke `f`. Used by `bbox`
/// and (later) by visitor-style writers.
fn for_each_xy(g: &Geometry, f: &mut dyn FnMut(f64, f64)) {
    match g {
        Geometry::Empty(_) => {}
        Geometry::Point(c) => f(c.x, c.y),
        Geometry::LineString(ls) => {
            for c in &ls.coords {
                f(c.x, c.y);
            }
        }
        Geometry::Polygon(p) => {
            for c in &p.exterior.coords {
                f(c.x, c.y);
            }
            for h in &p.holes {
                for c in &h.coords {
                    f(c.x, c.y);
                }
            }
        }
        Geometry::MultiPoint(v) => {
            for c in v {
                f(c.x, c.y);
            }
        }
        Geometry::MultiLineString(v) => {
            for ls in v {
                for c in &ls.coords {
                    f(c.x, c.y);
                }
            }
        }
        Geometry::MultiPolygon(v) => {
            for p in v {
                for c in &p.exterior.coords {
                    f(c.x, c.y);
                }
                for h in &p.holes {
                    for c in &h.coords {
                        f(c.x, c.y);
                    }
                }
            }
        }
        Geometry::GeometryCollection(v) => {
            for inner in v {
                for_each_xy(inner, f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_constructors_track_dimensions() {
        assert!(!Coord::xy(1.0, 2.0).has_z());
        assert!(!Coord::xy(1.0, 2.0).has_m());

        let c = Coord::xyz(1.0, 2.0, 3.0);
        assert!(c.has_z() && !c.has_m());
        assert_eq!(c.z, Some(3.0));

        let c = Coord::xym(1.0, 2.0, 99.0);
        assert!(!c.has_z() && c.has_m());
        assert_eq!(c.m, Some(99.0));

        let c = Coord::xyzm(1.0, 2.0, 3.0, 99.0);
        assert!(c.has_z() && c.has_m());
    }

    #[test]
    fn geometry_type_of_matches_variant() {
        assert_eq!(
            Geometry::Point(Coord::xy(0.0, 0.0)).type_of(),
            GeometryType::Point
        );
        assert_eq!(
            Geometry::Polygon(Polygon::default()).type_of(),
            GeometryType::Polygon
        );
        assert_eq!(
            Geometry::Empty(GeometryType::MultiPolygon).type_of(),
            GeometryType::MultiPolygon
        );
    }

    #[test]
    fn empty_detection() {
        assert!(Geometry::Empty(GeometryType::Point).is_empty());
        assert!(Geometry::LineString(LineString::default()).is_empty());
        assert!(!Geometry::Point(Coord::xy(0.0, 0.0)).is_empty());

        let ls = LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xy(1.0, 1.0)]);
        assert!(!Geometry::LineString(ls).is_empty());
    }

    #[test]
    fn bbox_of_polygon_with_hole() {
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
        let bbox = Geometry::Polygon(p).bbox().unwrap();
        assert_eq!(bbox, [0.0, 0.0, 10.0, 10.0]);
    }

    #[test]
    fn bbox_of_empty_geometry_is_none() {
        assert!(Geometry::Empty(GeometryType::Polygon).bbox().is_none());
        assert!(Geometry::LineString(LineString::default()).bbox().is_none());
    }

    #[test]
    fn bbox_skips_nan() {
        let g = Geometry::MultiPoint(vec![
            Coord::xy(1.0, 2.0),
            Coord::xy(f64::NAN, f64::NAN),
            Coord::xy(5.0, 6.0),
        ]);
        assert_eq!(g.bbox(), Some([1.0, 2.0, 5.0, 6.0]));
    }

    #[test]
    fn bbox_of_geometry_collection() {
        let g = Geometry::GeometryCollection(vec![
            Geometry::Point(Coord::xy(-1.0, -1.0)),
            Geometry::Point(Coord::xy(5.0, 5.0)),
        ]);
        assert_eq!(g.bbox(), Some([-1.0, -1.0, 5.0, 5.0]));
    }

    #[test]
    fn has_z_m_recursion() {
        let ls = LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xyz(1.0, 1.0, 5.0)]);
        let g = Geometry::LineString(ls);
        assert!(g.has_z());
        assert!(!g.has_m());

        let mls = Geometry::MultiLineString(vec![LineString::new(vec![Coord::xym(0.0, 0.0, 7.0)])]);
        assert!(!mls.has_z());
        assert!(mls.has_m());

        let collection =
            Geometry::GeometryCollection(vec![Geometry::Point(Coord::xyzm(0.0, 0.0, 1.0, 2.0))]);
        assert!(collection.has_z());
        assert!(collection.has_m());
    }
}

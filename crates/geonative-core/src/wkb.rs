//! OGC Simple Features **Well-Known Binary** (WKB) encoder for [`Geometry`].
//!
//! Produces little-endian, 2D-only WKB matching the SQL/MM Part 3 spec — the
//! same encoding used by PostGIS, SpatiaLite, GeoPackage, FlatGeoBuf, and
//! GeoParquet (the latter recommends WKB as the default geometry encoding for
//! interoperability).
//!
//! ## Type codes (2D)
//!
//! | Code | Type |
//! | ---: | --- |
//! | 1 | Point |
//! | 2 | LineString |
//! | 3 | Polygon |
//! | 4 | MultiPoint |
//! | 5 | MultiLineString |
//! | 6 | MultiPolygon |
//! | 7 | GeometryCollection |
//!
//! ## Empty geometries
//!
//! - `POINT EMPTY` is encoded with `NaN` for both X and Y (the convention
//!   used by PostGIS and GEOS).
//! - Collection-shaped empties (LineString, Polygon, Multi\*, Collection)
//!   serialize their child-count as 0.
//!
//! ## Z / M
//!
//! v0.1 emits 2D only. Geometries carrying Z or M ordinates drop them
//! silently — callers concerned with Z/M fidelity should error out before
//! encoding (the IR carries `has_z()` / `has_m()` for that purpose).

use crate::geometry::{Coord, Geometry, GeometryType, LineString, Polygon};

const BYTE_ORDER_LE: u8 = 1;

const WKB_POINT: u32 = 1;
const WKB_LINESTRING: u32 = 2;
const WKB_POLYGON: u32 = 3;
const WKB_MULTIPOINT: u32 = 4;
const WKB_MULTILINESTRING: u32 = 5;
const WKB_MULTIPOLYGON: u32 = 6;
const WKB_GEOMETRYCOLLECTION: u32 = 7;

impl Geometry {
    /// Encode as OGC Simple Features Well-Known Binary (little-endian, 2D).
    pub fn to_wkb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(estimated_size(self));
        write_geometry(self, &mut out);
        out
    }

    /// Encode WKB into a caller-provided buffer (clears it first). Useful for
    /// row-by-row writers that want to reuse a scratch buffer.
    pub fn write_wkb_into(&self, buf: &mut Vec<u8>) {
        buf.clear();
        buf.reserve(estimated_size(self));
        write_geometry(self, buf);
    }
}

fn estimated_size(g: &Geometry) -> usize {
    // 5 bytes preamble + 4 (count) + n_coords * 16 in the worst case.
    // We just upper-bound a bit; Vec will grow if we underestimate.
    let coord_count = count_coords(g);
    5 + 4 + coord_count * 16
}

fn count_coords(g: &Geometry) -> usize {
    match g {
        Geometry::Point(_) => 1,
        Geometry::LineString(ls) => ls.coords.len(),
        Geometry::Polygon(p) => p.exterior.coords.len() + p.holes.iter().map(|h| h.coords.len()).sum::<usize>(),
        Geometry::MultiPoint(v) => v.len(),
        Geometry::MultiLineString(v) => v.iter().map(|ls| ls.coords.len()).sum(),
        Geometry::MultiPolygon(v) => v
            .iter()
            .map(|p| p.exterior.coords.len() + p.holes.iter().map(|h| h.coords.len()).sum::<usize>())
            .sum(),
        Geometry::GeometryCollection(v) => v.iter().map(count_coords).sum(),
        Geometry::Empty(_) => 0,
    }
}

fn write_geometry(g: &Geometry, out: &mut Vec<u8>) {
    match g {
        Geometry::Point(c) => write_point(c, out),
        Geometry::LineString(ls) => write_linestring(ls, out),
        Geometry::Polygon(p) => write_polygon(p, out),
        Geometry::MultiPoint(v) => write_multipoint(v, out),
        Geometry::MultiLineString(v) => write_multilinestring(v, out),
        Geometry::MultiPolygon(v) => write_multipolygon(v, out),
        Geometry::GeometryCollection(v) => write_collection(v, out),
        Geometry::Empty(t) => write_empty(*t, out),
    }
}

fn write_preamble(out: &mut Vec<u8>, type_code: u32) {
    out.push(BYTE_ORDER_LE);
    out.extend_from_slice(&type_code.to_le_bytes());
}

fn write_xy(c: &Coord, out: &mut Vec<u8>) {
    out.extend_from_slice(&c.x.to_le_bytes());
    out.extend_from_slice(&c.y.to_le_bytes());
}

fn write_point(c: &Coord, out: &mut Vec<u8>) {
    write_preamble(out, WKB_POINT);
    write_xy(c, out);
}

fn write_linestring(ls: &LineString, out: &mut Vec<u8>) {
    write_preamble(out, WKB_LINESTRING);
    out.extend_from_slice(&(ls.coords.len() as u32).to_le_bytes());
    for c in &ls.coords {
        write_xy(c, out);
    }
}

fn write_polygon(p: &Polygon, out: &mut Vec<u8>) {
    write_preamble(out, WKB_POLYGON);
    let n_rings = 1 + p.holes.len();
    out.extend_from_slice(&(n_rings as u32).to_le_bytes());
    write_ring(&p.exterior, out);
    for h in &p.holes {
        write_ring(h, out);
    }
}

fn write_ring(ls: &LineString, out: &mut Vec<u8>) {
    out.extend_from_slice(&(ls.coords.len() as u32).to_le_bytes());
    for c in &ls.coords {
        write_xy(c, out);
    }
}

fn write_multipoint(pts: &[Coord], out: &mut Vec<u8>) {
    write_preamble(out, WKB_MULTIPOINT);
    out.extend_from_slice(&(pts.len() as u32).to_le_bytes());
    for c in pts {
        write_point(c, out);
    }
}

fn write_multilinestring(parts: &[LineString], out: &mut Vec<u8>) {
    write_preamble(out, WKB_MULTILINESTRING);
    out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
    for ls in parts {
        write_linestring(ls, out);
    }
}

fn write_multipolygon(polys: &[Polygon], out: &mut Vec<u8>) {
    write_preamble(out, WKB_MULTIPOLYGON);
    out.extend_from_slice(&(polys.len() as u32).to_le_bytes());
    for p in polys {
        write_polygon(p, out);
    }
}

fn write_collection(geoms: &[Geometry], out: &mut Vec<u8>) {
    write_preamble(out, WKB_GEOMETRYCOLLECTION);
    out.extend_from_slice(&(geoms.len() as u32).to_le_bytes());
    for g in geoms {
        write_geometry(g, out);
    }
}

fn write_empty(t: GeometryType, out: &mut Vec<u8>) {
    match t {
        GeometryType::Point => {
            // POINT EMPTY: x = y = NaN.
            write_preamble(out, WKB_POINT);
            out.extend_from_slice(&f64::NAN.to_le_bytes());
            out.extend_from_slice(&f64::NAN.to_le_bytes());
        }
        GeometryType::LineString => write_preamble_with_zero_count(out, WKB_LINESTRING),
        GeometryType::Polygon => write_preamble_with_zero_count(out, WKB_POLYGON),
        GeometryType::MultiPoint => write_preamble_with_zero_count(out, WKB_MULTIPOINT),
        GeometryType::MultiLineString => write_preamble_with_zero_count(out, WKB_MULTILINESTRING),
        GeometryType::MultiPolygon => write_preamble_with_zero_count(out, WKB_MULTIPOLYGON),
        GeometryType::GeometryCollection => {
            write_preamble_with_zero_count(out, WKB_GEOMETRYCOLLECTION)
        }
    }
}

fn write_preamble_with_zero_count(out: &mut Vec<u8>, type_code: u32) {
    write_preamble(out, type_code);
    out.extend_from_slice(&0u32.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_wkb_byte_exact() {
        let g = Geometry::Point(Coord::xy(1.0, 2.0));
        let wkb = g.to_wkb();
        // 1 byte order + 4 bytes type + 8 bytes x + 8 bytes y = 21
        assert_eq!(wkb.len(), 21);
        assert_eq!(wkb[0], 1); // LE
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 1); // Point
        assert_eq!(f64::from_le_bytes(wkb[5..13].try_into().unwrap()), 1.0);
        assert_eq!(f64::from_le_bytes(wkb[13..21].try_into().unwrap()), 2.0);
    }

    #[test]
    fn point_empty_uses_nan() {
        let wkb = Geometry::Empty(GeometryType::Point).to_wkb();
        assert_eq!(wkb.len(), 21);
        assert!(f64::from_le_bytes(wkb[5..13].try_into().unwrap()).is_nan());
        assert!(f64::from_le_bytes(wkb[13..21].try_into().unwrap()).is_nan());
    }

    #[test]
    fn linestring_wkb_layout() {
        let ls = LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xy(1.0, 1.0), Coord::xy(2.0, 0.0)]);
        let wkb = Geometry::LineString(ls).to_wkb();
        // 5 (preamble) + 4 (npts) + 3*16 = 57
        assert_eq!(wkb.len(), 57);
        assert_eq!(wkb[0], 1);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 3);
        // first coord
        assert_eq!(f64::from_le_bytes(wkb[9..17].try_into().unwrap()), 0.0);
        assert_eq!(f64::from_le_bytes(wkb[17..25].try_into().unwrap()), 0.0);
        // third coord
        assert_eq!(f64::from_le_bytes(wkb[41..49].try_into().unwrap()), 2.0);
        assert_eq!(f64::from_le_bytes(wkb[49..57].try_into().unwrap()), 0.0);
    }

    #[test]
    fn polygon_with_hole() {
        let exterior = LineString::new(vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(10.0, 0.0),
            Coord::xy(10.0, 10.0),
            Coord::xy(0.0, 10.0),
            Coord::xy(0.0, 0.0),
        ]);
        let hole = LineString::new(vec![
            Coord::xy(2.0, 2.0),
            Coord::xy(4.0, 2.0),
            Coord::xy(4.0, 4.0),
            Coord::xy(2.0, 4.0),
            Coord::xy(2.0, 2.0),
        ]);
        let p = Polygon::new(exterior, vec![hole]);
        let wkb = Geometry::Polygon(p).to_wkb();
        // 5 (preamble) + 4 (n_rings = 2) + 4 (ring0 npts) + 5*16 + 4 (ring1 npts) + 5*16
        assert_eq!(wkb.len(), 5 + 4 + 4 + 80 + 4 + 80);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 2); // 2 rings
        assert_eq!(u32::from_le_bytes(wkb[9..13].try_into().unwrap()), 5); // ring0: 5 pts
        // ring1 count follows ring0's 5 coords (80 bytes), at offset 13 + 80 = 93
        assert_eq!(u32::from_le_bytes(wkb[93..97].try_into().unwrap()), 5);
    }

    #[test]
    fn multipoint_each_point_carries_own_header() {
        let g = Geometry::MultiPoint(vec![Coord::xy(0.0, 0.0), Coord::xy(1.0, 1.0)]);
        let wkb = g.to_wkb();
        // 5 (preamble) + 4 (count) + 2 * 21 (each point full WKB)
        assert_eq!(wkb.len(), 5 + 4 + 42);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 2);
        // first inner point header
        assert_eq!(wkb[9], 1);
        assert_eq!(u32::from_le_bytes(wkb[10..14].try_into().unwrap()), 1);
    }

    #[test]
    fn multilinestring_wkb_layout() {
        let g = Geometry::MultiLineString(vec![
            LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xy(1.0, 1.0)]),
            LineString::new(vec![Coord::xy(2.0, 2.0), Coord::xy(3.0, 3.0), Coord::xy(4.0, 4.0)]),
        ]);
        let wkb = g.to_wkb();
        assert_eq!(wkb[0], 1);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 5);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 2);
        // First inner LineString starts at 9: byte_order + type + n_pts + coords
        assert_eq!(wkb[9], 1);
        assert_eq!(u32::from_le_bytes(wkb[10..14].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(wkb[14..18].try_into().unwrap()), 2); // 2 pts
    }

    #[test]
    fn empty_linestring_wkb() {
        let wkb = Geometry::Empty(GeometryType::LineString).to_wkb();
        assert_eq!(wkb.len(), 5 + 4);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 0);
    }

    #[test]
    fn empty_polygon_and_multipolygon() {
        for (t, code) in [
            (GeometryType::Polygon, 3),
            (GeometryType::MultiPolygon, 6),
            (GeometryType::MultiLineString, 5),
            (GeometryType::MultiPoint, 4),
            (GeometryType::GeometryCollection, 7),
        ] {
            let wkb = Geometry::Empty(t).to_wkb();
            assert_eq!(wkb.len(), 5 + 4, "empty {t:?}");
            assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), code);
            assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 0);
        }
    }

    #[test]
    fn geometry_collection_nests_correctly() {
        let g = Geometry::GeometryCollection(vec![
            Geometry::Point(Coord::xy(1.0, 2.0)),
            Geometry::LineString(LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xy(1.0, 1.0)])),
        ]);
        let wkb = g.to_wkb();
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 2);
        // First inner geom (point) at offset 9
        assert_eq!(wkb[9], 1);
        assert_eq!(u32::from_le_bytes(wkb[10..14].try_into().unwrap()), 1); // point
        // Second inner geom (linestring) starts at 9 + 21 = 30
        assert_eq!(wkb[30], 1);
        assert_eq!(u32::from_le_bytes(wkb[31..35].try_into().unwrap()), 2); // linestring
    }

    #[test]
    fn write_wkb_into_clears_buffer() {
        let mut scratch = vec![0xFFu8; 100];
        Geometry::Point(Coord::xy(5.0, 6.0)).write_wkb_into(&mut scratch);
        assert_eq!(scratch.len(), 21);
        assert_eq!(scratch[0], 1);
    }
}

//! OGC Simple Features **Well-Known Binary** (WKB) encoder + decoder for [`Geometry`].
//!
//! ## What this module is
//!
//! The codec for the OGC SF binary geometry format — the lingua franca that
//! every "serious" spatial system speaks: PostGIS, SpatiaLite, GeoPackage,
//! FlatGeoBuf, GeoParquet, MVT (variants), Shapefile (related). If we encode
//! once and decode once correctly, every downstream format becomes a
//! mechanical translation.
//!
//! ## How it sits in the architecture
//!
//! - **Encoder** ([`Geometry::to_wkb`]) walks the `Geometry` enum tree and
//!   serializes bytes. Used by `geonative-geoparquet` (and any future GPKG /
//!   MVT writer).
//! - **Decoder** ([`Geometry::from_wkb`]) is the inverse — bytes → enum tree.
//!   Used by future GeoParquet/GPKG readers.
//! - **BBox fast-path** ([`bbox_from_bytes`]) walks WKB without materializing
//!   the `Geometry` — for spatial filters that only need an envelope test.
//!
//! ## Clever bits
//!
//! - **Per-nested byte order.** ISO WKB allows each sub-geometry inside a
//!   collection to carry its own byte-order byte. The decoder respects this;
//!   the encoder always emits LE for simplicity.
//! - **`POINT EMPTY` as NaN/NaN.** The PostGIS/GEOS convention. Other empties
//!   serialize as their type code + child count 0.
//! - **2D-only scope.** Z/M variants are recognized by their type-code high
//!   bits (`0x80000000` / `0x40000000`) and ISO-extended codes (`1001..`); the
//!   decoder errors with a clear "unsupported" message rather than silently
//!   dropping coordinates. v0.1 scope cut; revisit in v0.2.
//! - **`write_wkb_into`** lets callers reuse a scratch buffer across rows —
//!   the GeoParquet writer uses this in its row-by-row loop to avoid
//!   per-feature allocation.
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

use crate::error::{Error, Result};
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

// ============================================================================
// Decoder
// ============================================================================

/// Mask off the high bits used for Z/M/curve modifiers on the "Extended" WKB
/// type codes. We only consume the base type code (1..=7).
const TYPE_LOW_MASK: u32 = 0xFF;
/// ISO-extended Z bit (some implementations encode 3D this way).
const ISO_Z_FLAG: u32 = 0x8000_0000;
/// ISO-extended M bit.
const ISO_M_FLAG: u32 = 0x4000_0000;

impl Geometry {
    /// Decode OGC Simple Features WKB bytes into a [`Geometry`].
    ///
    /// Accepts both little-endian and big-endian, and respects per-nested
    /// byte-order bytes inside collections (the ISO WKB rule).
    ///
    /// v0.1 returns [`Error::Unsupported`] for Z/M variants — the type code
    /// is recognized but ordinates beyond X/Y are not decoded.
    pub fn from_wkb(bytes: &[u8]) -> Result<Self> {
        let mut c = WkbCursor::new(bytes);
        decode_geometry(&mut c)
    }
}

/// Scan WKB bytes and compute `[xmin, ymin, xmax, ymax]` **without
/// materializing** the [`Geometry`]. Faster than `from_wkb(bytes)?.bbox()`
/// because no allocations happen for coords/rings.
///
/// Returns `None` for input that doesn't contain any finite coordinate
/// (e.g. `POINT EMPTY`, empty collections, or malformed bytes).
pub fn bbox_from_bytes(bytes: &[u8]) -> Option<[f64; 4]> {
    let mut c = WkbCursor::new(bytes);
    let mut acc = BboxAcc::default();
    walk_bbox(&mut c, &mut acc).ok()?;
    acc.finish()
}

#[derive(Default)]
struct BboxAcc {
    have_any: bool,
    xmin: f64,
    ymin: f64,
    xmax: f64,
    ymax: f64,
}

impl BboxAcc {
    fn add(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        if !self.have_any {
            self.have_any = true;
            self.xmin = x;
            self.ymin = y;
            self.xmax = x;
            self.ymax = y;
        } else {
            if x < self.xmin { self.xmin = x; }
            if y < self.ymin { self.ymin = y; }
            if x > self.xmax { self.xmax = x; }
            if y > self.ymax { self.ymax = y; }
        }
    }

    fn finish(self) -> Option<[f64; 4]> {
        if self.have_any {
            Some([self.xmin, self.ymin, self.xmax, self.ymax])
        } else {
            None
        }
    }
}

/// Cursor over WKB bytes. Carries no byte-order state itself; each call to
/// [`read_u32`] / [`read_f64`] takes the `le` flag explicitly because nested
/// geometries inside collections can flip endianness.
struct WkbCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> WkbCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.pos + n > self.bytes.len() {
            Err(Error::malformed(format!(
                "WKB truncated at byte {}: need {} more, have {}",
                self.pos,
                n,
                self.bytes.len().saturating_sub(self.pos)
            )))
        } else {
            Ok(())
        }
    }

    fn read_u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.bytes[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u32(&mut self, le: bool) -> Result<u32> {
        self.need(4)?;
        let s = &self.bytes[self.pos..self.pos + 4];
        let arr: [u8; 4] = s.try_into().unwrap();
        self.pos += 4;
        Ok(if le {
            u32::from_le_bytes(arr)
        } else {
            u32::from_be_bytes(arr)
        })
    }

    fn read_f64(&mut self, le: bool) -> Result<f64> {
        self.need(8)?;
        let s = &self.bytes[self.pos..self.pos + 8];
        let arr: [u8; 8] = s.try_into().unwrap();
        self.pos += 8;
        Ok(if le {
            f64::from_le_bytes(arr)
        } else {
            f64::from_be_bytes(arr)
        })
    }

    /// Read the standard preamble (byte order + type code) and split out the
    /// base type vs the Z/M flags.
    fn read_preamble(&mut self) -> Result<(bool, u32, bool, bool)> {
        let bo = self.read_u8()?;
        let le = match bo {
            0 => false,
            1 => true,
            other => return Err(Error::malformed(format!("WKB byte-order byte {other}; expected 0 or 1"))),
        };
        let raw_type = self.read_u32(le)?;
        // Detect Z/M via either the ISO high bits or the extended 1000-series codes.
        let iso_has_z = raw_type & ISO_Z_FLAG != 0;
        let iso_has_m = raw_type & ISO_M_FLAG != 0;
        let base = raw_type & TYPE_LOW_MASK;
        let (base, range_has_z, range_has_m) = if (1001..=1007).contains(&raw_type) {
            (raw_type - 1000, true, false)
        } else if (2001..=2007).contains(&raw_type) {
            (raw_type - 2000, false, true)
        } else if (3001..=3007).contains(&raw_type) {
            (raw_type - 3000, true, true)
        } else {
            (base, false, false)
        };
        Ok((le, base, iso_has_z || range_has_z, iso_has_m || range_has_m))
    }
}

fn decode_geometry(c: &mut WkbCursor) -> Result<Geometry> {
    let (le, base, has_z, has_m) = c.read_preamble()?;
    if has_z || has_m {
        return Err(Error::unsupported(
            "WKB Z/M geometries (v0.1 is 2D-only)".to_string(),
        ));
    }
    match base {
        1 => decode_point(c, le),
        2 => decode_linestring(c, le),
        3 => decode_polygon(c, le),
        4 => decode_multipoint(c, le),
        5 => decode_multilinestring(c, le),
        6 => decode_multipolygon(c, le),
        7 => decode_collection(c, le),
        other => Err(Error::unsupported(format!("WKB geometry type code {other}"))),
    }
}

fn decode_point(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    let x = c.read_f64(le)?;
    let y = c.read_f64(le)?;
    if x.is_nan() && y.is_nan() {
        return Ok(Geometry::Empty(GeometryType::Point));
    }
    Ok(Geometry::Point(Coord::xy(x, y)))
}

fn decode_linestring_coords(c: &mut WkbCursor, le: bool) -> Result<Vec<Coord>> {
    let n = c.read_u32(le)? as usize;
    let mut coords = Vec::with_capacity(n);
    for _ in 0..n {
        let x = c.read_f64(le)?;
        let y = c.read_f64(le)?;
        coords.push(Coord::xy(x, y));
    }
    Ok(coords)
}

fn decode_linestring(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    let coords = decode_linestring_coords(c, le)?;
    if coords.is_empty() {
        Ok(Geometry::Empty(GeometryType::LineString))
    } else {
        Ok(Geometry::LineString(LineString::new(coords)))
    }
}

fn decode_polygon(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    let n_rings = c.read_u32(le)? as usize;
    if n_rings == 0 {
        return Ok(Geometry::Empty(GeometryType::Polygon));
    }
    let mut rings: Vec<LineString> = Vec::with_capacity(n_rings);
    for _ in 0..n_rings {
        rings.push(LineString::new(decode_linestring_coords(c, le)?));
    }
    let exterior = rings.remove(0);
    Ok(Geometry::Polygon(Polygon::new(exterior, rings)))
}

fn decode_multipoint(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    // Children have their own headers per ISO WKB. The outer `le` flag is
    // ignored once we're inside the child.
    let _ = le;
    let n = c.read_u32_after_check(le)?;
    if n == 0 {
        return Ok(Geometry::Empty(GeometryType::MultiPoint));
    }
    let mut pts = Vec::with_capacity(n);
    for _ in 0..n {
        match decode_geometry(c)? {
            Geometry::Point(p) => pts.push(p),
            Geometry::Empty(GeometryType::Point) => pts.push(Coord::xy(f64::NAN, f64::NAN)),
            other => {
                return Err(Error::malformed(format!(
                    "MultiPoint child must be Point, got {:?}",
                    other.type_of()
                )))
            }
        }
    }
    Ok(Geometry::MultiPoint(pts))
}

fn decode_multilinestring(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    let n = c.read_u32_after_check(le)?;
    if n == 0 {
        return Ok(Geometry::Empty(GeometryType::MultiLineString));
    }
    let mut parts = Vec::with_capacity(n);
    for _ in 0..n {
        match decode_geometry(c)? {
            Geometry::LineString(ls) => parts.push(ls),
            Geometry::Empty(GeometryType::LineString) => parts.push(LineString::default()),
            other => {
                return Err(Error::malformed(format!(
                    "MultiLineString child must be LineString, got {:?}",
                    other.type_of()
                )))
            }
        }
    }
    Ok(Geometry::MultiLineString(parts))
}

fn decode_multipolygon(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    let n = c.read_u32_after_check(le)?;
    if n == 0 {
        return Ok(Geometry::Empty(GeometryType::MultiPolygon));
    }
    let mut polys = Vec::with_capacity(n);
    for _ in 0..n {
        match decode_geometry(c)? {
            Geometry::Polygon(p) => polys.push(p),
            Geometry::Empty(GeometryType::Polygon) => polys.push(Polygon::default()),
            other => {
                return Err(Error::malformed(format!(
                    "MultiPolygon child must be Polygon, got {:?}",
                    other.type_of()
                )))
            }
        }
    }
    Ok(Geometry::MultiPolygon(polys))
}

fn decode_collection(c: &mut WkbCursor, le: bool) -> Result<Geometry> {
    let n = c.read_u32_after_check(le)?;
    if n == 0 {
        return Ok(Geometry::Empty(GeometryType::GeometryCollection));
    }
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(decode_geometry(c)?);
    }
    Ok(Geometry::GeometryCollection(v))
}

impl<'a> WkbCursor<'a> {
    /// Read the child count u32, using the parent's byte order (the preamble
    /// for the children themselves is read by recursing into `decode_geometry`).
    fn read_u32_after_check(&mut self, le: bool) -> Result<usize> {
        Ok(self.read_u32(le)? as usize)
    }
}

// ============================================================================
// Bbox walker — same control flow as decoder, but no allocations.
// ============================================================================

fn walk_bbox(c: &mut WkbCursor, acc: &mut BboxAcc) -> Result<()> {
    let (le, base, has_z, has_m) = c.read_preamble()?;
    // Even for Z/M we could compute a 2D bbox by skipping extra ordinates,
    // but v0.1 stays consistent with the decoder: reject Z/M.
    if has_z || has_m {
        return Err(Error::unsupported("WKB Z/M (v0.1 is 2D-only)".to_string()));
    }
    match base {
        1 => {
            let x = c.read_f64(le)?;
            let y = c.read_f64(le)?;
            // POINT EMPTY = NaN/NaN, filtered by `acc.add`.
            acc.add(x, y);
        }
        2 => walk_xy_array(c, le, acc)?,
        3 => {
            let n_rings = c.read_u32(le)? as usize;
            for _ in 0..n_rings {
                walk_xy_array(c, le, acc)?;
            }
        }
        4 | 5 | 6 | 7 => {
            let n = c.read_u32(le)? as usize;
            for _ in 0..n {
                walk_bbox(c, acc)?;
            }
        }
        other => return Err(Error::unsupported(format!("WKB geometry type code {other}"))),
    }
    Ok(())
}

fn walk_xy_array(c: &mut WkbCursor, le: bool, acc: &mut BboxAcc) -> Result<()> {
    let n = c.read_u32(le)? as usize;
    for _ in 0..n {
        let x = c.read_f64(le)?;
        let y = c.read_f64(le)?;
        acc.add(x, y);
    }
    Ok(())
}

#[cfg(test)]
mod decoder_tests {
    use super::*;

    fn round_trip(g: &Geometry) {
        let bytes = g.to_wkb();
        let back = Geometry::from_wkb(&bytes).expect("decode");
        assert_eq!(&back, g, "round-trip mismatch for {g:?}");
    }

    #[test]
    fn round_trip_point() {
        round_trip(&Geometry::Point(Coord::xy(1.0, 2.0)));
        round_trip(&Geometry::Point(Coord::xy(-180.0, 90.0)));
    }

    #[test]
    fn round_trip_point_empty() {
        let g = Geometry::Empty(GeometryType::Point);
        let bytes = g.to_wkb();
        let back = Geometry::from_wkb(&bytes).unwrap();
        // POINT EMPTY round-trips through NaN/NaN and is normalized back to Empty.
        assert_eq!(back, g);
    }

    #[test]
    fn round_trip_linestring() {
        let ls = LineString::new(vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(1.0, 1.0),
            Coord::xy(2.0, 0.0),
        ]);
        round_trip(&Geometry::LineString(ls));
    }

    #[test]
    fn round_trip_polygon_with_hole() {
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
        round_trip(&Geometry::Polygon(p));
    }

    #[test]
    fn round_trip_multipoint() {
        round_trip(&Geometry::MultiPoint(vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(1.0, 2.0),
            Coord::xy(-3.0, 4.5),
        ]));
    }

    #[test]
    fn round_trip_multilinestring() {
        round_trip(&Geometry::MultiLineString(vec![
            LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xy(1.0, 1.0)]),
            LineString::new(vec![
                Coord::xy(2.0, 2.0),
                Coord::xy(3.0, 3.0),
                Coord::xy(4.0, 4.0),
            ]),
        ]));
    }

    #[test]
    fn round_trip_multipolygon() {
        let p1 = Polygon::new(
            LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(1.0, 0.0),
                Coord::xy(1.0, 1.0),
                Coord::xy(0.0, 0.0),
            ]),
            vec![],
        );
        let p2 = Polygon::new(
            LineString::new(vec![
                Coord::xy(10.0, 10.0),
                Coord::xy(11.0, 10.0),
                Coord::xy(11.0, 11.0),
                Coord::xy(10.0, 10.0),
            ]),
            vec![],
        );
        round_trip(&Geometry::MultiPolygon(vec![p1, p2]));
    }

    #[test]
    fn round_trip_geometry_collection() {
        round_trip(&Geometry::GeometryCollection(vec![
            Geometry::Point(Coord::xy(1.0, 2.0)),
            Geometry::LineString(LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(1.0, 1.0),
            ])),
        ]));
    }

    #[test]
    fn round_trip_empty_collection_shapes() {
        for t in [
            GeometryType::LineString,
            GeometryType::Polygon,
            GeometryType::MultiPoint,
            GeometryType::MultiLineString,
            GeometryType::MultiPolygon,
            GeometryType::GeometryCollection,
        ] {
            round_trip(&Geometry::Empty(t));
        }
    }

    #[test]
    fn decode_be_encoded_point() {
        // Hand-craft a big-endian Point(3.0, 4.0).
        let mut buf = vec![0u8]; // BE
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&3.0f64.to_be_bytes());
        buf.extend_from_slice(&4.0f64.to_be_bytes());
        let g = Geometry::from_wkb(&buf).unwrap();
        assert_eq!(g, Geometry::Point(Coord::xy(3.0, 4.0)));
    }

    #[test]
    fn decode_mixed_endian_collection() {
        // LE outer GeometryCollection containing one BE Point and one LE Point.
        let mut buf = Vec::new();
        buf.push(1u8);
        buf.extend_from_slice(&7u32.to_le_bytes()); // collection
        buf.extend_from_slice(&2u32.to_le_bytes()); // 2 children
        // BE point (1.0, 2.0)
        buf.push(0u8);
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&1.0f64.to_be_bytes());
        buf.extend_from_slice(&2.0f64.to_be_bytes());
        // LE point (3.0, 4.0)
        buf.push(1u8);
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&3.0f64.to_le_bytes());
        buf.extend_from_slice(&4.0f64.to_le_bytes());

        let g = Geometry::from_wkb(&buf).unwrap();
        match g {
            Geometry::GeometryCollection(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], Geometry::Point(Coord::xy(1.0, 2.0)));
                assert_eq!(v[1], Geometry::Point(Coord::xy(3.0, 4.0)));
            }
            _ => panic!("expected GeometryCollection"),
        }
    }

    #[test]
    fn truncated_input_errors_cleanly() {
        let truncated = &[1u8, 1, 0, 0, 0]; // type code only, no x/y
        assert!(Geometry::from_wkb(truncated).is_err());
    }

    #[test]
    fn z_variant_errors_as_unsupported() {
        // PointZ via ISO high bit
        let mut buf = vec![1u8];
        buf.extend_from_slice(&(1u32 | ISO_Z_FLAG).to_le_bytes());
        buf.extend_from_slice(&1.0f64.to_le_bytes());
        buf.extend_from_slice(&2.0f64.to_le_bytes());
        buf.extend_from_slice(&3.0f64.to_le_bytes()); // z
        let err = Geometry::from_wkb(&buf).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));

        // PointZ via 1001 extended code
        let mut buf = vec![1u8];
        buf.extend_from_slice(&1001u32.to_le_bytes());
        let err = Geometry::from_wkb(&buf).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn bad_byte_order_byte_errors() {
        let buf = [9u8, 1, 0, 0, 0]; // bo=9 (invalid)
        assert!(Geometry::from_wkb(&buf).is_err());
    }
}

#[cfg(test)]
mod bbox_walker_tests {
    use super::*;

    fn bbox_via_walker(g: &Geometry) -> Option<[f64; 4]> {
        bbox_from_bytes(&g.to_wkb())
    }

    #[test]
    fn bbox_walker_matches_geometry_bbox_for_all_variants() {
        let cases: Vec<Geometry> = vec![
            Geometry::Point(Coord::xy(5.0, 7.0)),
            Geometry::LineString(LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(10.0, -5.0),
                Coord::xy(2.0, 8.0),
            ])),
            Geometry::Polygon(Polygon::new(
                LineString::new(vec![
                    Coord::xy(0.0, 0.0),
                    Coord::xy(10.0, 0.0),
                    Coord::xy(10.0, 10.0),
                    Coord::xy(0.0, 10.0),
                    Coord::xy(0.0, 0.0),
                ]),
                vec![],
            )),
            Geometry::MultiPoint(vec![Coord::xy(-1.0, -1.0), Coord::xy(1.0, 1.0)]),
            Geometry::MultiLineString(vec![
                LineString::new(vec![Coord::xy(0.0, 0.0), Coord::xy(5.0, 5.0)]),
                LineString::new(vec![Coord::xy(100.0, -100.0), Coord::xy(101.0, -99.0)]),
            ]),
            Geometry::GeometryCollection(vec![
                Geometry::Point(Coord::xy(-50.0, -50.0)),
                Geometry::Point(Coord::xy(50.0, 50.0)),
            ]),
        ];
        for g in cases {
            assert_eq!(
                bbox_via_walker(&g),
                g.bbox(),
                "walker disagrees with Geometry::bbox for {g:?}"
            );
        }
    }

    #[test]
    fn bbox_walker_returns_none_for_empty_geoms() {
        for t in [
            GeometryType::Point,
            GeometryType::LineString,
            GeometryType::Polygon,
            GeometryType::MultiPolygon,
            GeometryType::GeometryCollection,
        ] {
            assert!(bbox_via_walker(&Geometry::Empty(t)).is_none(), "{t:?}");
        }
    }

    #[test]
    fn bbox_walker_returns_none_for_malformed_input() {
        assert!(bbox_from_bytes(&[1u8, 1, 0, 0, 0]).is_none()); // truncated point
        assert!(bbox_from_bytes(&[]).is_none()); // empty
        assert!(bbox_from_bytes(&[1, 99, 99, 99, 99]).is_none()); // bad type code
    }
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

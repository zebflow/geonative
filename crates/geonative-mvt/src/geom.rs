//! Encode a `Geometry` as MVT 2.1's command-stream (`repeated uint32 geometry`).
//!
//! Per the MVT spec, a feature's geometry is a flat sequence of varints
//! interpreted as alternating **command integers** and **parameter pairs**.
//! A command integer packs `(cmd_id (3 bits) | count (29 bits))`. Commands:
//!
//! | id | name | params per occurrence | usage |
//! | :-: | --- | :-: | --- |
//! | 1 | MoveTo | 1 (dx, dy) | first point of each part |
//! | 2 | LineTo | 1 (dx, dy) | subsequent points |
//! | 7 | ClosePath | 0 | end of a polygon ring |
//!
//! Parameters are signed deltas from the **last emitted point**, accumulating
//! across commands and even across parts of a multi-geometry. They are
//! emitted as zigzag-encoded varints.
//!
//! ## Per-type encoding
//!
//! - **Point**: MoveTo(1) + one (dx, dy)
//! - **MultiPoint**: MoveTo(N) + N (dx, dy) pairs
//! - **LineString**: MoveTo(1) + LineTo(N-1)
//! - **MultiLineString**: per part, MoveTo(1) + LineTo(N-1), with the delta
//!   accumulator carrying through to the next part
//! - **Polygon**: per ring, MoveTo(1) + LineTo(N-2) + ClosePath. The MVT spec
//!   omits the duplicate closing vertex (the IR has it; we drop it).
//! - **MultiPolygon**: per polygon, per ring, same as Polygon
//! - **GeometryCollection / Empty**: rejected — MVT layers carry one
//!   `GeomType` so collections don't fit cleanly. Callers should split or
//!   flatten before encoding.

use geonative_core::{Coord, Geometry, LineString, Polygon};
use geonative_tile::{LngLat, TileCoord};

use crate::error::{MvtError, Result};
use crate::proto::{write_varint, zigzag_encode};

/// MVT `GeomType` enum values (matches the protobuf schema).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MvtGeomType {
    Unknown = 0,
    Point = 1,
    LineString = 2,
    Polygon = 3,
}

/// The MVT `GeomType` that maps a `Geometry`, or an error for the cases the
/// MVT format can't represent.
pub fn classify(g: &Geometry) -> Result<MvtGeomType> {
    Ok(match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => MvtGeomType::Point,
        Geometry::LineString(_) | Geometry::MultiLineString(_) => MvtGeomType::LineString,
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => MvtGeomType::Polygon,
        Geometry::GeometryCollection(_) => {
            return Err(MvtError::Unsupported(
                "GeometryCollection — flatten before encoding".into(),
            ))
        }
        Geometry::Empty(_) => MvtGeomType::Unknown,
        // Future Geometry variants (e.g. curves, surfaces) — refuse rather
        // than silently misclassify.
        _ => {
            return Err(MvtError::Unsupported(
                "unrecognized Geometry variant — geonative-core may be newer than this crate"
                    .into(),
            ))
        }
    })
}

/// Encode `geom` (in WGS84 lng/lat) into the MVT command-stream, projecting
/// into the integer tile-pixel grid of `tile` at `extent`. Appends to `out`.
pub fn encode_geometry(
    geom: &Geometry,
    tile: TileCoord,
    extent: u32,
    out: &mut Vec<u32>,
) -> Result<()> {
    let mut cursor = Cursor::new(tile, extent);
    match geom {
        Geometry::Point(c) => cursor.move_to(out, std::slice::from_ref(c)),
        Geometry::MultiPoint(coords) => cursor.move_to(out, coords),
        Geometry::LineString(ls) => cursor.linestring(out, ls),
        Geometry::MultiLineString(parts) => {
            for ls in parts {
                cursor.linestring(out, ls);
            }
        }
        Geometry::Polygon(p) => cursor.polygon(out, p),
        Geometry::MultiPolygon(polys) => {
            for p in polys {
                cursor.polygon(out, p);
            }
        }
        Geometry::Empty(_) => {} // produce no commands
        Geometry::GeometryCollection(_) => {
            return Err(MvtError::Unsupported(
                "GeometryCollection — flatten before encoding".into(),
            ))
        }
        // Future Geometry variants — refuse cleanly.
        _ => {
            return Err(MvtError::Unsupported(
                "unrecognized Geometry variant — geonative-core may be newer than this crate"
                    .into(),
            ))
        }
    }
    Ok(())
}

const CMD_MOVE_TO: u32 = 1;
const CMD_LINE_TO: u32 = 2;
const CMD_CLOSE_PATH: u32 = 7;

/// Helper that owns the (cx, cy) delta accumulator across all commands of one
/// feature and the projection from lng/lat into integer tile pixels.
struct Cursor {
    tile: TileCoord,
    extent: u32,
    cx: i32,
    cy: i32,
}

impl Cursor {
    fn new(tile: TileCoord, extent: u32) -> Self {
        Self {
            tile,
            extent,
            cx: 0,
            cy: 0,
        }
    }

    fn project(&self, c: &Coord) -> (i32, i32) {
        self.tile.project_lnglat(LngLat::new(c.x, c.y), self.extent)
    }

    fn command_integer(cmd: u32, count: u32) -> u32 {
        (cmd & 0x7) | (count << 3)
    }

    fn push_deltas_for(&mut self, out: &mut Vec<u32>, coords: &[Coord]) {
        for c in coords {
            let (x, y) = self.project(c);
            let dx = x.wrapping_sub(self.cx);
            let dy = y.wrapping_sub(self.cy);
            self.cx = x;
            self.cy = y;
            out.push(zigzag_u32(dx));
            out.push(zigzag_u32(dy));
        }
    }

    fn move_to(&mut self, out: &mut Vec<u32>, coords: &[Coord]) {
        if coords.is_empty() {
            return;
        }
        out.push(Self::command_integer(CMD_MOVE_TO, coords.len() as u32));
        self.push_deltas_for(out, coords);
    }

    fn line_to(&mut self, out: &mut Vec<u32>, coords: &[Coord]) {
        if coords.is_empty() {
            return;
        }
        out.push(Self::command_integer(CMD_LINE_TO, coords.len() as u32));
        self.push_deltas_for(out, coords);
    }

    fn close_path(&mut self, out: &mut Vec<u32>) {
        out.push(Self::command_integer(CMD_CLOSE_PATH, 1));
    }

    fn linestring(&mut self, out: &mut Vec<u32>, ls: &LineString) {
        if ls.coords.is_empty() {
            return;
        }
        // First point is a MoveTo; rest are LineTo.
        let (first, rest) = ls.coords.split_first().unwrap();
        self.move_to(out, std::slice::from_ref(first));
        self.line_to(out, rest);
    }

    fn polygon(&mut self, out: &mut Vec<u32>, p: &Polygon) {
        self.ring(out, &p.exterior);
        for hole in &p.holes {
            self.ring(out, hole);
        }
    }

    fn ring(&mut self, out: &mut Vec<u32>, ring: &LineString) {
        if ring.coords.is_empty() {
            return;
        }
        // MVT rings omit the duplicate closing vertex; emit MoveTo(first) +
        // LineTo(middle..) + ClosePath. The IR closes its rings explicitly,
        // so we strip the trailing duplicate if present.
        let coords: &[Coord] =
            if ring.coords.len() >= 2 && ring.coords.first() == ring.coords.last() {
                &ring.coords[..ring.coords.len() - 1]
            } else {
                &ring.coords
            };
        if coords.is_empty() {
            return;
        }
        let (first, rest) = coords.split_first().unwrap();
        self.move_to(out, std::slice::from_ref(first));
        self.line_to(out, rest);
        self.close_path(out);
    }
}

/// Cast i32 → u32 via zigzag-of-i64 (handles the full i32 range cleanly).
fn zigzag_u32(n: i32) -> u32 {
    zigzag_encode(n as i64) as u32
}

/// Length-prefixed encode the command stream + return the bytes that go into
/// the protobuf `geometry` field (already packed as varints).
pub fn pack_command_stream(commands: &[u32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(commands.len() * 2);
    for &v in commands {
        write_varint(&mut buf, v as u64);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::LineString;

    fn z0_tile() -> TileCoord {
        TileCoord::new(0, 0, 0)
    }

    #[test]
    fn point_at_origin_produces_one_moveto() {
        // Top-left of z=0 tile is at (-180, 85.05). Use that — projects to (0, 0).
        let mut out = Vec::new();
        let p = Coord {
            x: -180.0,
            y: 85.051_128_779_806_59,
            z: None,
            m: None,
        };
        encode_geometry(&Geometry::Point(p), z0_tile(), 4096, &mut out).unwrap();
        // command_integer(MoveTo, 1) = (1 & 0x7) | (1 << 3) = 9
        assert_eq!(out[0], 9);
        // dx = 0 → zigzag 0; dy = 0 → zigzag 0
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 0);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn multipoint_emits_one_moveto_with_count_n() {
        let g = Geometry::MultiPoint(vec![
            Coord {
                x: 0.0,
                y: 0.0,
                z: None,
                m: None,
            },
            Coord {
                x: 90.0,
                y: 0.0,
                z: None,
                m: None,
            },
        ]);
        let mut out = Vec::new();
        encode_geometry(&g, z0_tile(), 4096, &mut out).unwrap();
        // (MoveTo, 2) = 1 | (2 << 3) = 17
        assert_eq!(out[0], 17);
        // 2 points * 2 params = 4 params total, plus 1 command = 5 entries
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn linestring_emits_moveto_then_lineto() {
        let ls = LineString::new(vec![
            Coord {
                x: 0.0,
                y: 0.0,
                z: None,
                m: None,
            },
            Coord {
                x: 10.0,
                y: 0.0,
                z: None,
                m: None,
            },
            Coord {
                x: 10.0,
                y: 10.0,
                z: None,
                m: None,
            },
        ]);
        let mut out = Vec::new();
        encode_geometry(&Geometry::LineString(ls), z0_tile(), 4096, &mut out).unwrap();
        // First command: MoveTo(1) = 9
        assert_eq!(out[0], 9);
        // First param pair (2 values)
        // Second command: LineTo(2) = 2 | (2 << 3) = 18
        assert_eq!(out[3], 18);
        // Then 2 * 2 = 4 params for LineTo
        assert_eq!(out.len(), 8); // 1 + 2 + 1 + 4
    }

    #[test]
    fn polygon_ring_closes_with_closepath_command() {
        let ring = LineString::new(vec![
            Coord {
                x: 0.0,
                y: 0.0,
                z: None,
                m: None,
            },
            Coord {
                x: 10.0,
                y: 0.0,
                z: None,
                m: None,
            },
            Coord {
                x: 10.0,
                y: 10.0,
                z: None,
                m: None,
            },
            Coord {
                x: 0.0,
                y: 10.0,
                z: None,
                m: None,
            },
            Coord {
                x: 0.0,
                y: 0.0,
                z: None,
                m: None,
            }, // closing duplicate
        ]);
        let p = Polygon::new(ring, vec![]);
        let mut out = Vec::new();
        encode_geometry(&Geometry::Polygon(p), z0_tile(), 4096, &mut out).unwrap();
        // The closing duplicate should be dropped; we should see exactly 4
        // distinct points = MoveTo(1) + LineTo(3) + ClosePath
        // = 1 + 2 + 1 + 6 + 1 = 11 entries
        assert_eq!(out.len(), 11);
        // last entry must be the ClosePath command int = 7 | (1 << 3) = 15
        assert_eq!(*out.last().unwrap(), 15);
    }

    #[test]
    fn geometry_collection_is_rejected() {
        let g = Geometry::GeometryCollection(vec![]);
        let err = encode_geometry(&g, z0_tile(), 4096, &mut Vec::new()).unwrap_err();
        assert!(matches!(err, MvtError::Unsupported(_)));
    }

    #[test]
    fn classify_maps_each_variant() {
        assert_eq!(
            classify(&Geometry::Point(Coord {
                x: 0.0,
                y: 0.0,
                z: None,
                m: None
            }))
            .unwrap(),
            MvtGeomType::Point
        );
        assert_eq!(
            classify(&Geometry::LineString(LineString::default())).unwrap(),
            MvtGeomType::LineString
        );
        assert_eq!(
            classify(&Geometry::Polygon(Polygon::default())).unwrap(),
            MvtGeomType::Polygon
        );
        assert_eq!(
            classify(&Geometry::Empty(geonative_core::GeometryType::Point)).unwrap(),
            MvtGeomType::Unknown
        );
    }

    #[test]
    fn deltas_accumulate_across_multipoint_points() {
        // Verify the cursor properly emits cumulative deltas: second point's
        // delta should be from FIRST point, not from origin.
        let g = Geometry::MultiPoint(vec![
            Coord {
                x: -180.0,
                y: 85.051_128_779_806_59,
                z: None,
                m: None,
            }, // projects to (0, 0)
            Coord {
                x: 0.0,
                y: 85.051_128_779_806_59,
                z: None,
                m: None,
            }, // projects to (2048, 0) at z=0/extent=4096
        ]);
        let mut out = Vec::new();
        encode_geometry(&g, z0_tile(), 4096, &mut out).unwrap();
        // MoveTo(2) = 17; then dx=0 dy=0 (zigzag 0); then dx=2048 dy=0 → zigzag(2048)=4096 zigzag(0)=0
        assert_eq!(out[0], 17);
        assert_eq!(out[1], 0); // first dx
        assert_eq!(out[2], 0); // first dy
        assert_eq!(out[3], 4096); // second dx (zigzag of +2048)
        assert_eq!(out[4], 0);
    }
}

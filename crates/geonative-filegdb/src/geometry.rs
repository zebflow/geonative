//! Shape buffer geometry decoder.
//!
//! Decodes FileGDB's compact varuint/varint-delta geometry encoding into the
//! geonative-core IR. Layout reference: GDAL `openfilegdb/filegdbtable.cpp`
//! (`GetAsGeometry`, `ReadPartDefs`, `ReadXYArray`).
//!
//! ## Encoding overview
//!
//! - **First field:** varuint geometry-type code. Low byte selects the base
//!   type (see [`SHPT`] constants). High bits flag Z/M/curve (unsupported in
//!   v0.1).
//! - **Point:** two varuints `vx`, `vy`. A stored value of `0` means "this
//!   ordinate is empty"; otherwise the quantized value is `vx - 1`, decoded
//!   as `(vx - 1) / xyscale + xorigin`.
//! - **Multipoint / Polyline / Polygon:** varuint nPoints; if non-zero, then
//!   varuint nParts (for polyline/polygon); skip 4 varuints (bbox xmin / ymin /
//!   dx / dy); then nParts-1 varuint per-part point counts (last is implicit);
//!   then `nPoints` (dx_varint, dy_varint) pairs walked as **cumulative
//!   deltas** through a single accumulator across all parts.
//! - **Dequantization** (for delta-encoded coords): `x = dxAcc / xyscale +
//!   xorigin`. No `-1` offset (that's only for the single-Point encoding).
//!
//! ## v0.1 scope
//!
//! - 2D Point (1), MultiPoint (8), Polyline (3), Polygon (5).
//! - Z/M variants (codes 9, 10, 11, 13, 15, 18, 19, 20, 21, 23, 25, 28, 31, 32),
//!   curves (50–54), and multipatch return [`GdbError::Unsupported`].
//!
//! ## Polygon ring orientation
//!
//! FileGDB uses **Esri convention**: clockwise = exterior ring, CCW = interior
//! (hole). The geonative-core IR is **OGC convention**: exterior CCW, interior
//! CW. We re-orient every ring on read by reversing its point order, so the
//! emitted [`Polygon`] is OGC-compliant.

use geonative_core::{Coord, Geometry, GeometryType, LineString, Polygon};

use crate::bytes::LeReader;
use crate::error::{GdbError, Result};
use crate::table::GeomFieldMeta;

/// Esri shape-type codes (low byte of the varuint geometry-type field).
#[allow(non_snake_case, dead_code)]
pub mod SHPT {
    pub const POINT: u32 = 1;
    pub const ARC: u32 = 3; // polyline
    pub const POLYGON: u32 = 5;
    pub const MULTIPOINT: u32 = 8;
    // Z/M and curve variants — recognised but unsupported in v0.1:
    pub const POINTZ: u32 = 9;
    pub const ARCZ: u32 = 10;
    pub const POINTZM: u32 = 11;
    pub const ARCZM: u32 = 13;
    pub const POLYGONZM: u32 = 15;
    pub const MULTIPOINTZM: u32 = 18;
    pub const POLYGONZ: u32 = 19;
    pub const MULTIPOINTZ: u32 = 20;
    pub const POINTM: u32 = 21;
    pub const ARCM: u32 = 23;
    pub const POLYGONM: u32 = 25;
    pub const MULTIPOINTM: u32 = 28;
    pub const MULTIPATCHM: u32 = 31;
    pub const MULTIPATCH: u32 = 32;
    pub const GENERAL_POLYLINE: u32 = 50;
    pub const GENERAL_POLYGON: u32 = 51;
    pub const GENERAL_POINT: u32 = 52;
    pub const GENERAL_MULTIPOINT: u32 = 53;
    pub const GENERAL_MULTIPATCH: u32 = 54;
}

const EXT_SHAPE_Z_FLAG: u32 = 0x8000_0000;
const EXT_SHAPE_M_FLAG: u32 = 0x4000_0000;
const EXT_SHAPE_CURVE_FLAG: u32 = 0x2000_0000;

/// Decode a shape-buffer blob (the bytes captured by
/// [`crate::DecodedRow::geometry_blob`]) into a [`Geometry`].
///
/// "General" type codes (50/51/52/53) carry the same coordinate encoding as
/// their non-General counterparts; the curve flag adds an `nCurves` field
/// after `nParts` plus segment-modifier records at the end of the blob.
/// **In v0.1 we silently drop curve descriptions** — the geometry is
/// reconstructed from the linear coordinate samples only.
pub fn decode_shape_buffer(blob: &[u8], meta: &GeomFieldMeta) -> Result<Geometry> {
    let mut r = LeReader::new(blob);
    let geom_type = r.read_varuint()? as u32;

    let has_z = geom_type & EXT_SHAPE_Z_FLAG != 0;
    let has_m = geom_type & EXT_SHAPE_M_FLAG != 0;
    let has_curve = geom_type & EXT_SHAPE_CURVE_FLAG != 0;

    if has_z || has_m {
        return Err(GdbError::unsupported(format!(
            "v0.1: Z/M geometry not supported (type 0x{geom_type:08X})"
        )));
    }

    let base_type = geom_type & 0xFF;
    match base_type {
        SHPT::POINT | SHPT::GENERAL_POINT => decode_point(&mut r, meta),
        SHPT::MULTIPOINT | SHPT::GENERAL_MULTIPOINT => decode_multipoint(&mut r, meta, has_curve),
        SHPT::ARC | SHPT::GENERAL_POLYLINE => decode_polyline(&mut r, meta, has_curve),
        SHPT::POLYGON | SHPT::GENERAL_POLYGON => decode_polygon(&mut r, meta, has_curve),

        // Z/M base-type variants: not supported in v0.1.
        SHPT::POINTZ
        | SHPT::POINTZM
        | SHPT::POINTM
        | SHPT::ARCZ
        | SHPT::ARCZM
        | SHPT::ARCM
        | SHPT::POLYGONZ
        | SHPT::POLYGONZM
        | SHPT::POLYGONM
        | SHPT::MULTIPOINTZ
        | SHPT::MULTIPOINTZM
        | SHPT::MULTIPOINTM => Err(GdbError::unsupported(format!(
            "v0.1: geometry type code {base_type} has Z/M ordinates; only 2D supported"
        ))),

        SHPT::MULTIPATCH | SHPT::MULTIPATCHM | SHPT::GENERAL_MULTIPATCH => Err(
            GdbError::unsupported("v0.1: multipatch geometry not supported"),
        ),

        other => Err(GdbError::unsupported(format!(
            "unknown geometry type code {other}"
        ))),
    }
}

fn decode_point(r: &mut LeReader, meta: &GeomFieldMeta) -> Result<Geometry> {
    let vx = r.read_varuint()?;
    let vy = r.read_varuint()?;
    if vx == 0 && vy == 0 {
        return Ok(Geometry::Empty(GeometryType::Point));
    }
    let x = if vx == 0 {
        f64::NAN
    } else {
        ((vx - 1) as f64) / meta.xyscale + meta.xorigin
    };
    let y = if vy == 0 {
        f64::NAN
    } else {
        ((vy - 1) as f64) / meta.xyscale + meta.yorigin
    };
    Ok(Geometry::Point(Coord::xy(x, y)))
}

fn decode_multipoint(r: &mut LeReader, meta: &GeomFieldMeta, has_curve: bool) -> Result<Geometry> {
    let n_points = r.read_varuint()? as usize;
    if n_points == 0 {
        return Ok(Geometry::Empty(GeometryType::MultiPoint));
    }
    if has_curve {
        let _n_curves = r.read_varuint()?; // curves on multipoint is unusual; ignore
    }
    skip_varuints(r, 4)?; // bbox xmin/ymin/dx/dy
    let coords = read_xy_array(r, n_points, meta)?;
    Ok(Geometry::MultiPoint(coords))
}

fn decode_polyline(r: &mut LeReader, meta: &GeomFieldMeta, has_curve: bool) -> Result<Geometry> {
    let (point_counts, all_coords) = read_parts_and_coords(r, meta, has_curve)?;
    if point_counts.is_empty() {
        return Ok(Geometry::Empty(GeometryType::LineString));
    }
    let parts = split_into_parts(all_coords, &point_counts);
    if parts.len() == 1 {
        Ok(Geometry::LineString(parts.into_iter().next().unwrap()))
    } else {
        Ok(Geometry::MultiLineString(parts))
    }
}

fn decode_polygon(r: &mut LeReader, meta: &GeomFieldMeta, has_curve: bool) -> Result<Geometry> {
    let (point_counts, all_coords) = read_parts_and_coords(r, meta, has_curve)?;
    if point_counts.is_empty() {
        return Ok(Geometry::Empty(GeometryType::Polygon));
    }
    let rings = split_into_parts(all_coords, &point_counts);
    let polygons = group_rings_esri_to_ogc(rings);
    if polygons.len() == 1 {
        Ok(Geometry::Polygon(polygons.into_iter().next().unwrap()))
    } else {
        Ok(Geometry::MultiPolygon(polygons))
    }
}

fn read_parts_and_coords(
    r: &mut LeReader,
    meta: &GeomFieldMeta,
    has_curve: bool,
) -> Result<(Vec<u32>, Vec<Coord>)> {
    let n_points = r.read_varuint()? as usize;
    if n_points == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let n_parts = r.read_varuint()? as usize;
    if n_parts == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    if has_curve {
        // For General* types with the curve flag, an nCurves varuint sits
        // between nParts and the bbox. The curve-description records follow
        // the coordinate array — we don't read them, so the geometry is
        // reconstructed from linear coordinate samples only (v0.1 limitation).
        let _n_curves = r.read_varuint()?;
    }
    skip_varuints(r, 4)?; // bbox xmin/ymin/dx/dy

    // Read first nParts-1 part lengths; last is total - sum.
    let mut point_counts: Vec<u32> = Vec::with_capacity(n_parts);
    let mut sum: u32 = 0;
    for _ in 0..n_parts.saturating_sub(1) {
        let c = r.read_varuint()? as u32;
        point_counts.push(c);
        sum = sum.saturating_add(c);
    }
    let total = n_points as u32;
    if sum > total {
        return Err(GdbError::malformed(format!(
            "geometry part-count sum {sum} exceeds total point count {total}"
        )));
    }
    point_counts.push(total - sum);

    let coords = read_xy_array(r, n_points, meta)?;
    Ok((point_counts, coords))
}

fn read_xy_array(r: &mut LeReader, n_points: usize, meta: &GeomFieldMeta) -> Result<Vec<Coord>> {
    let mut coords = Vec::with_capacity(n_points);
    let mut dx_acc: i64 = 0;
    let mut dy_acc: i64 = 0;
    for _ in 0..n_points {
        dx_acc = dx_acc.wrapping_add(r.read_varint()?);
        dy_acc = dy_acc.wrapping_add(r.read_varint()?);
        let x = (dx_acc as f64) / meta.xyscale + meta.xorigin;
        let y = (dy_acc as f64) / meta.xyscale + meta.yorigin;
        coords.push(Coord::xy(x, y));
    }
    Ok(coords)
}

fn skip_varuints(r: &mut LeReader, n: usize) -> Result<()> {
    for _ in 0..n {
        let _ = r.read_varuint()?;
    }
    Ok(())
}

fn split_into_parts(coords: Vec<Coord>, point_counts: &[u32]) -> Vec<LineString> {
    let mut parts = Vec::with_capacity(point_counts.len());
    let mut idx = 0usize;
    for &count in point_counts {
        let count = count as usize;
        let part_coords: Vec<Coord> = coords[idx..idx + count].to_vec();
        parts.push(LineString::new(part_coords));
        idx += count;
    }
    parts
}

/// Group raw Esri rings (CW outer, CCW hole) into OGC polygons (CCW outer,
/// CW hole). Each CW ring opens a new polygon; subsequent CCW rings attach
/// as holes to the most recent polygon. Every ring is reversed so the
/// emitted polygon uses OGC orientation.
fn group_rings_esri_to_ogc(rings: Vec<LineString>) -> Vec<Polygon> {
    let mut polygons: Vec<Polygon> = Vec::new();
    for ring in rings {
        let is_outer_esri = signed_area(&ring.coords) < 0.0;
        let mut reversed = ring;
        reversed.coords.reverse();
        if is_outer_esri {
            polygons.push(Polygon::new(reversed, Vec::new()));
        } else if let Some(last) = polygons.last_mut() {
            last.holes.push(reversed);
        } else {
            // Orphan inner ring with no preceding outer — treat as its own
            // polygon (already reversed to CCW).
            polygons.push(Polygon::new(reversed, Vec::new()));
        }
    }
    polygons
}

/// Signed area via shoelace formula. Positive = CCW (math convention),
/// negative = CW. Used to detect Esri ring orientation.
fn signed_area(coords: &[Coord]) -> f64 {
    if coords.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    let n = coords.len();
    for i in 0..n {
        let j = (i + 1) % n;
        sum += coords[i].x * coords[j].y - coords[j].x * coords[i].y;
    }
    sum * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_xy(xorigin: f64, yorigin: f64, xyscale: f64) -> GeomFieldMeta {
        GeomFieldMeta {
            srs_wkt: String::new(),
            has_m_origin_scale_tolerance: false,
            has_z_origin_scale_tolerance: false,
            layer_has_m: false,
            layer_has_z: false,
            xorigin,
            yorigin,
            xyscale,
            morigin: None,
            mscale: None,
            zorigin: None,
            zscale: None,
            xytolerance: 0.0,
            mtolerance: None,
            ztolerance: None,
            extent_xy: [0.0; 4],
            extent_z: None,
            extent_m: None,
            grid_resolutions: vec![],
        }
    }

    fn enc_varuint(buf: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            buf.push(((v & 0x7F) as u8) | 0x80);
            v >>= 7;
        }
        buf.push(v as u8);
    }

    fn enc_varint(buf: &mut Vec<u8>, v: i64) {
        let (mag, sign_bit) = if v < 0 {
            ((-v) as u64, 0x40u8)
        } else {
            (v as u64, 0u8)
        };
        // first byte holds 6 low magnitude bits + sign bit + continuation
        let lo6 = (mag & 0x3F) as u8;
        let rest = mag >> 6;
        if rest == 0 {
            buf.push(lo6 | sign_bit);
        } else {
            buf.push(lo6 | sign_bit | 0x80);
            let mut x = rest;
            while x >= 0x80 {
                buf.push(((x & 0x7F) as u8) | 0x80);
                x >>= 7;
            }
            buf.push(x as u8);
        }
    }

    #[test]
    fn point_dequantize_roundtrip() {
        let meta = meta_xy(-400.0, -400.0, 1e8);
        // x = 145.0, y = -37.0 → quantized = (x - origin) * scale + 1
        let vx = ((145.0 - (-400.0)) * 1e8) as u64 + 1;
        let vy = (((-37.0) - (-400.0)) * 1e8) as u64 + 1;
        let mut buf = Vec::new();
        enc_varuint(&mut buf, SHPT::POINT as u64);
        enc_varuint(&mut buf, vx);
        enc_varuint(&mut buf, vy);

        let g = decode_shape_buffer(&buf, &meta).unwrap();
        match g {
            Geometry::Point(c) => {
                assert!((c.x - 145.0).abs() < 1e-6, "x={}", c.x);
                assert!((c.y - (-37.0)).abs() < 1e-6, "y={}", c.y);
            }
            other => panic!("expected Point, got {other:?}"),
        }
    }

    #[test]
    fn point_empty_when_both_zero() {
        let meta = meta_xy(0.0, 0.0, 1.0);
        let mut buf = Vec::new();
        enc_varuint(&mut buf, SHPT::POINT as u64);
        enc_varuint(&mut buf, 0);
        enc_varuint(&mut buf, 0);
        let g = decode_shape_buffer(&buf, &meta).unwrap();
        assert_eq!(g, Geometry::Empty(GeometryType::Point));
    }

    #[test]
    fn polyline_two_point_linestring() {
        let meta = meta_xy(0.0, 0.0, 1.0);
        let mut buf = Vec::new();
        enc_varuint(&mut buf, SHPT::ARC as u64);
        enc_varuint(&mut buf, 2); // 2 points
        enc_varuint(&mut buf, 1); // 1 part
        for _ in 0..4 {
            enc_varuint(&mut buf, 0);
        } // bbox stub
          // (no part-count entry because n_parts - 1 == 0)
          // coords: (10, 20), (30, 40) as cumulative deltas
        enc_varint(&mut buf, 10);
        enc_varint(&mut buf, 20);
        enc_varint(&mut buf, 20);
        enc_varint(&mut buf, 20);

        let g = decode_shape_buffer(&buf, &meta).unwrap();
        match g {
            Geometry::LineString(ls) => {
                assert_eq!(ls.coords.len(), 2);
                assert!((ls.coords[0].x - 10.0).abs() < 1e-9);
                assert!((ls.coords[0].y - 20.0).abs() < 1e-9);
                assert!((ls.coords[1].x - 30.0).abs() < 1e-9);
                assert!((ls.coords[1].y - 40.0).abs() < 1e-9);
            }
            other => panic!("expected LineString, got {other:?}"),
        }
    }

    #[test]
    fn polyline_two_part_becomes_multilinestring() {
        let meta = meta_xy(0.0, 0.0, 1.0);
        let mut buf = Vec::new();
        enc_varuint(&mut buf, SHPT::ARC as u64);
        enc_varuint(&mut buf, 5); // total 5 points
        enc_varuint(&mut buf, 2); // 2 parts
        for _ in 0..4 {
            enc_varuint(&mut buf, 0);
        } // bbox stub
        enc_varuint(&mut buf, 2); // part 0 has 2 points; part 1 implicit = 3
                                  // coords (cumulative across both parts)
        enc_varint(&mut buf, 1);
        enc_varint(&mut buf, 1); // (1,1)
        enc_varint(&mut buf, 1);
        enc_varint(&mut buf, 0); // (2,1)
        enc_varint(&mut buf, 10);
        enc_varint(&mut buf, 10); // (12,11)
        enc_varint(&mut buf, 1);
        enc_varint(&mut buf, 0); // (13,11)
        enc_varint(&mut buf, 0);
        enc_varint(&mut buf, 1); // (13,12)

        let g = decode_shape_buffer(&buf, &meta).unwrap();
        match g {
            Geometry::MultiLineString(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].coords.len(), 2);
                assert_eq!(parts[1].coords.len(), 3);
                assert_eq!(parts[0].coords[1], Coord::xy(2.0, 1.0));
                assert_eq!(parts[1].coords[0], Coord::xy(12.0, 11.0));
                assert_eq!(parts[1].coords[2], Coord::xy(13.0, 12.0));
            }
            other => panic!("expected MultiLineString, got {other:?}"),
        }
    }

    #[test]
    fn signed_area_sanity() {
        // CCW unit square — positive
        let ccw = vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(1.0, 0.0),
            Coord::xy(1.0, 1.0),
            Coord::xy(0.0, 1.0),
            Coord::xy(0.0, 0.0),
        ];
        assert!(signed_area(&ccw) > 0.0);
        // CW reversal — negative
        let cw: Vec<Coord> = ccw.iter().rev().copied().collect();
        assert!(signed_area(&cw) < 0.0);
    }

    #[test]
    fn unsupported_z_variant_errors() {
        let meta = meta_xy(0.0, 0.0, 1.0);
        let mut buf = Vec::new();
        enc_varuint(&mut buf, SHPT::ARCZ as u64);
        let err = decode_shape_buffer(&buf, &meta).unwrap_err();
        assert!(matches!(err, GdbError::Unsupported(_)));
    }

    #[test]
    fn ring_grouping_esri_to_ogc() {
        // Esri outer (CW in math) followed by Esri inner (CCW). After
        // grouping + re-orientation: 1 Polygon with 1 hole, both rings flipped.
        let cw_outer = LineString::new(vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(0.0, 10.0),
            Coord::xy(10.0, 10.0),
            Coord::xy(10.0, 0.0),
            Coord::xy(0.0, 0.0),
        ]);
        let ccw_inner = LineString::new(vec![
            Coord::xy(2.0, 2.0),
            Coord::xy(4.0, 2.0),
            Coord::xy(4.0, 4.0),
            Coord::xy(2.0, 4.0),
            Coord::xy(2.0, 2.0),
        ]);
        assert!(signed_area(&cw_outer.coords) < 0.0);
        assert!(signed_area(&ccw_inner.coords) > 0.0);

        let polygons = group_rings_esri_to_ogc(vec![cw_outer, ccw_inner]);
        assert_eq!(polygons.len(), 1);
        assert_eq!(polygons[0].holes.len(), 1);
        // Exterior should now be CCW (positive signed area).
        assert!(signed_area(&polygons[0].exterior.coords) > 0.0);
        // Hole should now be CW.
        assert!(signed_area(&polygons[0].holes[0].coords) < 0.0);
    }
}

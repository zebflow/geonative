//! Decode an individual shape record into a `geonative_core::Geometry`.
//!
//! ## Per-type layouts (2D only — Z/M deferred to v0.2)
//!
//! All offsets are within the record's **content** (i.e. after the 8-byte
//! record header). The first 4 bytes are always the LE shape-type code.
//!
//! - **Point (type 1)** — `type(4) + x(8) + y(8)` = 20 bytes
//! - **MultiPoint (type 8)** — `type(4) + bbox(32) + num_points(4) +
//!   points(16*N)`
//! - **PolyLine (type 3) / Polygon (type 5)** — `type(4) + bbox(32) +
//!   num_parts(4) + num_points(4) + parts[num_parts](4*P) +
//!   points[num_points](16*N)`. Parts are 0-based indices into the points
//!   array marking the start of each part/ring.
//!
//! ## Polygon ring orientation
//!
//! Shapefile uses **CW = exterior, CCW = hole** — the opposite of the OGC
//! convention used by geonative-core. We compute each ring's signed area to
//! determine its role, then reverse every ring so the emitted polygon ends
//! up CCW-exterior / CW-hole (OGC) as the IR expects.

use geonative_core::{Coord, Geometry, GeometryType, LineString, Polygon};

use crate::bytes::Cursor;
use crate::error::{Result, ShpError};
use crate::header::ShapeType;

pub fn decode_record_content(content: &[u8]) -> Result<Geometry> {
    let mut c = Cursor::new(content);
    let stype = ShapeType::from_i32(c.read_i32_le()?)?;
    match stype {
        ShapeType::Null => Ok(Geometry::Empty(GeometryType::Point)),
        ShapeType::Point => decode_point(&mut c),
        ShapeType::Multipoint => decode_multipoint(&mut c),
        ShapeType::Polyline => decode_polyline_or_polygon(&mut c, /* polygon */ false),
        ShapeType::Polygon => decode_polyline_or_polygon(&mut c, /* polygon */ true),
        other => Err(ShpError::unsupported(format!(
            "shape type {other:?} (v0.1 supports 2D Null/Point/Polyline/Polygon/MultiPoint)"
        ))),
    }
}

fn decode_point(c: &mut Cursor<'_>) -> Result<Geometry> {
    let x = c.read_f64_le()?;
    let y = c.read_f64_le()?;
    Ok(Geometry::Point(Coord {
        x,
        y,
        z: None,
        m: None,
    }))
}

fn decode_multipoint(c: &mut Cursor<'_>) -> Result<Geometry> {
    // Skip 32-byte bbox.
    c.read_bytes(32)?;
    let n = c.read_i32_le()? as usize;
    let mut coords = Vec::with_capacity(n);
    for _ in 0..n {
        let x = c.read_f64_le()?;
        let y = c.read_f64_le()?;
        coords.push(Coord {
            x,
            y,
            z: None,
            m: None,
        });
    }
    if coords.is_empty() {
        Ok(Geometry::Empty(GeometryType::MultiPoint))
    } else {
        Ok(Geometry::MultiPoint(coords))
    }
}

fn decode_polyline_or_polygon(c: &mut Cursor<'_>, is_polygon: bool) -> Result<Geometry> {
    // Skip 32-byte bbox.
    c.read_bytes(32)?;
    let n_parts = c.read_i32_le()? as usize;
    let n_points = c.read_i32_le()? as usize;
    if n_parts == 0 || n_points == 0 {
        return Ok(if is_polygon {
            Geometry::Empty(GeometryType::Polygon)
        } else {
            Geometry::Empty(GeometryType::LineString)
        });
    }
    let mut parts: Vec<usize> = Vec::with_capacity(n_parts + 1);
    for _ in 0..n_parts {
        parts.push(c.read_i32_le()? as usize);
    }
    parts.push(n_points);

    let mut all_coords = Vec::with_capacity(n_points);
    for _ in 0..n_points {
        let x = c.read_f64_le()?;
        let y = c.read_f64_le()?;
        all_coords.push(Coord {
            x,
            y,
            z: None,
            m: None,
        });
    }

    let mut part_rings: Vec<LineString> = Vec::with_capacity(n_parts);
    for w in parts.windows(2) {
        let (start, end) = (w[0], w[1]);
        if start > end || end > n_points {
            return Err(ShpError::malformed(format!(
                "part indices out of range: {start}..{end} (total {n_points})"
            )));
        }
        part_rings.push(LineString::new(all_coords[start..end].to_vec()));
    }

    if is_polygon {
        let polygons = group_rings_esri_to_ogc(part_rings);
        if polygons.len() == 1 {
            Ok(Geometry::Polygon(polygons.into_iter().next().unwrap()))
        } else {
            Ok(Geometry::MultiPolygon(polygons))
        }
    } else if part_rings.len() == 1 {
        Ok(Geometry::LineString(part_rings.into_iter().next().unwrap()))
    } else {
        Ok(Geometry::MultiLineString(part_rings))
    }
}

/// Group Shapefile rings (CW outer, CCW hole) into OGC polygons (CCW outer,
/// CW hole) by reversing every ring after classifying. Same logic we use in
/// `geonative-filegdb`.
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
            polygons.push(Polygon::new(reversed, Vec::new()));
        }
    }
    polygons
}

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

    fn point_bytes(x: f64, y: f64) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1i32.to_le_bytes()); // type
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
        b
    }

    fn polyline_bytes(parts: &[Vec<(f64, f64)>]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&3i32.to_le_bytes()); // type Polyline
        for _ in 0..4 {
            b.extend_from_slice(&0.0f64.to_le_bytes()); // bbox
        }
        b.extend_from_slice(&(parts.len() as i32).to_le_bytes());
        let n_points: i32 = parts.iter().map(|p| p.len() as i32).sum();
        b.extend_from_slice(&n_points.to_le_bytes());
        let mut idx = 0i32;
        for part in parts {
            b.extend_from_slice(&idx.to_le_bytes());
            idx += part.len() as i32;
        }
        for part in parts {
            for &(x, y) in part {
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn decode_simple_point() {
        let g = decode_record_content(&point_bytes(1.5, -3.2)).unwrap();
        match g {
            Geometry::Point(c) => {
                assert_eq!(c.x, 1.5);
                assert_eq!(c.y, -3.2);
            }
            _ => panic!("expected Point"),
        }
    }

    #[test]
    fn decode_single_part_polyline_is_linestring() {
        let pts = vec![(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)];
        let g = decode_record_content(&polyline_bytes(&[pts])).unwrap();
        match g {
            Geometry::LineString(ls) => assert_eq!(ls.coords.len(), 3),
            _ => panic!("expected LineString"),
        }
    }

    #[test]
    fn decode_two_part_polyline_is_multilinestring() {
        let g = decode_record_content(&polyline_bytes(&[
            vec![(0.0, 0.0), (1.0, 1.0)],
            vec![(10.0, 10.0), (11.0, 11.0), (12.0, 12.0)],
        ]))
        .unwrap();
        match g {
            Geometry::MultiLineString(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].coords.len(), 2);
                assert_eq!(parts[1].coords.len(), 3);
            }
            _ => panic!("expected MultiLineString"),
        }
    }

    #[test]
    fn null_shape_returns_empty() {
        let b = 0i32.to_le_bytes();
        let g = decode_record_content(&b).unwrap();
        assert!(matches!(g, Geometry::Empty(GeometryType::Point)));
    }

    #[test]
    fn polygon_ring_orientation_flipped_to_ogc() {
        // Two CW rings (Esri outer) → expect 2 OGC polygons (CCW exterior each).
        let cw_outer: Vec<(f64, f64)> = vec![
            (0.0, 0.0),
            (0.0, 10.0),
            (10.0, 10.0),
            (10.0, 0.0),
            (0.0, 0.0),
        ];
        // Build polygon bytes with type=5
        let mut b = Vec::new();
        b.extend_from_slice(&5i32.to_le_bytes()); // Polygon
        for _ in 0..4 {
            b.extend_from_slice(&0.0f64.to_le_bytes());
        }
        b.extend_from_slice(&1i32.to_le_bytes()); // 1 part
        b.extend_from_slice(&(cw_outer.len() as i32).to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes()); // part 0 starts at point 0
        for &(x, y) in &cw_outer {
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
        }

        let g = decode_record_content(&b).unwrap();
        let poly = match g {
            Geometry::Polygon(p) => p,
            other => panic!("expected Polygon, got {:?}", other),
        };
        // After OGC reorientation, the exterior must be CCW (signed area > 0).
        assert!(signed_area(&poly.exterior.coords) > 0.0);
    }
}

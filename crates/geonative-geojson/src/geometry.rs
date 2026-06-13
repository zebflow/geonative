//! Convert between RFC 7946 JSON shapes and the geonative-core `Geometry`
//! tree. 2D only in v0.1 — Z/M coordinates are silently truncated on read,
//! and never emitted on write.
//!
//! ## Coordinate shape
//!
//! Per RFC 7946 § 3.1.1:
//! - Point         → `[x, y]`
//! - LineString    → `[[x, y], ...]`
//! - Polygon       → `[[[x, y], ...], ...]`  (rings; first is exterior)
//! - MultiPoint    → `[[x, y], ...]`
//! - MultiLineString → `[[[x, y], ...], ...]`
//! - MultiPolygon  → `[[[[x, y], ...], ...], ...]`
//! - GeometryCollection → has `geometries:` array, not `coordinates:`

use geonative_core::{Coord, Geometry, LineString, Polygon};
use serde_json::Value as Json;

use crate::error::{GeoJsonError, Result};

/// Parse a single GeoJSON geometry object → `Geometry`.
pub fn from_json(obj: &Json) -> Result<Geometry> {
    let m = obj
        .as_object()
        .ok_or_else(|| GeoJsonError::malformed("geometry must be a JSON object"))?;
    let ty = m
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| GeoJsonError::malformed("geometry missing 'type'"))?;

    match ty {
        "Point" => Ok(Geometry::Point(parse_coord(coords(m)?)?)),
        "LineString" => Ok(Geometry::LineString(parse_linestring(coords(m)?)?)),
        "Polygon" => Ok(Geometry::Polygon(parse_polygon(coords(m)?)?)),
        "MultiPoint" => {
            let arr = coords(m)?
                .as_array()
                .ok_or_else(|| GeoJsonError::malformed("MultiPoint coordinates must be array"))?;
            let pts = arr.iter().map(parse_coord).collect::<Result<Vec<_>>>()?;
            Ok(Geometry::MultiPoint(pts))
        }
        "MultiLineString" => {
            let arr = coords(m)?.as_array().ok_or_else(|| {
                GeoJsonError::malformed("MultiLineString coordinates must be array")
            })?;
            let lines = arr
                .iter()
                .map(parse_linestring)
                .collect::<Result<Vec<_>>>()?;
            Ok(Geometry::MultiLineString(lines))
        }
        "MultiPolygon" => {
            let arr = coords(m)?
                .as_array()
                .ok_or_else(|| GeoJsonError::malformed("MultiPolygon coordinates must be array"))?;
            let polys = arr.iter().map(parse_polygon).collect::<Result<Vec<_>>>()?;
            Ok(Geometry::MultiPolygon(polys))
        }
        "GeometryCollection" => {
            let inner = m
                .get("geometries")
                .and_then(Json::as_array)
                .ok_or_else(|| {
                    GeoJsonError::malformed("GeometryCollection requires 'geometries' array")
                })?;
            let items = inner.iter().map(from_json).collect::<Result<Vec<_>>>()?;
            Ok(Geometry::GeometryCollection(items))
        }
        other => Err(GeoJsonError::unsupported(format!(
            "geometry type '{other}'"
        ))),
    }
}

fn coords(m: &serde_json::Map<String, Json>) -> Result<&Json> {
    m.get("coordinates")
        .ok_or_else(|| GeoJsonError::malformed("geometry missing 'coordinates'"))
}

fn parse_coord(v: &Json) -> Result<Coord> {
    let arr = v
        .as_array()
        .ok_or_else(|| GeoJsonError::malformed("coordinate must be array"))?;
    if arr.len() < 2 {
        return Err(GeoJsonError::malformed(format!(
            "coordinate needs ≥ 2 numbers, got {}",
            arr.len()
        )));
    }
    let x = arr[0]
        .as_f64()
        .ok_or_else(|| GeoJsonError::malformed("coordinate x must be a number"))?;
    let y = arr[1]
        .as_f64()
        .ok_or_else(|| GeoJsonError::malformed("coordinate y must be a number"))?;
    // Z (and any further dims) intentionally truncated in v0.1.
    Ok(Coord::xy(x, y))
}

fn parse_linestring(v: &Json) -> Result<LineString> {
    let arr = v
        .as_array()
        .ok_or_else(|| GeoJsonError::malformed("LineString coordinates must be array"))?;
    let pts = arr.iter().map(parse_coord).collect::<Result<Vec<_>>>()?;
    Ok(LineString::new(pts))
}

fn parse_polygon(v: &Json) -> Result<Polygon> {
    let rings = v
        .as_array()
        .ok_or_else(|| GeoJsonError::malformed("Polygon coordinates must be array of rings"))?;
    let mut iter = rings.iter();
    let exterior = match iter.next() {
        Some(r) => parse_linestring(r)?,
        None => {
            return Err(GeoJsonError::malformed("Polygon must have ≥ 1 ring"));
        }
    };
    let holes = iter.map(parse_linestring).collect::<Result<Vec<_>>>()?;
    Ok(Polygon::new(exterior, holes))
}

// --- Writers -------------------------------------------------------------

/// Render `Geometry` as a RFC 7946 JSON object. Z/M are dropped — v0.1 is 2D.
pub fn to_json(g: &Geometry) -> Json {
    use serde_json::json;
    match g {
        Geometry::Point(c) => json!({ "type": "Point", "coordinates": coord_json(c) }),
        Geometry::LineString(ls) => json!({
            "type": "LineString",
            "coordinates": ls.coords.iter().map(coord_json).collect::<Vec<_>>(),
        }),
        Geometry::Polygon(p) => json!({
            "type": "Polygon",
            "coordinates": polygon_rings(p),
        }),
        Geometry::MultiPoint(pts) => json!({
            "type": "MultiPoint",
            "coordinates": pts.iter().map(coord_json).collect::<Vec<_>>(),
        }),
        Geometry::MultiLineString(lines) => json!({
            "type": "MultiLineString",
            "coordinates": lines
                .iter()
                .map(|ls| ls.coords.iter().map(coord_json).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
        }),
        Geometry::MultiPolygon(polys) => json!({
            "type": "MultiPolygon",
            "coordinates": polys.iter().map(polygon_rings).collect::<Vec<_>>(),
        }),
        Geometry::GeometryCollection(items) => json!({
            "type": "GeometryCollection",
            "geometries": items.iter().map(to_json).collect::<Vec<_>>(),
        }),
        // Unknown / future variants: emit a typed placeholder so the JSON
        // stays valid even though no spec'd GeoJSON type matches.
        _ => json!({ "type": "Unknown" }),
    }
}

fn coord_json(c: &Coord) -> Json {
    serde_json::json!([c.x, c.y])
}

fn polygon_rings(p: &Polygon) -> Vec<Vec<Json>> {
    let mut out: Vec<Vec<Json>> = Vec::with_capacity(1 + p.holes.len());
    out.push(p.exterior.coords.iter().map(coord_json).collect());
    for ring in &p.holes {
        out.push(ring.coords.iter().map(coord_json).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn point_round_trip() {
        let g = Geometry::Point(Coord::xy(1.0, 2.0));
        let j = to_json(&g);
        assert_eq!(j["type"], "Point");
        let back = from_json(&j).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn linestring_round_trip() {
        let g = Geometry::LineString(LineString::new(vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(1.0, 1.0),
            Coord::xy(2.0, 0.0),
        ]));
        let j = to_json(&g);
        let back = from_json(&j).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn polygon_with_hole_round_trip() {
        let outer = LineString::new(vec![
            Coord::xy(0.0, 0.0),
            Coord::xy(10.0, 0.0),
            Coord::xy(10.0, 10.0),
            Coord::xy(0.0, 10.0),
            Coord::xy(0.0, 0.0),
        ]);
        let hole = LineString::new(vec![
            Coord::xy(3.0, 3.0),
            Coord::xy(7.0, 3.0),
            Coord::xy(7.0, 7.0),
            Coord::xy(3.0, 7.0),
            Coord::xy(3.0, 3.0),
        ]);
        let g = Geometry::Polygon(Polygon::new(outer, vec![hole]));
        let j = to_json(&g);
        let back = from_json(&j).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn multipolygon_round_trip() {
        let p1 = Polygon::new(
            LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(1.0, 0.0),
                Coord::xy(0.0, 1.0),
                Coord::xy(0.0, 0.0),
            ]),
            vec![],
        );
        let p2 = Polygon::new(
            LineString::new(vec![
                Coord::xy(10.0, 10.0),
                Coord::xy(11.0, 10.0),
                Coord::xy(10.0, 11.0),
                Coord::xy(10.0, 10.0),
            ]),
            vec![],
        );
        let g = Geometry::MultiPolygon(vec![p1, p2]);
        let j = to_json(&g);
        let back = from_json(&j).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn geometrycollection_round_trip() {
        let g = Geometry::GeometryCollection(vec![
            Geometry::Point(Coord::xy(1.0, 2.0)),
            Geometry::LineString(LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(1.0, 1.0),
            ])),
        ]);
        let j = to_json(&g);
        let back = from_json(&j).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn truncates_z_on_read() {
        let j = json!({ "type": "Point", "coordinates": [1.0, 2.0, 9.99] });
        let g = from_json(&j).unwrap();
        assert_eq!(g, Geometry::Point(Coord::xy(1.0, 2.0)));
    }

    #[test]
    fn rejects_short_coord() {
        let j = json!({ "type": "Point", "coordinates": [1.0] });
        assert!(from_json(&j).is_err());
    }

    #[test]
    fn rejects_missing_type() {
        let j = json!({ "coordinates": [1.0, 2.0] });
        assert!(from_json(&j).is_err());
    }

    #[test]
    fn rejects_unknown_type() {
        let j = json!({ "type": "Sphere", "coordinates": [0, 0, 1] });
        let err = from_json(&j).unwrap_err();
        assert!(matches!(err, GeoJsonError::Unsupported(_)));
    }
}

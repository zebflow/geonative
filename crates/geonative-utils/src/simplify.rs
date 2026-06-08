//! Douglas-Peucker line simplification.
//!
//! Classic recursive algorithm: keep the two endpoints; find the vertex
//! farthest from the chord between them; if its perpendicular distance is
//! above `tolerance`, keep it and recurse on both halves; otherwise discard
//! all intermediate vertices.
//!
//! Complexity is `O(n²)` worst case, `O(n log n)` typical. Tolerances are
//! in the **same units as the input coordinates** — pass degree-based
//! tolerances for lng/lat data, meter-based for projected data.

use geonative_core::{Coord, Geometry, LineString, Polygon};

/// Simplify a polyline (or polygon ring without its closing duplicate) using
/// Douglas-Peucker. Always preserves the first and last coords.
///
/// Returns the input unchanged when it has fewer than 3 coords (nothing to
/// drop) or when `tolerance` is non-positive.
pub fn douglas_peucker(coords: &[Coord], tolerance: f64) -> Vec<Coord> {
    if coords.len() < 3 || !tolerance.is_finite() || tolerance <= 0.0 {
        return coords.to_vec();
    }
    let n = coords.len();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    dp_recurse(coords, 0, n - 1, tolerance * tolerance, &mut keep);

    coords
        .iter()
        .zip(keep.iter())
        .filter_map(|(c, &k)| if k { Some(*c) } else { None })
        .collect()
}

fn dp_recurse(coords: &[Coord], start: usize, end: usize, tol_sq: f64, keep: &mut [bool]) {
    if end <= start + 1 {
        return;
    }
    let (max_dist_sq, max_idx) = farthest_index(coords, start, end);
    if max_dist_sq > tol_sq {
        keep[max_idx] = true;
        dp_recurse(coords, start, max_idx, tol_sq, keep);
        dp_recurse(coords, max_idx, end, tol_sq, keep);
    }
}

/// Index (within `coords`) of the point farthest from the line `[start, end]`,
/// and its squared perpendicular distance.
fn farthest_index(coords: &[Coord], start: usize, end: usize) -> (f64, usize) {
    let a = coords[start];
    let b = coords[end];
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;

    let mut max_d_sq = -1.0;
    let mut max_idx = start;
    for (i, p) in coords.iter().enumerate().take(end).skip(start + 1) {
        let d_sq = if len_sq == 0.0 {
            let px = p.x - a.x;
            let py = p.y - a.y;
            px * px + py * py
        } else {
            // Perpendicular distance squared from p to the infinite line through a,b.
            let num = dy * p.x - dx * p.y + b.x * a.y - b.y * a.x;
            (num * num) / len_sq
        };
        if d_sq > max_d_sq {
            max_d_sq = d_sq;
            max_idx = i;
        }
    }
    (max_d_sq, max_idx)
}

/// Apply Douglas-Peucker to every linear part of a `Geometry`. Polygon rings
/// are kept closed (the closing duplicate is restored if simplification
/// dropped it). `Point` / `MultiPoint` / `Empty` pass through unchanged.
pub fn simplify_geometry(geom: &Geometry, tolerance: f64) -> Geometry {
    match geom {
        Geometry::Point(_) | Geometry::MultiPoint(_) | Geometry::Empty(_) => geom.clone(),
        Geometry::LineString(ls) => Geometry::LineString(simplify_linestring(ls, tolerance)),
        Geometry::MultiLineString(parts) => Geometry::MultiLineString(
            parts
                .iter()
                .map(|ls| simplify_linestring(ls, tolerance))
                .collect(),
        ),
        Geometry::Polygon(p) => Geometry::Polygon(simplify_polygon(p, tolerance)),
        Geometry::MultiPolygon(polys) => Geometry::MultiPolygon(
            polys
                .iter()
                .map(|p| simplify_polygon(p, tolerance))
                .collect(),
        ),
        Geometry::GeometryCollection(v) => Geometry::GeometryCollection(
            v.iter().map(|g| simplify_geometry(g, tolerance)).collect(),
        ),
        // Future variants (e.g. curves, surfaces): pass through unchanged.
        // A new SemVer-minor of geonative-core may add Geometry variants;
        // we don't break for callers that haven't upgraded geonative-utils.
        _ => geom.clone(),
    }
}

fn simplify_linestring(ls: &LineString, tolerance: f64) -> LineString {
    LineString::new(douglas_peucker(&ls.coords, tolerance))
}

fn simplify_polygon(p: &Polygon, tolerance: f64) -> Polygon {
    Polygon::new(
        simplify_ring(&p.exterior, tolerance),
        p.holes
            .iter()
            .map(|h| simplify_ring(h, tolerance))
            .collect(),
    )
}

/// Simplify a polygon ring, preserving the closing vertex.
///
/// The IR uses OGC-closed rings (first == last). DP could drop one of the
/// endpoints if the ring is small; we explicitly re-close after simplifying.
fn simplify_ring(ring: &LineString, tolerance: f64) -> LineString {
    if ring.coords.len() <= 4 {
        // Triangle (4 coords with closing dup): can't drop anything meaningful.
        return ring.clone();
    }
    let mut simplified = douglas_peucker(&ring.coords, tolerance);
    // Re-close if needed: simplification preserves first + last vertices, so
    // the close should still hold — but if input wasn't closed, leave it.
    if let (Some(first), Some(last)) = (simplified.first(), simplified.last()) {
        if first != last && ring.coords.first() == ring.coords.last() {
            simplified.push(*first);
        }
    }
    LineString::new(simplified)
}

/// A small preset tolerance (in **degrees**) for typical web-map zoom levels.
///
/// Calibrated so that a coord moved by less than `tolerance` would be
/// indistinguishable at the given zoom on a 4096-extent MVT tile. The
/// numbers are powers-of-two-ish, intentionally generous at low zooms (where
/// dropping detail is desirable for tile size) and tight at high zooms
/// (where users zoom in to see fine geometry).
///
/// Pipe `Geometry`s through `simplify_geometry(geom, tolerance_for_zoom(z))`
/// before MVT encoding to get LOD-aware tiles.
pub fn tolerance_for_zoom(z: u8) -> f64 {
    match z {
        0..=2 => 0.5,
        3..=5 => 0.1,
        6..=8 => 0.02,
        9..=11 => 0.004,
        12..=14 => 0.0008,
        15..=17 => 0.000_15,
        _ => 0.000_03,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::GeometryType;

    fn xy(x: f64, y: f64) -> Coord {
        Coord {
            x,
            y,
            z: None,
            m: None,
        }
    }

    #[test]
    fn dp_drops_colinear_interior_points() {
        // Five colinear points; tolerance > 0 should reduce to just endpoints.
        let pts = vec![
            xy(0.0, 0.0),
            xy(1.0, 0.0),
            xy(2.0, 0.0),
            xy(3.0, 0.0),
            xy(4.0, 0.0),
        ];
        let out = douglas_peucker(&pts, 0.001);
        assert_eq!(out, vec![xy(0.0, 0.0), xy(4.0, 0.0)]);
    }

    #[test]
    fn dp_keeps_jagged_extremes() {
        // Zigzag with peaks at (1,10) and (3,-10); the midpoint (2,0) is
        // colinear with the chord between the peaks, so DP correctly drops
        // it. Endpoints + two extremes = 4 kept.
        let pts = vec![
            xy(0.0, 0.0),
            xy(1.0, 10.0),
            xy(2.0, 0.0),
            xy(3.0, -10.0),
            xy(4.0, 0.0),
        ];
        let out = douglas_peucker(&pts, 0.5);
        assert!(out.contains(&xy(0.0, 0.0)), "endpoint dropped");
        assert!(out.contains(&xy(4.0, 0.0)), "endpoint dropped");
        assert!(out.contains(&xy(1.0, 10.0)), "first extreme dropped");
        assert!(out.contains(&xy(3.0, -10.0)), "second extreme dropped");
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn dp_keeps_non_colinear_intermediate() {
        // Same zigzag but with a midpoint that's not on the (1,10)→(3,-10)
        // chord. Tighten tolerance so the midpoint's perpendicular distance
        // from that chord (~0.5°) is comfortably above threshold.
        let pts = vec![
            xy(0.0, 0.0),
            xy(1.0, 10.0),
            xy(2.0, 5.0),
            xy(3.0, -10.0),
            xy(4.0, 0.0),
        ];
        let out = douglas_peucker(&pts, 0.1);
        assert_eq!(out.len(), pts.len(), "all 5 points should be kept");
    }

    #[test]
    fn dp_zero_or_negative_tolerance_is_identity() {
        let pts = vec![xy(0.0, 0.0), xy(1.0, 0.5), xy(2.0, 0.0)];
        assert_eq!(douglas_peucker(&pts, 0.0), pts);
        assert_eq!(douglas_peucker(&pts, -1.0), pts);
    }

    #[test]
    fn dp_short_input_is_identity() {
        let pts = vec![xy(0.0, 0.0), xy(1.0, 1.0)];
        assert_eq!(douglas_peucker(&pts, 0.5), pts);
    }

    #[test]
    fn dp_degenerate_chord_uses_distance_to_first_point() {
        // a == b at endpoints; perpendicular distance falls back to Euclidean.
        let pts = vec![xy(0.0, 0.0), xy(5.0, 5.0), xy(0.0, 0.0)];
        let out = douglas_peucker(&pts, 1.0);
        // Middle point is sqrt(50) ≈ 7.07 from the endpoints; > 1.0, so kept.
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn simplify_geometry_dispatches_per_variant() {
        // Point is unchanged.
        let p = Geometry::Point(xy(1.0, 2.0));
        assert_eq!(simplify_geometry(&p, 0.001), p);

        // Empty is unchanged.
        let e = Geometry::Empty(GeometryType::Polygon);
        assert_eq!(simplify_geometry(&e, 0.001), e);

        // LineString gets simplified.
        let ls = Geometry::LineString(LineString::new(vec![
            xy(0.0, 0.0),
            xy(1.0, 0.0),
            xy(2.0, 0.0),
            xy(3.0, 0.0),
        ]));
        match simplify_geometry(&ls, 0.001) {
            Geometry::LineString(out) => assert_eq!(out.coords.len(), 2),
            _ => panic!(),
        }
    }

    #[test]
    fn simplify_polygon_preserves_ring_closure() {
        let p = Polygon::new(
            LineString::new(vec![
                xy(0.0, 0.0),
                xy(10.0, 0.0),
                xy(10.001, 0.0001),
                xy(10.0, 10.0),
                xy(0.0, 10.0),
                xy(0.0, 0.0),
            ]),
            vec![],
        );
        let simplified = simplify_polygon(&p, 0.01);
        // The near-collinear vertex at (10.001, 0.0001) should be dropped.
        assert!(simplified.exterior.coords.len() < p.exterior.coords.len());
        // Ring must still be closed.
        assert_eq!(
            simplified.exterior.coords.first(),
            simplified.exterior.coords.last(),
            "ring closure lost: {:?}",
            simplified.exterior.coords
        );
    }

    #[test]
    fn simplify_polygon_with_hole_handles_both_rings() {
        let p = Polygon::new(
            LineString::new(vec![
                xy(0.0, 0.0),
                xy(10.0, 0.0),
                xy(10.0, 10.0),
                xy(5.0, 10.0001),
                xy(0.0, 10.0),
                xy(0.0, 0.0),
            ]),
            vec![LineString::new(vec![
                xy(2.0, 2.0),
                xy(4.0, 2.0),
                xy(4.001, 2.0),
                xy(4.0, 4.0),
                xy(2.0, 4.0),
                xy(2.0, 2.0),
            ])],
        );
        let simplified = simplify_polygon(&p, 0.001);
        assert_eq!(simplified.holes.len(), 1);
        assert_eq!(
            simplified.holes[0].coords.first(),
            simplified.holes[0].coords.last(),
            "hole closure lost"
        );
    }

    #[test]
    fn simplify_tiny_polygon_passes_through() {
        // Triangle (3 distinct points + closing dup): no simplification possible.
        let p = Polygon::new(
            LineString::new(vec![xy(0.0, 0.0), xy(1.0, 0.0), xy(0.5, 1.0), xy(0.0, 0.0)]),
            vec![],
        );
        let simplified = simplify_polygon(&p, 100.0);
        assert_eq!(simplified.exterior.coords.len(), 4);
    }

    #[test]
    fn tolerance_for_zoom_is_monotonic() {
        for z in 0u8..18 {
            assert!(
                tolerance_for_zoom(z) >= tolerance_for_zoom(z + 1),
                "tolerance not monotonically non-increasing at z={z}"
            );
        }
    }

    #[test]
    fn tolerance_for_zoom_low_zoom_aggressive() {
        // At z=0 the world fits in one 4096-pixel tile; 1 degree ≈ 11 pixels,
        // so tolerance 0.5° ≈ 5.5 px — a reasonable LOD threshold.
        assert!(tolerance_for_zoom(0) >= 0.1);
        assert!(tolerance_for_zoom(20) < 0.001);
    }
}

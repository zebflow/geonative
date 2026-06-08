//! Linear and areal measurements on the geonative-core IR.
//!
//! ## What this module is
//!
//! Pure-math primitives: Euclidean distance between two coords, length of
//! a polyline, signed/unsigned area of a polygon ring. They operate in the
//! same units as their input (degrees for lng/lat data, meters for projected
//! data) — there is **no** geodetic correction here. If you need
//! great-circle distances, project to a metric CRS first or wait for a
//! future `geonative-proj`-aware measure module.
//!
//! ## Clever bits
//!
//! - **`signed_area` uses the shoelace formula directly.** Positive result
//!   means CCW ring (OGC exterior); negative means CW (OGC interior).
//!   Callers that just want magnitude take `.abs()`.
//! - **`area` of a `Polygon` subtracts hole areas from the exterior.**
//!   Standard "shell minus holes" formula. Works for any polygon as long
//!   as holes are inside the exterior (the IR's invariant).
//! - **`length` ignores empty/single-point inputs** (returns 0.0).
//! - **No allocation.** All four entry points are non-allocating.

use geonative_core::{Coord, LineString, Polygon};

/// Euclidean distance between two coords in the input units (degrees /
/// meters / whatever).
pub fn distance(a: Coord, b: Coord) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    (dx * dx + dy * dy).sqrt()
}

/// Total length of a polyline (sum of segment lengths). Empty or
/// single-point inputs return 0.
pub fn length(ls: &LineString) -> f64 {
    if ls.coords.len() < 2 {
        return 0.0;
    }
    let mut total = 0.0;
    for w in ls.coords.windows(2) {
        total += distance(w[0], w[1]);
    }
    total
}

/// **Signed** area of a ring using the shoelace formula. Positive = CCW
/// (OGC exterior convention); negative = CW (interior / hole convention).
/// Rings with < 3 coords return 0.
pub fn signed_area(ring: &[Coord]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    let n = ring.len();
    for i in 0..n {
        let j = (i + 1) % n;
        sum += ring[i].x * ring[j].y - ring[j].x * ring[i].y;
    }
    sum * 0.5
}

/// Unsigned area of a single ring.
pub fn ring_area(ring: &[Coord]) -> f64 {
    signed_area(ring).abs()
}

/// Area of a polygon = exterior area minus the sum of hole areas. Returns
/// the absolute value (sign of the exterior winding doesn't matter for the
/// final magnitude).
pub fn area(p: &Polygon) -> f64 {
    let exterior = ring_area(&p.exterior.coords);
    let holes: f64 = p.holes.iter().map(|h| ring_area(&h.coords)).sum();
    (exterior - holes).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xy(x: f64, y: f64) -> Coord {
        Coord {
            x,
            y,
            z: None,
            m: None,
        }
    }

    #[test]
    fn distance_matches_pythagoras() {
        assert!((distance(xy(0.0, 0.0), xy(3.0, 4.0)) - 5.0).abs() < 1e-12);
        assert_eq!(distance(xy(1.0, 1.0), xy(1.0, 1.0)), 0.0);
    }

    #[test]
    fn length_of_two_segment_line() {
        let ls = LineString::new(vec![xy(0.0, 0.0), xy(3.0, 0.0), xy(3.0, 4.0)]);
        assert!((length(&ls) - 7.0).abs() < 1e-12);
    }

    #[test]
    fn length_of_short_input_is_zero() {
        assert_eq!(length(&LineString::default()), 0.0);
        assert_eq!(length(&LineString::new(vec![xy(1.0, 1.0)])), 0.0);
    }

    #[test]
    fn signed_area_unit_square_ccw_is_one() {
        let ring = vec![
            xy(0.0, 0.0),
            xy(1.0, 0.0),
            xy(1.0, 1.0),
            xy(0.0, 1.0),
            xy(0.0, 0.0),
        ];
        assert!((signed_area(&ring) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn signed_area_unit_square_cw_is_minus_one() {
        let mut ring = vec![
            xy(0.0, 0.0),
            xy(1.0, 0.0),
            xy(1.0, 1.0),
            xy(0.0, 1.0),
            xy(0.0, 0.0),
        ];
        ring.reverse();
        assert!((signed_area(&ring) - (-1.0)).abs() < 1e-12);
    }

    #[test]
    fn ring_area_is_unsigned() {
        let ring = vec![
            xy(0.0, 0.0),
            xy(1.0, 0.0),
            xy(1.0, 1.0),
            xy(0.0, 1.0),
            xy(0.0, 0.0),
        ];
        assert!((ring_area(&ring) - 1.0).abs() < 1e-12);
        let mut reversed = ring.clone();
        reversed.reverse();
        assert!((ring_area(&reversed) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn polygon_area_subtracts_holes() {
        let p = Polygon::new(
            LineString::new(vec![
                xy(0.0, 0.0),
                xy(10.0, 0.0),
                xy(10.0, 10.0),
                xy(0.0, 10.0),
                xy(0.0, 0.0),
            ]),
            vec![LineString::new(vec![
                xy(2.0, 2.0),
                xy(4.0, 2.0),
                xy(4.0, 4.0),
                xy(2.0, 4.0),
                xy(2.0, 2.0),
            ])],
        );
        // exterior 100 - hole 4 = 96
        assert!((area(&p) - 96.0).abs() < 1e-12);
    }

    #[test]
    fn polygon_with_multiple_holes_sums_them() {
        let p = Polygon::new(
            LineString::new(vec![
                xy(0.0, 0.0),
                xy(10.0, 0.0),
                xy(10.0, 10.0),
                xy(0.0, 10.0),
                xy(0.0, 0.0),
            ]),
            vec![
                LineString::new(vec![
                    xy(1.0, 1.0),
                    xy(2.0, 1.0),
                    xy(2.0, 2.0),
                    xy(1.0, 2.0),
                    xy(1.0, 1.0),
                ]),
                LineString::new(vec![
                    xy(5.0, 5.0),
                    xy(8.0, 5.0),
                    xy(8.0, 8.0),
                    xy(5.0, 8.0),
                    xy(5.0, 5.0),
                ]),
            ],
        );
        // 100 - 1 - 9 = 90
        assert!((area(&p) - 90.0).abs() < 1e-12);
    }

    #[test]
    fn degenerate_ring_zero_area() {
        assert_eq!(signed_area(&[]), 0.0);
        assert_eq!(signed_area(&[xy(0.0, 0.0)]), 0.0);
        assert_eq!(signed_area(&[xy(0.0, 0.0), xy(1.0, 1.0)]), 0.0);
    }
}

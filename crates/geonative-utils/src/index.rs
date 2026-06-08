//! Spatial-locality indexing helpers — primarily Hilbert curve encoding.
//!
//! ## What this module is
//!
//! Spatial keys that map 2D points to 1D distances along a space-filling
//! curve. Used to cluster physically-near features into the same Parquet
//! row group / SQLite page / R-tree bucket, making bbox-based predicates
//! highly selective.
//!
//! ## Architecture position
//!
//! Originally lived in `geonative-geoparquet::hilbert`; moved here so any
//! format crate (GeoParquet, GPKG, FlatGeoBuf, R-tree builders) can share
//! the same primitives.
//!
//! ## Clever bits
//!
//! - **Order = 16 bits per axis** → 65,536-cell grid. Sub-meter resolution
//!   for typical lng/lat datasets; Hilbert distances fit in u32 with room
//!   to spare (u64 return type is just for future expansion).
//! - **`u64::MAX` for non-quantizable inputs.** NaN / degenerate-bbox cases
//!   sort to the end of any Hilbert-sorted sequence rather than panicking.
//! - **Pure functions, no Indexer struct.** Callers compute their dataset
//!   bbox via [`union_bbox`], then map each feature with
//!   [`hilbert_distance_for`]. No hidden state, no allocation in the hot loop.

/// Convert grid-quantized `(x, y)` (each `0..2^order`) to a Hilbert distance.
/// `order` must be ≤ 32. For order = 16 the output is ≤ 2^32.
pub fn hilbert_xy2d(order: u32, x: u32, y: u32) -> u64 {
    let mut x = x as i64;
    let mut y = y as i64;
    let mut d: u64 = 0;
    let mut s: i64 = (1i64 << order) / 2;
    while s > 0 {
        let rx = if (x & s) > 0 { 1i64 } else { 0 };
        let ry = if (y & s) > 0 { 1i64 } else { 0 };
        d = d.wrapping_add((s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64);
        // rotate quadrant
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// Quantize a real `(x, y)` to integer grid coordinates `0..2^order`,
/// given the dataset's overall bbox. Returns `None` if any input is non-finite
/// or the bbox is degenerate (zero width or height).
pub fn quantize(
    x: f64,
    y: f64,
    bbox: [f64; 4], // xmin, ymin, xmax, ymax
    order: u32,
) -> Option<(u32, u32)> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let width = bbox[2] - bbox[0];
    let height = bbox[3] - bbox[1];
    // Reject degenerate bboxes (zero or negative width/height) AND NaN values.
    // Plain `<= 0.0` would mishandle NaN, so we check positivity explicitly.
    if width.is_nan() || height.is_nan() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let grid = (1u64 << order) - 1; // max index
    let nx = ((x - bbox[0]) / width).clamp(0.0, 1.0) * grid as f64;
    let ny = ((y - bbox[1]) / height).clamp(0.0, 1.0) * grid as f64;
    Some((nx as u32, ny as u32))
}

/// Combined: given a centroid and dataset bbox, return its Hilbert distance.
/// Returns `u64::MAX` for non-quantizable points so they sort to the end.
pub fn hilbert_distance_for(centroid: (f64, f64), bbox: [f64; 4], order: u32) -> u64 {
    match quantize(centroid.0, centroid.1, bbox, order) {
        Some((x, y)) => hilbert_xy2d(order, x, y),
        None => u64::MAX,
    }
}

/// Compute the union bbox of an iterator of `[xmin, ymin, xmax, ymax]`s.
/// Returns `None` if no finite bbox is seen.
pub fn union_bbox<I: IntoIterator<Item = [f64; 4]>>(iter: I) -> Option<[f64; 4]> {
    let mut acc: Option<[f64; 4]> = None;
    for b in iter {
        if !b.iter().all(|v| v.is_finite()) {
            continue;
        }
        match &mut acc {
            None => acc = Some(b),
            Some(a) => {
                a[0] = a[0].min(b[0]);
                a[1] = a[1].min(b[1]);
                a[2] = a[2].max(b[2]);
                a[3] = a[3].max(b[3]);
            }
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hilbert_origin_is_zero() {
        assert_eq!(hilbert_xy2d(16, 0, 0), 0);
    }

    #[test]
    fn hilbert_orders_corners_uniquely() {
        // Four corners of a 2x2 grid have unique distances 0,1,2,3.
        let mut dists: Vec<u64> = vec![
            hilbert_xy2d(1, 0, 0),
            hilbert_xy2d(1, 0, 1),
            hilbert_xy2d(1, 1, 0),
            hilbert_xy2d(1, 1, 1),
        ];
        dists.sort();
        assert_eq!(dists, vec![0, 1, 2, 3]);
    }

    #[test]
    fn nearby_points_cluster_under_hilbert() {
        // Two close points should have closer Hilbert distance than two far points,
        // averaged over many random pairs. We verify on a single deterministic case:
        // points in the same quadrant of order=4 grid → small distance diff;
        // points across diagonally opposite quadrants → larger diff.
        let order = 4;
        let close_a = hilbert_xy2d(order, 1, 1);
        let close_b = hilbert_xy2d(order, 2, 1);
        let far_a = hilbert_xy2d(order, 0, 0);
        let far_b = hilbert_xy2d(order, (1 << order) - 1, (1 << order) - 1);
        assert!(close_a.abs_diff(close_b) < far_a.abs_diff(far_b));
    }

    #[test]
    fn quantize_rejects_non_finite_or_degenerate() {
        let bbox = [0.0, 0.0, 10.0, 10.0];
        assert!(quantize(f64::NAN, 5.0, bbox, 16).is_none());
        assert!(quantize(5.0, f64::NEG_INFINITY, bbox, 16).is_none());

        // Degenerate (zero width)
        let degen = [0.0, 0.0, 0.0, 10.0];
        assert!(quantize(0.0, 5.0, degen, 16).is_none());
    }

    #[test]
    fn quantize_maps_extremes_to_grid_edges() {
        let bbox = [0.0, 0.0, 10.0, 10.0];
        let max = (1u32 << 16) - 1;
        assert_eq!(quantize(0.0, 0.0, bbox, 16), Some((0, 0)));
        assert_eq!(quantize(10.0, 10.0, bbox, 16), Some((max, max)));
        // Clamping
        assert_eq!(quantize(-1.0, 11.0, bbox, 16), Some((0, max)));
    }

    #[test]
    fn distance_for_null_inputs_sorts_last() {
        let bbox = [0.0, 0.0, 10.0, 10.0];
        let real = hilbert_distance_for((5.0, 5.0), bbox, 16);
        let null = hilbert_distance_for((f64::NAN, f64::NAN), bbox, 16);
        assert!(real < null);
        assert_eq!(null, u64::MAX);
    }

    #[test]
    fn union_bbox_grows_to_envelope() {
        let bboxes = vec![
            [0.0, 0.0, 1.0, 1.0],
            [-1.0, 5.0, 2.0, 6.0],
            [3.0, -2.0, 4.0, 0.5],
        ];
        let u = union_bbox(bboxes).unwrap();
        assert_eq!(u, [-1.0, -2.0, 4.0, 6.0]);
    }

    #[test]
    fn union_bbox_skips_non_finite() {
        let bboxes = vec![[0.0, 0.0, 1.0, 1.0], [f64::NAN, 0.0, 1.0, 1.0]];
        let u = union_bbox(bboxes).unwrap();
        assert_eq!(u, [0.0, 0.0, 1.0, 1.0]);
    }
}

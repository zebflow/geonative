//! Bbox-intersect filter middleware. Uses `Geometry::bbox()` for a coarse
//! intersect test — a feature whose bbox touches the query bbox passes
//! through whole, with no clipping. Exact predicates require an R-tree +
//! per-vertex tests and belong in `geonative-processing`'s overlay module
//! when that lands.

use std::path::Path;
use std::time::Instant;

use crate::error::Result;
use crate::io::{Sink, SinkOptions, Source};

/// Query bbox as `[xmin, ymin, xmax, ymax]`.
pub type Bbox2 = [f64; 4];

/// Coarse intersection predicate. Inclusive on edges.
pub fn bbox_intersects(a: Bbox2, b: Bbox2) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

#[derive(Debug, Clone, Copy)]
pub struct FilterStats {
    pub scanned: u64,
    pub kept: u64,
    pub elapsed_secs: f64,
}

/// Stream features from `src`, keep those whose `Geometry::bbox()` intersects
/// `query_bbox`, write the survivors to `dst`. Same Source/Sink machinery as
/// [`crate::convert`].
pub fn filter_bbox(
    src: &Path,
    dst: &Path,
    query_bbox: Bbox2,
    layer: Option<&str>,
    sink_opts: SinkOptions,
) -> Result<FilterStats> {
    let source = Source::open(src, layer)?;
    let schema = source.schema_cloned()?;
    let mut sink = Sink::create(dst, &schema, sink_opts)?;

    let start = Instant::now();
    let mut scanned: u64 = 0;
    let mut kept: u64 = 0;
    source.for_each(|feat| {
        scanned += 1;
        if let Some(geom) = feat.geometry.as_ref() {
            if let Some(fb) = geom.bbox() {
                if bbox_intersects(fb, query_bbox) {
                    sink.write(&feat)?;
                    kept += 1;
                }
            }
        }
        Ok(())
    })?;
    sink.close()?;

    Ok(FilterStats {
        scanned,
        kept,
        elapsed_secs: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_intersect_cases() {
        // identical
        assert!(bbox_intersects([0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0]));
        // edge touch is intersection per the <= semantics
        assert!(bbox_intersects([0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 2.0, 2.0]));
        // disjoint
        assert!(!bbox_intersects([0.0, 0.0, 1.0, 1.0], [2.0, 2.0, 3.0, 3.0]));
        // a contains b
        assert!(bbox_intersects([0.0, 0.0, 10.0, 10.0], [4.0, 4.0, 5.0, 5.0]));
    }
}

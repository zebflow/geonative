//! Bbox-intersect filter helpers. Uses `Geometry::bbox()` for a coarse
//! intersect test — a feature whose bbox touches the query bbox passes
//! through whole, with no clipping.
//!
//! For exact spatial predicates we'd need an R-tree + per-vertex tests; that
//! belongs in a future `clip` / `spatial-join` command, not here.
//!
//! Format dispatch lives in [`crate::io`]; this module only houses the
//! parsing + intersect predicate so they stay independently testable.

/// Query bbox as `[xmin, ymin, xmax, ymax]`.
pub type Bbox2 = [f64; 4];

pub fn bbox_intersects(a: [f64; 4], b: Bbox2) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

pub fn parse_bbox(s: &str) -> Result<Bbox2, String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(format!(
            "--bbox expects 4 comma-separated numbers (xmin,ymin,xmax,ymax), got: {s}"
        ));
    }
    let mut out = [0.0_f64; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<f64>()
            .map_err(|e| format!("--bbox component {i} ('{p}'): {e}"))?;
    }
    if out[0] > out[2] || out[1] > out[3] {
        return Err(format!(
            "--bbox is degenerate (xmin>xmax or ymin>ymax): [{}, {}, {}, {}]",
            out[0], out[1], out[2], out[3]
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bbox_ok() {
        let b = parse_bbox("1,2,3,4").unwrap();
        assert_eq!(b, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn parse_bbox_with_spaces() {
        let b = parse_bbox(" 1 , 2 , 3 , 4 ").unwrap();
        assert_eq!(b, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn parse_bbox_wrong_count() {
        assert!(parse_bbox("1,2,3").is_err());
        assert!(parse_bbox("1,2,3,4,5").is_err());
    }

    #[test]
    fn parse_bbox_degenerate() {
        assert!(parse_bbox("3,2,1,4").is_err());
        assert!(parse_bbox("1,4,3,2").is_err());
    }

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

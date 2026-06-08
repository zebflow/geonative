//! A `Feature` is one row: an optional feature ID, an optional geometry, and
//! an indexed attribute vector that parallels [`crate::Schema::fields`].

use crate::{geometry::Geometry, value::Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    /// Feature identifier. `None` for formats that don't expose stable FIDs.
    pub fid: Option<i64>,
    /// Geometry payload. `None` for attribute-only tables (GDB system tables,
    /// DBF-only Shapefile sidecars).
    pub geometry: Option<Geometry>,
    /// Attribute values, in the same order as `Schema::fields`. Length
    /// **must** equal `Schema::fields.len()`.
    pub attributes: Vec<Value>,
}

impl Feature {
    pub fn new(fid: Option<i64>, geometry: Option<Geometry>, attributes: Vec<Value>) -> Self {
        Self {
            fid,
            geometry,
            attributes,
        }
    }

    pub fn attribute(&self, idx: usize) -> Option<&Value> {
        self.attributes.get(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Coord, Geometry};

    #[test]
    fn construct_and_access() {
        let f = Feature::new(
            Some(42),
            Some(Geometry::Point(Coord::xy(1.0, 2.0))),
            vec![Value::Int32(7), Value::String("hi".into())],
        );
        assert_eq!(f.fid, Some(42));
        assert!(matches!(f.geometry, Some(Geometry::Point(_))));
        assert_eq!(f.attribute(0), Some(&Value::Int32(7)));
        assert_eq!(f.attribute(2), None);
    }
}

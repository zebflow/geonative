//! Layer schema — the table-of-contents for one layer's attributes +
//! geometry + CRS.
//!
//! ## Structure
//!
//! - **`fields: Vec<FieldDef>`** — attribute columns in **declared order**.
//!   Order is the public contract: DBF row layout, Arrow column indices,
//!   FileGDB row-blob nullable bitmap all derive from it. Lookup-by-name
//!   is available via `field_index(name)` against a precomputed `HashMap`.
//! - **`geometry: Option<GeomField>`** — optional because attribute-only
//!   tables exist (GDB system tables, DBF sidecars).
//! - **`crs: Crs`** — the source CRS, carried through verbatim. Readers
//!   populate it from `.prj` / GDB geom-field metadata / GeoParquet `geo`
//!   metadata / etc. Writers serialize it in each format's native form
//!   (WKT / PROJJSON / EPSG code / nothing).
//!
//! ## Clever bits
//!
//! - **`validate_row`** typechecks an attribute vector against the schema
//!   before writers ingest it — arity, type tags, nullability.
//! - **`name_index`** is `private` so callers can't desync it with `fields`;
//!   it's rebuilt on every `Schema::new`. Adding fields requires a fresh
//!   `Schema`, not a mutation.

use std::collections::HashMap;

use crate::{crs::Crs, geometry::GeometryType, value::ValueType, Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub alias: Option<String>,
    pub ty: ValueType,
    pub nullable: bool,
    /// Driver-defined width hint (string max-length, binary cap, …).
    pub width: Option<u32>,
}

impl FieldDef {
    pub fn new(name: impl Into<String>, ty: ValueType, nullable: bool) -> Self {
        Self {
            name: name.into(),
            alias: None,
            ty,
            nullable,
            width: None,
        }
    }

    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = Some(alias.into());
        self
    }

    pub fn with_width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }
}

/// Per-layer geometry column metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct GeomField {
    pub name: String,
    pub kind: GeometryType,
    pub has_z: bool,
    pub has_m: bool,
    /// xmin, ymin, zmin, xmax, ymax, zmax — `None` if not declared by the source.
    pub extent: Option<[f64; 6]>,
}

impl GeomField {
    pub fn new(name: impl Into<String>, kind: GeometryType) -> Self {
        Self {
            name: name.into(),
            kind,
            has_z: false,
            has_m: false,
            extent: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub fields: Vec<FieldDef>,
    pub geometry: Option<GeomField>,
    pub crs: Crs,
    name_index: HashMap<String, usize>,
}

impl Schema {
    pub fn new(fields: Vec<FieldDef>, geometry: Option<GeomField>, crs: Crs) -> Self {
        let name_index = fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i))
            .collect();
        Self {
            fields,
            geometry,
            crs,
            name_index,
        }
    }

    pub fn field(&self, idx: usize) -> Option<&FieldDef> {
        self.fields.get(idx)
    }

    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.name_index.get(name).copied()
    }

    pub fn field_by_name(&self, name: &str) -> Option<&FieldDef> {
        self.field_index(name).and_then(|i| self.fields.get(i))
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Validate that an attribute row has the right arity and each value's
    /// type tag matches the schema (nulls pass for nullable fields).
    pub fn validate_row(&self, values: &[crate::Value]) -> Result<()> {
        if values.len() != self.fields.len() {
            return Err(Error::schema(format!(
                "expected {} values, got {}",
                self.fields.len(),
                values.len()
            )));
        }
        for (i, (v, f)) in values.iter().zip(&self.fields).enumerate() {
            if v.is_null() {
                if !f.nullable {
                    return Err(Error::schema(format!(
                        "field {} ({}) is not nullable but value is null",
                        i, f.name
                    )));
                }
                continue;
            }
            match v.ty() {
                Some(t) if t == f.ty => {}
                Some(t) => {
                    return Err(Error::schema(format!(
                        "field {} ({}) expected {:?}, got {:?}",
                        i, f.name, f.ty, t
                    )))
                }
                None => unreachable!("non-null value must have a type"),
            }
        }
        Ok(())
    }
}

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields && self.geometry == other.geometry && self.crs == other.crs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    fn sample() -> Schema {
        Schema::new(
            vec![
                FieldDef::new("id", ValueType::Int64, false),
                FieldDef::new("name", ValueType::String, true),
            ],
            Some(GeomField::new("geom", GeometryType::Point)),
            Crs::Epsg(4326),
        )
    }

    #[test]
    fn name_index_lookup() {
        let s = sample();
        assert_eq!(s.field_index("id"), Some(0));
        assert_eq!(s.field_index("name"), Some(1));
        assert_eq!(s.field_index("missing"), None);
        assert_eq!(s.field_by_name("id").unwrap().ty, ValueType::Int64);
    }

    #[test]
    fn validate_row_happy_path() {
        let s = sample();
        s.validate_row(&[Value::Int64(1), Value::String("a".into())])
            .unwrap();
        s.validate_row(&[Value::Int64(1), Value::Null]).unwrap();
    }

    #[test]
    fn validate_row_rejects_null_in_non_nullable() {
        let s = sample();
        assert!(s
            .validate_row(&[Value::Null, Value::String("a".into())])
            .is_err());
    }

    #[test]
    fn validate_row_rejects_arity_mismatch() {
        let s = sample();
        assert!(s.validate_row(&[Value::Int64(1)]).is_err());
    }

    #[test]
    fn validate_row_rejects_type_mismatch() {
        let s = sample();
        assert!(s
            .validate_row(&[Value::String("nope".into()), Value::Null])
            .is_err());
    }
}

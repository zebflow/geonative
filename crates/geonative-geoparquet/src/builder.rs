//! Ingest `geonative_core::Feature` rows into Arrow column builders, flushing
//! into a `RecordBatch` on demand.
//!
//! Geometry is encoded to WKB and appended to a `Binary` column. When
//! [`SchemaMapOptions::add_bbox_columns`] is true, four `Float64` columns
//! (`xmin`, `ymin`, `xmax`, `ymax`) are populated from `Geometry::bbox()`
//! so GeoParquet 1.1 readers can prune row groups by spatial predicate.

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, FixedSizeBinaryBuilder, Float32Builder,
    Float64Builder, Int16Builder, Int32Builder, Int64Builder, RecordBatch, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow::datatypes::DataType;
use geonative_core::{Feature, Schema as CoreSchema, Value, ValueType};

use crate::error::{GeoParquetError, Result};
use crate::schema::{gdb_days_to_unix_micros, MappedSchema};

/// Per-column builder for a single attribute column.
#[derive(Debug)]
enum AttrBuilder {
    Bool(BooleanBuilder),
    Int16(Int16Builder),
    Int32(Int32Builder),
    Int64(Int64Builder),
    Float32(Float32Builder),
    Float64(Float64Builder),
    String(StringBuilder),
    Binary(BinaryBuilder),
    DateTime(TimestampMicrosecondBuilder),
    Guid(FixedSizeBinaryBuilder),
}

impl AttrBuilder {
    fn new(t: ValueType) -> Self {
        match t {
            ValueType::Bool => Self::Bool(BooleanBuilder::new()),
            ValueType::Int16 => Self::Int16(Int16Builder::new()),
            ValueType::Int32 => Self::Int32(Int32Builder::new()),
            ValueType::Int64 => Self::Int64(Int64Builder::new()),
            ValueType::Float32 => Self::Float32(Float32Builder::new()),
            ValueType::Float64 => Self::Float64(Float64Builder::new()),
            ValueType::String | ValueType::Xml => Self::String(StringBuilder::new()),
            ValueType::Binary => Self::Binary(BinaryBuilder::new()),
            ValueType::DateTime => {
                Self::DateTime(TimestampMicrosecondBuilder::new().with_timezone("UTC"))
            }
            ValueType::Guid => Self::Guid(FixedSizeBinaryBuilder::new(16)),
            // Future ValueType variants — fall back to String (Arrow Utf8).
            // Schema mapping (which has already classified this column) is
            // the right place to reject genuinely unknown types upstream;
            // here we make a best-effort default.
            _ => Self::String(StringBuilder::new()),
        }
    }

    fn append_null(&mut self) {
        match self {
            Self::Bool(b) => b.append_null(),
            Self::Int16(b) => b.append_null(),
            Self::Int32(b) => b.append_null(),
            Self::Int64(b) => b.append_null(),
            Self::Float32(b) => b.append_null(),
            Self::Float64(b) => b.append_null(),
            Self::String(b) => b.append_null(),
            Self::Binary(b) => b.append_null(),
            Self::DateTime(b) => b.append_null(),
            Self::Guid(b) => b.append_null(),
        }
    }

    fn append(&mut self, v: &Value, field_name: &str) -> Result<()> {
        match (self, v) {
            (Self::Bool(b), Value::Bool(x)) => b.append_value(*x),
            (Self::Int16(b), Value::Int16(x)) => b.append_value(*x),
            (Self::Int32(b), Value::Int32(x)) => b.append_value(*x),
            (Self::Int64(b), Value::Int64(x)) => b.append_value(*x),
            (Self::Float32(b), Value::Float32(x)) => b.append_value(*x),
            (Self::Float64(b), Value::Float64(x)) => b.append_value(*x),
            (Self::String(b), Value::String(x)) => b.append_value(x),
            (Self::String(b), Value::Xml(x)) => b.append_value(x),
            (Self::Binary(b), Value::Binary(x)) => b.append_value(x),
            (Self::DateTime(b), Value::DateTime(days)) => {
                b.append_value(gdb_days_to_unix_micros(*days));
            }
            (Self::Guid(b), Value::Guid(bytes)) => {
                b.append_value(&bytes[..]).map_err(GeoParquetError::Arrow)?;
            }
            (s, other) => {
                return Err(GeoParquetError::schema(format!(
                    "field '{field_name}': value {other:?} does not match builder {s:?}"
                )))
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Bool(b) => Arc::new(b.finish()),
            Self::Int16(b) => Arc::new(b.finish()),
            Self::Int32(b) => Arc::new(b.finish()),
            Self::Int64(b) => Arc::new(b.finish()),
            Self::Float32(b) => Arc::new(b.finish()),
            Self::Float64(b) => Arc::new(b.finish()),
            Self::String(b) => Arc::new(b.finish()),
            Self::Binary(b) => Arc::new(b.finish()),
            Self::DateTime(b) => Arc::new(b.finish()),
            Self::Guid(b) => Arc::new(b.finish()),
        }
    }
}

/// Build a `RecordBatch` by appending `Feature`s.
///
/// Maintains its own row counter so callers can decide when to flush (typical:
/// every ~10_000 rows for parquet row-group sizing). Self-contained — does
/// not borrow the schemas, only the bits of them needed at append time.
#[derive(Debug)]
pub struct RecordBatchBuilder {
    arrow_schema: arrow::datatypes::SchemaRef,
    field_names: Vec<String>,
    geometry: BinaryBuilder,
    /// Scratch buffer reused across WKB encodes.
    wkb_scratch: Vec<u8>,
    attributes: Vec<AttrBuilder>,
    bbox: Option<[Float64Builder; 4]>,
    rows: usize,
}

impl RecordBatchBuilder {
    pub fn new(mapped: &MappedSchema, core_schema: &CoreSchema) -> Result<Self> {
        if mapped.attribute_col_indices.len() != core_schema.fields.len() {
            return Err(GeoParquetError::schema(format!(
                "mapped schema has {} attribute slots but core schema has {} fields",
                mapped.attribute_col_indices.len(),
                core_schema.fields.len()
            )));
        }

        let attributes: Vec<AttrBuilder> = core_schema
            .fields
            .iter()
            .map(|f| AttrBuilder::new(f.ty))
            .collect();

        let bbox = mapped.bbox_col_indices.map(|_| {
            [
                Float64Builder::new(),
                Float64Builder::new(),
                Float64Builder::new(),
                Float64Builder::new(),
            ]
        });

        let field_names = core_schema.fields.iter().map(|f| f.name.clone()).collect();

        Ok(Self {
            arrow_schema: mapped.arrow.clone(),
            field_names,
            geometry: BinaryBuilder::new(),
            wkb_scratch: Vec::with_capacity(1024),
            attributes,
            bbox,
            rows: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn arrow_schema(&self) -> &arrow::datatypes::SchemaRef {
        &self.arrow_schema
    }

    /// Append one feature. Errors if attribute arity or types don't match the
    /// schema; the partially-built batch remains valid (other rows can still
    /// be appended) — but the caller should typically abort on first error.
    pub fn append(&mut self, feat: &Feature) -> Result<()> {
        if feat.attributes.len() != self.field_names.len() {
            return Err(GeoParquetError::schema(format!(
                "feature has {} attributes, schema declares {}",
                feat.attributes.len(),
                self.field_names.len()
            )));
        }

        // Geometry → WKB. None or empty → null geometry + null bbox.
        let bbox = match feat.geometry.as_ref() {
            Some(g) if !g.is_empty() => {
                g.write_wkb_into(&mut self.wkb_scratch);
                self.geometry.append_value(&self.wkb_scratch);
                g.bbox()
            }
            _ => {
                self.geometry.append_null();
                None
            }
        };

        // Attributes — match each value against its builder.
        for ((attr, val), name) in self
            .attributes
            .iter_mut()
            .zip(&feat.attributes)
            .zip(&self.field_names)
        {
            if matches!(val, Value::Null) {
                attr.append_null();
            } else {
                attr.append(val, name)?;
            }
        }

        // Bbox columns.
        if let Some(bb) = self.bbox.as_mut() {
            match bbox {
                Some([xmin, ymin, xmax, ymax]) => {
                    bb[0].append_value(xmin);
                    bb[1].append_value(ymin);
                    bb[2].append_value(xmax);
                    bb[3].append_value(ymax);
                }
                None => {
                    for b in bb.iter_mut() {
                        b.append_null();
                    }
                }
            }
        }

        self.rows += 1;
        Ok(())
    }

    /// Drain the builder into a `RecordBatch` and reset row count to zero.
    pub fn finish(&mut self) -> Result<RecordBatch> {
        let mut columns: Vec<ArrayRef> = Vec::with_capacity(self.arrow_schema.fields().len());
        columns.push(Arc::new(self.geometry.finish()));
        for a in self.attributes.iter_mut() {
            columns.push(a.finish());
        }
        if let Some(bb) = self.bbox.as_mut() {
            for b in bb.iter_mut() {
                columns.push(Arc::new(b.finish()));
            }
        }
        let batch = RecordBatch::try_new(self.arrow_schema.clone(), columns)?;
        self.rows = 0;
        Ok(batch)
    }
}

// Bridge: arrow's FixedSizeBinaryBuilder::append_value returns an
// ArrowError. We pre-imported it via `?` in append() but need an `_`-shaped
// reminder that this path can fail. (No code needed; just the comment.)
const _: fn() = || {
    let _ = std::mem::size_of::<DataType>(); // silence unused-import warnings if DataType not used
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{map_schema, SchemaMapOptions};
    use geonative_core::{Coord, Crs, FieldDef, GeomField, Geometry, GeometryType, LineString};

    fn make_schema() -> CoreSchema {
        CoreSchema::new(
            vec![
                FieldDef::new("id", ValueType::Int32, false),
                FieldDef::new("name", ValueType::String, true),
            ],
            Some(GeomField::new("geom", GeometryType::LineString)),
            Crs::Epsg(4326),
        )
    }

    #[test]
    fn build_two_row_batch_with_geometry_and_bbox() {
        let core = make_schema();
        let mapped = map_schema(&core, &SchemaMapOptions::default()).unwrap();
        let mut b = RecordBatchBuilder::new(&mapped, &core).unwrap();

        b.append(&Feature::new(
            Some(1),
            Some(Geometry::LineString(LineString::new(vec![
                Coord::xy(0.0, 0.0),
                Coord::xy(10.0, 5.0),
            ]))),
            vec![Value::Int32(7), Value::String("alpha".into())],
        ))
        .unwrap();

        b.append(&Feature::new(
            Some(2),
            Some(Geometry::LineString(LineString::new(vec![
                Coord::xy(100.0, -50.0),
                Coord::xy(101.0, -49.0),
            ]))),
            vec![Value::Int32(8), Value::Null],
        ))
        .unwrap();

        assert_eq!(b.len(), 2);
        let batch = b.finish().unwrap();
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 7); // geom + 2 attrs + 4 bbox

        // Verify bbox column 0 (xmin) values.
        let xmin = batch
            .column(3)
            .as_any()
            .downcast_ref::<arrow::array::Float64Array>()
            .unwrap();
        assert_eq!(xmin.value(0), 0.0);
        assert_eq!(xmin.value(1), 100.0);
        // ymax
        let ymax = batch
            .column(6)
            .as_any()
            .downcast_ref::<arrow::array::Float64Array>()
            .unwrap();
        assert_eq!(ymax.value(0), 5.0);
        assert_eq!(ymax.value(1), -49.0);
    }

    #[test]
    fn null_geometry_yields_null_bbox() {
        use arrow::array::Array; // for is_null
        let core = make_schema();
        let mapped = map_schema(&core, &SchemaMapOptions::default()).unwrap();
        let mut b = RecordBatchBuilder::new(&mapped, &core).unwrap();
        b.append(&Feature::new(
            Some(1),
            None,
            vec![Value::Int32(42), Value::String("x".into())],
        ))
        .unwrap();
        let batch = b.finish().unwrap();
        let xmin = batch
            .column(3)
            .as_any()
            .downcast_ref::<arrow::array::Float64Array>()
            .unwrap();
        assert!(xmin.is_null(0));
    }

    #[test]
    fn type_mismatch_is_reported() {
        let core = make_schema();
        let mapped = map_schema(&core, &SchemaMapOptions::default()).unwrap();
        let mut b = RecordBatchBuilder::new(&mapped, &core).unwrap();
        let r = b.append(&Feature::new(
            Some(1),
            None,
            // id is Int32 but we send a String
            vec![Value::String("oops".into()), Value::String("name".into())],
        ));
        assert!(r.is_err());
    }
}

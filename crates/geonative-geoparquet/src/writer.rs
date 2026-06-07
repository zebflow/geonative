//! High-level GeoParquet writer.
//!
//! Usage:
//!
//! ```ignore
//! use geonative_geoparquet::{GeoParquetWriter, WriterOptions};
//! let mut w = GeoParquetWriter::create(file, &schema, WriterOptions::default())?;
//! for feature in iter {
//!     w.write(&feature?)?;
//! }
//! w.close()?;
//! ```

use std::io::Write;

use geonative_core::{Feature, GeometryType, Schema as CoreSchema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

use crate::builder::RecordBatchBuilder;
use crate::error::{GeoParquetError, Result};
use crate::meta::{build_geo_metadata_json, GeoMetadataInput};
use crate::schema::{map_schema, MappedSchema, SchemaMapOptions};

/// User-tunable options for the writer.
#[derive(Debug, Clone)]
pub struct WriterOptions {
    /// Rows per batch / Parquet row group. Default 10_000 — matches zebflow's
    /// `geoparquet_optimize.rs`.
    pub batch_size: usize,
    /// Compression codec for parquet pages. Default ZSTD level 3 (a good
    /// size/speed trade-off; also matches zebflow).
    pub compression: Compression,
    /// Add `xmin/ymin/xmax/ymax` covering columns + declare them in the
    /// GeoParquet 1.1 `covering` metadata. Enables row-group pruning by
    /// readers that honour the spec.
    pub add_bbox_columns: bool,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            batch_size: 10_000,
            compression: Compression::ZSTD(Default::default()),
            add_bbox_columns: true,
        }
    }
}

/// Writes features into a Parquet file with GeoParquet 1.1 metadata.
///
/// The writer is bound to one input schema; the layer's geometry-type
/// declaration is captured at construction time and emitted in the `geo`
/// metadata. Multi-geometry-type layers are not yet auto-detected — the
/// schema's declared type wins.
pub struct GeoParquetWriter<W: Write + Send> {
    inner: ArrowWriter<W>,
    mapped: MappedSchema,
    builder: RecordBatchBuilder,
    core_schema: CoreSchema,
    batch_size: usize,
    add_bbox_columns: bool,
    layer_geometry_type: GeometryType,
}

impl<W: Write + Send> GeoParquetWriter<W> {
    pub fn create(sink: W, schema: &CoreSchema, opts: WriterOptions) -> Result<Self> {
        let schema_map_opts = SchemaMapOptions {
            add_bbox_columns: opts.add_bbox_columns,
            ..Default::default()
        };
        let mapped = map_schema(schema, &schema_map_opts)?;

        let writer_props = WriterProperties::builder()
            .set_compression(opts.compression)
            .set_max_row_group_row_count(Some(opts.batch_size))
            .build();

        let inner = ArrowWriter::try_new(sink, mapped.arrow.clone(), Some(writer_props))?;

        let builder = RecordBatchBuilder::new(&mapped, schema)?;

        let layer_geometry_type = schema
            .geometry
            .as_ref()
            .map(|g| g.kind)
            .unwrap_or(GeometryType::Point); // arbitrary default for attr-only

        Ok(Self {
            inner,
            mapped,
            builder,
            core_schema: schema.clone(),
            batch_size: opts.batch_size,
            add_bbox_columns: opts.add_bbox_columns,
            layer_geometry_type,
        })
    }

    /// Append one feature; flushes a row group when `batch_size` is reached.
    pub fn write(&mut self, feat: &Feature) -> Result<()> {
        self.builder.append(feat)?;
        if self.builder.len() >= self.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.builder.is_empty() {
            return Ok(());
        }
        let batch = self.builder.finish()?;
        self.inner.write(&batch)?;
        Ok(())
    }

    /// Finish remaining rows, attach the `geo` metadata, and close the file.
    pub fn close(mut self) -> Result<()> {
        self.flush()?;

        let geo_json = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: self
                .mapped
                .arrow
                .field(self.mapped.geometry_col_index)
                .name(),
            layer_geometry_type: self.layer_geometry_type,
            crs: &self.core_schema.crs,
            include_bbox_covering: self.add_bbox_columns,
        });
        self.inner.append_key_value_metadata(KeyValue {
            key: "geo".to_string(),
            value: Some(geo_json),
        });

        self.inner.close()?;
        Ok(())
    }
}

// Helper: write a Feature iterator into a fresh file at `path`. Convenience
// wrapper for the common case of "convert all of a layer".
pub fn write_features_to_path<I>(
    path: impl AsRef<std::path::Path>,
    schema: &CoreSchema,
    features: I,
    opts: WriterOptions,
) -> Result<usize>
where
    I: IntoIterator<Item = std::result::Result<Feature, geonative_core::Error>>,
{
    let file = std::fs::File::create(path)?;
    let mut w = GeoParquetWriter::create(file, schema, opts)?;
    let mut count = 0usize;
    for feat in features {
        let feat = feat?;
        w.write(&feat)?;
        count += 1;
    }
    w.close()?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::{Coord, Crs, FieldDef, GeomField, Geometry, LineString, Value, ValueType};
    use std::io::Cursor;

    fn make_schema() -> CoreSchema {
        CoreSchema::new(
            vec![
                FieldDef::new("id", ValueType::Int32, false),
                FieldDef::new("name", ValueType::String, true),
            ],
            Some(GeomField::new("geometry", GeometryType::LineString)),
            Crs::Epsg(4326),
        )
    }

    #[test]
    fn write_and_close_yields_nonempty_parquet() {
        let schema = make_schema();
        let mut buf = Vec::<u8>::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut w = GeoParquetWriter::create(cursor, &schema, WriterOptions::default()).unwrap();

            for i in 0i32..5 {
                let feat = Feature::new(
                    Some(i as i64 + 1),
                    Some(Geometry::LineString(LineString::new(vec![
                        Coord::xy(i as f64, i as f64),
                        Coord::xy((i + 1) as f64, (i + 1) as f64),
                    ]))),
                    vec![Value::Int32(i + 1), Value::String(format!("name-{i}"))],
                );
                w.write(&feat).unwrap();
            }
            w.close().unwrap();
        }

        // Parquet magic at end: "PAR1"
        assert!(buf.len() > 100, "parquet file should have body");
        assert_eq!(&buf[buf.len() - 4..], b"PAR1");
        // And at start
        assert_eq!(&buf[..4], b"PAR1");
    }

    #[test]
    fn roundtrip_geo_metadata_present_in_file() {
        use parquet::file::reader::{FileReader, SerializedFileReader};

        let schema = make_schema();
        let mut buf = Vec::<u8>::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut w = GeoParquetWriter::create(cursor, &schema, WriterOptions::default()).unwrap();
            w.write(&Feature::new(
                Some(1),
                Some(Geometry::LineString(LineString::new(vec![
                    Coord::xy(0.0, 0.0),
                    Coord::xy(1.0, 1.0),
                ]))),
                vec![Value::Int32(1), Value::String("only".into())],
            ))
            .unwrap();
            w.close().unwrap();
        }

        let reader = SerializedFileReader::new(bytes::Bytes::from(buf)).unwrap();
        let md = reader.metadata().file_metadata();
        let geo = md
            .key_value_metadata()
            .as_ref()
            .and_then(|kvs| kvs.iter().find(|kv| kv.key == "geo"))
            .and_then(|kv| kv.value.clone())
            .expect("geo metadata not found");
        assert!(geo.contains(r#""version":"1.1.0""#));
        assert!(geo.contains(r#""encoding":"WKB""#));
        assert!(geo.contains(r#""code":4326"#));
        assert!(geo.contains(r#""covering""#));
    }
}

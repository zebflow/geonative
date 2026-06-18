//! Async, object-store-backed GeoParquet writer.
//!
//! Mirrors [`crate::writer::GeoParquetWriter`] but streams output through
//! [`object_store`]'s multipart-upload path — so writes go directly to
//! S3 / Azure Blob / GCS / R2 / local-fs as bytes are produced, with no
//! local materialisation step.
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use object_store::aws::AmazonS3Builder;
//! use geonative_geoparquet::async_writer::GeoParquetAsyncWriter;
//!
//! let store: Arc<dyn object_store::ObjectStore> = Arc::new(
//!     AmazonS3Builder::new()
//!         .with_bucket_name("my-bucket")
//!         .with_region("ap-southeast-2")
//!         .build()?
//! );
//! let mut writer = GeoParquetAsyncWriter::create(
//!     store,
//!     "out/vicmap.parquet".into(),
//!     &schema,
//!     WriterOptions::default(),
//! ).await?;
//! for feature in features {
//!     writer.write(&feature).await?;
//! }
//! writer.close().await?;
//! ```

use std::sync::Arc;

use geonative_core::{Feature, GeometryType, Schema as CoreSchema};
use object_store::buffered::BufWriter;
use object_store::ObjectStore;
use parquet::arrow::AsyncArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

use crate::builder::RecordBatchBuilder;
use crate::error::Result;
use crate::meta::{build_geo_metadata_json, GeoMetadataInput};
use crate::schema::{map_schema, MappedSchema, SchemaMapOptions};
use crate::writer::WriterOptions;

/// Async GeoParquet writer streaming to any [`ObjectStore`].
pub struct GeoParquetAsyncWriter {
    inner: AsyncArrowWriter<BufWriter>,
    mapped: MappedSchema,
    builder: RecordBatchBuilder,
    core_schema: CoreSchema,
    layer_geometry_type: GeometryType,
    batch_size: usize,
    add_bbox_columns: bool,
}

impl std::fmt::Debug for GeoParquetAsyncWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoParquetAsyncWriter")
            .field("schema_fields", &self.core_schema.fields.len())
            .field("pending_rows", &self.builder.len())
            .field("batch_size", &self.batch_size)
            .finish()
    }
}

impl GeoParquetAsyncWriter {
    /// Create a writer that streams to `store` at `path`. Multipart upload
    /// starts on first flush; finalised on `close`.
    pub async fn create(
        store: Arc<dyn ObjectStore>,
        path: object_store::path::Path,
        schema: &CoreSchema,
        opts: WriterOptions,
    ) -> Result<Self> {
        let schema_map_opts = SchemaMapOptions {
            add_bbox_columns: opts.add_bbox_columns,
            preserve_source_geometry_name: opts.preserve_source_geometry_name,
            ..Default::default()
        };
        let mapped = map_schema(schema, &schema_map_opts)?;

        // BufWriter wraps the store's multipart-upload session as an
        // AsyncWrite sink. AsyncArrowWriter then streams parquet bytes
        // through it. The first part uploads when the buffer crosses the
        // 5 MB threshold (S3 minimum); the final part flushes on close.
        let buf_writer = BufWriter::new(store, path);

        let writer_props = WriterProperties::builder()
            .set_compression(opts.compression)
            .set_max_row_group_row_count(Some(opts.batch_size))
            .build();

        let inner = AsyncArrowWriter::try_new(buf_writer, mapped.arrow.clone(), Some(writer_props))?;
        let builder = RecordBatchBuilder::new(&mapped, schema)?;

        let layer_geometry_type = schema
            .geometry
            .as_ref()
            .map(|g| g.kind)
            .unwrap_or(GeometryType::Point);

        Ok(Self {
            inner,
            mapped,
            builder,
            core_schema: schema.clone(),
            layer_geometry_type,
            batch_size: opts.batch_size,
            add_bbox_columns: opts.add_bbox_columns,
        })
    }

    /// Append one feature. Flushes a row group when `batch_size` is reached.
    pub async fn write(&mut self, feature: &Feature) -> Result<()> {
        self.builder.append(feature)?;
        if self.builder.len() >= self.batch_size {
            self.flush().await?;
        }
        Ok(())
    }

    /// Finalise: flush any pending features, write GeoParquet `geo`
    /// metadata, close the parquet writer, and complete the multipart
    /// upload to the object store.
    pub async fn close(mut self) -> Result<()> {
        self.flush().await?;

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
        self.inner.close().await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        if self.builder.is_empty() {
            return Ok(());
        }
        let batch = self.builder.finish()?;
        self.inner.write(&batch).await?;
        Ok(())
    }
}

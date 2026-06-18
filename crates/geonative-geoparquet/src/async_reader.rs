//! Async, object-store-backed GeoParquet reader.
//!
//! Mirrors [`crate::reader::GeoParquetReader`] but reads via the
//! [`object_store`] crate — so the source can be S3, Azure Blob, GCS, R2,
//! HTTP, or a local file, transparently. **Only the byte ranges needed for
//! the read are fetched** (footer first → metadata → per-row-group
//! ranges) so a 50 GB GeoParquet in S3 is processed without ever
//! downloading the whole file.
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use object_store::aws::AmazonS3Builder;
//! use geonative_geoparquet::async_reader::GeoParquetAsyncReader;
//! use futures::StreamExt;
//!
//! let store: Arc<dyn object_store::ObjectStore> = Arc::new(
//!     AmazonS3Builder::new()
//!         .with_bucket_name("my-bucket")
//!         .with_region("ap-southeast-2")
//!         .build()?
//! );
//! let reader = GeoParquetAsyncReader::open(store, "data/vicmap.parquet".into()).await?;
//! let mut features = reader.into_features();
//! while let Some(feat) = features.next().await {
//!     let feat = feat?;
//!     // process
//! }
//! ```

use std::sync::Arc;

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef as ArrowSchemaRef;
use futures::stream::Stream;
use futures::StreamExt;
use geonative_core::{Feature, Schema as CoreSchema};
use object_store::ObjectStore;
use parquet::arrow::async_reader::{ParquetObjectReader, ParquetRecordBatchStream};
use parquet::arrow::ParquetRecordBatchStreamBuilder;

use crate::error::{GeoParquetError, Result};
use crate::reader::{decode_row, reconstruct_schema};

/// Async, range-fetching GeoParquet reader.
pub struct GeoParquetAsyncReader {
    schema: CoreSchema,
    arrow_schema: ArrowSchemaRef,
    geom_col_idx: usize,
    attr_col_indices: Vec<usize>,
    stream_builder: Option<ParquetRecordBatchStreamBuilder<ParquetObjectReader>>,
}

impl std::fmt::Debug for GeoParquetAsyncReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoParquetAsyncReader")
            .field("schema_fields", &self.schema.fields.len())
            .field("arrow_schema_fields", &self.arrow_schema.fields().len())
            .field("geom_col_idx", &self.geom_col_idx)
            .finish()
    }
}

impl GeoParquetAsyncReader {
    /// Open a GeoParquet object from `store` at `path`. Reads the footer
    /// (typically <100 KB regardless of file size) and parses the schema +
    /// GeoParquet metadata. Feature data is not fetched until you iterate.
    pub async fn open(store: Arc<dyn ObjectStore>, path: object_store::path::Path) -> Result<Self> {
        // ParquetObjectReader fetches the footer lazily; it will issue a
        // HEAD on first metadata access. No need for us to pre-fetch.
        let object_reader = ParquetObjectReader::new(store, path);
        let stream_builder = ParquetRecordBatchStreamBuilder::new(object_reader).await?;

        let arrow_schema = stream_builder.schema().clone();
        let kv = stream_builder
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .as_ref()
            .and_then(|kvs| kvs.iter().find(|kv| kv.key == "geo"))
            .and_then(|kv| kv.value.clone());

        let (schema, geom_col_idx, attr_col_indices) =
            reconstruct_schema(&arrow_schema, kv.as_deref())?;

        Ok(Self {
            schema,
            arrow_schema,
            geom_col_idx,
            attr_col_indices,
            stream_builder: Some(stream_builder),
        })
    }

    /// The reconstructed `geonative-core` schema. Available immediately
    /// after `open` — no further async work needed.
    pub fn schema(&self) -> &CoreSchema {
        &self.schema
    }

    pub fn arrow_schema(&self) -> &ArrowSchemaRef {
        &self.arrow_schema
    }

    /// Consume the reader and produce an async stream of `Feature`s.
    /// Row groups fetch lazily — each `poll_next` may trigger one
    /// object-store range read.
    pub fn into_features(mut self) -> AsyncFeatureStream {
        let stream = self
            .stream_builder
            .take()
            .expect("stream builder already consumed")
            .build()
            .expect("parquet stream build");
        AsyncFeatureStream {
            inner: stream,
            geom_col_idx: self.geom_col_idx,
            attr_col_indices: self.attr_col_indices,
            buffered: std::collections::VecDeque::new(),
        }
    }
}

/// Stream of decoded `Feature`s pulled from row-group batches.
pub struct AsyncFeatureStream {
    inner: ParquetRecordBatchStream<ParquetObjectReader>,
    geom_col_idx: usize,
    attr_col_indices: Vec<usize>,
    /// Features decoded from the last batch but not yet emitted.
    buffered: std::collections::VecDeque<Result<Feature>>,
}

impl AsyncFeatureStream {
    fn decode_batch(&mut self, batch: RecordBatch) {
        for row in 0..batch.num_rows() {
            self.buffered.push_back(decode_row(
                &batch,
                row,
                self.geom_col_idx,
                &self.attr_col_indices,
            ));
        }
    }
}

impl Stream for AsyncFeatureStream {
    type Item = Result<Feature>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Feature>>> {
        use std::task::Poll;
        loop {
            if let Some(item) = self.buffered.pop_front() {
                return Poll::Ready(Some(item));
            }
            match self.inner.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Ok(batch))) => {
                    self.decode_batch(batch);
                    // Loop to drain (or pull next if batch was empty).
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(GeoParquetError::Parquet(e))))
                }
            }
        }
    }
}

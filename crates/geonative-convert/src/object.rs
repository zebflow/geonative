//! Async, object-store-backed convert pipeline.
//!
//! Mirror of [`crate::convert::convert`] for the cases where the source or
//! sink lives in an object store (S3 / Azure / GCS / R2 / HTTP).
//!
//! ## Design
//!
//! A [`DataLocation`] is either a local path or a `(ObjectStore, Path)`
//! pair. [`convert_async`] dispatches on the pair:
//!
//! | source         | sink           | path                                          |
//! |----------------|----------------|-----------------------------------------------|
//! | Local          | Local          | delegates to sync `convert()` in a blocking task |
//! | Local (any)    | ObjectStore (parquet) | sync reader → mpsc → async parquet writer |
//! | ObjectStore (parquet) | Local (any) | async parquet reader → blocking-task sink |
//! | ObjectStore (parquet) | ObjectStore (parquet) | pure-async reader → writer |
//!
//! The constraint **"remote side must be GeoParquet"** holds for Sprint
//! 15a — it's the only format with a true range-read async path today. COG
//! (raster) is Sprint 15b; PMTiles (vector tiles) is Sprint 15c.
//!
//! Why no tempfile fallback for "remote non-parquet" yet: range-read is
//! the whole point. If the remote side were FileGDB or Shapefile we'd be
//! downloading the entire archive to a temp dir before doing anything,
//! which is the workflow Zebflow already has on its plain-sync side. We
//! reserve the async path for formats that *actually benefit* from it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use geonative_core::{Crs, Feature, Schema};
use geonative_geoparquet::{GeoParquetAsyncReader, GeoParquetAsyncWriter, WriterOptions};
use geonative_proj::Transformer;
use object_store::{path::Path as OsPath, ObjectStore};

use crate::convert::ConvertStats;
use crate::error::{ConvertError, Result};
use crate::io::{Format, Modality, Sink, SinkOptions, Source};

/// Where a dataset lives.
#[derive(Clone)]
pub enum DataLocation {
    /// Filesystem path; format inferred from the extension.
    Local(PathBuf),
    /// Object store URL (S3 / Azure / GCS / R2 / HTTP). Format inferred
    /// from `ext` — pass the extension explicitly so we don't have to
    /// round-trip through filesystem semantics.
    ObjectStore {
        store: Arc<dyn ObjectStore>,
        path: OsPath,
        /// Extension without the leading dot, e.g. `"parquet"`. Drives
        /// format dispatch.
        ext: String,
    },
}

impl std::fmt::Debug for DataLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(p) => write!(f, "Local({})", p.display()),
            Self::ObjectStore { path, ext, .. } => {
                write!(f, "ObjectStore(path={path}, ext={ext})")
            }
        }
    }
}

impl DataLocation {
    fn format(&self) -> Result<Format> {
        match self {
            Self::Local(p) => Format::from_path(p),
            Self::ObjectStore { ext, .. } => Format::from_extension(ext),
        }
    }

    fn is_remote(&self) -> bool {
        matches!(self, Self::ObjectStore { .. })
    }
}

/// Mirror of [`crate::convert::ConvertOptions`] for the async path. The
/// sync version has a progress callback that's tricky to bridge cleanly
/// across the spawn_blocking boundary — we drop it here for v0.
#[derive(Clone, Debug, Default)]
pub struct AsyncConvertOptions {
    /// FileGDB-only: which layer to extract from the source.
    pub layer: Option<String>,
    /// Sink configuration (batch size, Hilbert sort, bbox columns).
    pub sink: SinkOptions,
    /// Reproject every feature's geometry to this CRS mid-stream.
    pub to_crs: Option<Crs>,
}

/// Convert from any supported source to any supported sink, handling
/// object-store endpoints. See module docs for the dispatch matrix.
pub async fn convert_async(
    source: DataLocation,
    sink: DataLocation,
    opts: AsyncConvertOptions,
) -> Result<ConvertStats> {
    let src_fmt = source.format()?;
    let dst_fmt = sink.format()?;

    if src_fmt.modality() != Modality::Vector || dst_fmt.modality() != Modality::Vector {
        return Err(ConvertError::invalid(
            "convert_async v0 is vector-only — raster async path (COG range-read) lands in Sprint 15b",
        ));
    }

    // Both local → just delegate to sync via spawn_blocking so callers
    // don't have to maintain a parallel API to handle this case.
    if !source.is_remote() && !sink.is_remote() {
        let DataLocation::Local(src_path) = source else {
            unreachable!()
        };
        let DataLocation::Local(dst_path) = sink else {
            unreachable!()
        };
        return delegate_sync(src_path, dst_path, opts).await;
    }

    // Validate any remote side is GeoParquet — the only async-capable
    // format in v0.
    if source.is_remote() && src_fmt != Format::GeoParquet {
        return Err(ConvertError::invalid(format!(
            "async ObjectStore source must be .parquet (got .{})",
            src_fmt.label()
        )));
    }
    if sink.is_remote() && dst_fmt != Format::GeoParquet {
        return Err(ConvertError::invalid(format!(
            "async ObjectStore sink must be .parquet (got .{})",
            dst_fmt.label()
        )));
    }

    match (source, sink) {
        (
            DataLocation::ObjectStore {
                store: src_store,
                path: src_path,
                ..
            },
            DataLocation::ObjectStore {
                store: dst_store,
                path: dst_path,
                ..
            },
        ) => s3_to_s3(src_store, src_path, dst_store, dst_path, opts).await,

        (
            DataLocation::Local(src_path),
            DataLocation::ObjectStore {
                store: dst_store,
                path: dst_path,
                ..
            },
        ) => local_to_s3(src_path, dst_store, dst_path, opts).await,

        (
            DataLocation::ObjectStore {
                store: src_store,
                path: src_path,
                ..
            },
            DataLocation::Local(dst_path),
        ) => s3_to_local(src_store, src_path, dst_path, opts).await,

        // Local→Local was handled above.
        (DataLocation::Local(_), DataLocation::Local(_)) => unreachable!("handled above"),
    }
}

async fn delegate_sync(
    src: PathBuf,
    dst: PathBuf,
    opts: AsyncConvertOptions,
) -> Result<ConvertStats> {
    let sync_opts = crate::convert::ConvertOptions {
        layer: opts.layer,
        sink: opts.sink,
        to_crs: opts.to_crs,
        progress: None,
    };
    tokio::task::spawn_blocking(move || crate::convert::convert(&src, &dst, sync_opts))
        .await
        .map_err(|e| ConvertError::invalid(format!("blocking convert task: {e}")))?
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn writer_opts_from(sink: &SinkOptions) -> WriterOptions {
    WriterOptions {
        batch_size: sink.batch_size,
        add_bbox_columns: sink.add_bbox_columns,
        hilbert_sort: sink.hilbert_sort,
        hilbert_memory_budget_bytes: sink.hilbert_memory_budget_bytes,
        ..WriterOptions::default()
    }
}

/// Apply the (optional) reprojector to a feature in-place. Returns the
/// possibly-mutated feature on success.
fn reproject_inplace(mut feat: Feature, transformer: Option<&Transformer>) -> Result<Feature> {
    if let Some(t) = transformer {
        if let Some(g) = feat.geometry.as_mut() {
            t.transform_geometry(g)?;
        }
    }
    Ok(feat)
}

/// CRS-rewrite a schema if a target was requested.
fn maybe_retarget_crs(mut schema: Schema, to: &Option<Crs>) -> Schema {
    if let Some(target) = to {
        schema.crs = target.clone();
    }
    schema
}

async fn s3_to_s3(
    src_store: Arc<dyn ObjectStore>,
    src_path: OsPath,
    dst_store: Arc<dyn ObjectStore>,
    dst_path: OsPath,
    opts: AsyncConvertOptions,
) -> Result<ConvertStats> {
    let reader = GeoParquetAsyncReader::open(src_store, src_path).await?;
    let schema = reader.schema().clone();
    let transformer = match &opts.to_crs {
        Some(target) => Some(Transformer::from_crs(&schema.crs, target)?),
        None => None,
    };
    let out_schema = maybe_retarget_crs(schema, &opts.to_crs);

    let mut writer = GeoParquetAsyncWriter::create(
        dst_store,
        dst_path,
        &out_schema,
        writer_opts_from(&opts.sink),
    )
    .await?;

    let mut stream = reader.into_features();
    let start = Instant::now();
    let mut count: u64 = 0;
    while let Some(feat) = stream.next().await {
        let feat = feat?;
        let feat = reproject_inplace(feat, transformer.as_ref())?;
        writer.write(&feat).await?;
        count += 1;
    }
    writer.close().await?;

    Ok(ConvertStats {
        features: count,
        elapsed_secs: start.elapsed().as_secs_f64(),
        output_bytes: 0, // would need a HEAD call to know post-multipart size
    })
}

async fn local_to_s3(
    src_path: PathBuf,
    dst_store: Arc<dyn ObjectStore>,
    dst_path: OsPath,
    opts: AsyncConvertOptions,
) -> Result<ConvertStats> {
    // Open the source on a blocking thread to grab its schema, then start
    // a producer task that streams features into an mpsc channel. The
    // consumer (async writer) pulls from the channel back here.
    let (schema_tx, schema_rx) = tokio::sync::oneshot::channel::<Result<Schema>>();
    let (feat_tx, mut feat_rx) = tokio::sync::mpsc::channel::<Result<Feature>>(256);

    let layer = opts.layer.clone();
    let src_for_task = src_path.clone();
    let producer = tokio::task::spawn_blocking(move || {
        let source = match Source::open(&src_for_task, layer.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                let _ = schema_tx.send(Err(e));
                return;
            }
        };
        let schema = match source.schema_cloned() {
            Ok(s) => s,
            Err(e) => {
                let _ = schema_tx.send(Err(e));
                return;
            }
        };
        if schema_tx.send(Ok(schema)).is_err() {
            return; // receiver dropped, abort
        }
        // Stream each feature into the channel. blocking_send is the right
        // call from inside spawn_blocking — it cooperates with the runtime.
        let _ = source.for_each(|feat| {
            // If the consumer hung up, stop early.
            feat_tx
                .blocking_send(Ok(feat))
                .map_err(|e| ConvertError::invalid(format!("feature channel closed: {e}")))?;
            Ok(())
        });
    });

    let schema = schema_rx
        .await
        .map_err(|e| ConvertError::invalid(format!("schema oneshot: {e}")))??;
    let transformer = match &opts.to_crs {
        Some(target) => Some(Transformer::from_crs(&schema.crs, target)?),
        None => None,
    };
    let out_schema = maybe_retarget_crs(schema, &opts.to_crs);

    let mut writer = GeoParquetAsyncWriter::create(
        dst_store,
        dst_path,
        &out_schema,
        writer_opts_from(&opts.sink),
    )
    .await?;

    let start = Instant::now();
    let mut count: u64 = 0;
    while let Some(item) = feat_rx.recv().await {
        let feat = item?;
        let feat = reproject_inplace(feat, transformer.as_ref())?;
        writer.write(&feat).await?;
        count += 1;
    }
    writer.close().await?;
    producer
        .await
        .map_err(|e| ConvertError::invalid(format!("producer join: {e}")))?;

    Ok(ConvertStats {
        features: count,
        elapsed_secs: start.elapsed().as_secs_f64(),
        output_bytes: 0,
    })
}

async fn s3_to_local(
    src_store: Arc<dyn ObjectStore>,
    src_path: OsPath,
    dst_path: PathBuf,
    opts: AsyncConvertOptions,
) -> Result<ConvertStats> {
    let reader = GeoParquetAsyncReader::open(src_store, src_path).await?;
    let schema = reader.schema().clone();
    let transformer = match &opts.to_crs {
        Some(target) => Some(Transformer::from_crs(&schema.crs, target)?),
        None => None,
    };
    let out_schema = maybe_retarget_crs(schema, &opts.to_crs);

    // Channel: async-side pushes features in, blocking sink drains them.
    let (feat_tx, feat_rx) = std::sync::mpsc::sync_channel::<Result<Feature>>(256);

    let sink_opts = opts.sink.clone();
    let dst_for_task = dst_path.clone();
    let schema_for_task = out_schema.clone();
    let consumer = tokio::task::spawn_blocking(move || -> Result<u64> {
        let mut sink = Sink::create(&dst_for_task, &schema_for_task, sink_opts)?;
        let mut count = 0u64;
        while let Ok(item) = feat_rx.recv() {
            let feat = item?;
            sink.write(&feat)?;
            count += 1;
        }
        sink.close()?;
        Ok(count)
    });

    let mut stream = reader.into_features();
    let start = Instant::now();
    while let Some(feat) = stream.next().await {
        let feat = feat?;
        let feat = reproject_inplace(feat, transformer.as_ref())?;
        if feat_tx.send(Ok(feat)).is_err() {
            // Sink died — stop pulling.
            break;
        }
    }
    drop(feat_tx); // signal end-of-stream to the consumer

    let count = consumer
        .await
        .map_err(|e| ConvertError::invalid(format!("consumer join: {e}")))??;

    let output_bytes = std::fs::metadata(&dst_path).map(|m| m.len()).unwrap_or(0);
    Ok(ConvertStats {
        features: count,
        elapsed_secs: start.elapsed().as_secs_f64(),
        output_bytes,
    })
}

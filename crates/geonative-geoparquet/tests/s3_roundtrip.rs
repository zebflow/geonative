//! End-to-end async object_store roundtrip against MinIO.
//!
//! Self-skips when MinIO isn't reachable at `127.0.0.1:9100`, so
//! `cargo test --features s3` stays green on machines without docker.
//!
//! Prerequisites when you do want it to run:
//!
//! ```sh
//! docker run -d --name geonative-test-minio \
//!     -p 9100:9000 -p 9101:9001 \
//!     -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
//!     quay.io/minio/minio server /data --console-address ":9001"
//!
//! docker exec geonative-test-minio \
//!     mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
//! docker exec geonative-test-minio mc mb -p local/geonative-test
//! ```

#![cfg(feature = "s3")]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use geonative_core::{
    Coord, Crs, Feature, FieldDef, GeomField, Geometry, GeometryType, LineString, Schema, Value,
    ValueType,
};
use geonative_geoparquet::{GeoParquetAsyncReader, GeoParquetAsyncWriter, WriterOptions};
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

const MINIO_ENDPOINT: &str = "http://127.0.0.1:9100";
const MINIO_KEY: &str = "minioadmin";
const MINIO_SECRET: &str = "minioadmin";

fn try_build_store(bucket: &str) -> Option<Arc<dyn ObjectStore>> {
    let s3 = AmazonS3Builder::new()
        .with_endpoint(MINIO_ENDPOINT)
        .with_access_key_id(MINIO_KEY)
        .with_secret_access_key(MINIO_SECRET)
        .with_bucket_name(bucket)
        .with_region("us-east-1")
        .with_allow_http(true)
        .build()
        .ok()?;
    Some(Arc::new(s3))
}

/// Plain TCP connect probe — if MinIO isn't running locally we self-skip
/// so contributors without docker still get green test runs.
async fn minio_reachable() -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect("127.0.0.1:9100"),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some()
}

const BUCKET: &str = "geonative-test";

fn sample_schema() -> Schema {
    Schema::new(
        vec![
            FieldDef::new("id", ValueType::Int32, false),
            FieldDef::new("label", ValueType::String, true),
        ],
        Some(GeomField::new("geometry", GeometryType::LineString)),
        Crs::Epsg(4326),
    )
}

fn sample_features(n: usize) -> Vec<Feature> {
    (0..n)
        .map(|i| {
            Feature::new(
                Some(i as i64 + 1),
                Some(Geometry::LineString(LineString::new(vec![
                    Coord::xy(i as f64, i as f64),
                    Coord::xy((i + 1) as f64, (i + 1) as f64),
                ]))),
                vec![Value::Int32(i as i32 + 1), Value::String(format!("row-{i}"))],
            )
        })
        .collect()
}

#[tokio::test]
async fn s3_async_writer_then_reader_roundtrip() {
    if !minio_reachable().await {
        eprintln!("skipping s3_async_writer_then_reader_roundtrip: MinIO not reachable at {MINIO_ENDPOINT}");
        return;
    }

    let store = try_build_store(BUCKET).expect("build s3 store");

    let schema = sample_schema();
    let features = sample_features(2500);
    let path = Path::from("roundtrip/sample.parquet");

    // --- Write ---
    {
        let mut w = GeoParquetAsyncWriter::create(
            store.clone(),
            path.clone(),
            &schema,
            WriterOptions {
                batch_size: 500, // force multiple row groups
                ..Default::default()
            },
        )
        .await
        .expect("create async writer");
        for feat in &features {
            w.write(feat).await.expect("write feature");
        }
        w.close().await.expect("close async writer");
    }

    // --- Verify object lands ---
    let meta = store.head(&path).await.expect("head uploaded object");
    assert!(meta.size > 0, "uploaded parquet has non-zero size");

    // --- Read back ---
    let reader = GeoParquetAsyncReader::open(store.clone(), path.clone())
        .await
        .expect("open async reader");
    assert_eq!(reader.schema().fields.len(), 2);
    assert_eq!(
        reader
            .schema()
            .geometry
            .as_ref()
            .map(|g| g.name.clone())
            .as_deref(),
        Some("geometry")
    );

    let mut stream = reader.into_features();
    let mut decoded = Vec::new();
    while let Some(feat) = stream.next().await {
        decoded.push(feat.expect("decode feature"));
    }
    assert_eq!(decoded.len(), features.len());

    // Spot-check first/last preserve attribute round-trip.
    assert_eq!(decoded[0].attributes[0], Value::Int32(1));
    assert_eq!(decoded[0].attributes[1], Value::String("row-0".into()));
    let last = decoded.last().unwrap();
    assert_eq!(last.attributes[0], Value::Int32(features.len() as i32));

    // --- Cleanup ---
    let _ = store.delete(&path).await;
}

/// Real-fixture range-read against MinIO. Uploads an externally-produced
/// GeoParquet (DuckDB/PostGIS export with bbox covering columns), then
/// reads it back via the async path. Catches metadata quirks our own
/// writer won't ever produce.
///
/// Self-skips if the fixture isn't present locally.
#[tokio::test]
async fn s3_async_reader_handles_external_fixture() {
    const FIXTURE_PATH: &str = "/Users/mala0061/Dev/research/spatial-index-retrieval/implementations/zebflow-basic/data/users/superadmin/default/files/developer-ready/support_existing_greater_melbourne_boundary.parquet";

    if !minio_reachable().await {
        eprintln!("skipping s3_async_reader_handles_external_fixture: MinIO not reachable");
        return;
    }
    if !std::path::Path::new(FIXTURE_PATH).exists() {
        eprintln!("skipping s3_async_reader_handles_external_fixture: fixture missing at {FIXTURE_PATH}");
        return;
    }

    let store = try_build_store(BUCKET).expect("build s3 store");
    let path = Path::from("fixtures/greater_melbourne_boundary.parquet");

    // Upload the fixture bytes via object_store.
    let bytes = std::fs::read(FIXTURE_PATH).expect("read fixture");
    let bytes_len = bytes.len();
    store
        .put(&path, bytes.into())
        .await
        .expect("put fixture object");

    // Range-read via the async reader. Range-fetch means the actual GETs
    // pull the parquet footer first (~tail of the object), then per-row-group
    // ranges — we don't download the whole `bytes_len` payload.
    let reader = GeoParquetAsyncReader::open(store.clone(), path.clone())
        .await
        .expect("open external fixture");
    let schema_fields = reader.schema().fields.len();
    assert!(schema_fields > 0, "fixture must declare at least 1 attribute");

    let mut stream = reader.into_features();
    let mut count = 0usize;
    let mut first: Option<Feature> = None;
    while let Some(feat) = stream.next().await {
        let f = feat.expect("decode external feature");
        if first.is_none() {
            first = Some(f.clone());
        }
        count += 1;
    }
    assert!(count > 0, "external fixture decoded zero features");
    let first = first.unwrap();
    assert_eq!(first.attributes.len(), schema_fields);
    assert!(first.geometry.is_some(), "first feature should have geometry");

    eprintln!(
        "external fixture: uploaded {bytes_len} bytes, decoded {count} features, {schema_fields} attribute fields"
    );

    let _ = store.delete(&path).await;
}

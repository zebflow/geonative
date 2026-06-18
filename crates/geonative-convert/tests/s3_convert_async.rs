//! Async convert pipeline exercised against MinIO.
//!
//! Self-skips when MinIO isn't reachable. Prereqs are the same as the
//! geonative-geoparquet `s3_roundtrip` test — see that file's doc-comment
//! for the docker / mc commands.

#![cfg(feature = "s3")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use geonative_convert::{convert_async, AsyncConvertOptions, DataLocation};
use geonative_core::{
    Coord, Crs, Feature, FieldDef, GeomField, Geometry, GeometryType, LineString, Schema, Value,
    ValueType,
};
use geonative_geoparquet::{write_features_to_path, WriterOptions};
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as OsPath;
use object_store::{ObjectStore, ObjectStoreExt};

const MINIO_ENDPOINT: &str = "http://127.0.0.1:9100";
const MINIO_KEY: &str = "minioadmin";
const MINIO_SECRET: &str = "minioadmin";
const BUCKET: &str = "geonative-test";

fn try_build_store() -> Option<Arc<dyn ObjectStore>> {
    let s3 = AmazonS3Builder::new()
        .with_endpoint(MINIO_ENDPOINT)
        .with_access_key_id(MINIO_KEY)
        .with_secret_access_key(MINIO_SECRET)
        .with_bucket_name(BUCKET)
        .with_region("us-east-1")
        .with_allow_http(true)
        .build()
        .ok()?;
    Some(Arc::new(s3))
}

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

fn write_local_parquet_fixture(dir: &tempfile::TempDir, n: usize) -> PathBuf {
    let p = dir.path().join("fixture.parquet");
    let schema = sample_schema();
    let features = sample_features(n);
    write_features_to_path(
        &p,
        &schema,
        features.into_iter().map(Ok),
        WriterOptions::default(),
    )
    .expect("write fixture");
    p
}

#[tokio::test]
async fn convert_async_local_to_s3_parquet() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = write_local_parquet_fixture(&tmp, 1500);
    let store = try_build_store().unwrap();
    let dst_path = OsPath::from("convert_async/local_to_s3.parquet");

    let stats = convert_async(
        DataLocation::Local(src.clone()),
        DataLocation::ObjectStore {
            store: store.clone(),
            path: dst_path.clone(),
            ext: "parquet".into(),
        },
        AsyncConvertOptions::default(),
    )
    .await
    .expect("convert local→s3");

    assert_eq!(stats.features, 1500);
    let head = store.head(&dst_path).await.expect("head");
    assert!(head.size > 0);

    let _ = store.delete(&dst_path).await;
}

#[tokio::test]
async fn convert_async_s3_to_local_parquet() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }
    // Stage a fixture in S3 first by doing local→s3.
    let tmp = tempfile::tempdir().unwrap();
    let local_src = write_local_parquet_fixture(&tmp, 800);
    let store = try_build_store().unwrap();
    let s3_path = OsPath::from("convert_async/s3_source.parquet");
    convert_async(
        DataLocation::Local(local_src),
        DataLocation::ObjectStore {
            store: store.clone(),
            path: s3_path.clone(),
            ext: "parquet".into(),
        },
        AsyncConvertOptions::default(),
    )
    .await
    .expect("stage fixture in s3");

    // Now pull it back to a local file.
    let dst = tmp.path().join("pulled.parquet");
    let stats = convert_async(
        DataLocation::ObjectStore {
            store: store.clone(),
            path: s3_path.clone(),
            ext: "parquet".into(),
        },
        DataLocation::Local(dst.clone()),
        AsyncConvertOptions::default(),
    )
    .await
    .expect("convert s3→local");
    assert_eq!(stats.features, 800);
    assert!(dst.exists() && std::fs::metadata(&dst).unwrap().len() > 0);

    let _ = store.delete(&s3_path).await;
}

#[tokio::test]
async fn convert_async_s3_to_s3_parquet() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let local = write_local_parquet_fixture(&tmp, 600);
    let store = try_build_store().unwrap();

    let src_path = OsPath::from("convert_async/s3s3_source.parquet");
    let dst_path = OsPath::from("convert_async/s3s3_dest.parquet");

    // Stage.
    convert_async(
        DataLocation::Local(local),
        DataLocation::ObjectStore {
            store: store.clone(),
            path: src_path.clone(),
            ext: "parquet".into(),
        },
        AsyncConvertOptions::default(),
    )
    .await
    .expect("stage");

    // Server-to-server copy via convert_async.
    let stats = convert_async(
        DataLocation::ObjectStore {
            store: store.clone(),
            path: src_path.clone(),
            ext: "parquet".into(),
        },
        DataLocation::ObjectStore {
            store: store.clone(),
            path: dst_path.clone(),
            ext: "parquet".into(),
        },
        AsyncConvertOptions::default(),
    )
    .await
    .expect("convert s3→s3");

    assert_eq!(stats.features, 600);
    assert!(store.head(&dst_path).await.unwrap().size > 0);

    let _ = store.delete(&src_path).await;
    let _ = store.delete(&dst_path).await;
}

#[tokio::test]
async fn convert_async_rejects_non_parquet_remote() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }
    let store = try_build_store().unwrap();
    let err = convert_async(
        DataLocation::ObjectStore {
            store,
            path: OsPath::from("nope.geojson"),
            ext: "geojson".into(),
        },
        DataLocation::Local("/tmp/whatever.parquet".into()),
        AsyncConvertOptions::default(),
    )
    .await
    .expect_err("should reject non-parquet remote source");
    let msg = err.to_string();
    assert!(
        msg.contains(".parquet") || msg.contains("parquet"),
        "expected parquet-only msg, got: {msg}"
    );
}

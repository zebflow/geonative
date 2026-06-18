//! Async, object-store-backed PMTiles reader exercised against MinIO.
//!
//! Self-skips when MinIO isn't reachable. Same prereqs as the other
//! S3-tagged tests — see `geonative-geoparquet/tests/s3_roundtrip.rs`.

#![cfg(feature = "s3")]

use std::sync::Arc;
use std::time::Duration;

use geonative_pmtiles::{Compression, PmTilesAsyncReader, PmTilesWriter, TileType, WriterOptions};
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

fn fake_tile_bytes(seed: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    for i in 0..32 {
        buf.push(((seed.wrapping_mul(1103515245).wrapping_add(i)) & 0xff) as u8);
    }
    buf
}

fn build_pmtiles_locally(path: &std::path::Path, leaf_split_threshold: usize, leaf_size: usize) {
    let opts = WriterOptions {
        tile_type: TileType::Mvt,
        internal_compression: Compression::Gzip,
        tile_compression: Compression::Gzip,
        min_zoom: 0,
        max_zoom: 3,
        bounds: [140.0, -39.0, 150.0, -34.0],
        center: (145.0, -37.0, 2),
        metadata_json: br#"{"name":"async-test"}"#.to_vec(),
        leaf_split_threshold,
        leaf_size,
    };
    let file = std::fs::File::create(path).unwrap();
    let mut w = PmTilesWriter::create(file, opts);
    // Full coverage z=0..3 = 1 + 4 + 16 + 64 = 85 tiles, all distinct
    let mut seed = 1u64;
    for z in 0u8..=3 {
        let dim = 1u32 << z;
        for x in 0..dim {
            for y in 0..dim {
                w.add_tile(z, x, y, &fake_tile_bytes(seed)).unwrap();
                seed += 1;
            }
        }
    }
    w.finish().unwrap();
}

#[tokio::test]
async fn async_pmtiles_roundtrip_against_minio() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }

    // --- Build a local PMTiles (root-only, no leaves). ---
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("test.pmtiles");
    build_pmtiles_locally(&local, 16_384, 4_096);
    let bytes = std::fs::read(&local).unwrap();
    let bytes_len = bytes.len();

    // --- Upload to MinIO. ---
    let store = try_build_store().unwrap();
    let s3_path = OsPath::from("async_pmtiles/test.pmtiles");
    store.put(&s3_path, bytes.into()).await.expect("put");

    // --- Open via PmTilesAsyncReader (2 GETs: header + root). ---
    let reader = PmTilesAsyncReader::open(store.clone(), s3_path.clone())
        .await
        .expect("open async");
    let h = reader.header();
    assert_eq!(h.tile_entries_count, 85);
    assert_eq!(h.addressed_tiles_count, 85);
    assert_eq!(h.min_zoom, 0);
    assert_eq!(h.max_zoom, 3);

    // --- Pull every tile via per-tile range GETs. ---
    let mut seed = 1u64;
    for z in 0u8..=3 {
        let dim = 1u32 << z;
        for x in 0..dim {
            for y in 0..dim {
                let expected = fake_tile_bytes(seed);
                let got = reader
                    .get_tile(z, x, y)
                    .await
                    .unwrap()
                    .unwrap_or_else(|| panic!("missing tile ({z},{x},{y})"));
                assert_eq!(got, expected, "tile bytes mismatch at ({z},{x},{y})");
                seed += 1;
            }
        }
    }

    // Tile not in the archive (z=4 wasn't added) → None, not error.
    assert!(reader.get_tile(4, 0, 0).await.unwrap().is_none());

    // Metadata roundtrips.
    let meta = reader.metadata().await.unwrap();
    assert_eq!(meta, br#"{"name":"async-test"}"#);

    let _ = store.delete(&s3_path).await;

    eprintln!(
        "async pmtiles roundtrip ok: uploaded {bytes_len} B, decoded 85 tiles via range-read"
    );
}

#[tokio::test]
async fn async_pmtiles_with_leaf_dirs_against_minio() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }

    // Force leaf splitting with a small threshold.
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("leaves.pmtiles");
    build_pmtiles_locally(&local, 4, 4);
    let bytes = std::fs::read(&local).unwrap();

    let store = try_build_store().unwrap();
    let s3_path = OsPath::from("async_pmtiles/leaves.pmtiles");
    store.put(&s3_path, bytes.into()).await.expect("put");

    let reader = PmTilesAsyncReader::open(store.clone(), s3_path.clone())
        .await
        .expect("open async (leaves)");
    assert!(
        reader.header().leaf_dirs_length > 0,
        "leaves should be present"
    );

    // Spot-check a few tiles across zoom levels — these hit the leaf-
    // fetch path (root → leaf → tile), validating cache + lookup.
    assert!(reader.get_tile(0, 0, 0).await.unwrap().is_some());
    assert!(reader.get_tile(2, 1, 2).await.unwrap().is_some());
    assert!(reader.get_tile(3, 5, 6).await.unwrap().is_some());

    let _ = store.delete(&s3_path).await;
}

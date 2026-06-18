//! Async, range-fetching COG reader exercised against MinIO.
//!
//! Self-skips when MinIO isn't reachable at `127.0.0.1:9100`. Same prereqs
//! as `geonative-geoparquet/tests/s3_roundtrip.rs` — see that file for the
//! docker / mc commands. Bucket: `geonative-test`.

#![cfg(feature = "s3")]

use std::sync::Arc;
use std::time::Duration;

use geonative_core::raster::{
    Band, BandDescriptor, GeoTransform, PixelType, RasterProfile, RasterTile,
};
use geonative_core::Crs;
use geonative_geotiff::{AsyncCog, Compression, GeoTiffWriter, WriterOptions};
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

fn band_u8(name: &str) -> BandDescriptor {
    BandDescriptor::new(Some(name.into()), PixelType::U8)
}

fn make_tile(width: u32, height: u32, fill: u8) -> RasterTile {
    let pixels = (width as usize) * (height as usize);
    RasterTile {
        width,
        height,
        bands: vec![Band::new(band_u8("v"), vec![fill; pixels])],
        geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
        crs: Crs::Epsg(3857),
    }
}

fn profile(width: u32, height: u32, tile_size: u32, crs: Crs) -> RasterProfile {
    RasterProfile {
        width,
        height,
        bands: vec![band_u8("v")],
        geo_transform: GeoTransform::north_up(144.0, -37.0, 0.0005, 0.0005),
        crs,
        tile_size: [tile_size, tile_size],
        pyramid_levels: 1,
    }
}

/// Build a 4×4-pixel tiled GeoTIFF (2×2 tiles, each 2×2 px, filled with
/// distinct values), upload it to MinIO, and read it back via AsyncCog.
#[tokio::test]
async fn async_cog_roundtrip_against_minio() {
    if !minio_reachable().await {
        eprintln!("skip: MinIO unreachable");
        return;
    }

    // --- Build a local COG synthesised in a tempfile. ---
    let dir = tempfile::tempdir().unwrap();
    let local_path = dir.path().join("synth.tif");
    {
        let p = profile(4, 4, 2, Crs::Epsg(3857));
        let file = std::fs::File::create(&local_path).unwrap();
        let mut w = GeoTiffWriter::create(
            file,
            &p,
            WriterOptions {
                compression: Compression::Deflate,
                deflate_level: 6,
            },
        )
        .unwrap();
        w.write_tile(0, 0, 0, &make_tile(2, 2, 11)).unwrap();
        w.write_tile(0, 1, 0, &make_tile(2, 2, 22)).unwrap();
        w.write_tile(0, 0, 1, &make_tile(2, 2, 33)).unwrap();
        w.write_tile(0, 1, 1, &make_tile(2, 2, 44)).unwrap();
        w.close().unwrap();
    }
    let bytes = std::fs::read(&local_path).unwrap();
    let bytes_len = bytes.len();
    assert!(bytes_len > 0);

    // --- Upload to MinIO. ---
    let store = try_build_store().unwrap();
    let s3_path = OsPath::from("async_cog/synth.tif");
    store
        .put(&s3_path, bytes.into())
        .await
        .expect("put COG to s3");

    // --- Open via AsyncCog. Should issue 1 HEAD + 1 GET for metadata. ---
    let cog = AsyncCog::open(store.clone(), s3_path.clone())
        .await
        .expect("open async cog");
    assert_eq!(cog.file_size(), bytes_len as u64);
    let pr = cog.profile();
    assert_eq!(pr.width, 4);
    assert_eq!(pr.height, 4);
    assert_eq!(pr.tile_size, [2, 2]);
    assert_eq!(pr.crs, Crs::Epsg(3857));
    assert_eq!(cog.pyramid_level_count(), 1);

    // --- Read all four tiles via per-tile range GETs, verify pixel values ---
    let t00 = cog.read_tile(0, 0, 0).await.expect("tile 0,0");
    let t10 = cog.read_tile(0, 1, 0).await.expect("tile 1,0");
    let t01 = cog.read_tile(0, 0, 1).await.expect("tile 0,1");
    let t11 = cog.read_tile(0, 1, 1).await.expect("tile 1,1");
    assert_eq!(t00.bands[0].data, vec![11; 4]);
    assert_eq!(t10.bands[0].data, vec![22; 4]);
    assert_eq!(t01.bands[0].data, vec![33; 4]);
    assert_eq!(t11.bands[0].data, vec![44; 4]);

    // --- Out-of-range tile should error cleanly without panicking ---
    assert!(cog.read_tile(0, 99, 0).await.is_err());
    assert!(cog.read_tile(5, 0, 0).await.is_err());

    let _ = store.delete(&s3_path).await;

    eprintln!(
        "async cog roundtrip ok: uploaded {bytes_len} B, decoded 4 tiles via range-read"
    );
}

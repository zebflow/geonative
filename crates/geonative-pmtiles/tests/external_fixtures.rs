//! Real-fixture compat: read PMTiles files produced by go-pmtiles via the
//! protomaps/PMTiles repository's canonical spec test fixtures.
//!
//! Self-skips if the fixtures aren't present in `/tmp/pmtiles-fixtures/`.
//! Download them with:
//!
//! ```sh
//! mkdir -p /tmp/pmtiles-fixtures
//! cd /tmp/pmtiles-fixtures
//! for f in test_fixture_1.pmtiles test_fixture_2.pmtiles empty.pmtiles invalid.pmtiles invalid_v4.pmtiles; do
//!     curl -sL -o "$f" "https://raw.githubusercontent.com/protomaps/PMTiles/main/js/test/data/$f"
//! done
//! ```
//!
//! These tests are how we verify that we read what every other PMTiles
//! implementation (go-pmtiles, pmtiles-java, JS pmtiles, maplibre, …)
//! reads. Catches subtle encoding mismatches that pure self-roundtrip
//! tests would silently agree on.

use std::path::PathBuf;

use geonative_pmtiles::{Compression, PmTilesReader, PmtilesError, TileType};

const FIXTURE_DIR: &str = "/tmp/pmtiles-fixtures";

fn fixture(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(FIXTURE_DIR).join(name);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn read_test_fixture_1_round_world_one_tile() {
    let Some(path) = fixture("test_fixture_1.pmtiles") else {
        eprintln!("skip: {FIXTURE_DIR}/test_fixture_1.pmtiles missing");
        return;
    };

    let mut r = PmTilesReader::open(&path).expect("open external fixture 1");

    // Snapshot the header fields up front so we can release the borrow
    // before calling the mutable `get_tile`.
    let (
        tile_type,
        tile_compression,
        internal_compression,
        addressed,
        entries,
        contents,
        min_zoom,
        max_zoom,
        min_lon_e7,
        min_lat_e7,
        max_lon_e7,
        max_lat_e7,
    ) = {
        let h = r.header();
        (
            h.tile_type,
            h.tile_compression,
            h.internal_compression,
            h.addressed_tiles_count,
            h.tile_entries_count,
            h.tile_contents_count,
            h.min_zoom,
            h.max_zoom,
            h.min_lon_e7,
            h.min_lat_e7,
            h.max_lon_e7,
            h.max_lat_e7,
        )
    };

    assert_eq!(tile_type, TileType::Mvt, "fixture 1 should be MVT");
    assert_eq!(tile_compression, Compression::Gzip);
    assert_eq!(internal_compression, Compression::Gzip);
    assert!(addressed >= 1);
    assert!(entries >= 1);
    assert!(contents >= 1);
    assert!(min_zoom <= max_zoom, "min/max zoom must be sane");

    // Pull the tile at (z=0, 0, 0) — fixture 1 covers the whole world at
    // zoom 0 with a single MVT tile.
    let tile = r
        .get_tile(0, 0, 0)
        .expect("get_tile (0,0,0)")
        .expect("fixture 1 must have (0,0,0)");
    assert!(!tile.is_empty(), "tile bytes should be non-empty");

    // Tile bytes are gzipped MVT (header says tile_compression=Gzip).
    // Verify the gzip magic so we know we returned the right blob, not
    // garbage from the wrong offset.
    assert_eq!(
        &tile[0..2],
        &[0x1f, 0x8b],
        "tile bytes should start with gzip magic 1f 8b"
    );

    eprintln!(
        "fixture 1 ok: zooms {min_zoom}-{max_zoom}, addressed={addressed}, entries={entries}, contents={contents}, bbox e7=({min_lon_e7},{min_lat_e7}) → ({max_lon_e7},{max_lat_e7})"
    );
}

#[test]
fn read_test_fixture_2_metadata() {
    let Some(path) = fixture("test_fixture_2.pmtiles") else {
        eprintln!("skip: {FIXTURE_DIR}/test_fixture_2.pmtiles missing");
        return;
    };

    let mut r = PmTilesReader::open(&path).expect("open external fixture 2");
    let meta = r.metadata().expect("read metadata");
    // Metadata is JSON per spec. The Protomaps fixtures encode it as
    // gzipped JSON; our codec layer has already decompressed by now.
    assert!(
        meta.is_empty() || meta.starts_with(b"{"),
        "metadata should be empty or start with JSON '{{', got first 16 bytes = {:02x?}",
        &meta[..meta.len().min(16)]
    );
}

#[test]
fn empty_file_errors_cleanly() {
    let Some(path) = fixture("empty.pmtiles") else {
        eprintln!("skip: {FIXTURE_DIR}/empty.pmtiles missing");
        return;
    };
    let err = PmTilesReader::open(&path).expect_err("empty file must not open");
    // Specifically Truncated — file is 0 bytes, can't fit 127-byte header.
    assert!(
        matches!(err, PmtilesError::Truncated { .. }),
        "expected Truncated, got {err:?}"
    );
}

#[test]
fn invalid_magic_errors_cleanly() {
    let Some(path) = fixture("invalid.pmtiles") else {
        eprintln!("skip: {FIXTURE_DIR}/invalid.pmtiles missing");
        return;
    };
    let err = PmTilesReader::open(&path).expect_err("invalid magic must not open");
    // Either NotPmtiles (bad first bytes) or UnsupportedVersion — both
    // are acceptable "this isn't a v3 PMTiles" outcomes.
    assert!(
        matches!(
            err,
            PmtilesError::NotPmtiles(_) | PmtilesError::UnsupportedVersion(_)
        ),
        "expected NotPmtiles or UnsupportedVersion, got {err:?}"
    );
}

#[test]
fn future_version_4_is_rejected() {
    let Some(path) = fixture("invalid_v4.pmtiles") else {
        eprintln!("skip: {FIXTURE_DIR}/invalid_v4.pmtiles missing");
        return;
    };
    let err = PmTilesReader::open(&path).expect_err("v4 must not open as v3");
    assert!(
        matches!(err, PmtilesError::UnsupportedVersion(4)),
        "expected UnsupportedVersion(4), got {err:?}"
    );
}

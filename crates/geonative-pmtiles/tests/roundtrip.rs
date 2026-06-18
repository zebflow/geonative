//! Local roundtrip + leaf-split correctness for PMTiles.

use geonative_pmtiles::{Compression, PmTilesReader, PmTilesWriter, TileType, WriterOptions};

fn opts() -> WriterOptions {
    WriterOptions {
        tile_type: TileType::Mvt,
        internal_compression: Compression::Gzip,
        tile_compression: Compression::Gzip,
        min_zoom: 0,
        max_zoom: 4,
        bounds: [-180.0, -85.0, 180.0, 85.0],
        center: (0.0, 0.0, 0),
        metadata_json: br#"{"name":"roundtrip-test"}"#.to_vec(),
        leaf_split_threshold: 16_384,
        leaf_size: 4_096,
    }
}

/// Synthesise N distinct tile blobs.
fn fake_tile_bytes(seed: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(32);
    for i in 0..16 {
        buf.push(((seed.wrapping_mul(1103515245).wrapping_add(i as u64)) & 0xff) as u8);
    }
    buf
}

#[test]
fn write_then_read_single_tile_zoom0() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let bytes = b"hello world".to_vec();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = PmTilesWriter::create(file, opts());
        w.add_tile(0, 0, 0, &bytes).unwrap();
        w.finish().unwrap();
    }

    let mut r = PmTilesReader::open(&path).unwrap();
    let h = r.header();
    assert_eq!(h.min_zoom, 0);
    assert_eq!(h.max_zoom, 4);
    assert_eq!(h.tile_entries_count, 1);
    assert_eq!(h.addressed_tiles_count, 1);
    assert_eq!(h.tile_contents_count, 1);

    let got = r.get_tile(0, 0, 0).unwrap().expect("tile (0,0,0)");
    assert_eq!(got, bytes);
    assert!(r.get_tile(1, 0, 0).unwrap().is_none()); // not added
}

#[test]
fn write_then_read_full_z2_grid() {
    // Add every tile at zoom 2 (16 tiles), each with distinct content.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = PmTilesWriter::create(file, opts());
        for x in 0..4 {
            for y in 0..4 {
                let bytes = fake_tile_bytes((x * 4 + y + 1) as u64);
                w.add_tile(2, x, y, &bytes).unwrap();
            }
        }
        w.finish().unwrap();
    }

    let mut r = PmTilesReader::open(&path).unwrap();
    for x in 0..4 {
        for y in 0..4 {
            let expected = fake_tile_bytes((x * 4 + y + 1) as u64);
            let got = r.get_tile(2, x, y).unwrap().expect("z2 tile");
            assert_eq!(got, expected, "tile (2,{x},{y})");
        }
    }
}

#[test]
fn dedup_and_run_merge_shrink_directory() {
    // 16 z2 tiles, all identical → should collapse to 1 unique content
    // and one run of length 16 in the directory.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let identical = b"all-tiles-are-the-same".to_vec();
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = PmTilesWriter::create(file, opts());
        for x in 0..4 {
            for y in 0..4 {
                w.add_tile(2, x, y, &identical).unwrap();
            }
        }
        w.finish().unwrap();
    }

    let mut r = PmTilesReader::open(&path).unwrap();
    let h = r.header();
    assert_eq!(h.addressed_tiles_count, 16);
    assert_eq!(h.tile_contents_count, 1, "dedup should collapse to 1");
    assert_eq!(
        h.tile_entries_count, 1,
        "run-length merge should collapse to 1 directory entry"
    );
    // All 16 still resolvable as the same bytes.
    for x in 0..4 {
        for y in 0..4 {
            assert_eq!(r.get_tile(2, x, y).unwrap().unwrap(), identical);
        }
    }
}

#[test]
fn leaf_splitting_triggers_at_threshold() {
    // Force leaf splitting with a very low threshold + leaf_size so we
    // can verify the read path exercises leaf-fetch + binary search.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut o = opts();
    o.leaf_split_threshold = 4;
    o.leaf_size = 4;

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = PmTilesWriter::create(file, o);
        // Add 16 distinct tiles at z2 → 16 entries > 4 threshold → splits.
        for x in 0..4 {
            for y in 0..4 {
                let bytes = fake_tile_bytes((x * 4 + y + 100) as u64);
                w.add_tile(2, x, y, &bytes).unwrap();
            }
        }
        w.finish().unwrap();
    }

    let mut r = PmTilesReader::open(&path).unwrap();
    let h = r.header();
    assert!(
        h.leaf_dirs_length > 0,
        "leaf-dir section should be populated when threshold is exceeded"
    );
    // Verify every tile still reads correctly via the leaf-fetch path.
    for x in 0..4 {
        for y in 0..4 {
            let expected = fake_tile_bytes((x * 4 + y + 100) as u64);
            let got = r.get_tile(2, x, y).unwrap().expect("tile via leaf");
            assert_eq!(got, expected);
        }
    }
}

#[test]
fn metadata_roundtrips_through_reader() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut o = opts();
    o.metadata_json = br#"{"name":"vicmap","attribution":"Vicmap"}"#.to_vec();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = PmTilesWriter::create(file, o.clone());
        w.add_tile(0, 0, 0, b"x").unwrap();
        w.finish().unwrap();
    }

    let mut r = PmTilesReader::open(&path).unwrap();
    let meta = r.metadata().unwrap();
    assert_eq!(meta, o.metadata_json);
}

#[test]
fn re_adding_same_coord_overwrites() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut w = PmTilesWriter::create(file, opts());
        w.add_tile(0, 0, 0, b"first").unwrap();
        w.add_tile(0, 0, 0, b"second").unwrap(); // overwrite
        w.finish().unwrap();
    }

    let mut r = PmTilesReader::open(&path).unwrap();
    assert_eq!(r.get_tile(0, 0, 0).unwrap().unwrap(), b"second");
    assert_eq!(r.header().tile_entries_count, 1);
}

//! End-to-end: synthesise a tiny tiled GeoTIFF on disk, open it through
//! the public API, verify schema + tile reads.
//!
//! We don't ship real-world TIFF fixtures; the format is well-specified
//! enough that synthetic round-trips catch the same bugs.

use geonative_core::raster::{PixelType, RasterLayer};
use geonative_core::Crs;
use geonative_geotiff::format::{compression, tags, DType};
use geonative_geotiff::GeoTiff;
use std::io::Write;

/// Build a 4×4-tile, 2×2-pixels-per-tile single-band U8 TIFF where each
/// tile is filled with a unique constant value. Returns the file path.
///
/// Layout:
///   tile (0,0): all 1s    tile (1,0): all 2s    tile (2,0): all 3s    tile (3,0): all 4s
///   tile (0,1): all 5s    tile (1,1): all 6s    tile (2,1): all 7s    tile (3,1): all 8s
///   …
fn build_tiled_tiff(path: &std::path::Path) {
    // Tile data: 16 tiles × 4 bytes each. Each tile is 2×2 of value (i+1).
    let mut tile_data: Vec<Vec<u8>> = Vec::with_capacity(16);
    for i in 0..16u8 {
        tile_data.push(vec![i + 1; 4]);
    }

    // We'll lay the file out as:
    //   [0..8]    header
    //   [8..]     16 × 4 bytes tile data (= 64 bytes), all back-to-back
    //   then      ModelPixelScale (24), ModelTiepoint (48), GeoKeyDirectory (16)
    //   then      Arrays: TileOffsets (16 × u32 = 64), TileByteCounts (16 × u32 = 64)
    //   then      IFD
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // first IFD offset (patched at end)

    // Tile pixel data — record each tile's offset.
    let mut tile_offsets = Vec::new();
    let mut tile_byte_counts = Vec::new();
    for td in &tile_data {
        tile_offsets.push(buf.len() as u32);
        tile_byte_counts.push(td.len() as u32);
        buf.extend_from_slice(td);
    }

    // ModelPixelScale: [0.5, 0.5, 0]
    let scale_off = buf.len();
    for v in [0.5f64, 0.5, 0.0] {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    // ModelTiepoint: [0, 0, 0, 100, 200, 0]
    let tie_off = buf.len();
    for v in [0.0f64, 0.0, 0.0, 100.0, 200.0, 0.0] {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    // GeoKeyDirectory: header + 1 entry (ProjectedCSTypeGeoKey = 3857)
    let geo_off = buf.len();
    for v in [1u16, 1, 0, 1, 3072, 0, 1, 3857] {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    // TileOffsets array: 16 × u32
    let to_off = buf.len();
    for o in &tile_offsets {
        buf.extend_from_slice(&o.to_le_bytes());
    }
    // TileByteCounts array: 16 × u32
    let bc_off = buf.len();
    for c in &tile_byte_counts {
        buf.extend_from_slice(&c.to_le_bytes());
    }

    // IFD
    let ifd_off = buf.len();
    // Tags we write (10 total):
    //   ImageWidth=8, ImageLength=8, BitsPerSample=8, Compression=1,
    //   PhotometricInterpretation=1, SamplesPerPixel=1,
    //   TileWidth=2, TileLength=2, TileOffsets, TileByteCounts,
    //   ModelPixelScale, ModelTiepoint, GeoKeyDirectory
    let entries: Vec<(u16, DType, u32, [u8; 4])> = vec![
        (tags::IMAGE_WIDTH, DType::Short, 1, u16_inline(8)),
        (tags::IMAGE_LENGTH, DType::Short, 1, u16_inline(8)),
        (tags::BITS_PER_SAMPLE, DType::Short, 1, u16_inline(8)),
        (
            tags::COMPRESSION,
            DType::Short,
            1,
            u16_inline(compression::NONE),
        ),
        (
            tags::PHOTOMETRIC_INTERPRETATION,
            DType::Short,
            1,
            u16_inline(1),
        ),
        (tags::SAMPLES_PER_PIXEL, DType::Short, 1, u16_inline(1)),
        (tags::TILE_WIDTH, DType::Short, 1, u16_inline(2)),
        (tags::TILE_LENGTH, DType::Short, 1, u16_inline(2)),
        (
            tags::TILE_OFFSETS,
            DType::Long,
            16,
            (to_off as u32).to_le_bytes(),
        ),
        (
            tags::TILE_BYTE_COUNTS,
            DType::Long,
            16,
            (bc_off as u32).to_le_bytes(),
        ),
        (
            tags::MODEL_PIXEL_SCALE,
            DType::Double,
            3,
            (scale_off as u32).to_le_bytes(),
        ),
        (
            tags::MODEL_TIEPOINT,
            DType::Double,
            6,
            (tie_off as u32).to_le_bytes(),
        ),
        (
            tags::GEO_KEY_DIRECTORY,
            DType::Short,
            8,
            (geo_off as u32).to_le_bytes(),
        ),
    ];

    buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, dtype, count, value) in &entries {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&(*dtype as u16).to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(value);
    }
    buf.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0

    // Patch first IFD offset (bytes 4..8 of header).
    buf[4..8].copy_from_slice(&(ifd_off as u32).to_le_bytes());

    // Write to disk.
    let mut f = std::fs::File::create(path).expect("create");
    f.write_all(&buf).expect("write");
}

fn u16_inline(v: u16) -> [u8; 4] {
    let mut bytes = [0u8; 4];
    bytes[..2].copy_from_slice(&v.to_le_bytes());
    bytes
}

fn workdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("geotiff_test_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn open_and_read_profile() {
    let dir = workdir("profile");
    let path = dir.join("synth.tif");
    build_tiled_tiff(&path);

    let tif = GeoTiff::open(&path).expect("open");
    let p = tif.profile();
    assert_eq!(p.width, 8);
    assert_eq!(p.height, 8);
    assert_eq!(p.tile_size, [2, 2]);
    assert_eq!(p.bands.len(), 1);
    assert_eq!(p.bands[0].dtype, PixelType::U8);
    assert_eq!(p.crs, Crs::Epsg(3857));
    // GeoTransform: origin (100, 200), pixel (0.5, -0.5)
    assert_eq!(p.geo_transform.origin, [100.0, 200.0]);
    assert_eq!(p.geo_transform.pixel_size, [0.5, -0.5]);
    assert!(p.geo_transform.is_north_up());
    assert_eq!(p.pyramid_levels, 1);
}

#[test]
fn read_tile_at_origin() {
    let dir = workdir("origin");
    let path = dir.join("synth.tif");
    build_tiled_tiff(&path);

    let tif = GeoTiff::open(&path).expect("open");
    let t = tif.read_tile(0, 0, 0).expect("read");
    assert_eq!(t.width, 2);
    assert_eq!(t.height, 2);
    assert_eq!(t.bands.len(), 1);
    // Tile (0,0) was filled with value 1.
    assert_eq!(t.bands[0].data, vec![1, 1, 1, 1]);
    // Tile-origin world coords match the source GeoTransform's origin.
    assert_eq!(t.geo_transform.origin, [100.0, 200.0]);
}

#[test]
fn read_tile_in_middle() {
    let dir = workdir("middle");
    let path = dir.join("synth.tif");
    build_tiled_tiff(&path);

    let tif = GeoTiff::open(&path).expect("open");
    // tile (2, 1) → fill value 2*4 + 2 + 1 = wait, the layout is:
    //   row 0: tiles 1,2,3,4 (x=0..3)
    //   row 1: tiles 5,6,7,8
    //   so (x=2, y=1) → tile index 1*4 + 2 = 6 → fill 7.
    let t = tif.read_tile(0, 2, 1).expect("read");
    assert_eq!(t.bands[0].data, vec![7, 7, 7, 7]);
    // Tile origin: shifted by (2 * 2 px * 0.5) east and (1 * 2 px * 0.5) south.
    assert_eq!(t.geo_transform.origin[0], 100.0 + 2.0);
    assert_eq!(t.geo_transform.origin[1], 200.0 - 1.0);
}

#[test]
fn out_of_range_tile_errors() {
    let dir = workdir("oor");
    let path = dir.join("synth.tif");
    build_tiled_tiff(&path);

    let tif = GeoTiff::open(&path).expect("open");
    // 4×4 grid, so (4, 0) is out of range.
    assert!(tif.read_tile(0, 4, 0).is_err());
    assert!(tif.read_tile(99, 0, 0).is_err());
}

#[test]
fn tile_well_formed() {
    let dir = workdir("wf");
    let path = dir.join("synth.tif");
    build_tiled_tiff(&path);

    let tif = GeoTiff::open(&path).expect("open");
    for ty in 0..4 {
        for tx in 0..4 {
            let t = tif.read_tile(0, tx, ty).unwrap();
            assert!(t.is_well_formed(), "tile ({tx},{ty}) malformed");
        }
    }
}

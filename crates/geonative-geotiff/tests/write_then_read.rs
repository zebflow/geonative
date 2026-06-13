//! Write a tiled GeoTIFF via `GeoTiffWriter`, read it back via `GeoTiff`,
//! verify schema + tile contents + geo metadata round-trip.
//!
//! This is the "is the writer producing valid TIFF?" gate. If a TIFF we
//! write can't be read by our own reader (let alone QGIS/GDAL), the
//! writer has a real bug.

use geonative_core::raster::{
    Band, BandDescriptor, GeoTransform, PixelType, RasterLayer, RasterProfile, RasterTile,
};
use geonative_core::Crs;
use geonative_geotiff::{Compression, GeoTiff, GeoTiffWriter, WriterOptions};

fn workdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "geotiff_write_test_{}_{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn rgb_band(name: &str) -> BandDescriptor {
    BandDescriptor::new(Some(name.into()), PixelType::U8)
}

fn make_tile(width: u32, height: u32, fill: u8) -> RasterTile {
    let pixels = (width as usize) * (height as usize);
    RasterTile {
        width,
        height,
        bands: vec![Band::new(rgb_band("v"), vec![fill; pixels])],
        geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
        crs: Crs::Epsg(3857),
    }
}

fn profile(width: u32, height: u32, tile_size: u32, crs: Crs) -> RasterProfile {
    RasterProfile {
        width,
        height,
        bands: vec![rgb_band("v")],
        geo_transform: GeoTransform::north_up(144.0, -37.0, 0.0005, 0.0005),
        crs,
        tile_size: [tile_size, tile_size],
        pyramid_levels: 1,
    }
}

#[test]
fn uncompressed_round_trip() {
    let dir = workdir("uncompressed");
    let path = dir.join("out.tif");

    let p = profile(4, 4, 2, Crs::Epsg(3857));
    let file = std::fs::File::create(&path).unwrap();
    let mut w = GeoTiffWriter::create(
        file,
        &p,
        WriterOptions {
            compression: Compression::None,
            ..WriterOptions::default()
        },
    )
    .unwrap();
    // 4 tiles forming the full image; pixel value = tile-index + 1
    w.write_tile(0, 0, 0, &make_tile(2, 2, 1)).unwrap();
    w.write_tile(0, 1, 0, &make_tile(2, 2, 2)).unwrap();
    w.write_tile(0, 0, 1, &make_tile(2, 2, 3)).unwrap();
    w.write_tile(0, 1, 1, &make_tile(2, 2, 4)).unwrap();
    w.close().unwrap();

    let read = GeoTiff::open(&path).unwrap();
    let rp = read.profile();
    assert_eq!(rp.width, 4);
    assert_eq!(rp.height, 4);
    assert_eq!(rp.tile_size, [2, 2]);
    assert_eq!(rp.crs, Crs::Epsg(3857));
    assert_eq!(rp.geo_transform.origin, [144.0, -37.0]);

    assert_eq!(read.read_tile(0, 0, 0).unwrap().bands[0].data, vec![1; 4]);
    assert_eq!(read.read_tile(0, 1, 0).unwrap().bands[0].data, vec![2; 4]);
    assert_eq!(read.read_tile(0, 0, 1).unwrap().bands[0].data, vec![3; 4]);
    assert_eq!(read.read_tile(0, 1, 1).unwrap().bands[0].data, vec![4; 4]);
}

#[test]
fn deflate_round_trip() {
    let dir = workdir("deflate");
    let path = dir.join("out.tif");

    let p = profile(4, 4, 2, Crs::Epsg(3857));
    let file = std::fs::File::create(&path).unwrap();
    let mut w = GeoTiffWriter::create(
        file,
        &p,
        WriterOptions {
            compression: Compression::Deflate,
            deflate_level: 6,
        },
    )
    .unwrap();
    w.write_tile(0, 0, 0, &make_tile(2, 2, 1)).unwrap();
    w.write_tile(0, 1, 0, &make_tile(2, 2, 2)).unwrap();
    w.write_tile(0, 0, 1, &make_tile(2, 2, 3)).unwrap();
    w.write_tile(0, 1, 1, &make_tile(2, 2, 4)).unwrap();
    w.close().unwrap();

    let read = GeoTiff::open(&path).unwrap();
    assert_eq!(read.read_tile(0, 0, 0).unwrap().bands[0].data, vec![1; 4]);
    assert_eq!(read.read_tile(0, 1, 1).unwrap().bands[0].data, vec![4; 4]);
}

#[test]
fn lzw_round_trip() {
    let dir = workdir("lzw");
    let path = dir.join("out.tif");

    let p = profile(4, 4, 2, Crs::Epsg(3857));
    let file = std::fs::File::create(&path).unwrap();
    let mut w = GeoTiffWriter::create(
        file,
        &p,
        WriterOptions {
            compression: Compression::Lzw,
            ..WriterOptions::default()
        },
    )
    .unwrap();
    w.write_tile(0, 0, 0, &make_tile(2, 2, 11)).unwrap();
    w.write_tile(0, 1, 0, &make_tile(2, 2, 22)).unwrap();
    w.write_tile(0, 0, 1, &make_tile(2, 2, 33)).unwrap();
    w.write_tile(0, 1, 1, &make_tile(2, 2, 44)).unwrap();
    w.close().unwrap();

    let read = GeoTiff::open(&path).unwrap();
    assert_eq!(read.read_tile(0, 0, 0).unwrap().bands[0].data, vec![11; 4]);
    assert_eq!(read.read_tile(0, 1, 0).unwrap().bands[0].data, vec![22; 4]);
    assert_eq!(read.read_tile(0, 0, 1).unwrap().bands[0].data, vec![33; 4]);
    assert_eq!(read.read_tile(0, 1, 1).unwrap().bands[0].data, vec![44; 4]);
}

#[test]
fn geographic_crs_round_trip() {
    // EPSG:4326 (geographic) should write GeographicTypeGeoKey
    let dir = workdir("geographic");
    let path = dir.join("out.tif");

    let p = profile(2, 2, 2, Crs::Epsg(4326));
    let file = std::fs::File::create(&path).unwrap();
    let mut w = GeoTiffWriter::create(file, &p, WriterOptions::default()).unwrap();
    w.write_tile(0, 0, 0, &make_tile(2, 2, 99)).unwrap();
    w.close().unwrap();

    let read = GeoTiff::open(&path).unwrap();
    assert_eq!(read.profile().crs, Crs::Epsg(4326));
}

#[test]
fn geo_transform_preserved() {
    let dir = workdir("geo");
    let path = dir.join("out.tif");

    let p = profile(2, 2, 2, Crs::Epsg(7855)); // Vicmap MGA Zone 55
    let file = std::fs::File::create(&path).unwrap();
    let mut w = GeoTiffWriter::create(file, &p, WriterOptions::default()).unwrap();
    w.write_tile(0, 0, 0, &make_tile(2, 2, 1)).unwrap();
    w.close().unwrap();

    let read = GeoTiff::open(&path).unwrap();
    let rp = read.profile();
    assert_eq!(rp.geo_transform.origin, [144.0, -37.0]);
    assert_eq!(rp.geo_transform.pixel_size, [0.0005, -0.0005]);
    assert!(rp.geo_transform.is_north_up());
}

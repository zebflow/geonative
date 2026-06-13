//! End-to-end raster convert test. Builds a real TIFF on disk via the
//! geotiff writer, runs convert(input.tif, output.cog, opts), reads back
//! the result to verify schema + tile contents survived.

use geonative_convert::{convert, ConvertOptions};
use geonative_core::raster::{
    Band, BandDescriptor, GeoTransform, PixelType, RasterLayer, RasterProfile, RasterTile,
};
use geonative_core::Crs;
use geonative_geotiff::{Compression, GeoTiff, GeoTiffWriter, WriterOptions};

fn workdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "convert_raster_e2e_{}_{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn make_tile(fill: u8) -> RasterTile {
    RasterTile {
        width: 2,
        height: 2,
        bands: vec![Band::new(
            BandDescriptor::new(Some("v".into()), PixelType::U8),
            vec![fill; 4],
        )],
        geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
        crs: Crs::Epsg(3857),
    }
}

fn write_input_tiff(path: &std::path::Path) {
    let profile = RasterProfile {
        width: 4,
        height: 4,
        bands: vec![BandDescriptor::new(Some("v".into()), PixelType::U8)],
        geo_transform: GeoTransform::north_up(144.0, -37.0, 0.0005, 0.0005),
        crs: Crs::Epsg(7855),
        tile_size: [2, 2],
        pyramid_levels: 1,
    };
    let file = std::fs::File::create(path).unwrap();
    let mut w = GeoTiffWriter::create(
        file,
        &profile,
        WriterOptions {
            compression: Compression::Deflate,
            ..WriterOptions::default()
        },
    )
    .unwrap();
    w.write_tile(0, 0, 0, &make_tile(10)).unwrap();
    w.write_tile(0, 1, 0, &make_tile(20)).unwrap();
    w.write_tile(0, 0, 1, &make_tile(30)).unwrap();
    w.write_tile(0, 1, 1, &make_tile(40)).unwrap();
    w.close().unwrap();
}

#[test]
fn convert_tif_to_cog() {
    let dir = workdir("tif2cog");
    let src = dir.join("input.tif");
    let dst = dir.join("output.cog");
    write_input_tiff(&src);

    let stats = convert(&src, &dst, ConvertOptions::default()).unwrap();
    assert_eq!(stats.features, 4, "should have streamed 4 tiles");
    assert!(stats.output_bytes > 0);
    assert!(dst.exists());

    // Re-read the output through the public reader; everything must match.
    let read = GeoTiff::open(&dst).unwrap();
    let p = read.profile();
    assert_eq!(p.width, 4);
    assert_eq!(p.height, 4);
    assert_eq!(p.crs, Crs::Epsg(7855));
    assert_eq!(p.geo_transform.origin, [144.0, -37.0]);

    // Tile contents intact
    assert_eq!(read.read_tile(0, 0, 0).unwrap().bands[0].data, vec![10; 4]);
    assert_eq!(read.read_tile(0, 1, 0).unwrap().bands[0].data, vec![20; 4]);
    assert_eq!(read.read_tile(0, 0, 1).unwrap().bands[0].data, vec![30; 4]);
    assert_eq!(read.read_tile(0, 1, 1).unwrap().bands[0].data, vec![40; 4]);
}

#[test]
fn convert_tif_to_tif() {
    // Same dispatch even when the output extension is .tif (regular GeoTIFF) —
    // both .tif and .cog map to Format::GeoTiff and use the COG-shaped writer.
    let dir = workdir("tif2tif");
    let src = dir.join("input.tif");
    let dst = dir.join("output.tif");
    write_input_tiff(&src);

    let stats = convert(&src, &dst, ConvertOptions::default()).unwrap();
    assert_eq!(stats.features, 4);
    assert!(dst.exists());
}

#[test]
fn raster_input_to_vector_output_errors() {
    // Cross-modal isn't supported in v0.1; should error clearly.
    let dir = workdir("crossmodal");
    let src = dir.join("input.tif");
    let dst = dir.join("output.parquet");
    write_input_tiff(&src);

    let err = convert(&src, &dst, ConvertOptions::default()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("vectorize") || msg.contains("cross-modal"),
        "expected cross-modal error, got: {msg}"
    );
}

#[test]
fn convert_png_with_sidecar_to_cog() {
    // The Phase E case: user uploads PNG + world file, we normalise to COG.
    let dir = workdir("png2cog");
    let src = dir.join("upload.png");
    let wld = dir.join("upload.pgw");
    let dst = dir.join("normalized.cog");

    // Synth a 4×4 RGB PNG.
    let file = std::fs::File::create(&src).unwrap();
    let buf = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(buf, 4, 4);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().unwrap();
    let pixels: Vec<u8> = (0..16).flat_map(|_| [10u8, 20, 30]).collect();
    writer.write_image_data(&pixels).unwrap();
    drop(writer);

    std::fs::write(&wld, "0.5\n0\n0\n-0.5\n144\n-37\n").unwrap();

    let stats = convert(&src, &dst, ConvertOptions::default()).unwrap();
    assert_eq!(stats.features, 1, "single-image input yields one tile");
    assert!(dst.exists());

    // Re-read the COG and verify
    let read = GeoTiff::open(&dst).unwrap();
    let p = read.profile();
    assert_eq!(p.width, 4);
    assert_eq!(p.height, 4);
    assert_eq!(p.bands.len(), 3);
    // Origin from world file: centre (144, -37) - half pixel (0.25, -0.25)
    // = upper-left (143.75, -36.75)
    assert!((p.geo_transform.origin[0] - 143.75).abs() < 1e-9);
    assert!((p.geo_transform.origin[1] - (-36.75)).abs() < 1e-9);
}

#[test]
fn png_without_world_file_errors() {
    let dir = workdir("no_sidecar");
    let src = dir.join("noworld.png");
    let dst = dir.join("out.cog");

    let file = std::fs::File::create(&src).unwrap();
    let buf = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(buf, 2, 2);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().unwrap();
    writer.write_image_data(&[0u8; 12]).unwrap();
    drop(writer);

    let err = convert(&src, &dst, ConvertOptions::default()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("world file"),
        "expected missing-world-file error, got: {msg}"
    );
}

#[test]
fn raster_to_crs_errors_in_v01() {
    // Raster reproject is deferred to Phase F; should error clearly rather
    // than silently dropping the option.
    let dir = workdir("toCrs");
    let src = dir.join("input.tif");
    let dst = dir.join("output.cog");
    write_input_tiff(&src);

    let opts = ConvertOptions {
        to_crs: Some(Crs::Epsg(3857)),
        ..ConvertOptions::default()
    };
    let err = convert(&src, &dst, opts).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("reproject") || msg.contains("to-crs"),
        "expected reproject deferred error, got: {msg}"
    );
}

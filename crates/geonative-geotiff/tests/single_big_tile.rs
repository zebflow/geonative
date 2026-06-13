//! Regression: writer with a single big tile (tile_size = image size).
//! Mimics what convert does after stitching multi-tile sources.

use geonative_core::raster::*;
use geonative_core::Crs;
use geonative_geotiff::*;

#[test]
fn single_big_tile_round_trip() {
    let profile = RasterProfile {
        width: 4,
        height: 4,
        bands: vec![BandDescriptor::new(Some("v".into()), PixelType::U8)],
        geo_transform: GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
        crs: Crs::Epsg(3857),
        tile_size: [4, 4],
        pyramid_levels: 1,
    };
    let path = std::env::temp_dir().join(format!(
        "geotiff_single_big_tile_{}.tif",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let file = std::fs::File::create(&path).unwrap();
    let mut w = GeoTiffWriter::create(
        file,
        &profile,
        WriterOptions {
            compression: Compression::Deflate,
            deflate_level: 6,
        },
    )
    .unwrap();
    let data = vec![
        10, 10, 20, 20, 10, 10, 20, 20, 30, 30, 40, 40, 30, 30, 40, 40,
    ];
    let tile = RasterTile {
        width: 4,
        height: 4,
        bands: vec![Band::new(
            BandDescriptor::new(Some("v".into()), PixelType::U8),
            data.clone(),
        )],
        geo_transform: profile.geo_transform,
        crs: profile.crs.clone(),
    };
    w.write_tile(0, 0, 0, &tile).unwrap();
    w.close().unwrap();

    let r = GeoTiff::open(&path).unwrap();
    assert_eq!(r.profile().width, 4);
    let read = r.read_tile(0, 0, 0).expect("read failed");
    assert_eq!(read.bands[0].data, data);
}

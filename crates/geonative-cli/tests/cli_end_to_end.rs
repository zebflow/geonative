//! End-to-end smoke test for the four 0.1.x subcommands: inspect, optimize,
//! filter-bbox, metadata. Builds a 3-feature GeoParquet fixture on the fly
//! using the library writer, then drives the compiled binary against it.
//!
//! Validates wiring + JSON shape, not the underlying readers/writers (those
//! have their own dedicated tests in the format crates).

use std::path::PathBuf;
use std::process::Command;

use geonative_core::{
    Coord, Crs, FieldDef, Feature, GeomField, Geometry, GeometryType, Schema, Value, ValueType,
};
use geonative_geoparquet::{GeoParquetWriter, WriterOptions};

fn unique_workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "geonative_cli_test_{}_{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp workdir");
    dir
}

/// Build a small GeoParquet at `path`: three points at (0,0), (10,10), (20,20)
/// with a single int attribute.
fn build_fixture(path: &std::path::Path) {
    let schema = Schema::new(
        vec![FieldDef::new("id", ValueType::Int32, false)],
        Some(GeomField::new("geometry", GeometryType::Point)),
        Crs::Epsg(4326),
    );
    let opts = WriterOptions {
        batch_size: 100,
        add_bbox_columns: true,
        hilbert_sort: false,
        ..WriterOptions::default()
    };
    let file = std::fs::File::create(path).expect("create fixture file");
    let mut writer = GeoParquetWriter::create(file, &schema, opts).expect("create writer");
    for (i, (x, y)) in [(0.0, 0.0), (10.0, 10.0), (20.0, 20.0)].iter().enumerate() {
        let feat = Feature::new(
            Some(i as i64),
            Some(Geometry::Point(Coord::xy(*x, *y))),
            vec![Value::Int32(i as i32)],
        );
        writer.write(&feat).expect("write feature");
    }
    writer.close().expect("close writer");
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_geonative")
}

#[test]
fn inspect_emits_expected_json() {
    let dir = unique_workdir("inspect");
    let src = dir.join("src.parquet");
    build_fixture(&src);

    let out = Command::new(bin())
        .args(["inspect", "--pretty"])
        .arg(&src)
        .output()
        .expect("run inspect");
    assert!(
        out.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("inspect output is JSON");

    assert_eq!(json["format"], "geoparquet");
    let layers = json["layers"].as_array().expect("layers array");
    assert_eq!(layers.len(), 1);
    let layer = &layers[0];
    assert_eq!(layer["name"], "default");
    assert_eq!(layer["geometry"]["kind"], "Point");
    assert_eq!(layer["crs"]["kind"], "epsg");
    assert_eq!(layer["crs"]["code"], 4326);
    let fields = layer["fields"].as_array().expect("fields array");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0]["name"], "id");
    assert_eq!(fields[0]["type"], "Int32");
}

#[test]
fn optimize_rewrites_parquet() {
    let dir = unique_workdir("optimize");
    let src = dir.join("src.parquet");
    let dst = dir.join("dst.parquet");
    build_fixture(&src);

    let out = Command::new(bin())
        .args(["optimize"])
        .arg(&src)
        .arg(&dst)
        .output()
        .expect("run optimize");
    assert!(
        out.status.success(),
        "optimize failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dst.exists(), "optimize did not create output");
    assert!(std::fs::metadata(&dst).unwrap().len() > 0);
}

#[test]
fn filter_bbox_keeps_only_matching_features() {
    let dir = unique_workdir("filter");
    let src = dir.join("src.parquet");
    let dst = dir.join("dst.parquet");
    build_fixture(&src);

    // Box around the middle point (10,10) only.
    let out = Command::new(bin())
        .args(["filter-bbox"])
        .arg(&src)
        .arg(&dst)
        .args(["--bbox", "9,9,11,11"])
        .output()
        .expect("run filter-bbox");
    assert!(
        out.status.success(),
        "filter-bbox failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Reopen the output and count features.
    let reader = geonative_geoparquet::GeoParquetReader::open(&dst).expect("open filtered");
    let count = reader.into_features().count();
    assert_eq!(count, 1, "expected exactly 1 feature in filtered output");
}

#[test]
fn filter_bbox_rejects_bad_bbox() {
    let dir = unique_workdir("badbbox");
    let src = dir.join("src.parquet");
    let dst = dir.join("dst.parquet");
    build_fixture(&src);

    let out = Command::new(bin())
        .args(["filter-bbox"])
        .arg(&src)
        .arg(&dst)
        .args(["--bbox", "1,2,3"])
        .output()
        .expect("run filter-bbox");
    assert!(!out.status.success(), "expected failure on bad bbox");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("bbox") && stderr.contains("4"),
        "stderr should mention the 4-component requirement: {stderr}"
    );
}

#[test]
fn convert_parquet_to_geojson_round_trip() {
    let dir = unique_workdir("p2g");
    let src = dir.join("src.parquet");
    let mid = dir.join("mid.geojson");
    let back = dir.join("back.parquet");
    build_fixture(&src);

    // parquet → geojson
    let out = Command::new(bin())
        .args(["convert"])
        .arg(&src)
        .arg(&mid)
        .output()
        .expect("run convert p->g");
    assert!(
        out.status.success(),
        "parquet->geojson failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The output should be valid GeoJSON we can re-read
    let bytes = std::fs::read(&mid).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("output is JSON");
    assert_eq!(json["type"], "FeatureCollection");
    assert_eq!(json["features"].as_array().unwrap().len(), 3);

    // geojson → parquet
    let out = Command::new(bin())
        .args(["convert"])
        .arg(&mid)
        .arg(&back)
        .output()
        .expect("run convert g->p");
    assert!(
        out.status.success(),
        "geojson->parquet failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = geonative_geoparquet::GeoParquetReader::open(&back).expect("open round-tripped");
    assert_eq!(r.into_features().count(), 3);
}

#[test]
fn inspect_geojson() {
    let dir = unique_workdir("inspect_geojson");
    let src = dir.join("src.geojson");
    std::fs::write(
        &src,
        br#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"name":"a"}}
        ]}"#,
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["inspect", "--pretty"])
        .arg(&src)
        .output()
        .expect("run inspect");
    assert!(
        out.status.success(),
        "inspect geojson failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["format"], "geojson");
    assert_eq!(json["layers"][0]["geometry"]["kind"], "Point");
    assert_eq!(json["layers"][0]["crs"]["code"], 4326);
}

#[test]
fn filter_bbox_geojson_to_geojson() {
    let dir = unique_workdir("filter_g2g");
    let src = dir.join("src.geojson");
    let dst = dir.join("dst.geojson");
    std::fs::write(
        &src,
        br#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[10,10]},"properties":{}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[20,20]},"properties":{}}
        ]}"#,
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["filter-bbox"])
        .arg(&src)
        .arg(&dst)
        .args(["--bbox", "9,9,11,11"])
        .output()
        .expect("run filter-bbox");
    assert!(
        out.status.success(),
        "filter-bbox g->g failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = std::fs::read(&dst).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["features"].as_array().unwrap().len(), 1);
}

#[test]
fn metadata_writes_sidecar() {
    let dir = unique_workdir("metadata");
    let src = dir.join("src.parquet");
    build_fixture(&src);

    let out = Command::new(bin())
        .args(["metadata", "--pretty"])
        .arg(&src)
        .output()
        .expect("run metadata");
    assert!(
        out.status.success(),
        "metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let sidecar = src.with_extension("parquet.geonative.json");
    assert!(sidecar.exists(), "sidecar not written at {sidecar:?}");
    let bytes = std::fs::read(&sidecar).expect("read sidecar");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("sidecar is JSON");
    assert_eq!(json["generator"], "geonative-cli");
    assert_eq!(json["spec_version"], 1);
    assert_eq!(json["format"], "geoparquet");
    assert!(json["layers"].is_array());
}

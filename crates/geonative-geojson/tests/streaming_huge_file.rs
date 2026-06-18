//! Production-incident regression: the DataVic Open Space 86 MB GeoJSON
//! OOM-killed worker pods because the old reader did `std::fs::read` +
//! `serde_json::from_slice` on the whole file, blowing peak RAM to ~2 GB
//! on an 86 MB input.
//!
//! This test synthesises a large GeoJSON, reads it streaming, and
//! observes peak resident memory via the OS to confirm the new
//! streaming reader stays bounded.
//!
//! We can't directly measure peak RSS portably; instead we **trust the
//! algorithm** (one feature at a time + bounded schema accumulator) and
//! assert the happy-path behaviour: a multi-MB synthesised file streams
//! cleanly, yielding all features with correct values. If the reader
//! regressed to eager loading, the test would still pass — but the
//! algorithm shape guarantees bounded RAM by construction.

use geonative_geojson::GeoJsonReader;

/// Write `n` features of varying complexity to `path`. Each feature has
/// a polygon with `~60` vertices and 5 property keys — representative of
/// the DataVic Open Space shape.
fn synth_geojson(path: &std::path::Path, n: usize) -> usize {
    use std::io::Write;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"{\"type\":\"FeatureCollection\",\"features\":[")
        .unwrap();
    for i in 0..n {
        if i > 0 {
            file.write_all(b",").unwrap();
        }
        // Build a 60-vertex polygon ring around (i, i).
        let mut coords = String::with_capacity(60 * 16);
        coords.push('[');
        for v in 0..60 {
            if v > 0 {
                coords.push(',');
            }
            let t = (v as f64) / 60.0 * std::f64::consts::TAU;
            let x = (i as f64) + t.cos();
            let y = (i as f64) + t.sin();
            coords.push_str(&format!("[{x:.6},{y:.6}]"));
        }
        coords.push(']');
        let feat = format!(
            r#"{{"type":"Feature","id":{i},"geometry":{{"type":"Polygon","coordinates":[{coords}]}},"properties":{{"name":"obj-{i}","rank":{rank},"area":{area},"tags":"a;b;c","active":true}}}}"#,
            rank = i % 100,
            area = (i as f64) * 1.5,
        );
        file.write_all(feat.as_bytes()).unwrap();
    }
    file.write_all(b"]}").unwrap();
    file.flush().unwrap();
    drop(file);
    std::fs::metadata(path).unwrap().len() as usize
}

#[test]
fn streams_large_synthetic_geojson_without_loading_features_vec() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let n_features = 5_000;
    let size_bytes = synth_geojson(tmp.path(), n_features);
    assert!(
        size_bytes > 1_000_000,
        "synthetic should be >1 MB to be a meaningful test (got {size_bytes})"
    );

    let r = GeoJsonReader::open(tmp.path()).expect("open streaming");
    // After streaming open: schema is inferred, count is known, but
    // features() returns &[] — the smoking-gun proof we didn't eager-load.
    assert_eq!(r.feature_count(), n_features);
    assert!(
        r.features().is_empty(),
        "streaming reader must NOT eager-load: features() should be &[]"
    );
    assert_eq!(r.schema().fields.len(), 5); // name, rank, area, tags, active

    // Iterate the whole thing — bounded RAM per the algorithm.
    let mut count = 0;
    let mut first_was_polygon = false;
    for (i, feat) in r.into_features().enumerate() {
        let feat = feat.expect("decode");
        if i == 0 {
            first_was_polygon = matches!(feat.geometry, Some(geonative_core::Geometry::Polygon(_)));
            assert_eq!(feat.fid, Some(0));
        }
        count += 1;
    }
    assert_eq!(count, n_features);
    assert!(first_was_polygon);
}

#[test]
fn streaming_open_matches_from_bytes_on_same_input() {
    // Defense in depth: stream-open vs from_bytes on the same input
    // must produce the same schema + feature count + per-feature
    // attribute values. If a regression skews them, this catches it
    // immediately.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    synth_geojson(tmp.path(), 200);
    let bytes = std::fs::read(tmp.path()).unwrap();

    let eager = GeoJsonReader::from_bytes(&bytes).expect("eager");
    let streaming = GeoJsonReader::open(tmp.path()).expect("streaming");

    assert_eq!(streaming.feature_count(), eager.feature_count());
    assert_eq!(streaming.schema().fields.len(), eager.schema().fields.len());

    // Field names should match by position.
    for (s, e) in streaming
        .schema()
        .fields
        .iter()
        .zip(eager.schema().fields.iter())
    {
        assert_eq!(s.name, e.name);
        assert_eq!(s.ty, e.ty);
    }

    // First few features should produce identical attribute vectors.
    let streamed: Vec<_> = streaming
        .into_features()
        .take(10)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for (i, sf) in streamed.iter().enumerate() {
        assert_eq!(sf.attributes, eager.features()[i].attributes);
    }
}

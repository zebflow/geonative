//! GeoJSON writer — streams a `FeatureCollection` to any `Write` sink, one
//! feature at a time. Memory use is O(one feature), so multi-GB outputs are
//! fine to produce.
//!
//! Output is **compact** by default (no whitespace). Use a pretty-print
//! wrapper at the consumer side if needed — keeping the writer compact lets
//! it stream efficiently and keeps `.geojson.gz` sizes small.

use std::io::Write;

use geonative_core::{Feature, Schema};
use serde_json::{Map as JsonMap, Value as Json};

use crate::error::{GeoJsonError, Result};
use crate::geometry::to_json as geom_to_json;
use crate::properties::value_to_json;

#[derive(Debug)]
pub struct GeoJsonWriter<W: Write> {
    inner: W,
    schema: Schema,
    state: State,
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    BeforeFirst,
    Streaming,
    Closed,
}

impl<W: Write> GeoJsonWriter<W> {
    /// Create a writer that targets `sink`. Emits the FeatureCollection
    /// opening immediately so partial files are still parseable as JSON
    /// even after a crash (well — `]}` will be missing, but a recovery
    /// tool can patch that).
    pub fn create(mut sink: W, schema: &Schema) -> Result<Self> {
        // Open the collection. We don't write the CRS member: RFC 7946
        // removed it, and consumers that need it can read it from a sibling
        // sidecar (the `metadata` subcommand) or the source format directly.
        sink.write_all(br#"{"type":"FeatureCollection","features":["#)?;
        Ok(Self {
            inner: sink,
            schema: schema.clone(),
            state: State::BeforeFirst,
        })
    }

    pub fn write(&mut self, feat: &Feature) -> Result<()> {
        if self.state == State::Closed {
            return Err(GeoJsonError::malformed("writer already closed"));
        }
        let separator: &[u8] = if self.state == State::BeforeFirst {
            self.state = State::Streaming;
            &[]
        } else {
            b","
        };
        self.inner.write_all(separator)?;

        let obj = feature_to_json(&self.schema, feat);
        // Use serde_json's writer-targeted serialiser to avoid an intermediate String.
        serde_json::to_writer(&mut self.inner, &obj)?;
        Ok(())
    }

    pub fn close(mut self) -> Result<W> {
        self.inner.write_all(b"]}")?;
        self.state = State::Closed;
        Ok(self.inner)
    }
}

fn feature_to_json(schema: &Schema, feat: &Feature) -> Json {
    let mut props = JsonMap::new();
    for (i, field) in schema.fields.iter().enumerate() {
        let v = feat
            .attributes
            .get(i)
            .map(value_to_json)
            .unwrap_or(Json::Null);
        // RFC 7946 §7.1.1 — properties value may be JSON null; we keep them
        // explicit so reader-side schema inference recovers nullability.
        props.insert(field.name.clone(), v);
    }
    let mut obj = JsonMap::new();
    obj.insert("type".into(), Json::String("Feature".into()));
    if let Some(fid) = feat.fid {
        obj.insert("id".into(), Json::Number(fid.into()));
    }
    obj.insert(
        "geometry".into(),
        feat.geometry
            .as_ref()
            .map(geom_to_json)
            .unwrap_or(Json::Null),
    );
    obj.insert("properties".into(), Json::Object(props));
    Json::Object(obj)
}

/// Convenience: write `features` to `path` as a complete FeatureCollection.
pub fn write_features_to_path<P, I>(path: P, schema: &Schema, features: I) -> Result<()>
where
    P: AsRef<std::path::Path>,
    I: IntoIterator<Item = Feature>,
{
    let file = std::fs::File::create(path)?;
    let mut w = GeoJsonWriter::create(std::io::BufWriter::new(file), schema)?;
    for f in features {
        w.write(&f)?;
    }
    let buf = w.close()?;
    buf.into_inner()
        .map_err(|e| GeoJsonError::Io(e.into_error()))?
        .sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::GeoJsonReader;
    use geonative_core::{
        Coord, Crs, FieldDef, GeomField, Geometry, GeometryType, Value, ValueType,
    };

    fn sample_schema() -> Schema {
        Schema::new(
            vec![
                FieldDef::new("name", ValueType::String, false),
                FieldDef::new("rank", ValueType::Int64, true),
            ],
            Some(GeomField::new("geometry", GeometryType::Point)),
            Crs::Epsg(4326),
        )
    }

    #[test]
    fn writes_then_reads_back() {
        let schema = sample_schema();
        let features = vec![
            Feature::new(
                Some(1),
                Some(Geometry::Point(Coord::xy(1.0, 2.0))),
                vec![Value::String("alice".into()), Value::Int64(10)],
            ),
            Feature::new(
                Some(2),
                Some(Geometry::Point(Coord::xy(3.0, 4.0))),
                vec![Value::String("bob".into()), Value::Null],
            ),
        ];

        let mut buf = Vec::new();
        let mut w = GeoJsonWriter::create(&mut buf, &schema).unwrap();
        for f in &features {
            w.write(f).unwrap();
        }
        w.close().unwrap();

        let r = GeoJsonReader::from_bytes(&buf).unwrap();
        assert_eq!(r.feature_count(), 2);
        assert_eq!(r.features()[0].fid, Some(1));
        assert_eq!(r.features()[1].fid, Some(2));
        // 'rank' nullable round-trip:
        assert_eq!(r.features()[1].attributes[1], Value::Null);
        assert_eq!(r.features()[0].geometry, features[0].geometry);
    }

    #[test]
    fn empty_collection_is_valid_json() {
        let schema = sample_schema();
        let mut buf = Vec::new();
        let w = GeoJsonWriter::create(&mut buf, &schema).unwrap();
        w.close().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["type"], "FeatureCollection");
        assert_eq!(v["features"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn writing_after_close_errors() {
        let schema = sample_schema();
        let mut buf = Vec::new();
        let mut w = GeoJsonWriter::create(&mut buf, &schema).unwrap();
        w.write(&Feature::new(
            None,
            Some(Geometry::Point(Coord::xy(0.0, 0.0))),
            vec![Value::String("x".into()), Value::Int64(1)],
        ))
        .unwrap();
        // simulate post-close write by manually flipping state (close consumes self)
        // Instead, just check that close() is idempotent-equivalent — i.e. we
        // can't call write after close because close takes ownership. Make
        // sure the "closed already" branch is reachable via the state field
        // by constructing a writer, marking it closed, and writing.
        w.state = State::Closed;
        let err = w
            .write(&Feature::new(
                None,
                Some(Geometry::Point(Coord::xy(0.0, 0.0))),
                vec![Value::String("y".into()), Value::Int64(2)],
            ))
            .unwrap_err();
        assert!(matches!(err, GeoJsonError::Malformed(_)));
    }

    #[test]
    fn null_geometry_round_trips() {
        let schema = sample_schema();
        let feat = Feature::new(
            None,
            None,
            vec![Value::String("nogeo".into()), Value::Int64(99)],
        );
        let mut buf = Vec::new();
        let mut w = GeoJsonWriter::create(&mut buf, &schema).unwrap();
        w.write(&feat).unwrap();
        w.close().unwrap();
        let r = GeoJsonReader::from_bytes(&buf).unwrap();
        assert!(r.features()[0].geometry.is_none());
    }
}

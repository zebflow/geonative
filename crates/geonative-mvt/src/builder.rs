//! Build one MVT layer at a time: encode geometries, intern keys + values,
//! emit feature bodies into a layer-level scratch buffer.
//!
//! The layer body it produces is the protobuf payload of the outer
//! `Tile.layers[N]` field. The Tile assembler in `lib.rs` wraps it.

use std::collections::HashMap;

use geonative_core::{Feature, Schema as CoreSchema, Value};
use geonative_tile::TileCoord;

use crate::error::{MvtError, Result};
use crate::geom::{classify, encode_geometry};
use crate::proto::{
    self, write_double_field, write_float_field, write_message_field, write_packed_varint_field,
    write_string_field, write_varint_field,
};

/// One discrete value stored in a layer's `values` pool.
///
/// `Float` and `Double` are stored by their bit-pattern so we can hash and
/// dedup NaN-bearing values without violating `Hash`/`Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum InternedValue {
    String(String),
    Bool(bool),
    Int(i64),
    /// Stored as IEEE-754 bit pattern. Encoded as `float_value` (field 2).
    Float(u32),
    /// Stored as IEEE-754 bit pattern. Encoded as `double_value` (field 3).
    Double(u64),
}

/// Builder that accumulates one layer's feature bodies.
///
/// `key_pool` / `value_pool` deduplicate attribute keys and values across
/// every feature in the layer — that's the central space-saving trick of
/// the MVT format.
#[derive(Debug)]
pub struct LayerBuilder<'a> {
    name: String,
    tile: TileCoord,
    extent: u32,
    schema: &'a CoreSchema,
    /// Each entry is one feature's encoded body (the bytes that go inside
    /// `Layer.features[N]`).
    feature_bodies: Vec<Vec<u8>>,
    key_pool: Vec<String>,
    key_index: HashMap<String, u32>,
    value_pool: Vec<InternedValue>,
    value_index: HashMap<InternedValue, u32>,
    /// Scratch space for command streams; reused across features.
    cmd_scratch: Vec<u32>,
}

impl<'a> LayerBuilder<'a> {
    pub fn new(
        name: impl Into<String>,
        tile: TileCoord,
        extent: u32,
        schema: &'a CoreSchema,
    ) -> Self {
        Self {
            name: name.into(),
            tile,
            extent,
            schema,
            feature_bodies: Vec::new(),
            key_pool: Vec::new(),
            key_index: HashMap::new(),
            value_pool: Vec::new(),
            value_index: HashMap::new(),
            cmd_scratch: Vec::with_capacity(64),
        }
    }

    /// Encode one feature into the layer. Geometries that don't fit MVT's
    /// model (e.g. `GeometryCollection`) return [`MvtError::Unsupported`].
    pub fn add_feature(&mut self, feat: &Feature) -> Result<()> {
        let geom = match feat.geometry.as_ref() {
            Some(g) => g,
            None => return Ok(()), // attribute-only rows have no place in MVT
        };
        let geom_type = classify(geom)?;

        self.cmd_scratch.clear();
        encode_geometry(geom, self.tile, self.extent, &mut self.cmd_scratch)?;

        // No commands → skip (empty geometries become no-op rows; not useful).
        if self.cmd_scratch.is_empty() {
            return Ok(());
        }

        // Build tags: [key_idx, value_idx, key_idx, value_idx, ...] for each
        // non-null attribute, in schema field order.
        let mut tags: Vec<u32> = Vec::with_capacity(feat.attributes.len() * 2);
        if feat.attributes.len() != self.schema.fields.len() {
            return Err(MvtError::Schema(format!(
                "feature has {} attrs, schema declares {}",
                feat.attributes.len(),
                self.schema.fields.len()
            )));
        }
        for (field, value) in self.schema.fields.iter().zip(&feat.attributes) {
            if matches!(value, Value::Null) {
                continue;
            }
            let Some(iv) = to_interned(value) else {
                continue; // skip unsupported variants (Binary, Guid, etc.)
            };
            let kidx = self.intern_key(&field.name);
            let vidx = self.intern_value(iv);
            tags.push(kidx);
            tags.push(vidx);
        }

        // Build the Feature message body.
        // Schema:
        //   optional uint64 id = 1;
        //   repeated uint32 tags = 2 [packed];
        //   optional GeomType type = 3;
        //   repeated uint32 geometry = 4 [packed];
        let mut body = Vec::with_capacity(self.cmd_scratch.len() * 2 + tags.len() * 2 + 8);
        if let Some(fid) = feat.fid {
            // MVT id is uint64; only emit for non-negative FIDs.
            if fid >= 0 {
                write_varint_field(&mut body, 1, fid as u64);
            }
        }
        if !tags.is_empty() {
            write_packed_varint_field(&mut body, 2, &tags);
        }
        write_varint_field(&mut body, 3, geom_type as u64);
        write_packed_varint_field(&mut body, 4, &self.cmd_scratch);

        self.feature_bodies.push(body);
        Ok(())
    }

    fn intern_key(&mut self, key: &str) -> u32 {
        if let Some(&i) = self.key_index.get(key) {
            return i;
        }
        let i = self.key_pool.len() as u32;
        self.key_pool.push(key.to_string());
        self.key_index.insert(key.to_string(), i);
        i
    }

    fn intern_value(&mut self, v: InternedValue) -> u32 {
        if let Some(&i) = self.value_index.get(&v) {
            return i;
        }
        let i = self.value_pool.len() as u32;
        self.value_index.insert(v.clone(), i);
        self.value_pool.push(v);
        i
    }

    /// Serialize the layer into protobuf bytes (the payload of one
    /// `Tile.layers[N]` length-delimited message).
    pub fn finish(self) -> Vec<u8> {
        let mut layer =
            Vec::with_capacity(self.feature_bodies.iter().map(|b| b.len()).sum::<usize>() + 256);

        // Layer fields (MVT 2.1):
        //   required string name = 1;
        //   repeated Feature features = 2;
        //   repeated string keys = 3;
        //   repeated Value values = 4;
        //   optional uint32 extent = 5;
        //   required uint32 version = 15;
        write_string_field(&mut layer, 1, &self.name);
        for body in &self.feature_bodies {
            write_message_field(&mut layer, 2, body);
        }
        for key in &self.key_pool {
            write_string_field(&mut layer, 3, key);
        }
        for v in &self.value_pool {
            let body = encode_value_message(v);
            write_message_field(&mut layer, 4, &body);
        }
        write_varint_field(&mut layer, 5, self.extent as u64);
        write_varint_field(&mut layer, 15, 2); // MVT version 2

        layer
    }

    pub fn feature_count(&self) -> usize {
        self.feature_bodies.len()
    }

    pub fn key_count(&self) -> usize {
        self.key_pool.len()
    }

    pub fn value_count(&self) -> usize {
        self.value_pool.len()
    }
}

fn encode_value_message(v: &InternedValue) -> Vec<u8> {
    // Value message (MVT 2.1):
    //   optional string string_value = 1;
    //   optional float  float_value  = 2;
    //   optional double double_value = 3;
    //   optional int64  int_value    = 4;
    //   optional uint64 uint_value   = 5;
    //   optional sint64 sint_value   = 6;
    //   optional bool   bool_value   = 7;
    let mut out = Vec::new();
    match v {
        InternedValue::String(s) => write_string_field(&mut out, 1, s),
        InternedValue::Float(bits) => write_float_field(&mut out, 2, f32::from_bits(*bits)),
        InternedValue::Double(bits) => write_double_field(&mut out, 3, f64::from_bits(*bits)),
        InternedValue::Int(n) if *n >= 0 => write_varint_field(&mut out, 4, *n as u64),
        InternedValue::Int(n) => {
            // Use sint_value (field 6) for negatives — uses zigzag, smaller wire.
            proto::write_tag(&mut out, 6, proto::WIRE_VARINT);
            proto::write_varint(&mut out, proto::zigzag_encode(*n));
        }
        InternedValue::Bool(b) => write_varint_field(&mut out, 7, *b as u64),
    }
    out
}

/// Map a `geonative_core::Value` into an `InternedValue`. Returns `None`
/// for variants that MVT can't represent in its value pool (`Binary`,
/// `Guid`, `Xml`, `DateTime` — readers typically don't surface these
/// from MVT anyway).
fn to_interned(v: &Value) -> Option<InternedValue> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => InternedValue::Bool(*b),
        Value::Int16(n) => InternedValue::Int(*n as i64),
        Value::Int32(n) => InternedValue::Int(*n as i64),
        Value::Int64(n) => InternedValue::Int(*n),
        Value::Float32(f) => InternedValue::Float(f.to_bits()),
        Value::Float64(f) => InternedValue::Double(f.to_bits()),
        Value::String(s) => InternedValue::String(s.clone()),
        Value::DateTime(d) => InternedValue::Double(d.to_bits()),
        Value::Xml(s) => InternedValue::String(s.clone()),
        Value::Binary(_) | Value::Guid(_) => return None,
        // Future Value variants (e.g. Decimal, Json) — skip rather than
        // guess at the right MVT representation.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::{
        Coord, Crs, FieldDef, GeomField, Geometry, GeometryType, LineString, Schema, ValueType,
    };

    fn make_schema() -> Schema {
        Schema::new(
            vec![
                FieldDef::new("id", ValueType::Int32, false),
                FieldDef::new("name", ValueType::String, true),
            ],
            Some(GeomField::new("geom", GeometryType::LineString)),
            Crs::Epsg(4326),
        )
    }

    #[test]
    fn keys_and_values_are_interned_across_features() {
        let s = make_schema();
        let tile = TileCoord::new(0, 0, 0);
        let mut lb = LayerBuilder::new("roads", tile, 4096, &s);

        for (id, name) in [(1, "a"), (2, "a"), (3, "b")] {
            lb.add_feature(&Feature::new(
                Some(id),
                Some(Geometry::LineString(LineString::new(vec![
                    Coord {
                        x: 0.0,
                        y: 0.0,
                        z: None,
                        m: None,
                    },
                    Coord {
                        x: 10.0,
                        y: 0.0,
                        z: None,
                        m: None,
                    },
                ]))),
                vec![Value::Int32(id as i32), Value::String(name.into())],
            ))
            .unwrap();
        }

        assert_eq!(lb.feature_count(), 3);
        // 2 distinct keys ("id", "name"); 4 distinct values (Int 1, 2, 3 + String "a", "b" — 5)
        assert_eq!(lb.key_count(), 2);
        // 3 distinct ids + 2 distinct names = 5 unique values
        assert_eq!(lb.value_count(), 5);
    }

    #[test]
    fn null_attributes_are_skipped() {
        let s = make_schema();
        let tile = TileCoord::new(0, 0, 0);
        let mut lb = LayerBuilder::new("roads", tile, 4096, &s);
        lb.add_feature(&Feature::new(
            Some(1),
            Some(Geometry::LineString(LineString::new(vec![
                Coord {
                    x: 0.0,
                    y: 0.0,
                    z: None,
                    m: None,
                },
                Coord {
                    x: 10.0,
                    y: 0.0,
                    z: None,
                    m: None,
                },
            ]))),
            vec![Value::Int32(1), Value::Null],
        ))
        .unwrap();
        // Only "id" interned, not "name".
        assert_eq!(lb.key_count(), 1);
        assert_eq!(lb.value_count(), 1);
    }

    #[test]
    fn feature_without_geometry_is_dropped() {
        let s = make_schema();
        let tile = TileCoord::new(0, 0, 0);
        let mut lb = LayerBuilder::new("roads", tile, 4096, &s);
        lb.add_feature(&Feature::new(
            Some(1),
            None,
            vec![Value::Int32(1), Value::String("a".into())],
        ))
        .unwrap();
        assert_eq!(lb.feature_count(), 0);
    }

    #[test]
    fn empty_geometry_is_dropped() {
        let s = make_schema();
        let tile = TileCoord::new(0, 0, 0);
        let mut lb = LayerBuilder::new("roads", tile, 4096, &s);
        lb.add_feature(&Feature::new(
            Some(1),
            Some(Geometry::Empty(GeometryType::LineString)),
            vec![Value::Int32(1), Value::String("a".into())],
        ))
        .unwrap();
        assert_eq!(lb.feature_count(), 0);
    }
}

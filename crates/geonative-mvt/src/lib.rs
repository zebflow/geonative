//! # geonative-mvt
//!
//! Hand-rolled Mapbox Vector Tile (MVT 2.1) encoder. Consumes any
//! `geonative_core::Feature` stream + a [`geonative_tile::TileCoord`] and
//! emits the byte payload that web-map clients (Mapbox GL JS, MapLibre,
//! OpenLayers, …) expect for `/z/x/y.mvt` endpoints.
//!
//! ## Why hand-rolled protobuf?
//!
//! The MVT 2.1 schema is **tiny** (one outer Tile + Layer + Feature + Value
//! message — see <https://github.com/mapbox/vector-tile-spec/tree/master/2.1>).
//! Hand-rolling avoids a `prost` / `protobuf-codegen` build dependency and
//! keeps the entire encoder under 1000 lines you can audit in one sitting.
//!
//! ## What's in it
//!
//! - [`encode_tile`] / [`encode_tile_bytes`] — high-level: take a tile coord
//!   + one-or-more `(layer_name, schema, features)` triples → MVT bytes.
//! - [`TileBuilder`] — incremental builder if you want to interleave
//!   layer-by-layer instead of building a tile in one shot.
//! - [`builder::LayerBuilder`] — re-exported; used inside `TileBuilder` and
//!   useful if you want fine-grained control.
//! - [`proto`] — low-level protobuf wire-format helpers (varint, zigzag, tags).
//! - [`geom`] — geometry → MVT command stream (MoveTo/LineTo/ClosePath).
//!
//! ## Clever bits
//!
//! - **Cursor pattern for command-stream deltas.** A single `(cx, cy)`
//!   accumulator persists across all commands and parts of a feature, so
//!   `MultiLineString` part transitions don't accidentally reset state.
//! - **Closing-vertex stripped** from polygon rings before emission — the
//!   IR carries the duplicate (OGC convention), MVT doesn't.
//! - **Key/value pools deduplicate inside one layer.** Three features with
//!   the same `"name": "Main St"` write one shared value entry plus tag
//!   references. The pools live inside [`builder::LayerBuilder`].
//! - **Negative integers go to the `sint_value` field** (zigzag-encoded) for
//!   smaller wire form than the unsigned `int_value` path.
//! - **`GeometryCollection` is rejected** (it has no MVT analogue); empty
//!   geometries silently produce zero commands and a feature is dropped.
//! - **No reliance on a CRS transform crate.** Coordinates are assumed to be
//!   in WGS84 (lng/lat). If your data is in some other CRS, reproject before
//!   handing features to the encoder.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod builder;
pub mod error;
pub mod geom;
pub mod proto;

pub use builder::LayerBuilder;
pub use error::{MvtError, Result};
pub use geom::MvtGeomType;
pub use geonative_tile::{LngLat, Metatile, TileCoord};

use geonative_core::{Feature, Schema};

/// Options for building an MVT tile.
#[derive(Debug, Clone)]
pub struct MvtOptions {
    /// Tile pixel-grid side length. MVT spec default is 4096.
    pub extent: u32,
}

impl Default for MvtOptions {
    fn default() -> Self {
        Self { extent: 4096 }
    }
}

/// One layer's worth of input: a name + schema + features. Borrowed so
/// callers can stream from any source.
#[derive(Debug)]
pub struct LayerInput<'a, I> {
    pub name: &'a str,
    pub schema: &'a Schema,
    pub features: I,
}

/// Incrementally builds an MVT tile by accepting one layer at a time.
///
/// The on-disk MVT format puts layers back-to-back as length-delimited
/// submessages of the outer Tile. This builder accumulates each finished
/// layer's bytes and concatenates them in [`finish`](Self::finish).
pub struct TileBuilder {
    tile: TileCoord,
    options: MvtOptions,
    layer_bodies: Vec<Vec<u8>>,
}

impl std::fmt::Debug for TileBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileBuilder")
            .field("tile", &self.tile)
            .field("extent", &self.options.extent)
            .field("layers_so_far", &self.layer_bodies.len())
            .finish()
    }
}

impl TileBuilder {
    pub fn new(tile: TileCoord, options: MvtOptions) -> Self {
        Self {
            tile,
            options,
            layer_bodies: Vec::new(),
        }
    }

    /// Add a layer whose features come from any iterator. Errors during
    /// individual feature encoding propagate immediately.
    pub fn add_layer<I, T>(&mut self, name: &str, schema: &Schema, features: I) -> Result<()>
    where
        I: IntoIterator<Item = T>,
        T: AsFeatureRef,
    {
        let mut lb = LayerBuilder::new(name, self.tile, self.options.extent, schema);
        for f in features {
            lb.add_feature(f.as_feature_ref())?;
        }
        self.layer_bodies.push(lb.finish());
        Ok(())
    }

    /// Finalize the tile into MVT bytes.
    pub fn finish(self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(self.layer_bodies.iter().map(|b| b.len()).sum::<usize>() + 16);
        for body in &self.layer_bodies {
            proto::write_message_field(&mut out, 3, body); // Tile.layers (field 3)
        }
        out
    }
}

/// Helper trait so [`TileBuilder::add_layer`] accepts both `Feature` and
/// `&Feature` iterators.
pub trait AsFeatureRef {
    fn as_feature_ref(&self) -> &Feature;
}

impl AsFeatureRef for Feature {
    fn as_feature_ref(&self) -> &Feature {
        self
    }
}

impl AsFeatureRef for &Feature {
    fn as_feature_ref(&self) -> &Feature {
        self
    }
}

/// One-shot helper: encode a single-layer tile from any feature iterator.
pub fn encode_tile_bytes<I, T>(
    tile: TileCoord,
    layer_name: &str,
    schema: &Schema,
    features: I,
    options: MvtOptions,
) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = T>,
    T: AsFeatureRef,
{
    let mut tb = TileBuilder::new(tile, options);
    tb.add_layer(layer_name, schema, features)?;
    Ok(tb.finish())
}

/// One-shot helper: encode a multi-layer tile.
pub fn encode_tile<'a, I>(tile: TileCoord, layers: I, options: MvtOptions) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = LayerInput<'a, Vec<Feature>>>,
{
    let mut tb = TileBuilder::new(tile, options);
    for li in layers {
        tb.add_layer(li.name, li.schema, li.features)?;
    }
    Ok(tb.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::{
        Coord, Crs, FieldDef, GeomField, Geometry, GeometryType, LineString, ValueType,
    };

    fn schema() -> Schema {
        Schema::new(
            vec![FieldDef::new("name", ValueType::String, true)],
            Some(GeomField::new("geom", GeometryType::LineString)),
            Crs::Epsg(4326),
        )
    }

    #[test]
    fn one_feature_tile_has_a_layer_message() {
        let tile = TileCoord::new(0, 0, 0);
        let s = schema();
        let feats = vec![Feature::new(
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
            vec![geonative_core::Value::String("Main".into())],
        )];

        let bytes = encode_tile_bytes(tile, "roads", &s, &feats, MvtOptions::default()).unwrap();
        // First byte must be the Tile.layers tag = (3 << 3) | 2 = 26 = 0x1A
        assert!(
            bytes.starts_with(&[0x1A]),
            "expected layers tag, got {:?}",
            &bytes[..8]
        );
        // The bytes should be non-trivial size — name + features + keys/values + version.
        assert!(bytes.len() > 30);
    }

    #[test]
    fn empty_tile_serializes_to_empty_bytes() {
        let tb = TileBuilder::new(TileCoord::new(0, 0, 0), MvtOptions::default());
        let bytes = tb.finish();
        assert!(bytes.is_empty());
    }

    #[test]
    fn two_layers_produce_two_layer_messages() {
        let tile = TileCoord::new(0, 0, 0);
        let s = schema();
        let mut tb = TileBuilder::new(tile, MvtOptions::default());
        let f = Feature::new(
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
            vec![geonative_core::Value::String("Main".into())],
        );
        tb.add_layer("a", &s, std::iter::once(&f)).unwrap();
        tb.add_layer("b", &s, std::iter::once(&f)).unwrap();
        let bytes = tb.finish();
        // Two layer tags: 0x1A appears twice
        let layer_tag_count = bytes.iter().filter(|&&b| b == 0x1A).count();
        // At least two — could occur incidentally inside payloads but for this
        // synthetic input it should be exactly the prefix of each layer.
        assert!(layer_tag_count >= 2);
    }
}

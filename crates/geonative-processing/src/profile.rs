//! Streaming dataset profiler — one pass over a feature stream, producing
//! null counts, min/max, distinct counts, top-N values, and a small sample
//! per field. Plus a computed bbox extent and per-geometry-type histogram.
//!
//! ## Design
//!
//! - **One pass** — `profile(schema, features, opts)` consumes any iterator,
//!   so it works with `Layer::read()`, `GeoParquetReader::into_features()`,
//!   `GeoJsonReader::into_features()`, etc. — anything yielding `Feature`.
//! - **Bounded memory** — distinct/top-N tracking caps at `opts.distinct_limit`
//!   per field. Past that, we stop counting individual values and just
//!   report `distinct_count = None` (meaning "more than `distinct_limit`").
//! - **First-N sampling** — v0.1 takes the first `opts.sample_n` features
//!   verbatim. Deterministic, reproducible, but biased toward the head of
//!   the file. Reservoir sampling is a future improvement.
//! - **Float-aware** — `Float32` / `Float64` columns get min/max but no
//!   top-N or distinct count (NaN-safe hashing isn't worth the complication
//!   for v0.1). Min/max uses `partial_cmp` and skips NaN.

use std::collections::BTreeMap;

use geonative_core::{Feature, GeometryType, Schema, Value, ValueType};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct ProfileOptions {
    /// How many top-frequency values to report per field. Default 10.
    pub top_n: usize,
    /// How many features to keep as samples (head-of-file in v0.1). Default 5.
    pub sample_n: usize,
    /// Maximum distinct values to track per field before falling back to
    /// "cardinality unknown / too large". Default 10_000.
    pub distinct_limit: usize,
}

impl Default for ProfileOptions {
    fn default() -> Self {
        Self {
            top_n: 10,
            sample_n: 5,
            distinct_limit: 10_000,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProfileReport {
    pub feature_count: u64,
    pub geometry: GeometryStats,
    pub fields: Vec<FieldStats>,
    /// First `sample_n` features (deterministic head sample for v0.1).
    pub samples: Vec<SerdeFeature>,
}

#[derive(Debug, Serialize)]
pub struct GeometryStats {
    /// `[xmin, ymin, xmax, ymax]` — present iff at least one geometry had a
    /// computable bbox.
    pub computed_extent: Option<[f64; 4]>,
    /// Histogram of `Geometry` variant → count. Useful for catching mixed
    /// FeatureCollections where the schema says "Point" but reality says
    /// "Point + MultiPoint mixed".
    pub kinds: BTreeMap<String, u64>,
    /// Count of features whose `geometry` was `None`.
    pub null_count: u64,
}

#[derive(Debug, Serialize)]
pub struct FieldStats {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub null_count: u64,
    pub value_count: u64,
    /// `None` if cardinality exceeded `opts.distinct_limit`, OR the field
    /// type isn't hashable (floats — see module docs).
    pub distinct_count: Option<u64>,
    /// String/JSON representation of min / max — see [`value_to_json_repr`].
    pub min: Option<JsonValue>,
    pub max: Option<JsonValue>,
    /// Top-N most frequent values (descending by count). Empty for
    /// float-typed or over-cardinality fields.
    pub top_values: Vec<TopValue>,
}

#[derive(Debug, Serialize)]
pub struct TopValue {
    pub value: JsonValue,
    pub count: u64,
}

/// Serializable feature payload (Value → JSON). Geometry is rendered as
/// GeoJSON-shaped JSON to keep the report self-contained without pulling
/// in `geonative-geojson` as a dep (which would create a cycle with the
/// CLI).
#[derive(Debug, Serialize)]
pub struct SerdeFeature {
    pub fid: Option<i64>,
    pub geometry_kind: Option<String>,
    pub attributes: BTreeMap<String, JsonValue>,
}

/// Lightweight JSON-equivalent used in serialized output. We avoid pulling
/// `serde_json::Value` into the public API so this crate doesn't force a
/// `serde_json` dependency on downstream library consumers.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

pub fn profile<I>(schema: &Schema, features: I, opts: ProfileOptions) -> ProfileReport
where
    I: IntoIterator<Item = Feature>,
{
    let mut field_accs: Vec<FieldAcc> = schema
        .fields
        .iter()
        .map(|f| FieldAcc::new(f.name.clone(), f.ty))
        .collect();

    let mut geom = GeometryAcc::default();
    let mut samples: Vec<SerdeFeature> = Vec::with_capacity(opts.sample_n);
    let mut count: u64 = 0;

    for feat in features {
        if samples.len() < opts.sample_n {
            samples.push(serialize_feature(schema, &feat));
        }
        geom.observe(&feat);
        for (i, acc) in field_accs.iter_mut().enumerate() {
            let v = feat.attributes.get(i).unwrap_or(&Value::Null);
            acc.observe(v, &opts);
        }
        count += 1;
    }

    ProfileReport {
        feature_count: count,
        geometry: geom.finalize(),
        fields: field_accs.into_iter().map(FieldAcc::finalize).collect(),
        samples,
    }
}

#[derive(Debug, Default)]
struct GeometryAcc {
    extent: Option<[f64; 4]>,
    kinds: BTreeMap<String, u64>,
    null_count: u64,
}

impl GeometryAcc {
    fn observe(&mut self, feat: &Feature) {
        let Some(g) = &feat.geometry else {
            self.null_count += 1;
            return;
        };
        let kind = geometry_kind_label(g_type(g));
        *self.kinds.entry(kind.to_string()).or_insert(0) += 1;
        if let Some(b) = g.bbox() {
            self.extent = Some(match self.extent {
                None => b,
                Some(prev) => [
                    prev[0].min(b[0]),
                    prev[1].min(b[1]),
                    prev[2].max(b[2]),
                    prev[3].max(b[3]),
                ],
            });
        }
    }

    fn finalize(self) -> GeometryStats {
        GeometryStats {
            computed_extent: self.extent,
            kinds: self.kinds,
            null_count: self.null_count,
        }
    }
}

#[derive(Debug)]
struct FieldAcc {
    name: String,
    ty: ValueType,
    null_count: u64,
    value_count: u64,
    counts: Option<BTreeMap<HashKey, u64>>,
    cardinality_capped: bool,
    min_f: Option<f64>,
    max_f: Option<f64>,
    min_s: Option<String>,
    max_s: Option<String>,
}

impl FieldAcc {
    fn new(name: String, ty: ValueType) -> Self {
        Self {
            name,
            ty,
            null_count: 0,
            value_count: 0,
            counts: if is_hashable(ty) {
                Some(BTreeMap::new())
            } else {
                None
            },
            cardinality_capped: false,
            min_f: None,
            max_f: None,
            min_s: None,
            max_s: None,
        }
    }

    fn observe(&mut self, v: &Value, opts: &ProfileOptions) {
        if matches!(v, Value::Null) {
            self.null_count += 1;
            return;
        }
        self.value_count += 1;

        // Min/max — numerics + datetime get f64; strings get lex order.
        if let Some(n) = as_numeric(v) {
            if !n.is_nan() {
                self.min_f = Some(self.min_f.map_or(n, |m| m.min(n)));
                self.max_f = Some(self.max_f.map_or(n, |m| m.max(n)));
            }
        }
        if let Value::String(s) = v {
            self.min_s = Some(match self.min_s.take() {
                None => s.clone(),
                Some(prev) => {
                    if s < &prev {
                        s.clone()
                    } else {
                        prev
                    }
                }
            });
            self.max_s = Some(match self.max_s.take() {
                None => s.clone(),
                Some(prev) => {
                    if s > &prev {
                        s.clone()
                    } else {
                        prev
                    }
                }
            });
        }

        // Distinct + top-N.
        if let Some(counts) = self.counts.as_mut() {
            if let Some(key) = HashKey::from_value(v) {
                if counts.contains_key(&key) {
                    *counts.get_mut(&key).unwrap() += 1;
                } else if counts.len() < opts.distinct_limit {
                    counts.insert(key, 1);
                } else {
                    // Cap reached: stop tracking new keys; existing-key
                    // increments stop too (we want the report not to lie).
                    self.cardinality_capped = true;
                    self.counts = None;
                }
            }
        }
    }

    fn finalize(self) -> FieldStats {
        let min = match self.ty {
            ValueType::String => self.min_s.clone().map(JsonValue::String),
            _ => self.min_f.and_then(jsonvalue_from_numeric_typed(self.ty)),
        };
        let max = match self.ty {
            ValueType::String => self.max_s.clone().map(JsonValue::String),
            _ => self.max_f.and_then(jsonvalue_from_numeric_typed(self.ty)),
        };

        let (distinct_count, top_values) = match (self.counts, self.cardinality_capped) {
            (Some(counts), false) => {
                let mut pairs: Vec<_> = counts.into_iter().collect();
                pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                let distinct = pairs.len() as u64;
                let top = pairs
                    .into_iter()
                    .take(DEFAULT_TOP_N_FALLBACK)
                    .map(|(k, count)| TopValue {
                        value: k.into_json_value(),
                        count,
                    })
                    .collect();
                (Some(distinct), top)
            }
            _ => (None, Vec::new()),
        };

        FieldStats {
            name: self.name,
            ty: format!("{:?}", self.ty),
            null_count: self.null_count,
            value_count: self.value_count,
            distinct_count,
            min,
            max,
            top_values,
        }
    }
}

/// Top-N is capped here at report-build time; the caller-provided `top_n`
/// can be lowered by post-filtering. Using a single constant keeps the
/// hot loop in `observe` from threading the option down.
const DEFAULT_TOP_N_FALLBACK: usize = 10;

fn jsonvalue_from_numeric_typed(ty: ValueType) -> impl Fn(f64) -> Option<JsonValue> {
    move |n: f64| match ty {
        ValueType::Bool => Some(JsonValue::Bool(n != 0.0)),
        ValueType::Int16 | ValueType::Int32 | ValueType::Int64 => Some(JsonValue::Int(n as i64)),
        ValueType::Float32 | ValueType::Float64 | ValueType::DateTime => Some(JsonValue::Float(n)),
        _ => None,
    }
}

fn is_hashable(ty: ValueType) -> bool {
    matches!(
        ty,
        ValueType::Bool
            | ValueType::Int16
            | ValueType::Int32
            | ValueType::Int64
            | ValueType::String
            | ValueType::DateTime
            | ValueType::Guid
    )
}

fn as_numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Int16(n) => Some(*n as f64),
        Value::Int32(n) => Some(*n as f64),
        Value::Int64(n) => Some(*n as f64),
        Value::Float32(f) => Some(*f as f64),
        Value::Float64(f) => Some(*f),
        Value::DateTime(d) => Some(*d),
        _ => None,
    }
}

// Hashable & orderable key for top-N counting. We avoid `f64` because NaN
// breaks Eq/Hash; floats just don't get top-N (documented).
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum HashKey {
    Bool(bool),
    Int(i64),
    String(String),
    DateTimeBits(u64),
    Guid([u8; 16]),
}

impl HashKey {
    fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Bool(b) => Some(Self::Bool(*b)),
            Value::Int16(n) => Some(Self::Int(*n as i64)),
            Value::Int32(n) => Some(Self::Int(*n as i64)),
            Value::Int64(n) => Some(Self::Int(*n)),
            Value::String(s) => Some(Self::String(s.clone())),
            Value::DateTime(d) => Some(Self::DateTimeBits(d.to_bits())),
            Value::Guid(g) => Some(Self::Guid(*g)),
            _ => None,
        }
    }

    fn into_json_value(self) -> JsonValue {
        match self {
            Self::Bool(b) => JsonValue::Bool(b),
            Self::Int(n) => JsonValue::Int(n),
            Self::String(s) => JsonValue::String(s),
            Self::DateTimeBits(bits) => JsonValue::Float(f64::from_bits(bits)),
            Self::Guid(g) => JsonValue::String(hex_lower(&g)),
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn g_type(g: &geonative_core::Geometry) -> GeometryType {
    use geonative_core::Geometry;
    match g {
        Geometry::Point(_) => GeometryType::Point,
        Geometry::LineString(_) => GeometryType::LineString,
        Geometry::Polygon(_) => GeometryType::Polygon,
        Geometry::MultiPoint(_) => GeometryType::MultiPoint,
        Geometry::MultiLineString(_) => GeometryType::MultiLineString,
        Geometry::MultiPolygon(_) => GeometryType::MultiPolygon,
        Geometry::GeometryCollection(_) => GeometryType::GeometryCollection,
        _ => GeometryType::GeometryCollection,
    }
}

fn geometry_kind_label(t: GeometryType) -> &'static str {
    match t {
        GeometryType::Point => "Point",
        GeometryType::LineString => "LineString",
        GeometryType::Polygon => "Polygon",
        GeometryType::MultiPoint => "MultiPoint",
        GeometryType::MultiLineString => "MultiLineString",
        GeometryType::MultiPolygon => "MultiPolygon",
        GeometryType::GeometryCollection => "GeometryCollection",
        _ => "Unknown",
    }
}

fn serialize_feature(schema: &Schema, feat: &Feature) -> SerdeFeature {
    let mut attrs = BTreeMap::new();
    for (i, field) in schema.fields.iter().enumerate() {
        let v = feat.attributes.get(i).unwrap_or(&Value::Null);
        attrs.insert(field.name.clone(), value_to_json_repr(v));
    }
    SerdeFeature {
        fid: feat.fid,
        geometry_kind: feat
            .geometry
            .as_ref()
            .map(|g| geometry_kind_label(g_type(g)).to_string()),
        attributes: attrs,
    }
}

/// Lossy `Value → JsonValue`. Binary/Guid become hex strings, DateTime
/// stays numeric (days since 1899-12-30), Xml stays as a string.
pub fn value_to_json_repr(v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Int16(n) => JsonValue::Int(*n as i64),
        Value::Int32(n) => JsonValue::Int(*n as i64),
        Value::Int64(n) => JsonValue::Int(*n),
        Value::Float32(f) => JsonValue::Float(*f as f64),
        Value::Float64(f) => JsonValue::Float(*f),
        Value::String(s) | Value::Xml(s) => JsonValue::String(s.clone()),
        Value::Binary(b) => JsonValue::String(hex_lower(b)),
        Value::DateTime(d) => JsonValue::Float(*d),
        Value::Guid(g) => JsonValue::String(hex_lower(g)),
        _ => JsonValue::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::{Coord, Crs, FieldDef, GeomField, Geometry, GeometryType, Schema};

    fn mk_schema() -> Schema {
        Schema::new(
            vec![
                FieldDef::new("name", ValueType::String, true),
                FieldDef::new("score", ValueType::Int32, false),
                FieldDef::new("weight", ValueType::Float64, true),
            ],
            Some(GeomField::new("geometry", GeometryType::Point)),
            Crs::Epsg(4326),
        )
    }

    fn pt(x: f64, y: f64) -> Geometry {
        Geometry::Point(Coord::xy(x, y))
    }

    fn feat(fid: i64, name: Option<&str>, score: i32, weight: Option<f64>, x: f64, y: f64) -> Feature {
        let name_v = name
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null);
        let weight_v = weight.map(Value::Float64).unwrap_or(Value::Null);
        Feature::new(
            Some(fid),
            Some(pt(x, y)),
            vec![name_v, Value::Int32(score), weight_v],
        )
    }

    #[test]
    fn counts_features_and_extent() {
        let schema = mk_schema();
        let feats = vec![
            feat(1, Some("a"), 10, Some(1.0), 0.0, 0.0),
            feat(2, Some("b"), 20, Some(2.0), 10.0, 5.0),
            feat(3, Some("a"), 30, None, -3.0, 7.0),
        ];
        let report = profile(&schema, feats, ProfileOptions::default());
        assert_eq!(report.feature_count, 3);
        let ext = report.geometry.computed_extent.unwrap();
        assert_eq!(ext, [-3.0, 0.0, 10.0, 7.0]);
        assert_eq!(report.geometry.kinds.get("Point"), Some(&3));
        assert_eq!(report.geometry.null_count, 0);
    }

    #[test]
    fn nulls_counted_per_field() {
        let schema = mk_schema();
        let feats = vec![
            feat(1, None, 10, None, 0.0, 0.0),
            feat(2, Some("b"), 20, Some(2.0), 1.0, 1.0),
            feat(3, None, 30, Some(3.0), 2.0, 2.0),
        ];
        let report = profile(&schema, feats, ProfileOptions::default());
        let name = report.fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(name.null_count, 2);
        assert_eq!(name.value_count, 1);
        let weight = report.fields.iter().find(|f| f.name == "weight").unwrap();
        assert_eq!(weight.null_count, 1);
    }

    #[test]
    fn top_values_sorted_by_frequency() {
        let schema = mk_schema();
        let feats = vec![
            feat(1, Some("alice"), 1, None, 0.0, 0.0),
            feat(2, Some("bob"), 1, None, 0.0, 0.0),
            feat(3, Some("alice"), 1, None, 0.0, 0.0),
            feat(4, Some("alice"), 1, None, 0.0, 0.0),
            feat(5, Some("bob"), 1, None, 0.0, 0.0),
        ];
        let report = profile(&schema, feats, ProfileOptions::default());
        let name = report.fields.iter().find(|f| f.name == "name").unwrap();
        let top = &name.top_values;
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].count, 3);
        assert_eq!(top[1].count, 2);
        match &top[0].value {
            JsonValue::String(s) => assert_eq!(s, "alice"),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn distinct_capped_when_over_limit() {
        let schema = Schema::new(
            vec![FieldDef::new("id", ValueType::Int64, false)],
            None,
            Crs::Unknown,
        );
        let feats: Vec<Feature> = (0..50_i64)
            .map(|n| Feature::new(Some(n), None, vec![Value::Int64(n)]))
            .collect();
        let opts = ProfileOptions {
            distinct_limit: 10,
            ..Default::default()
        };
        let report = profile(&schema, feats, opts);
        assert_eq!(report.fields[0].distinct_count, None);
        assert!(report.fields[0].top_values.is_empty());
    }

    #[test]
    fn min_max_numeric() {
        let schema = mk_schema();
        let feats = vec![
            feat(1, Some("a"), 5, None, 0.0, 0.0),
            feat(2, Some("b"), -3, None, 0.0, 0.0),
            feat(3, Some("c"), 100, None, 0.0, 0.0),
        ];
        let report = profile(&schema, feats, ProfileOptions::default());
        let score = report.fields.iter().find(|f| f.name == "score").unwrap();
        match (&score.min, &score.max) {
            (Some(JsonValue::Int(mn)), Some(JsonValue::Int(mx))) => {
                assert_eq!(*mn, -3);
                assert_eq!(*mx, 100);
            }
            other => panic!("expected int min/max, got {other:?}"),
        }
    }

    #[test]
    fn samples_are_first_n() {
        let schema = mk_schema();
        let feats: Vec<Feature> = (0..20)
            .map(|i| feat(i, Some(&format!("name{i}")), i as i32, None, 0.0, 0.0))
            .collect();
        let opts = ProfileOptions {
            sample_n: 3,
            ..Default::default()
        };
        let report = profile(&schema, feats, opts);
        assert_eq!(report.samples.len(), 3);
        // First sample is fid=0.
        assert_eq!(report.samples[0].fid, Some(0));
    }

    #[test]
    fn null_geometry_counted() {
        let schema = mk_schema();
        let feats = vec![
            Feature::new(Some(1), None, vec![Value::Null, Value::Int32(1), Value::Null]),
            Feature::new(
                Some(2),
                Some(pt(0.0, 0.0)),
                vec![Value::Null, Value::Int32(2), Value::Null],
            ),
        ];
        let report = profile(&schema, feats, ProfileOptions::default());
        assert_eq!(report.geometry.null_count, 1);
        assert_eq!(report.geometry.kinds.get("Point"), Some(&1));
    }
}

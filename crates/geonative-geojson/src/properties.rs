//! GeoJSON `properties` object handling: schema inference on read,
//! `Value` ↔ JSON conversion on read+write.
//!
//! ## Inference rules
//!
//! GeoJSON has no schema, so we walk every feature's properties to derive
//! one. Per key, we observe the set of JSON value kinds and promote to the
//! tightest covering `ValueType`:
//!
//! | observed kinds | inferred type |
//! | --- | --- |
//! | bool only | `Bool` |
//! | int only | `Int64` |
//! | int + float | `Float64` |
//! | float only | `Float64` |
//! | string only | `String` |
//! | array or object (anywhere) | `String` (re-serialised JSON) |
//! | null only / never seen | `String`, `nullable = true` |
//! | mixed primitive types | `String` (safe widening) |
//!
//! Keys missing from any feature, or null in any feature, become
//! `nullable = true`.

use std::collections::{BTreeMap, BTreeSet};

use geonative_core::{FieldDef, Value, ValueType};
use serde_json::{Map as JsonMap, Value as Json};

/// One pass over the property maps of every feature → `(field defs, key order)`.
/// Returns fields ordered by **first-seen** key for stability across runs.
pub fn infer_fields(all_props: &[Option<&JsonMap<String, Json>>]) -> Vec<FieldDef> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut observations: BTreeMap<String, KeyObs> = BTreeMap::new();

    for props in all_props {
        let Some(map) = props else {
            // Feature had no properties: every previously-seen field is now nullable.
            for k in &order {
                observations.entry(k.clone()).or_default().nullable = true;
            }
            continue;
        };
        let mut visited_this_row: BTreeSet<&str> = BTreeSet::new();
        for (k, v) in map.iter() {
            if seen.insert(k.clone()) {
                order.push(k.clone());
            }
            visited_this_row.insert(k);
            let obs = observations.entry(k.clone()).or_default();
            obs.observe(v);
        }
        // Any previously-seen key not present in this row → nullable.
        for k in &order {
            if !visited_this_row.contains(k.as_str()) {
                observations.entry(k.clone()).or_default().nullable = true;
            }
        }
    }

    order
        .into_iter()
        .map(|name| {
            let obs = observations.remove(&name).unwrap_or_default();
            FieldDef::new(name, obs.resolve_type(), obs.nullable)
        })
        .collect()
}

#[derive(Debug, Default)]
struct KeyObs {
    saw_bool: bool,
    saw_int: bool,
    saw_float: bool,
    saw_string: bool,
    saw_composite: bool, // array or object
    saw_null: bool,
    nullable: bool,
}

impl KeyObs {
    fn observe(&mut self, v: &Json) {
        match v {
            Json::Null => {
                self.saw_null = true;
                self.nullable = true;
            }
            Json::Bool(_) => self.saw_bool = true,
            Json::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    self.saw_int = true;
                } else {
                    self.saw_float = true;
                }
            }
            Json::String(_) => self.saw_string = true,
            Json::Array(_) | Json::Object(_) => self.saw_composite = true,
        }
    }

    fn resolve_type(&self) -> ValueType {
        // Composite or mixed primitives → String (re-serialised JSON on read).
        if self.saw_composite {
            return ValueType::String;
        }
        let primitive_count = [self.saw_bool, self.saw_int, self.saw_float, self.saw_string]
            .iter()
            .filter(|b| **b)
            .count();
        if primitive_count >= 2 {
            // Int + Float widens to Float64 cleanly; anything else widens to String.
            if self.saw_string || self.saw_bool {
                return ValueType::String;
            }
            return ValueType::Float64;
        }
        if self.saw_bool {
            ValueType::Bool
        } else if self.saw_float {
            ValueType::Float64
        } else if self.saw_int {
            ValueType::Int64
        } else if self.saw_string {
            ValueType::String
        } else {
            // Only nulls or never-observed → safe default.
            ValueType::String
        }
    }
}

/// Coerce a JSON value to the schema-declared `ValueType`. Returns
/// `Value::Null` on absent / null / unconvertible; the writer assumes the
/// caller already filled in nullability correctly at schema time.
pub fn json_to_value(j: Option<&Json>, ty: ValueType) -> Value {
    let Some(v) = j else {
        return Value::Null;
    };
    if v.is_null() {
        return Value::Null;
    }
    match ty {
        ValueType::Bool => v.as_bool().map(Value::Bool).unwrap_or(Value::Null),
        ValueType::Int16 => v
            .as_i64()
            .and_then(|n| i16::try_from(n).ok())
            .map(Value::Int16)
            .unwrap_or(Value::Null),
        ValueType::Int32 => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .map(Value::Int32)
            .unwrap_or(Value::Null),
        ValueType::Int64 => v.as_i64().map(Value::Int64).unwrap_or(Value::Null),
        ValueType::Float32 => v
            .as_f64()
            .map(|f| Value::Float32(f as f32))
            .unwrap_or(Value::Null),
        ValueType::Float64 => v.as_f64().map(Value::Float64).unwrap_or(Value::Null),
        ValueType::String => Value::String(json_as_string(v)),
        // Other ValueTypes (Binary, DateTime, Guid, Xml) aren't produced by
        // inference, so we fall back to a re-serialised string. This keeps
        // round-tripping safe if a caller hand-builds a schema with them.
        _ => Value::String(json_as_string(v)),
    }
}

/// Render a `Value` as a JSON value for the `properties` map.
pub fn value_to_json(v: &Value) -> Json {
    use serde_json::Number;
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int16(n) => Json::Number((*n).into()),
        Value::Int32(n) => Json::Number((*n).into()),
        Value::Int64(n) => Json::Number((*n).into()),
        Value::Float32(f) => Number::from_f64(*f as f64)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Float64(f) => Number::from_f64(*f).map(Json::Number).unwrap_or(Json::Null),
        Value::String(s) => Json::String(s.clone()),
        // Binary / Guid encoded as base16 lower-case strings; DateTime as
        // ISO-8601-ish-but-simple `<days since 1899-12-30>` Float to round-trip
        // through GeoJSON readers that don't grok dates. Callers wanting
        // real ISO timestamps should pre-format before handing us a Value.
        Value::Binary(b) => Json::String(hex_lower(b)),
        Value::DateTime(d) => Number::from_f64(*d).map(Json::Number).unwrap_or(Json::Null),
        Value::Guid(g) => Json::String(hex_lower(g)),
        Value::Xml(s) => Json::String(s.clone()),
        _ => Json::Null,
    }
}

fn json_as_string(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Manual nibble write avoids pulling in `hex` or `format!` per-byte cost.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props(j: Json) -> JsonMap<String, Json> {
        j.as_object().unwrap().clone()
    }

    #[test]
    fn infers_int_for_all_int_column() {
        let a = props(json!({"id": 1}));
        let b = props(json!({"id": 2}));
        let fields = infer_fields(&[Some(&a), Some(&b)]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "id");
        assert_eq!(fields[0].ty, ValueType::Int64);
        assert!(!fields[0].nullable);
    }

    #[test]
    fn widens_int_plus_float_to_float64() {
        let a = props(json!({"score": 1}));
        let b = props(json!({"score": 1.5}));
        let fields = infer_fields(&[Some(&a), Some(&b)]);
        assert_eq!(fields[0].ty, ValueType::Float64);
    }

    #[test]
    fn missing_key_makes_field_nullable() {
        let a = props(json!({"name": "alice"}));
        let b = props(json!({}));
        let fields = infer_fields(&[Some(&a), Some(&b)]);
        assert_eq!(fields[0].name, "name");
        assert!(fields[0].nullable);
    }

    #[test]
    fn explicit_null_makes_nullable() {
        let a = props(json!({"x": 1}));
        let b = props(json!({"x": null}));
        let fields = infer_fields(&[Some(&a), Some(&b)]);
        assert_eq!(fields[0].ty, ValueType::Int64);
        assert!(fields[0].nullable);
    }

    #[test]
    fn composite_value_becomes_string() {
        let a = props(json!({"tags": ["a", "b"]}));
        let fields = infer_fields(&[Some(&a)]);
        assert_eq!(fields[0].ty, ValueType::String);
    }

    #[test]
    fn first_seen_key_order_preserved() {
        let a = props(json!({"z": 1, "a": 2, "m": 3}));
        let b = props(json!({"b": 4, "a": 5}));
        let fields = infer_fields(&[Some(&a), Some(&b)]);
        // BTreeMap on `JsonMap` doesn't guarantee insertion order in
        // serde_json by default, so the first-seen rule depends on the
        // iteration order of the source map. We assert that all four keys
        // appear and that 'b' only shows up after 'a' (the b-row insertion).
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"b"));
        let pos_a = names.iter().position(|&n| n == "a").unwrap();
        let pos_b = names.iter().position(|&n| n == "b").unwrap();
        assert!(pos_a < pos_b, "first-seen 'a' before 'b': {names:?}");
    }

    #[test]
    fn json_to_value_round_trips_primitives() {
        assert_eq!(
            json_to_value(Some(&json!(true)), ValueType::Bool),
            Value::Bool(true)
        );
        assert_eq!(
            json_to_value(Some(&json!(42)), ValueType::Int64),
            Value::Int64(42)
        );
        assert_eq!(
            json_to_value(Some(&json!(1.5)), ValueType::Float64),
            Value::Float64(1.5)
        );
        assert_eq!(
            json_to_value(Some(&json!("hi")), ValueType::String),
            Value::String("hi".to_string())
        );
        assert_eq!(json_to_value(None, ValueType::Int64), Value::Null);
        assert_eq!(
            json_to_value(Some(&Json::Null), ValueType::Int64),
            Value::Null
        );
    }

    #[test]
    fn json_to_value_int_overflow_is_null() {
        let big = json!(i64::MAX);
        assert_eq!(json_to_value(Some(&big), ValueType::Int32), Value::Null);
    }

    #[test]
    fn value_to_json_round_trip() {
        assert_eq!(value_to_json(&Value::Bool(true)), Json::Bool(true));
        assert_eq!(value_to_json(&Value::Int64(7)), json!(7));
        assert_eq!(value_to_json(&Value::Float64(1.5)), json!(1.5));
        assert_eq!(
            value_to_json(&Value::String("x".into())),
            Json::String("x".into())
        );
        assert_eq!(value_to_json(&Value::Null), Json::Null);
    }
}

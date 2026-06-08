//! Attribute values and their type tags.
//!
//! `Value` is the per-cell payload; `ValueType` is the discriminant used in
//! [`crate::FieldDef`] to declare what type a column holds.
//!
//! ## DateTime
//!
//! v0.1 stores datetimes as `f64` **days since 1899-12-30 00:00:00**, matching
//! the raw GDB encoding. This is lossless and zero-dep. Writers convert to
//! their preferred representation (ISO 8601, Arrow Timestamp, …) at write time.
//! A strong-typed `DateTime` value is on the roadmap for v0.2.

/// One attribute value. Marked `#[non_exhaustive]` because future versions
/// may add variants (e.g. `Date` / `Time` / `DateTimeOffset` once we move
/// off the f64-days representation, or `Json` / `Decimal` for richer schemas)
/// without that counting as a SemVer-breaking change.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Null,
    Bool(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float64(f64),
    String(String),
    Binary(Vec<u8>),
    /// Days since 1899-12-30 00:00:00 (Esri convention). See module docs.
    DateTime(f64),
    /// 16-byte UUID, raw bytes (no string formatting).
    Guid([u8; 16]),
    /// XML payload, stored verbatim.
    Xml(String),
}

/// Type discriminant for [`Value`]. Used by [`crate::FieldDef`] to describe
/// schema without binding a per-cell value. Tracks [`Value`] one-for-one;
/// the same `#[non_exhaustive]` rationale applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValueType {
    Bool,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    String,
    Binary,
    DateTime,
    Guid,
    Xml,
}

impl Value {
    /// Returns `None` for `Value::Null`, otherwise the type of the variant.
    pub fn ty(&self) -> Option<ValueType> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(_) => ValueType::Bool,
            Value::Int16(_) => ValueType::Int16,
            Value::Int32(_) => ValueType::Int32,
            Value::Int64(_) => ValueType::Int64,
            Value::Float32(_) => ValueType::Float32,
            Value::Float64(_) => ValueType::Float64,
            Value::String(_) => ValueType::String,
            Value::Binary(_) => ValueType::Binary,
            Value::DateTime(_) => ValueType::DateTime,
            Value::Guid(_) => ValueType::Guid,
            Value::Xml(_) => ValueType::Xml,
        })
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ty_returns_none_for_null() {
        assert_eq!(Value::Null.ty(), None);
        assert!(Value::Null.is_null());
    }

    #[test]
    fn ty_matches_variant() {
        assert_eq!(Value::Bool(true).ty(), Some(ValueType::Bool));
        assert_eq!(Value::Int16(1).ty(), Some(ValueType::Int16));
        assert_eq!(Value::Int32(1).ty(), Some(ValueType::Int32));
        assert_eq!(Value::Int64(1).ty(), Some(ValueType::Int64));
        assert_eq!(Value::Float32(1.0).ty(), Some(ValueType::Float32));
        assert_eq!(Value::Float64(1.0).ty(), Some(ValueType::Float64));
        assert_eq!(Value::String("x".into()).ty(), Some(ValueType::String));
        assert_eq!(Value::Binary(vec![]).ty(), Some(ValueType::Binary));
        assert_eq!(Value::DateTime(0.0).ty(), Some(ValueType::DateTime));
        assert_eq!(Value::Guid([0; 16]).ty(), Some(ValueType::Guid));
        assert_eq!(Value::Xml("<x/>".into()).ty(), Some(ValueType::Xml));
        assert!(!Value::Int32(1).is_null());
    }
}

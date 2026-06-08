//! dBASE III+/IV table reader for `.dbf` attribute sidecars.
//!
//! ## Layout (header)
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 0 | Version flags |
//! | 1..4 | Last-update YY MM DD |
//! | 4..8 | Number of records (u32 LE) |
//! | 8..10 | Header length in bytes (u16 LE) |
//! | 10..12 | Record length in bytes (u16 LE) |
//! | 29 | Language Driver ID (codepage hint) |
//!
//! Then `(header_length - 32 - 1) / 32` field descriptors of 32 bytes each,
//! terminated by `0x0D`. Each record starts with a 1-byte deletion flag
//! (`0x20` active, `0x2A` deleted) and is followed by fixed-width field
//! values.
//!
//! ## v0.1 field type coverage
//!
//! - `C` Character — UTF-8/ASCII trimmed, → `Value::String`
//! - `N` Numeric (ASCII digits) — parsed to `Value::Int64` if no decimals,
//!   else `Value::Float64`
//! - `F` Float (ASCII) — `Value::Float64`
//! - `D` Date (YYYYMMDD) — `Value::DateTime` (days since 1899-12-30)
//! - `L` Logical (T/F/Y/N/?) — `Value::Bool`, blank or `?` → `Value::Null`
//! - Anything else (`M` memo, `B`, `G`, `OLE`, …) → `Value::Null`
//!
//! ## Encoding
//!
//! v0.1 assumes ASCII / UTF-8 input. Real `.cpg` / LDID handling is
//! deferred to v0.2 — until then non-UTF-8 strings come through with
//! invalid bytes replaced.

use geonative_core::{Crs, FieldDef, GeomField, Schema, Value, ValueType};

use crate::error::{Result, ShpError};

#[derive(Debug, Clone)]
pub struct DbfHeader {
    pub n_records: u32,
    pub header_len: u16,
    pub record_len: u16,
    pub fields: Vec<DbfField>,
}

#[derive(Debug, Clone)]
pub struct DbfField {
    pub name: String,
    pub kind: u8, // ASCII type char
    pub length: u8,
    pub decimals: u8,
    /// Byte offset of this field within a record (after the 1-byte
    /// deletion flag).
    pub offset_in_record: usize,
}

pub fn parse_header(bytes: &[u8]) -> Result<DbfHeader> {
    if bytes.len() < 32 {
        return Err(ShpError::malformed("dbf shorter than 32-byte header"));
    }
    let n_records = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let header_len = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let record_len = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
    if (header_len as usize) > bytes.len() {
        return Err(ShpError::malformed(format!(
            "dbf header_len {header_len} > file size {}",
            bytes.len()
        )));
    }

    let mut fields = Vec::new();
    let mut pos = 32usize;
    let mut field_offset = 1usize; // after the 1-byte deletion flag
    while pos < header_len as usize && bytes.get(pos) != Some(&0x0D) {
        if pos + 32 > bytes.len() {
            return Err(ShpError::malformed("dbf field descriptor truncated"));
        }
        let name_bytes = &bytes[pos..pos + 11];
        let name_end = name_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_bytes.len());
        let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();
        let kind = bytes[pos + 11];
        let length = bytes[pos + 16];
        let decimals = bytes[pos + 17];
        fields.push(DbfField {
            name,
            kind,
            length,
            decimals,
            offset_in_record: field_offset,
        });
        field_offset += length as usize;
        pos += 32;
    }

    Ok(DbfHeader {
        n_records,
        header_len,
        record_len,
        fields,
    })
}

/// Map a DBF field to a core `FieldDef`.
pub fn field_to_def(f: &DbfField) -> FieldDef {
    let ty = match f.kind {
        b'C' => ValueType::String,
        b'N' if f.decimals == 0 => ValueType::Int64,
        b'N' | b'F' => ValueType::Float64,
        b'D' => ValueType::DateTime,
        b'L' => ValueType::Bool,
        _ => ValueType::String, // memo / binary / unsupported → represent as text
    };
    FieldDef::new(f.name.clone(), ty, true).with_width(f.length as u32)
}

/// Build a `Schema` from the DBF header + a geometry-column descriptor for
/// the companion `.shp`.
pub fn build_schema(header: &DbfHeader, geom: GeomField, crs: Crs) -> Schema {
    let fields = header.fields.iter().map(field_to_def).collect();
    Schema::new(fields, Some(geom), crs)
}

/// Decode the value of a single field within a record. `record_bytes`
/// includes the 1-byte deletion flag at index 0.
pub fn decode_field(record_bytes: &[u8], field: &DbfField) -> Value {
    let start = field.offset_in_record;
    let end = start + field.length as usize;
    if end > record_bytes.len() {
        return Value::Null;
    }
    let raw = &record_bytes[start..end];
    let trimmed = trim_ascii(raw);

    match field.kind {
        b'C' => {
            if trimmed.is_empty() {
                Value::Null
            } else {
                Value::String(String::from_utf8_lossy(trimmed).into_owned())
            }
        }
        b'N' => {
            let s = std::str::from_utf8(trimmed).unwrap_or("").trim();
            if s.is_empty() {
                return Value::Null;
            }
            if field.decimals == 0 {
                s.parse::<i64>()
                    .ok()
                    .map(Value::Int64)
                    .unwrap_or(Value::Null)
            } else {
                s.parse::<f64>()
                    .ok()
                    .map(Value::Float64)
                    .unwrap_or(Value::Null)
            }
        }
        b'F' => {
            let s = std::str::from_utf8(trimmed).unwrap_or("").trim();
            if s.is_empty() {
                return Value::Null;
            }
            s.parse::<f64>()
                .ok()
                .map(Value::Float64)
                .unwrap_or(Value::Null)
        }
        b'D' => {
            // 8-byte YYYYMMDD
            if trimmed.len() != 8 {
                return Value::Null;
            }
            let s = std::str::from_utf8(trimmed).unwrap_or("");
            if s.chars().all(|c| c == ' ') || s.is_empty() {
                return Value::Null;
            }
            let y: i32 = s[0..4].parse().unwrap_or(0);
            let m: u32 = s[4..6].parse().unwrap_or(0);
            let d: u32 = s[6..8].parse().unwrap_or(0);
            if y == 0 && m == 0 && d == 0 {
                return Value::Null;
            }
            Value::DateTime(ymd_to_gdb_days(y, m, d))
        }
        b'L' => match trimmed.first() {
            Some(b'T') | Some(b't') | Some(b'Y') | Some(b'y') => Value::Bool(true),
            Some(b'F') | Some(b'f') | Some(b'N') | Some(b'n') => Value::Bool(false),
            _ => Value::Null,
        },
        _ => {
            // Memo / binary / unsupported: surface as raw text or null.
            if trimmed.is_empty() {
                Value::Null
            } else {
                Value::String(String::from_utf8_lossy(trimmed).into_owned())
            }
        }
    }
}

/// Trim ASCII spaces from both ends (matches dBASE padding convention).
fn trim_ascii(b: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = b.len();
    while start < end && b[start] == b' ' {
        start += 1;
    }
    while end > start && b[end - 1] == b' ' {
        end -= 1;
    }
    &b[start..end]
}

/// Convert a calendar date to "days since 1899-12-30 00:00:00" (the same
/// convention used by `geonative_core::Value::DateTime`). Pure integer math.
fn ymd_to_gdb_days(year: i32, month: u32, day: u32) -> f64 {
    // Gregorian → days since epoch, then offset.
    // Use a simple Julian-style calculation.
    let (y, m) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };
    let a = (y as i64) / 100;
    let b = 2 - a + a / 4;
    let jdn = (365.25 * (y as i64 + 4716) as f64) as i64
        + (30.6001 * (m as i64 + 1) as f64) as i64
        + day as i64
        + b
        - 1524;
    // JDN of 1899-12-30 = 2415019
    (jdn - 2_415_019) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dbf(fields: &[(&str, u8, u8, u8)], records: &[&[u8]]) -> Vec<u8> {
        let n_records = records.len() as u32;
        let header_len = (32 + fields.len() * 32 + 1) as u16;
        let record_len: u16 = 1 + fields.iter().map(|f| f.2 as u16).sum::<u16>();
        let mut buf = vec![0u8; 32];
        buf[0] = 0x03;
        buf[4..8].copy_from_slice(&n_records.to_le_bytes());
        buf[8..10].copy_from_slice(&header_len.to_le_bytes());
        buf[10..12].copy_from_slice(&record_len.to_le_bytes());
        for (name, kind, length, decimals) in fields {
            let mut desc = [0u8; 32];
            let name_bytes = name.as_bytes();
            desc[..name_bytes.len()].copy_from_slice(name_bytes);
            desc[11] = *kind;
            desc[16] = *length;
            desc[17] = *decimals;
            buf.extend_from_slice(&desc);
        }
        buf.push(0x0D);
        for r in records {
            assert_eq!(r.len(), record_len as usize, "test record length mismatch");
            buf.extend_from_slice(r);
        }
        buf.push(0x1A);
        buf
    }

    #[test]
    fn parse_simple_header() {
        // ID width 10 + NAME width 8 + 1 deletion flag = 19-byte records.
        let dbf = make_dbf(
            &[("ID", b'N', 10, 0), ("NAME", b'C', 8, 0)],
            &[b" 0000000001Alice   "],
        );
        let h = parse_header(&dbf).unwrap();
        assert_eq!(h.n_records, 1);
        assert_eq!(h.fields.len(), 2);
        assert_eq!(h.fields[0].name, "ID");
        assert_eq!(h.fields[1].name, "NAME");
    }

    #[test]
    fn decode_integer_string_and_bool() {
        // 1 (deletion flag ' ') + 5 (ID " 0042") + 5 (NAME "Alice") + 1 (OK "T") = 12.
        let dbf = make_dbf(
            &[("ID", b'N', 5, 0), ("NAME", b'C', 5, 0), ("OK", b'L', 1, 0)],
            &[b"  0042AliceT"],
        );
        let h = parse_header(&dbf).unwrap();
        let rec_start = h.header_len as usize;
        let rec = &dbf[rec_start..rec_start + h.record_len as usize];
        assert_eq!(decode_field(rec, &h.fields[0]), Value::Int64(42));
        assert_eq!(
            decode_field(rec, &h.fields[1]),
            Value::String("Alice".into())
        );
        assert_eq!(decode_field(rec, &h.fields[2]), Value::Bool(true));
    }

    #[test]
    fn decode_float_with_decimals() {
        // 1 + 7 = 8 bytes per record.
        let dbf = make_dbf(&[("VAL", b'N', 7, 2)], &[b"   12.34"]);
        let h = parse_header(&dbf).unwrap();
        let rec_start = h.header_len as usize;
        let rec = &dbf[rec_start..rec_start + h.record_len as usize];
        match decode_field(rec, &h.fields[0]) {
            Value::Float64(f) => assert!((f - 12.34).abs() < 1e-9),
            other => panic!("expected float, got {:?}", other),
        }
    }

    #[test]
    fn decode_date_field() {
        let dbf = make_dbf(&[("D", b'D', 8, 0)], &[b" 20240601"]);
        let h = parse_header(&dbf).unwrap();
        let rec_start = h.header_len as usize;
        let rec = &dbf[rec_start..rec_start + h.record_len as usize];
        match decode_field(rec, &h.fields[0]) {
            Value::DateTime(d) => {
                // Round-trip: 2024-06-01 ≈ 45444 GDB days (sanity check, not exact)
                assert!(d > 45000.0 && d < 46000.0, "got {d}");
            }
            other => panic!("expected datetime, got {:?}", other),
        }
    }

    #[test]
    fn blank_numeric_is_null() {
        // 1 + 5 = 6 bytes per record (all spaces = active flag + blank field).
        let dbf = make_dbf(&[("N", b'N', 5, 0)], &[b"      "]);
        let h = parse_header(&dbf).unwrap();
        let rec_start = h.header_len as usize;
        let rec = &dbf[rec_start..rec_start + h.record_len as usize];
        assert_eq!(decode_field(rec, &h.fields[0]), Value::Null);
    }
}

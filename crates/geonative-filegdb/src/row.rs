//! Row decoder. Given a row blob from `.gdbtable` and the parsed
//! [`FieldSection`], produces a [`DecodedRow`] with one value per schema
//! field.
//!
//! Layout reference (verified against GDAL `openfilegdb/filegdbtable.cpp`,
//! `FileGDBTable::GetFieldValue`):
//!
//! 1. **i32 row length** (in the `.gdbtable` file, BEFORE the blob — handled
//!    by the caller; this module operates on the blob itself).
//! 2. **Null bitmap**: `ceil(num_nullable_fields / 8)` bytes. Only fields
//!    with `nullable = true` get a bit. Bit `i` (0-indexed, LSB-first within
//!    each byte) corresponds to the i-th nullable field in schema order. A
//!    set bit means NULL.
//! 3. **Field values** in schema order, only for non-null fields. OBJECTID
//!    consumes ZERO bytes — its value comes from the row's `.gdbtablx` index.
//!    Geometry is varuint-length-prefixed and is kept as a raw `Vec<u8>` for
//!    downstream decoding by the geometry phase.

use geonative_core::Value;

use crate::bytes::LeReader;
use crate::error::{GdbError, Result};
use crate::table::{FieldSection, FieldTypeCode};

/// A decoded row.
#[derive(Debug, Clone)]
pub struct DecodedRow {
    /// Feature ID. Comes from `.gdbtablx` row index + 1 (the OBJECTID field
    /// in the row blob carries no bytes).
    pub fid: i64,
    /// One value per `FieldSection::fields`, in the same order. OBJECTID
    /// slots are filled with `Value::Int64(fid)`. Geometry slots are filled
    /// with `Value::Null` here — the actual geometry is in `geometry_blob`.
    pub values: Vec<Value>,
    /// Raw shape buffer bytes for the geometry field, if any. `None` if the
    /// row has no geometry field or the geometry is null.
    pub geometry_blob: Option<Vec<u8>>,
}

/// Decode a row blob.
///
/// `blob_bytes` is the row payload — **NOT** including the i32 row-length
/// prefix in `.gdbtable`. The caller is responsible for reading that prefix
/// and slicing out the blob.
pub fn decode_row_blob(
    blob_bytes: &[u8],
    fid: i64,
    field_section: &FieldSection,
) -> Result<DecodedRow> {
    let mut r = LeReader::new(blob_bytes);

    // Count nullable fields → null bitmap size in bytes.
    let n_nullable = field_section.fields.iter().filter(|f| f.nullable).count();
    let null_bitmap_bytes = n_nullable.div_ceil(8);
    let null_bitmap: Vec<u8> = if null_bitmap_bytes > 0 {
        r.read_bytes(null_bitmap_bytes)?.to_vec()
    } else {
        Vec::new()
    };

    let strings_utf8 = field_section.flags.strings_utf8();
    let mut values = Vec::with_capacity(field_section.fields.len());
    let mut nullable_idx = 0usize;
    let mut geometry_blob: Option<Vec<u8>> = None;

    for field in &field_section.fields {
        // OBJECTID has no row bytes; value is synthesized from FID.
        if field.ty == FieldTypeCode::ObjectId {
            values.push(Value::Int64(fid));
            continue;
        }

        let is_null = if field.nullable {
            let bit_set = test_bit(&null_bitmap, nullable_idx);
            nullable_idx += 1;
            bit_set
        } else {
            false
        };

        if is_null {
            values.push(Value::Null);
            continue;
        }

        let v = match field.ty {
            FieldTypeCode::ObjectId => unreachable!("handled above"),

            FieldTypeCode::Int16 => Value::Int16(r.read_i16()?),
            FieldTypeCode::Int32 => Value::Int32(r.read_i32()?),
            FieldTypeCode::Int64 => Value::Int64(r.read_i64()?),
            FieldTypeCode::Float32 => Value::Float32(r.read_f32()?),
            FieldTypeCode::Float64 => Value::Float64(r.read_f64()?),

            FieldTypeCode::DateTime
            | FieldTypeCode::DateOnly
            | FieldTypeCode::TimeOnly => Value::DateTime(r.read_f64()?),

            FieldTypeCode::DateTimeWithOffset => {
                // Per GDAL: f64 days + i16 offset minutes. v0.1 IR doesn't
                // carry the offset; drop it. (v0.2 may extend Value with an
                // OffsetDateTime variant.)
                let days = r.read_f64()?;
                let _offset_minutes = r.read_i16()?;
                Value::DateTime(days)
            }

            FieldTypeCode::String => {
                let n = r.read_varuint()? as usize;
                if strings_utf8 {
                    Value::String(r.read_utf8(n)?)
                } else {
                    if n % 2 != 0 {
                        return Err(GdbError::malformed(format!(
                            "UTF-16 string byte length {n} is odd"
                        )));
                    }
                    Value::String(r.read_utf16_le(n / 2)?)
                }
            }

            // XML is always UTF-8 per GDAL, independent of the layer flag.
            FieldTypeCode::Xml => {
                let n = r.read_varuint()? as usize;
                Value::Xml(r.read_utf8(n)?)
            }

            FieldTypeCode::Binary => {
                let n = r.read_varuint()? as usize;
                Value::Binary(r.read_bytes(n)?.to_vec())
            }

            FieldTypeCode::Guid | FieldTypeCode::GlobalId => {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(r.read_bytes(16)?);
                Value::Guid(buf)
            }

            FieldTypeCode::Geometry => {
                let n = r.read_varuint()? as usize;
                geometry_blob = Some(r.read_bytes(n)?.to_vec());
                Value::Null // geometry decoded in a later phase
            }

            FieldTypeCode::Raster => {
                return Err(GdbError::unsupported(
                    "raster fields are not yet supported (v0.1)",
                ));
            }
        };

        values.push(v);
    }

    Ok(DecodedRow {
        fid,
        values,
        geometry_blob,
    })
}

#[inline]
fn test_bit(bitmap: &[u8], idx: usize) -> bool {
    bitmap.get(idx / 8).copied().unwrap_or(0) & (1u8 << (idx % 8)) != 0
}

/// Helper: read the i32 row length prefix in `.gdbtable` at `offset`, then
/// return the slice of `blob_len` bytes that constitute the row blob.
pub fn slice_row_blob(table_bytes: &[u8], offset: u64) -> Result<&[u8]> {
    let off = offset as usize;
    if off + 4 > table_bytes.len() {
        return Err(GdbError::malformed(format!(
            "row offset {off} + 4-byte length header exceeds .gdbtable size {}",
            table_bytes.len()
        )));
    }
    let len_bytes = &table_bytes[off..off + 4];
    let blob_len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    if blob_len <= 0 {
        return Err(GdbError::malformed(format!(
            "row at offset {off}: non-positive blob length {blob_len}"
        )));
    }
    let start = off + 4;
    let end = start + blob_len as usize;
    if end > table_bytes.len() {
        return Err(GdbError::malformed(format!(
            "row at offset {off}: blob length {blob_len} runs past .gdbtable end"
        )));
    }
    Ok(&table_bytes[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Field, FieldSection, FieldTypeCode, LayerFlags};

    fn fields(field_specs: &[(&str, FieldTypeCode, bool)]) -> FieldSection {
        let fields: Vec<Field> = field_specs
            .iter()
            .map(|(n, ty, nullable)| Field {
                name: n.to_string(),
                alias: None,
                ty: *ty,
                nullable: *nullable,
                width: None,
                default_raw: Vec::new(),
                geometry: None,
            })
            .collect();
        FieldSection {
            fields,
            flags: LayerFlags(0x100), // strings UTF-8
            format_version: 4,
        }
    }

    #[test]
    fn decode_objectid_only() {
        let fs = fields(&[("OID", FieldTypeCode::ObjectId, false)]);
        let blob = Vec::new(); // no nullable fields → no bitmap; no data either
        let row = decode_row_blob(&blob, 42, &fs).unwrap();
        assert_eq!(row.values, vec![Value::Int64(42)]);
        assert_eq!(row.fid, 42);
    }

    #[test]
    fn decode_objectid_and_two_int32_no_null() {
        let fs = fields(&[
            ("OID", FieldTypeCode::ObjectId, false),
            ("A", FieldTypeCode::Int32, false),
            ("B", FieldTypeCode::Int32, false),
        ]);
        // null bitmap = 0 bytes (no nullable fields); then 2 i32s
        let mut blob = Vec::new();
        blob.extend_from_slice(&111i32.to_le_bytes());
        blob.extend_from_slice(&222i32.to_le_bytes());
        let row = decode_row_blob(&blob, 7, &fs).unwrap();
        assert_eq!(
            row.values,
            vec![Value::Int64(7), Value::Int32(111), Value::Int32(222)]
        );
    }

    #[test]
    fn decode_nullable_field_present_and_absent() {
        let fs = fields(&[
            ("OID", FieldTypeCode::ObjectId, false),
            ("A", FieldTypeCode::Int32, true),
            ("B", FieldTypeCode::Int32, true),
        ]);
        // 2 nullable fields → 1 byte bitmap.
        // bit 0 = A, bit 1 = B. Set bit 1 → B is null; A is present.
        let mut blob = vec![0b00000010u8];
        blob.extend_from_slice(&999i32.to_le_bytes()); // A
        // (no bytes for B, it's null)
        let row = decode_row_blob(&blob, 1, &fs).unwrap();
        assert_eq!(
            row.values,
            vec![Value::Int64(1), Value::Int32(999), Value::Null]
        );
    }

    #[test]
    fn decode_string_utf8() {
        let fs = fields(&[("S", FieldTypeCode::String, false)]);
        let mut blob = Vec::new();
        // No null bitmap (no nullable fields).
        // varuint length = 5, then "hello"
        blob.push(5);
        blob.extend_from_slice(b"hello");
        let row = decode_row_blob(&blob, 1, &fs).unwrap();
        assert_eq!(row.values, vec![Value::String("hello".to_string())]);
    }

    #[test]
    fn decode_string_utf16() {
        let mut fs = fields(&[("S", FieldTypeCode::String, false)]);
        fs.flags = LayerFlags(0); // bit 8 clear → UTF-16
        let mut blob = Vec::new();
        // 4 bytes of UTF-16 = "Hi" (2 chars × 2 bytes)
        blob.push(4); // varuint byte length = 4
        blob.extend_from_slice(&[0x48, 0x00, 0x69, 0x00]);
        let row = decode_row_blob(&blob, 1, &fs).unwrap();
        assert_eq!(row.values, vec![Value::String("Hi".to_string())]);
    }

    #[test]
    fn decode_geometry_blob_captured_separately() {
        let fs = fields(&[
            ("OID", FieldTypeCode::ObjectId, false),
            ("SHAPE", FieldTypeCode::Geometry, true),
            ("Tail", FieldTypeCode::Int32, false),
        ]);
        // 1 nullable field (SHAPE) → 1 byte bitmap. Bit 0 = SHAPE; bit clear → present.
        let mut blob = vec![0b00000000u8];
        // SHAPE: varuint length 3 + 3 bytes
        blob.push(3);
        blob.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        // Tail Int32
        blob.extend_from_slice(&77i32.to_le_bytes());
        let row = decode_row_blob(&blob, 9, &fs).unwrap();
        assert_eq!(
            row.values,
            vec![Value::Int64(9), Value::Null, Value::Int32(77)]
        );
        assert_eq!(row.geometry_blob.as_deref(), Some(&[0xAA, 0xBB, 0xCC][..]));
    }

    #[test]
    fn test_bit_lsb_first() {
        // byte 0b00000101 → bits 0 and 2 set
        let bm = [0b00000101u8];
        assert!(test_bit(&bm, 0));
        assert!(!test_bit(&bm, 1));
        assert!(test_bit(&bm, 2));
        assert!(!test_bit(&bm, 3));

        // crossing byte boundary
        let bm = [0x00, 0b00000010u8];
        assert!(!test_bit(&bm, 0));
        assert!(!test_bit(&bm, 8));
        assert!(test_bit(&bm, 9));
    }

    #[test]
    fn slice_row_blob_extracts_correct_range() {
        // Synthesize a .gdbtable fragment with a single row at offset 100
        let mut table = vec![0u8; 100];
        // i32 row length = 5
        table.extend_from_slice(&5i32.to_le_bytes());
        // 5 bytes of blob
        table.extend_from_slice(b"hello");

        let blob = slice_row_blob(&table, 100).unwrap();
        assert_eq!(blob, b"hello");
    }
}

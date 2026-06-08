//! Protobuf wire-format primitives — varint, zigzag, field tags,
//! length-delimited values.
//!
//! Spec: <https://protobuf.dev/programming-guides/encoding/>. We only
//! implement the wire types MVT 2.1 actually uses (varint, length-delimited).

/// Encode a `u64` as a protobuf base-128 varint.
pub fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// Zigzag-encode a signed i64 to its u64 wire representation.
/// `0 → 0`, `-1 → 1`, `1 → 2`, `-2 → 3`, `2 → 4`, …
pub fn zigzag_encode(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

/// Encode a protobuf field-tag varint: `(field_number << 3) | wire_type`.
pub fn write_tag(buf: &mut Vec<u8>, field_number: u32, wire_type: u8) {
    debug_assert!(wire_type <= 5);
    write_varint(buf, ((field_number as u64) << 3) | (wire_type as u64));
}

// Wire-type constants per the protobuf spec.
pub const WIRE_VARINT: u8 = 0;
pub const WIRE_64BIT: u8 = 1;
pub const WIRE_LEN_DELIM: u8 = 2;
pub const WIRE_32BIT: u8 = 5;

/// Write a varint field (`tag` + varint value).
pub fn write_varint_field(buf: &mut Vec<u8>, field_number: u32, value: u64) {
    write_tag(buf, field_number, WIRE_VARINT);
    write_varint(buf, value);
}

/// Write a `string` field (tag + length-prefixed UTF-8 bytes).
pub fn write_string_field(buf: &mut Vec<u8>, field_number: u32, s: &str) {
    write_tag(buf, field_number, WIRE_LEN_DELIM);
    write_varint(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

/// Write a `bytes` field.
pub fn write_bytes_field(buf: &mut Vec<u8>, field_number: u32, b: &[u8]) {
    write_tag(buf, field_number, WIRE_LEN_DELIM);
    write_varint(buf, b.len() as u64);
    buf.extend_from_slice(b);
}

/// Write a `float` field (32-bit, IEEE 754, little-endian).
pub fn write_float_field(buf: &mut Vec<u8>, field_number: u32, v: f32) {
    write_tag(buf, field_number, WIRE_32BIT);
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Write a `double` field (64-bit, IEEE 754, little-endian).
pub fn write_double_field(buf: &mut Vec<u8>, field_number: u32, v: f64) {
    write_tag(buf, field_number, WIRE_64BIT);
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Write a length-delimited submessage: tag + varint(length) + raw bytes.
/// Useful when the caller has already produced the inner message body.
pub fn write_message_field(buf: &mut Vec<u8>, field_number: u32, body: &[u8]) {
    write_tag(buf, field_number, WIRE_LEN_DELIM);
    write_varint(buf, body.len() as u64);
    buf.extend_from_slice(body);
}

/// Write a packed-repeated field (length-delimited region of back-to-back varints).
pub fn write_packed_varint_field(buf: &mut Vec<u8>, field_number: u32, values: &[u32]) {
    if values.is_empty() {
        return; // emit nothing for empty repeated fields
    }
    write_tag(buf, field_number, WIRE_LEN_DELIM);
    // Compute payload length first.
    let mut payload_len = 0u64;
    for v in values {
        payload_len += varint_len(*v as u64);
    }
    write_varint(buf, payload_len);
    for v in values {
        write_varint(buf, *v as u64);
    }
}

/// Number of bytes a u64 takes when varint-encoded.
pub fn varint_len(mut v: u64) -> u64 {
    let mut n = 1u64;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_low_values() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 0);
        assert_eq!(buf, vec![0x00]);
        buf.clear();
        write_varint(&mut buf, 1);
        assert_eq!(buf, vec![0x01]);
        buf.clear();
        write_varint(&mut buf, 127);
        assert_eq!(buf, vec![0x7F]);
        buf.clear();
        write_varint(&mut buf, 128);
        assert_eq!(buf, vec![0x80, 0x01]);
        buf.clear();
        write_varint(&mut buf, 300);
        assert_eq!(buf, vec![0xAC, 0x02]);
    }

    #[test]
    fn zigzag_values() {
        assert_eq!(zigzag_encode(0), 0);
        assert_eq!(zigzag_encode(-1), 1);
        assert_eq!(zigzag_encode(1), 2);
        assert_eq!(zigzag_encode(-2), 3);
        assert_eq!(zigzag_encode(2), 4);
        assert_eq!(zigzag_encode(2147483647), 4294967294);
        assert_eq!(zigzag_encode(-2147483648), 4294967295);
    }

    #[test]
    fn tag_encoding() {
        let mut buf = Vec::new();
        // field_number=1, wire_type=2 → (1 << 3) | 2 = 10 = 0x0A
        write_tag(&mut buf, 1, WIRE_LEN_DELIM);
        assert_eq!(buf, vec![0x0A]);
        buf.clear();
        // field_number=15, wire_type=0 → (15 << 3) | 0 = 120 = 0x78
        write_tag(&mut buf, 15, WIRE_VARINT);
        assert_eq!(buf, vec![0x78]);
    }

    #[test]
    fn string_field_layout() {
        let mut buf = Vec::new();
        write_string_field(&mut buf, 1, "abc");
        // tag(1,LEN) = 0x0A, len(3) = 0x03, "abc"
        assert_eq!(buf, vec![0x0A, 0x03, b'a', b'b', b'c']);
    }

    #[test]
    fn packed_varint_field_skipped_when_empty() {
        let mut buf = Vec::new();
        write_packed_varint_field(&mut buf, 2, &[]);
        assert!(buf.is_empty());
    }

    #[test]
    fn packed_varint_field_emits_payload() {
        let mut buf = Vec::new();
        write_packed_varint_field(&mut buf, 4, &[1, 200, 300]);
        // tag(4,LEN)=0x22, payload_len=1+2+2=5, then 0x01 0xC8 0x01 0xAC 0x02
        assert_eq!(buf, vec![0x22, 0x05, 0x01, 0xC8, 0x01, 0xAC, 0x02]);
    }

    #[test]
    fn varint_len_matches_actual_emission() {
        for v in [0u64, 1, 127, 128, 16383, 16384, 1 << 35, u64::MAX] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            assert_eq!(buf.len() as u64, varint_len(v));
        }
    }
}

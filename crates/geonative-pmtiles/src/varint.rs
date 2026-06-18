//! Protobuf-style LEB128 varint codec.
//!
//! PMTiles directory entries are sequences of varints. Each varint
//! encodes a u64 in little-endian base-128 with the high bit set on every
//! continuation byte. The format is identical to protobuf's `int64`/`uint64`
//! wire encoding.

use crate::error::{PmtilesError, Result};

/// Append a varint-encoded `value` to `buf`.
pub fn write_u64(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// Decode one varint from `bytes`. Returns `(value, bytes_read)`.
pub fn read_u64(bytes: &[u8]) -> Result<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    // u64 needs at most ⌈64/7⌉ = 10 bytes
    for (i, &b) in bytes.iter().enumerate().take(10) {
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok((result, i + 1));
        }
        shift = shift.saturating_add(7);
    }
    Err(PmtilesError::malformed(
        "varint exceeds 10 bytes (overflow or unterminated)",
    ))
}

/// Convenience wrapper: decode and advance an offset cursor in one step.
pub fn read_u64_at(bytes: &[u8], offset: &mut usize) -> Result<u64> {
    if *offset >= bytes.len() {
        return Err(PmtilesError::malformed("varint read past end of buffer"));
    }
    let (v, n) = read_u64(&bytes[*offset..])?;
    *offset += n;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small_values() {
        for v in [0u64, 1, 127, 128, 255, 256, 16383, 16384, u32::MAX as u64] {
            let mut buf = Vec::new();
            write_u64(&mut buf, v);
            let (decoded, n) = read_u64(&buf).unwrap();
            assert_eq!(decoded, v);
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn roundtrip_u64_max() {
        let mut buf = Vec::new();
        write_u64(&mut buf, u64::MAX);
        assert_eq!(buf.len(), 10);
        let (decoded, n) = read_u64(&buf).unwrap();
        assert_eq!(decoded, u64::MAX);
        assert_eq!(n, 10);
    }

    #[test]
    fn read_advances_cursor() {
        let mut buf = Vec::new();
        write_u64(&mut buf, 42);
        write_u64(&mut buf, 1_000_000);
        write_u64(&mut buf, 7);
        let mut off = 0;
        assert_eq!(read_u64_at(&buf, &mut off).unwrap(), 42);
        assert_eq!(read_u64_at(&buf, &mut off).unwrap(), 1_000_000);
        assert_eq!(read_u64_at(&buf, &mut off).unwrap(), 7);
        assert_eq!(off, buf.len());
    }

    #[test]
    fn unterminated_varint_errors() {
        // 10 continuation-bit bytes with no terminator
        let buf = vec![0xff; 11];
        assert!(read_u64(&buf).is_err());
    }
}

//! Low-level byte primitives for parsing FileGDB binary structures.
//!
//! The reader is a zero-copy cursor over `&[u8]`. All integer types are
//! little-endian (FileGDB is exclusively LE).
//!
//! ## Custom integer widths
//!
//! `.gdbtablx` stores row offsets as 4, 5, or 6-byte little-endian unsigned
//! integers depending on file size. We expose [`read_uint_le`] for variable
//! width and convenience [`read_uint40_le`] / [`read_uint48_le`] wrappers.
//!
//! ## Varuint / varint
//!
//! FileGDB uses two base-128 encodings inside the geometry shape buffer:
//!
//! - **varuint**: standard LEB128 — each byte's low 7 bits contribute to the
//!   value; MSB 0x80 is a continuation flag.
//! - **varint** (signed): same continuation rule, but **bit 0x40 of the first
//!   byte is the sign bit**. The first byte contributes 6 magnitude bits
//!   (`b0 & 0x3F`), subsequent bytes contribute 7 each. The sign bit is
//!   applied to the assembled magnitude.
//!
//! These are the encodings documented in the rouault/dump_gdbtable FGDB-Spec.

use crate::error::{GdbError, Result};

/// Zero-copy little-endian cursor over a byte slice.
#[derive(Debug, Clone)]
pub struct LeReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> LeReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.buf.len() {
            return Err(GdbError::Eof { pos: self.buf.len(), need: pos - self.buf.len() });
        }
        self.pos = pos;
        Ok(())
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.seek(self.pos.saturating_add(n))
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            Err(GdbError::Eof { pos: self.pos, need: n })
        } else {
            Ok(())
        }
    }

    pub fn peek_u8(&self) -> Result<u8> {
        self.need(1)?;
        Ok(self.buf[self.pos])
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let s = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let s = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        let s = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }

    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_u32()? as i32)
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        Ok(self.read_u64()? as i64)
    }

    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    /// Read an unsigned little-endian integer of `width_bytes` (1..=8).
    /// Used for the variable-width row offsets in `.gdbtablx`.
    pub fn read_uint_le(&mut self, width_bytes: usize) -> Result<u64> {
        if width_bytes == 0 || width_bytes > 8 {
            return Err(GdbError::malformed(format!(
                "read_uint_le: invalid width {width_bytes}"
            )));
        }
        let s = self.read_bytes(width_bytes)?;
        let mut v: u64 = 0;
        for (i, b) in s.iter().enumerate() {
            v |= (*b as u64) << (i * 8);
        }
        Ok(v)
    }

    pub fn read_uint40_le(&mut self) -> Result<u64> {
        self.read_uint_le(5)
    }

    pub fn read_uint48_le(&mut self) -> Result<u64> {
        self.read_uint_le(6)
    }

    /// Read a base-128 LEB128 unsigned varint.
    pub fn read_varuint(&mut self) -> Result<u64> {
        let mut value: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let start_pos = self.pos;
            let b = self.read_u8()?;
            value |= ((b & 0x7F) as u64).checked_shl(shift).ok_or(
                GdbError::VarintOverflow { pos: start_pos },
            )?;
            if b & 0x80 == 0 {
                return Ok(value);
            }
            shift = shift.checked_add(7).ok_or(GdbError::VarintOverflow { pos: start_pos })?;
            if shift >= 64 {
                return Err(GdbError::VarintOverflow { pos: start_pos });
            }
        }
    }

    /// Read a base-128 signed varint where the **first byte's bit 0x40 is the
    /// sign bit** (the FileGDB convention, not zigzag).
    pub fn read_varint(&mut self) -> Result<i64> {
        let start_pos = self.pos;
        let b0 = self.read_u8()?;
        let sign = (b0 & 0x40) != 0;
        let mut value: u64 = (b0 & 0x3F) as u64;
        if (b0 & 0x80) == 0 {
            return Ok(apply_sign(value, sign));
        }
        let mut shift: u32 = 6;
        loop {
            let b = self.read_u8()?;
            value |= ((b & 0x7F) as u64).checked_shl(shift).ok_or(
                GdbError::VarintOverflow { pos: start_pos },
            )?;
            if (b & 0x80) == 0 {
                return Ok(apply_sign(value, sign));
            }
            shift = shift.checked_add(7).ok_or(GdbError::VarintOverflow { pos: start_pos })?;
            if shift >= 64 {
                return Err(GdbError::VarintOverflow { pos: start_pos });
            }
        }
    }

    /// Read `n_chars` UTF-16-LE code units and decode to `String`.
    /// Returns [`GdbError::InvalidUtf16`] on malformed surrogate pairs.
    pub fn read_utf16_le(&mut self, n_chars: usize) -> Result<String> {
        let pos = self.pos;
        let bytes = self.read_bytes(n_chars * 2)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).map_err(|_| GdbError::InvalidUtf16 { pos })
    }

    /// Read exactly `n` bytes as UTF-8.
    pub fn read_utf8(&mut self, n: usize) -> Result<String> {
        let pos = self.pos;
        let bytes = self.read_bytes(n)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| GdbError::malformed(format!("invalid UTF-8 at pos {pos}")))
    }
}

#[inline]
fn apply_sign(magnitude: u64, sign: bool) -> i64 {
    if sign {
        // Wrap to handle i64::MIN cleanly; magnitudes that large don't occur
        // in valid FileGDB data, so this is defensive rather than expected.
        (magnitude as i64).wrapping_neg()
    } else {
        magnitude as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_position_and_skip() {
        let mut r = LeReader::new(b"\x01\x02\x03\x04");
        assert_eq!(r.position(), 0);
        assert_eq!(r.remaining(), 4);
        assert_eq!(r.read_u8().unwrap(), 0x01);
        assert_eq!(r.position(), 1);
        r.skip(2).unwrap();
        assert_eq!(r.position(), 3);
        assert_eq!(r.read_u8().unwrap(), 0x04);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn eof_is_reported() {
        let mut r = LeReader::new(b"\x01");
        r.read_u8().unwrap();
        assert!(matches!(r.read_u8(), Err(GdbError::Eof { .. })));
    }

    #[test]
    fn le_integers() {
        let buf = [
            0x01u8, 0x00, // u16 = 1
            0xFF, 0xFF, 0xFF, 0xFF, // i32 = -1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F, // f64 = 1.0
        ];
        let mut r = LeReader::new(&buf);
        assert_eq!(r.read_u16().unwrap(), 1);
        assert_eq!(r.read_i32().unwrap(), -1);
        assert_eq!(r.read_f64().unwrap(), 1.0);
    }

    #[test]
    fn varuint_single_byte() {
        // 0x00 -> 0, 0x7F -> 127
        let mut r = LeReader::new(&[0x00, 0x7F]);
        assert_eq!(r.read_varuint().unwrap(), 0);
        assert_eq!(r.read_varuint().unwrap(), 127);
    }

    #[test]
    fn varuint_multi_byte() {
        // 128 = 0x80 0x01 ; 16384 = 0x80 0x80 0x01
        let mut r = LeReader::new(&[0x80, 0x01, 0x80, 0x80, 0x01]);
        assert_eq!(r.read_varuint().unwrap(), 128);
        assert_eq!(r.read_varuint().unwrap(), 16384);
    }

    #[test]
    fn varuint_overflow_guarded() {
        // ten continuation bytes -> overflow
        let buf = vec![0xFFu8; 11];
        let mut r = LeReader::new(&buf);
        assert!(matches!(r.read_varuint(), Err(GdbError::VarintOverflow { .. })));
    }

    #[test]
    fn varint_single_byte_signs() {
        // 0x00 -> +0; 0x05 -> +5 (low 6 bits, no sign, no continuation)
        // 0x45 -> sign=1, magnitude=5 -> -5
        // 0x3F -> +63 ; 0x7F -> -63
        let mut r = LeReader::new(&[0x00, 0x05, 0x45, 0x3F, 0x7F]);
        assert_eq!(r.read_varint().unwrap(), 0);
        assert_eq!(r.read_varint().unwrap(), 5);
        assert_eq!(r.read_varint().unwrap(), -5);
        assert_eq!(r.read_varint().unwrap(), 63);
        assert_eq!(r.read_varint().unwrap(), -63);
    }

    #[test]
    fn varint_multi_byte() {
        // value 100 = 0b 1100100
        // first byte takes 6 low bits: 0b100100 = 0x24, with continuation -> 0xA4
        // second byte takes next bits: 0b1 << 6 -> 0x01
        // so 100 = 0xA4 0x01
        // -100 = 0xE4 0x01 (set sign bit 0x40 on first byte)
        let mut r = LeReader::new(&[0xA4, 0x01, 0xE4, 0x01]);
        assert_eq!(r.read_varint().unwrap(), 100);
        assert_eq!(r.read_varint().unwrap(), -100);
    }

    #[test]
    fn read_uint_le_widths() {
        // 5-byte: 0x01 0x02 0x03 0x04 0x05 -> 0x0504030201
        let buf = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];
        let mut r = LeReader::new(&buf);
        assert_eq!(r.read_uint_le(5).unwrap(), 0x05_04_03_02_01);
        r.seek(0).unwrap();
        assert_eq!(r.read_uint40_le().unwrap(), 0x05_04_03_02_01);
        r.seek(0).unwrap();
        assert_eq!(r.read_uint48_le().unwrap(), 0x06_05_04_03_02_01);
    }

    #[test]
    fn read_utf16_le_basic() {
        // "Hi" in UTF-16-LE: 0x48 0x00 0x69 0x00
        let mut r = LeReader::new(&[0x48, 0x00, 0x69, 0x00]);
        assert_eq!(r.read_utf16_le(2).unwrap(), "Hi");
    }

    #[test]
    fn read_utf8_basic() {
        let mut r = LeReader::new(b"hello");
        assert_eq!(r.read_utf8(5).unwrap(), "hello");
    }
}

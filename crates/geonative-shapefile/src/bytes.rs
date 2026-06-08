//! Mixed-endianness byte primitives for the Esri Shapefile format.
//!
//! Shapefiles use **big-endian for management fields** (file/record lengths,
//! record numbers, byte-counts) and **little-endian for data** (shape type
//! codes, coordinates, counts). The cursor exposes both flavors so callers
//! never silently mis-read a field.
//!
//! Lengths are stored in **16-bit words** (i.e. bytes / 2). Helpers convert.

use crate::error::{Result, ShpError};

#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.bytes.len() {
            return Err(ShpError::malformed(format!(
                "seek past EOF: {pos} > {}",
                self.bytes.len()
            )));
        }
        self.pos = pos;
        Ok(())
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            Err(ShpError::malformed(format!(
                "truncated input at byte {}: need {} more, have {}",
                self.pos,
                n,
                self.remaining()
            )))
        } else {
            Ok(())
        }
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn read_i32_be(&mut self) -> Result<i32> {
        let s = self.read_bytes(4)?;
        Ok(i32::from_be_bytes(s.try_into().unwrap()))
    }

    pub fn read_i32_le(&mut self) -> Result<i32> {
        let s = self.read_bytes(4)?;
        Ok(i32::from_le_bytes(s.try_into().unwrap()))
    }

    pub fn read_f64_le(&mut self) -> Result<f64> {
        let s = self.read_bytes(8)?;
        Ok(f64::from_le_bytes(s.try_into().unwrap()))
    }
}

/// Convert a Shapefile 16-bit-word count to bytes.
#[inline]
pub fn words_to_bytes(words: i32) -> usize {
    (words as i64 * 2) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mixed_endianness() {
        // 0x00000001 (BE int32 = 1) + 0x01000000 (LE int32 = 1)
        let mut c = Cursor::new(&[0, 0, 0, 1, 1, 0, 0, 0]);
        assert_eq!(c.read_i32_be().unwrap(), 1);
        assert_eq!(c.read_i32_le().unwrap(), 1);
    }

    #[test]
    fn truncation_errors() {
        let mut c = Cursor::new(&[1, 2, 3]);
        assert!(c.read_i32_be().is_err());
    }

    #[test]
    fn words_conversion() {
        assert_eq!(words_to_bytes(50), 100);
    }
}

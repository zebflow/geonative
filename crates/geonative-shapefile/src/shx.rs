//! `.shx` index — fixed 8-byte records of `(offset_words, content_len_words)`.
//!
//! Both fields are **big-endian** int32s. Offsets are word counts from the
//! start of the `.shp` file (so byte offset = offset_words * 2). The index
//! lets us seek directly to the N-th shape record in `.shp` without scanning.

use crate::bytes::Cursor;
use crate::error::Result;
use crate::header;

#[derive(Debug, Clone)]
pub struct ShxRecord {
    /// Byte offset of the record header in `.shp`.
    pub offset_bytes: u64,
    /// Content length in bytes (excludes the 8-byte record header).
    pub content_len_bytes: u32,
}

#[derive(Debug, Clone)]
pub struct Shx {
    pub header: header::ShpHeader,
    pub records: Vec<ShxRecord>,
}

pub fn parse(bytes: &[u8]) -> Result<Shx> {
    let h = header::parse(bytes)?;
    let body = &bytes[header::SHP_HEADER_BYTES..];
    let n = body.len() / 8;
    let mut records = Vec::with_capacity(n);
    let mut c = Cursor::new(body);
    for _ in 0..n {
        let offset_words = c.read_i32_be()?;
        let len_words = c.read_i32_be()?;
        records.push(ShxRecord {
            offset_bytes: (offset_words as i64 * 2) as u64,
            content_len_bytes: (len_words as i64 * 2) as u32,
        });
    }
    Ok(Shx { header: h, records })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{SHP_FILE_CODE, SHP_VERSION};

    fn synth_shx(records: &[(i32, i32)]) -> Vec<u8> {
        let total_words = 50 + records.len() as i32 * 4;
        let mut buf = vec![0u8; 100];
        buf[0..4].copy_from_slice(&SHP_FILE_CODE.to_be_bytes());
        buf[24..28].copy_from_slice(&total_words.to_be_bytes());
        buf[28..32].copy_from_slice(&SHP_VERSION.to_le_bytes());
        buf[32..36].copy_from_slice(&1i32.to_le_bytes()); // Point
                                                          // bbox = zeros, that's fine
        for &(off, len) in records {
            buf.extend_from_slice(&off.to_be_bytes());
            buf.extend_from_slice(&len.to_be_bytes());
        }
        buf
    }

    #[test]
    fn parses_two_records() {
        let shx = parse(&synth_shx(&[(50, 10), (62, 10)])).unwrap();
        assert_eq!(shx.records.len(), 2);
        assert_eq!(shx.records[0].offset_bytes, 100);
        assert_eq!(shx.records[0].content_len_bytes, 20);
        assert_eq!(shx.records[1].offset_bytes, 124);
    }
}

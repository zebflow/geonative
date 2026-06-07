//! Parser for the `.gdbtablx` row-offset index.
//!
//! Maps a logical row index (0-based) to a byte offset in the companion
//! `.gdbtable`. Layout reference: GDAL `openfilegdb/filegdbtable.cpp` —
//! `ReadTableXHeaderV3` / `ReadTableXHeaderV4`.
//!
//! ## Sparse blocks
//!
//! The offset section is always sized as `n1024BlocksPresent × 1024 ×
//! tablxOffsetSize` bytes. When the underlying table has long runs of deleted
//! rows, entire 1024-row blocks may be **absent** — the table physically
//! stores fewer offsets, and a trailing **block-map bitmap** records which
//! logical 1024-row blocks correspond to which physical block in the offsets
//! array.
//!
//! Lookup for logical row `iRow`:
//! 1. `block_idx = iRow / 1024`
//! 2. If a bitmap exists and bit `block_idx` is unset → row absent.
//! 3. Otherwise `physical_block_idx = popcount(bitmap[0..block_idx])` (number
//!    of present blocks before this one).
//! 4. `offset_idx = physical_block_idx * 1024 + (iRow % 1024)`.
//! 5. Read the `tablxOffsetSize`-byte LE int at `offsets_raw[offset_idx]`.
//!    A value of 0 means the row is deleted/absent.

use crate::bytes::LeReader;
use crate::error::{GdbError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TablxVersion {
    /// 32-bit OBJECTIDs.
    V3,
    /// 64-bit OBJECTIDs (ArcGIS Pro 3.2+).
    V4,
}

#[derive(Debug, Clone)]
pub struct TablxHeader {
    pub version: TablxVersion,
    /// Number of 1024-row blocks for which offsets are physically stored.
    pub n_1024_blocks_present: u64,
    /// Logical total record count (v3 only; v4 stores this elsewhere).
    /// For v0.1 we only read v3 here.
    pub total_record_count: i64,
    /// 4, 5, or 6 — bytes per offset entry. Caps file size at 4 GB / 1 TB / 256 TB.
    pub offset_size: u32,
}

/// Parse the 16-byte header. Version is inferred from the first u32.
pub fn parse_tablx_header(r: &mut LeReader) -> Result<TablxHeader> {
    let version_raw = r.read_u32()?;
    let version = match version_raw {
        3 => TablxVersion::V3,
        4 => TablxVersion::V4,
        v => return Err(GdbError::malformed(format!("unknown .gdbtablx version {v}"))),
    };

    let (n_1024_blocks_present, total_record_count, offset_size) = match version {
        TablxVersion::V3 => {
            let n_blocks = r.read_u32()? as u64;
            let total = r.read_i32()? as i64;
            let osz = r.read_u32()?;
            (n_blocks, total, osz)
        }
        TablxVersion::V4 => {
            // V4 packs n1024BlocksPresent as a 64-bit value at offset 4..12,
            // then offset_size at 12..16. total_record_count for v4 is not in
            // .gdbtablx; it comes from the .gdbtable header.
            let n_blocks = r.read_u64()?;
            let osz = r.read_u32()?;
            (n_blocks, -1, osz)
        }
    };

    if !(4..=6).contains(&offset_size) {
        return Err(GdbError::malformed(format!(
            ".gdbtablx offset_size {offset_size} out of range (4..=6)"
        )));
    }
    if matches!(version, TablxVersion::V3) {
        if n_1024_blocks_present == 0 && total_record_count != 0 {
            return Err(GdbError::malformed(
                ".gdbtablx v3 declares no blocks but nonzero records",
            ));
        }
        if total_record_count < 0 {
            return Err(GdbError::malformed(format!(
                ".gdbtablx v3 negative total_record_count {total_record_count}"
            )));
        }
    }

    Ok(TablxHeader {
        version,
        n_1024_blocks_present,
        total_record_count,
        offset_size,
    })
}

#[derive(Debug, Clone)]
pub struct Tablx {
    pub header: TablxHeader,
    /// Physically-stored offsets, exactly `n_1024_blocks_present * 1024`
    /// entries. Each is a byte offset into `.gdbtable`; 0 means deleted row.
    pub offsets_raw: Vec<u64>,
    /// Block-presence bitmap, only present when the table is sparse and the
    /// trailer's `nBitmapInt32Words` is nonzero. Indexed by logical block
    /// index (`iRow / 1024`).
    pub block_map: Option<Vec<u8>>,
    /// Number of bits in `block_map`. Each bit represents one 1024-row block.
    pub bits_for_block_map: u32,
}

impl Tablx {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut r = LeReader::new(bytes);
        let header = parse_tablx_header(&mut r)?;

        // Offsets section: n_1024_blocks_present * 1024 entries of offset_size bytes each.
        let n_offsets = header.n_1024_blocks_present.saturating_mul(1024);
        let osz = header.offset_size as usize;
        let mut offsets_raw = Vec::with_capacity(n_offsets as usize);
        for _ in 0..n_offsets {
            offsets_raw.push(r.read_uint_le(osz)?);
        }

        // Optional trailer (16 bytes) + optional block-map bitmap.
        let mut block_map: Option<Vec<u8>> = None;
        let mut bits_for_block_map: u32 = 0;
        if header.n_1024_blocks_present > 0 {
            if r.remaining() < 16 {
                return Err(GdbError::malformed(
                    ".gdbtablx truncated: missing trailer",
                ));
            }
            let n_bitmap_int32_words = r.read_u32()?;
            let n_bits_for_block_map = r.read_u32()?;
            let n_1024_blocks_bis = r.read_u32()?;
            let _n_leading_non_zero_32_bit_words = r.read_u32()?;
            if n_1024_blocks_bis as u64 != header.n_1024_blocks_present {
                return Err(GdbError::malformed(format!(
                    ".gdbtablx trailer block count {n_1024_blocks_bis} != header {}",
                    header.n_1024_blocks_present
                )));
            }

            if n_bitmap_int32_words != 0 {
                bits_for_block_map = n_bits_for_block_map;
                let n_bytes = bits_for_block_map.div_ceil(8) as usize;
                if r.remaining() < n_bytes {
                    return Err(GdbError::malformed(format!(
                        ".gdbtablx block-map bitmap declares {n_bytes} bytes but only {} remain",
                        r.remaining()
                    )));
                }
                block_map = Some(r.read_bytes(n_bytes)?.to_vec());

                // Consistency check: popcount must equal n_1024_blocks_present.
                let popcount: u64 = block_map
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|b| b.count_ones() as u64)
                    .sum();
                if popcount != header.n_1024_blocks_present {
                    return Err(GdbError::malformed(format!(
                        ".gdbtablx block-map popcount {popcount} != n_1024_blocks_present {}",
                        header.n_1024_blocks_present
                    )));
                }
            } else if n_bits_for_block_map != header.n_1024_blocks_present as u32 {
                // No bitmap → blocks 0..n_1024_blocks_present are all present
                // and that count must match.
                return Err(GdbError::malformed(
                    ".gdbtablx trailer says no bitmap but block counts disagree",
                ));
            }
        }

        Ok(Self {
            header,
            offsets_raw,
            block_map,
            bits_for_block_map,
        })
    }

    /// Byte offset into `.gdbtable` for the given **0-indexed** logical row,
    /// or `None` if the row is deleted/absent.
    pub fn offset_for(&self, row_idx: u64) -> Option<u64> {
        let block_idx = (row_idx / 1024) as u32;
        let in_block = (row_idx % 1024) as usize;

        let physical_block_idx = if let Some(map) = &self.block_map {
            if block_idx >= self.bits_for_block_map {
                return None;
            }
            // Check bit
            let byte_pos = (block_idx / 8) as usize;
            let bit_in_byte = (block_idx % 8) as u8;
            if map.get(byte_pos).copied().unwrap_or(0) & (1u8 << bit_in_byte) == 0 {
                return None;
            }
            // popcount of bits BEFORE block_idx
            let mut popcount: u32 = 0;
            for i in 0..byte_pos {
                popcount += map[i].count_ones();
            }
            // partial byte
            let partial = map[byte_pos] & ((1u8 << bit_in_byte) - 1);
            popcount += partial.count_ones();
            popcount
        } else {
            // No sparse bitmap; logical block index == physical block index.
            if block_idx as u64 >= self.header.n_1024_blocks_present {
                return None;
            }
            block_idx
        };

        let offset_idx = physical_block_idx as usize * 1024 + in_block;
        let raw = *self.offsets_raw.get(offset_idx)?;
        if raw == 0 {
            None
        } else {
            Some(raw)
        }
    }

    /// Iterate present `(row_idx, offset_in_gdbtable)` pairs in row order.
    /// `row_idx` is 0-based; add 1 to recover FID.
    pub fn iter_present(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        // Upper bound on row index is bits_for_block_map * 1024 if sparse,
        // else n_1024_blocks_present * 1024.
        let max_blocks = if self.block_map.is_some() {
            self.bits_for_block_map as u64
        } else {
            self.header.n_1024_blocks_present
        };
        let max_rows = max_blocks * 1024;

        (0..max_rows).filter_map(move |i| self.offset_for(i).map(|o| (i, o)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_v3_dense(offsets: &[u64], offset_size: u32) -> Vec<u8> {
        // 1024 entries per block; pad with zeros.
        let n_blocks = offsets.len().div_ceil(1024);
        let mut padded = offsets.to_vec();
        padded.resize(n_blocks * 1024, 0);

        let mut buf = Vec::new();
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&(n_blocks as u32).to_le_bytes()); // blocks present
        buf.extend_from_slice(&(offsets.len() as i32).to_le_bytes()); // total records
        buf.extend_from_slice(&offset_size.to_le_bytes()); // offset size
        for off in padded {
            // Write `offset_size` low bytes of `off` in LE.
            let bytes = off.to_le_bytes();
            buf.extend_from_slice(&bytes[..offset_size as usize]);
        }
        // 16-byte trailer: nBitmapInt32Words=0, nBitsForBlockMap=n_blocks,
        // n1024BlocksBis=n_blocks, nLeadingNonZero32BitWords=0.
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&(n_blocks as u32).to_le_bytes());
        buf.extend_from_slice(&(n_blocks as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf
    }

    #[test]
    fn dense_v3_offsets() {
        let offsets = vec![100u64, 200, 0, 400]; // row 2 deleted
        let bytes = synth_v3_dense(&offsets, 4);

        let t = Tablx::parse(&bytes).unwrap();
        assert_eq!(t.header.version, TablxVersion::V3);
        assert_eq!(t.header.n_1024_blocks_present, 1);
        assert_eq!(t.header.total_record_count, 4);
        assert_eq!(t.offset_size_bytes_check(), 4);
        assert!(t.block_map.is_none());

        assert_eq!(t.offset_for(0), Some(100));
        assert_eq!(t.offset_for(1), Some(200));
        assert_eq!(t.offset_for(2), None); // deleted
        assert_eq!(t.offset_for(3), Some(400));
        assert_eq!(t.offset_for(4), None); // beyond declared rows, but inside the block — value 0

        let present: Vec<(u64, u64)> = t.iter_present().collect();
        assert_eq!(present, vec![(0, 100), (1, 200), (3, 400)]);
    }

    #[test]
    fn offset_size_5_supports_large_files() {
        // 6 GB offset: 0x1_8000_0000
        let big = 0x1_8000_0000u64;
        let bytes = synth_v3_dense(&[big], 5);
        let t = Tablx::parse(&bytes).unwrap();
        assert_eq!(t.offset_for(0), Some(big));
    }

    #[test]
    fn rejects_bad_offset_size() {
        let mut buf = vec![0u8; 16];
        buf[..4].copy_from_slice(&3u32.to_le_bytes());
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[8..12].copy_from_slice(&1i32.to_le_bytes());
        buf[12..16].copy_from_slice(&3u32.to_le_bytes()); // invalid: < 4
        assert!(Tablx::parse(&buf).is_err());

        buf[12..16].copy_from_slice(&7u32.to_le_bytes()); // invalid: > 6
        assert!(Tablx::parse(&buf).is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let mut buf = vec![0u8; 16];
        buf[..4].copy_from_slice(&9u32.to_le_bytes());
        assert!(Tablx::parse(&buf).is_err());
    }

    impl Tablx {
        // Tiny helper for the test above so we don't need to keep recomputing.
        fn offset_size_bytes_check(&self) -> u32 {
            self.header.offset_size
        }
    }
}

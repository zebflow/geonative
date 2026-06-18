//! PMTiles directory entries — the workhorse of the format.
//!
//! A directory is a flat list of [`Entry`] records, each describing either:
//!
//! - **A tile (or run of identical tiles)** — `run_length ≥ 1`, with
//!   `offset` pointing into the tile-data section.
//! - **A leaf-directory pointer** — `run_length == 0`, with `offset`
//!   pointing into the leaf-directories section.
//!
//! Entries are sorted by `tile_id`. They're encoded in "transposed
//! columnar" form for compressibility: all `tile_id`s first (delta-coded),
//! then all `run_length`s, then all `length`s, then all `offset`s. Each
//! column is then serialised as a sequence of varints (LEB128). After
//! that, the whole blob is gzipped.
//!
//! Spec: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md#directory-layout>

use crate::error::{PmtilesError, Result};
use crate::varint;

/// One directory entry. Field semantics depend on `run_length`:
///
/// - `run_length ≥ 1`: this is a tile entry. `offset` is the byte position
///   of the tile within the tile-data section; `length` is its byte size.
///   `run_length > 1` means the next `run_length - 1` consecutive tile_ids
///   alias the same bytes (deduplicated empty tiles, for example).
/// - `run_length == 0`: this is a leaf-directory pointer. `offset` is into
///   the leaf-directories section; `length` is the byte size of the
///   compressed leaf directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub tile_id: u64,
    pub run_length: u32,
    pub length: u32,
    pub offset: u64,
}

/// Encode entries in PMTiles transposed-columnar form.
///
/// Layout (all values are varints):
///
/// ```text
///   n
///   tile_id[0]               (absolute)
///   tile_id[i] - tile_id[i-1] (delta) for i = 1..n
///   run_length[0..n]
///   length[0..n]
///   offset[i]                — see special rules below
/// ```
///
/// Offset encoding rule per spec:
///
/// - `offset == 0`: the value `0` literally means "same start as the
///   previous tile entry concatenated end-to-end" — i.e.
///   `prev.offset + prev.length`. So a 0 placeholder shrinks every entry
///   in a densely packed run.
/// - Otherwise: encoded as `offset + 1` so the varint is non-zero.
pub fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + entries.len() * 8);
    varint::write_u64(&mut out, entries.len() as u64);

    // tile_ids (delta).
    let mut prev_tile_id: u64 = 0;
    for (i, e) in entries.iter().enumerate() {
        let delta = if i == 0 {
            e.tile_id
        } else {
            e.tile_id - prev_tile_id
        };
        varint::write_u64(&mut out, delta);
        prev_tile_id = e.tile_id;
    }

    // run_lengths.
    for e in entries {
        varint::write_u64(&mut out, e.run_length as u64);
    }

    // lengths.
    for e in entries {
        varint::write_u64(&mut out, e.length as u64);
    }

    // offsets with the prev+length elision rule.
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            let prev = &entries[i - 1];
            if e.offset == prev.offset + prev.length as u64 {
                varint::write_u64(&mut out, 0);
                continue;
            }
        }
        varint::write_u64(&mut out, e.offset + 1);
    }

    out
}

/// Inverse of [`encode`]. Decodes the entire entry list.
pub fn decode(bytes: &[u8]) -> Result<Vec<Entry>> {
    let mut off = 0usize;
    let n = varint::read_u64_at(bytes, &mut off)? as usize;

    // tile_ids.
    let mut tile_ids = Vec::with_capacity(n);
    let mut acc: u64 = 0;
    for _ in 0..n {
        let delta = varint::read_u64_at(bytes, &mut off)?;
        acc = acc
            .checked_add(delta)
            .ok_or_else(|| PmtilesError::malformed("tile_id overflow"))?;
        tile_ids.push(acc);
    }

    // run_lengths.
    let mut run_lengths = Vec::with_capacity(n);
    for _ in 0..n {
        let v = varint::read_u64_at(bytes, &mut off)?;
        if v > u32::MAX as u64 {
            return Err(PmtilesError::malformed("run_length exceeds u32"));
        }
        run_lengths.push(v as u32);
    }

    // lengths.
    let mut lengths = Vec::with_capacity(n);
    for _ in 0..n {
        let v = varint::read_u64_at(bytes, &mut off)?;
        if v > u32::MAX as u64 {
            return Err(PmtilesError::malformed("entry length exceeds u32"));
        }
        lengths.push(v as u32);
    }

    // offsets with the prev+length elision rule.
    let mut entries: Vec<Entry> = Vec::with_capacity(n);
    for i in 0..n {
        let raw = varint::read_u64_at(bytes, &mut off)?;
        let resolved = if raw == 0 {
            if i == 0 {
                return Err(PmtilesError::malformed(
                    "first entry's offset cannot use the prev-end placeholder",
                ));
            }
            let prev = &entries[i - 1];
            prev.offset + prev.length as u64
        } else {
            raw - 1
        };
        entries.push(Entry {
            tile_id: tile_ids[i],
            run_length: run_lengths[i],
            length: lengths[i],
            offset: resolved,
        });
    }

    Ok(entries)
}

/// Binary-search for the entry whose tile_id is ≤ `tile_id` AND whose run
/// covers `tile_id`. Returns `None` if no such entry exists (i.e. the
/// requested tile_id falls in a gap).
///
/// This is the core lookup hot-path: the reader calls it once per
/// directory level to find the right tile entry (or a leaf-dir pointer
/// to chase).
pub fn find_tile<'a>(entries: &'a [Entry], tile_id: u64) -> Option<&'a Entry> {
    // partition_point gives us the first entry with tile_id > target.
    let idx = entries.partition_point(|e| e.tile_id <= tile_id);
    if idx == 0 {
        return None;
    }
    let cand = &entries[idx - 1];
    if cand.run_length == 0 {
        // Leaf-dir pointer: tile_id falls in this leaf's range iff the
        // next entry's tile_id is greater than `tile_id` (i.e. cand still
        // owns this id). That's already guaranteed by partition_point.
        Some(cand)
    } else if tile_id < cand.tile_id + cand.run_length as u64 {
        Some(cand)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(tile_id: u64, run_length: u32, length: u32, offset: u64) -> Entry {
        Entry {
            tile_id,
            run_length,
            length,
            offset,
        }
    }

    #[test]
    fn encode_decode_empty() {
        let bytes = encode(&[]);
        let back = decode(&bytes).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn encode_decode_single() {
        let entries = vec![e(42, 1, 100, 1024)];
        let bytes = encode(&entries);
        assert_eq!(decode(&bytes).unwrap(), entries);
    }

    #[test]
    fn encode_decode_many_with_consecutive_offsets() {
        // Tiles packed consecutively in tile-data — should trigger the
        // prev-end placeholder for entries 1..N.
        let entries = vec![
            e(10, 1, 50, 0),
            e(11, 1, 50, 50),
            e(12, 1, 50, 100),
            e(13, 1, 75, 150),
            e(20, 1, 200, 1_000_000), // non-consecutive: full offset
        ];
        let bytes = encode(&entries);
        let back = decode(&bytes).unwrap();
        assert_eq!(back, entries);
    }

    #[test]
    fn encode_decode_leaf_pointer() {
        let entries = vec![e(0, 0, 4096, 0), e(8192, 0, 2048, 4096)];
        let bytes = encode(&entries);
        let back = decode(&bytes).unwrap();
        assert_eq!(back, entries);
    }

    #[test]
    fn find_tile_hits_single_entry() {
        let entries = vec![e(10, 1, 50, 0), e(11, 1, 50, 50), e(20, 1, 30, 100)];
        assert_eq!(find_tile(&entries, 10).unwrap().tile_id, 10);
        assert_eq!(find_tile(&entries, 11).unwrap().tile_id, 11);
        assert_eq!(find_tile(&entries, 20).unwrap().tile_id, 20);
    }

    #[test]
    fn find_tile_inside_run() {
        let entries = vec![e(10, 5, 50, 0)]; // covers tile_ids 10..15
        assert!(find_tile(&entries, 9).is_none());
        assert_eq!(find_tile(&entries, 10).unwrap().tile_id, 10);
        assert_eq!(find_tile(&entries, 14).unwrap().tile_id, 10);
        assert!(find_tile(&entries, 15).is_none());
    }

    #[test]
    fn find_tile_gap_returns_none() {
        let entries = vec![e(10, 1, 50, 0), e(20, 1, 50, 50)];
        assert!(find_tile(&entries, 15).is_none());
    }

    #[test]
    fn find_tile_in_leaf_pointer_range() {
        // Leaf at tile_id=0 covers everything up to the next entry's tile_id.
        let entries = vec![e(0, 0, 4096, 0), e(100, 1, 50, 0)];
        assert_eq!(find_tile(&entries, 50).unwrap().tile_id, 0); // hits leaf
        assert_eq!(find_tile(&entries, 100).unwrap().tile_id, 100); // hits real
    }
}

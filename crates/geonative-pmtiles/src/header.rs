//! Fixed 127-byte PMTiles v3 header parser + serializer.
//!
//! Spec: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md#header>
//!
//! Field-by-field layout (all little-endian unless noted):
//!
//! | offset | size | field |
//! |-------:|-----:|-------|
//! | 0      | 7    | magic "PMTiles" |
//! | 7      | 1    | spec_version (= 3) |
//! | 8      | 8    | root_dir_offset |
//! | 16     | 8    | root_dir_length |
//! | 24     | 8    | json_metadata_offset |
//! | 32     | 8    | json_metadata_length |
//! | 40     | 8    | leaf_dirs_offset |
//! | 48     | 8    | leaf_dirs_length |
//! | 56     | 8    | tile_data_offset |
//! | 64     | 8    | tile_data_length |
//! | 72     | 8    | addressed_tiles_count |
//! | 80     | 8    | tile_entries_count |
//! | 88     | 8    | tile_contents_count |
//! | 96     | 1    | clustered (0/1) |
//! | 97     | 1    | internal_compression |
//! | 98     | 1    | tile_compression |
//! | 99     | 1    | tile_type |
//! | 100    | 1    | min_zoom |
//! | 101    | 1    | max_zoom |
//! | 102    | 4    | min_lon_e7 (i32) |
//! | 106    | 4    | min_lat_e7 (i32) |
//! | 110    | 4    | max_lon_e7 (i32) |
//! | 114    | 4    | max_lat_e7 (i32) |
//! | 118    | 1    | center_zoom |
//! | 119    | 4    | center_lon_e7 (i32) |
//! | 123    | 4    | center_lat_e7 (i32) |
//! | 127    | —    | end |

use crate::error::{PmtilesError, Result};

pub const HEADER_LEN: usize = 127;
pub const PMTILES_MAGIC: &[u8; 7] = b"PMTiles";
pub const SPEC_VERSION: u8 = 3;

/// What's inside each tile blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TileType {
    Unknown = 0,
    Mvt = 1,
    Png = 2,
    Jpeg = 3,
    Webp = 4,
    Avif = 5,
}

impl TileType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Mvt,
            2 => Self::Png,
            3 => Self::Jpeg,
            4 => Self::Webp,
            5 => Self::Avif,
            _ => Self::Unknown,
        }
    }
}

/// How directories / tile bytes are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Compression {
    Unknown = 0,
    None = 1,
    Gzip = 2,
    Brotli = 3,
    Zstd = 4,
}

impl Compression {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::None,
            2 => Self::Gzip,
            3 => Self::Brotli,
            4 => Self::Zstd,
            _ => Self::Unknown,
        }
    }
}

/// Parsed/buildable PMTiles header. Same field set as the wire format,
/// but pre/post bbox conversion is done in caller code.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub root_dir_offset: u64,
    pub root_dir_length: u64,
    pub json_metadata_offset: u64,
    pub json_metadata_length: u64,
    pub leaf_dirs_offset: u64,
    pub leaf_dirs_length: u64,
    pub tile_data_offset: u64,
    pub tile_data_length: u64,
    pub addressed_tiles_count: u64,
    pub tile_entries_count: u64,
    pub tile_contents_count: u64,
    pub clustered: bool,
    pub internal_compression: Compression,
    pub tile_compression: Compression,
    pub tile_type: TileType,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub min_lon_e7: i32,
    pub min_lat_e7: i32,
    pub max_lon_e7: i32,
    pub max_lat_e7: i32,
    pub center_zoom: u8,
    pub center_lon_e7: i32,
    pub center_lat_e7: i32,
}

impl Header {
    /// Read fields from a 127-byte slice.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(PmtilesError::Truncated {
                offset: 0,
                needed: HEADER_LEN as u64,
                total: bytes.len() as u64,
            });
        }
        if &bytes[0..7] != PMTILES_MAGIC {
            let mut got = [0u8; 8];
            got.copy_from_slice(&bytes[0..8]);
            return Err(PmtilesError::NotPmtiles(got));
        }
        let version = bytes[7];
        if version != SPEC_VERSION {
            return Err(PmtilesError::UnsupportedVersion(version));
        }
        let u64_at = |off: usize| u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
        let i32_at = |off: usize| i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        Ok(Self {
            root_dir_offset: u64_at(8),
            root_dir_length: u64_at(16),
            json_metadata_offset: u64_at(24),
            json_metadata_length: u64_at(32),
            leaf_dirs_offset: u64_at(40),
            leaf_dirs_length: u64_at(48),
            tile_data_offset: u64_at(56),
            tile_data_length: u64_at(64),
            addressed_tiles_count: u64_at(72),
            tile_entries_count: u64_at(80),
            tile_contents_count: u64_at(88),
            clustered: bytes[96] != 0,
            internal_compression: Compression::from_u8(bytes[97]),
            tile_compression: Compression::from_u8(bytes[98]),
            tile_type: TileType::from_u8(bytes[99]),
            min_zoom: bytes[100],
            max_zoom: bytes[101],
            min_lon_e7: i32_at(102),
            min_lat_e7: i32_at(106),
            max_lon_e7: i32_at(110),
            max_lat_e7: i32_at(114),
            center_zoom: bytes[118],
            center_lon_e7: i32_at(119),
            center_lat_e7: i32_at(123),
        })
    }

    /// Serialize to a fresh 127-byte vector.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN];
        out[0..7].copy_from_slice(PMTILES_MAGIC);
        out[7] = SPEC_VERSION;
        out[8..16].copy_from_slice(&self.root_dir_offset.to_le_bytes());
        out[16..24].copy_from_slice(&self.root_dir_length.to_le_bytes());
        out[24..32].copy_from_slice(&self.json_metadata_offset.to_le_bytes());
        out[32..40].copy_from_slice(&self.json_metadata_length.to_le_bytes());
        out[40..48].copy_from_slice(&self.leaf_dirs_offset.to_le_bytes());
        out[48..56].copy_from_slice(&self.leaf_dirs_length.to_le_bytes());
        out[56..64].copy_from_slice(&self.tile_data_offset.to_le_bytes());
        out[64..72].copy_from_slice(&self.tile_data_length.to_le_bytes());
        out[72..80].copy_from_slice(&self.addressed_tiles_count.to_le_bytes());
        out[80..88].copy_from_slice(&self.tile_entries_count.to_le_bytes());
        out[88..96].copy_from_slice(&self.tile_contents_count.to_le_bytes());
        out[96] = self.clustered as u8;
        out[97] = self.internal_compression as u8;
        out[98] = self.tile_compression as u8;
        out[99] = self.tile_type as u8;
        out[100] = self.min_zoom;
        out[101] = self.max_zoom;
        out[102..106].copy_from_slice(&self.min_lon_e7.to_le_bytes());
        out[106..110].copy_from_slice(&self.min_lat_e7.to_le_bytes());
        out[110..114].copy_from_slice(&self.max_lon_e7.to_le_bytes());
        out[114..118].copy_from_slice(&self.max_lat_e7.to_le_bytes());
        out[118] = self.center_zoom;
        out[119..123].copy_from_slice(&self.center_lon_e7.to_le_bytes());
        out[123..127].copy_from_slice(&self.center_lat_e7.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Header {
        Header {
            root_dir_offset: 127,
            root_dir_length: 200,
            json_metadata_offset: 327,
            json_metadata_length: 100,
            leaf_dirs_offset: 427,
            leaf_dirs_length: 0,
            tile_data_offset: 427,
            tile_data_length: 1024,
            addressed_tiles_count: 16,
            tile_entries_count: 16,
            tile_contents_count: 16,
            clustered: true,
            internal_compression: Compression::Gzip,
            tile_compression: Compression::Gzip,
            tile_type: TileType::Mvt,
            min_zoom: 0,
            max_zoom: 4,
            min_lon_e7: -1_800_000_000,
            min_lat_e7: -900_000_000,
            max_lon_e7: 1_800_000_000,
            max_lat_e7: 900_000_000,
            center_zoom: 2,
            center_lon_e7: 0,
            center_lat_e7: 0,
        }
    }

    #[test]
    fn header_is_exactly_127_bytes() {
        assert_eq!(sample().to_bytes().len(), HEADER_LEN);
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let h = sample();
        let bytes = h.to_bytes();
        let parsed = Header::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = sample().to_bytes();
        bytes[0..7].copy_from_slice(b"NOTPMTI");
        assert!(matches!(
            Header::parse(&bytes).unwrap_err(),
            PmtilesError::NotPmtiles(_)
        ));
    }

    #[test]
    fn rejects_wrong_version() {
        let mut bytes = sample().to_bytes();
        bytes[7] = 2; // we only handle v3
        assert!(matches!(
            Header::parse(&bytes).unwrap_err(),
            PmtilesError::UnsupportedVersion(2)
        ));
    }

    #[test]
    fn rejects_truncated() {
        let bytes = sample().to_bytes();
        assert!(matches!(
            Header::parse(&bytes[..50]).unwrap_err(),
            PmtilesError::Truncated { .. }
        ));
    }
}

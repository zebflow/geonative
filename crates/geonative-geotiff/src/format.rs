//! TIFF wire format: header, IFD, tag entries.
//!
//! ## The TIFF structure (read it once, useful forever)
//!
//! ```text
//!  ┌──────────────────────┐
//!  │ Header (8 or 16 B)   │  byte order ("II" or "MM") + magic + first IFD offset
//!  └──────────┬───────────┘
//!             ▼
//!  ┌──────────────────────┐
//!  │ IFD #0               │  count + entries[count] + next-IFD offset
//!  │   tag_id, type, …    │
//!  │   tag_id, type, …    │
//!  │   …                  │
//!  └──────────┬───────────┘
//!             │ (chain via next-IFD offset; COG uses this for overviews)
//!             ▼
//!  ┌──────────────────────┐
//!  │ IFD #1 (overview)    │
//!  │   …                  │
//!  └──────────┬───────────┘
//!             ▼
//!  ┌──────────────────────┐
//!  │ Pixel data           │  (StripOffsets / TileOffsets point here)
//!  └──────────────────────┘
//! ```
//!
//! ## Classic vs BigTIFF
//!
//! Classic TIFF caps offsets at 4 GB (u32). BigTIFF widens them to u64 to
//! support multi-TB files (common for satellite-imagery mosaics). The two
//! variants differ in:
//!
//! - Magic word: 42 (classic) vs 43 (BigTIFF)
//! - Offset width: u32 vs u64
//! - Entry count width: u16 vs u64
//! - Tag-value-inline cap: 4 bytes vs 8 bytes
//!
//! We handle both transparently; downstream code just sees `u64` offsets.

use crate::error::{GtiffError, Result};

/// Byte order: little-endian (`II`) or big-endian (`MM`). Real-world TIFFs
/// are overwhelmingly little-endian; we read both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    pub fn u16(self, b: &[u8]) -> u16 {
        match self {
            ByteOrder::Little => u16::from_le_bytes([b[0], b[1]]),
            ByteOrder::Big => u16::from_be_bytes([b[0], b[1]]),
        }
    }
    pub fn u32(self, b: &[u8]) -> u32 {
        match self {
            ByteOrder::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            ByteOrder::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
    pub fn u64(self, b: &[u8]) -> u64 {
        match self {
            ByteOrder::Little => {
                u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
            }
            ByteOrder::Big => u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        }
    }
    pub fn f64(self, b: &[u8]) -> f64 {
        f64::from_bits(self.u64(b))
    }
    pub fn f32(self, b: &[u8]) -> f32 {
        f32::from_bits(self.u32(b))
    }
}

/// TIFF header — what's at the start of the file.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub byte_order: ByteOrder,
    pub big_tiff: bool,
    /// Offset to IFD #0 from the start of the file.
    pub first_ifd_offset: u64,
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 {
            return Err(GtiffError::Truncated {
                offset: 0,
                needed: 8,
                total: bytes.len() as u64,
            });
        }
        let byte_order = match &bytes[0..2] {
            b"II" => ByteOrder::Little,
            b"MM" => ByteOrder::Big,
            other => {
                return Err(GtiffError::NotATiff([
                    other[0], other[1], bytes[2], bytes[3],
                ]))
            }
        };
        let magic = byte_order.u16(&bytes[2..4]);
        match magic {
            42 => Ok(Self {
                byte_order,
                big_tiff: false,
                first_ifd_offset: byte_order.u32(&bytes[4..8]) as u64,
            }),
            43 => {
                if bytes.len() < 16 {
                    return Err(GtiffError::Truncated {
                        offset: 8,
                        needed: 8,
                        total: bytes.len() as u64,
                    });
                }
                // BigTIFF header: 2 (order) + 2 (magic=43) + 2 (offset_size, always 8)
                // + 2 (reserved=0) + 8 (first IFD offset).
                let offset_size = byte_order.u16(&bytes[4..6]);
                if offset_size != 8 {
                    return Err(GtiffError::malformed(format!(
                        "BigTIFF offset size {offset_size}, expected 8"
                    )));
                }
                Ok(Self {
                    byte_order,
                    big_tiff: true,
                    first_ifd_offset: byte_order.u64(&bytes[8..16]),
                })
            }
            other => Err(GtiffError::UnsupportedMagic(other)),
        }
    }
}

/// TIFF tag data types — every tag has one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    Byte = 1,
    Ascii = 2,
    Short = 3,
    Long = 4,
    Rational = 5,
    SByte = 6,
    Undefined = 7,
    SShort = 8,
    SLong = 9,
    SRational = 10,
    Float = 11,
    Double = 12,
    /// BigTIFF additions
    Long8 = 16,
    SLong8 = 17,
    Ifd8 = 18,
}

impl DType {
    pub fn from_u16(v: u16) -> Result<Self> {
        Ok(match v {
            1 => Self::Byte,
            2 => Self::Ascii,
            3 => Self::Short,
            4 => Self::Long,
            5 => Self::Rational,
            6 => Self::SByte,
            7 => Self::Undefined,
            8 => Self::SShort,
            9 => Self::SLong,
            10 => Self::SRational,
            11 => Self::Float,
            12 => Self::Double,
            16 => Self::Long8,
            17 => Self::SLong8,
            18 => Self::Ifd8,
            other => return Err(GtiffError::malformed(format!("unknown tag dtype {other}"))),
        })
    }

    /// Bytes per scalar element of this type.
    pub fn size(self) -> usize {
        match self {
            Self::Byte | Self::Ascii | Self::SByte | Self::Undefined => 1,
            Self::Short | Self::SShort => 2,
            Self::Long | Self::SLong | Self::Float => 4,
            Self::Rational
            | Self::SRational
            | Self::Double
            | Self::Long8
            | Self::SLong8
            | Self::Ifd8 => 8,
        }
    }
}

/// One entry in an IFD: tag id + type + count + value-or-offset.
#[derive(Debug, Clone)]
pub struct TagEntry {
    pub tag: u16,
    pub dtype: DType,
    pub count: u64,
    /// Bytes containing either the scalar value (inline if fits) or an
    /// offset to the value array (otherwise). We keep them raw; the
    /// `as_*` methods below resolve them with the file context.
    pub value_bytes: [u8; 8],
}

impl TagEntry {
    /// Read the actual values pointed to by this entry, given the full
    /// file bytes. Returns an owned buffer of (count × dtype.size()) bytes.
    ///
    /// The inline case copies 4–8 bytes — negligible vs the file I/O the
    /// caller has already done to get here. Iterators below take this
    /// `Vec<u8>` by reference so per-tag access is still cheap.
    pub fn read_values(&self, file: &[u8], order: ByteOrder, big_tiff: bool) -> Result<Vec<u8>> {
        let total = self.count * self.dtype.size() as u64;
        let inline_cap = if big_tiff { 8 } else { 4 };
        if total <= inline_cap {
            Ok(self.value_bytes[..total as usize].to_vec())
        } else {
            let offset = if big_tiff {
                order.u64(&self.value_bytes)
            } else {
                order.u32(&self.value_bytes[0..4]) as u64
            };
            let start = offset as usize;
            let end = start
                .checked_add(total as usize)
                .ok_or_else(|| GtiffError::malformed("tag offset overflow"))?;
            if end > file.len() {
                return Err(GtiffError::Truncated {
                    offset: start as u64,
                    needed: total,
                    total: file.len() as u64,
                });
            }
            Ok(file[start..end].to_vec())
        }
    }

    /// Read this entry's first value as an integer, regardless of its
    /// declared dtype (Short/Long/Long8 all map to u64).
    pub fn as_u64_first(&self, file: &[u8], order: ByteOrder, big_tiff: bool) -> Result<u64> {
        let bytes = self.read_values(file, order, big_tiff)?;
        Ok(match self.dtype {
            DType::Byte | DType::Undefined => bytes[0] as u64,
            DType::Short => order.u16(&bytes[0..2]) as u64,
            DType::Long => order.u32(&bytes[0..4]) as u64,
            DType::Long8 | DType::Ifd8 => order.u64(&bytes[0..8]),
            _ => {
                return Err(GtiffError::malformed(format!(
                    "tag {} dtype {:?} cannot be read as u64",
                    self.tag, self.dtype
                )))
            }
        })
    }

    /// Iterate this entry's values as u64s. Convenient for tag arrays
    /// where each element is an offset or a dimension.
    pub fn iter_u64(&self, file: &[u8], order: ByteOrder, big_tiff: bool) -> Result<U64Iter> {
        let bytes = self.read_values(file, order, big_tiff)?;
        Ok(U64Iter {
            bytes,
            order,
            dtype: self.dtype,
            count: self.count,
            pos: 0,
        })
    }

    /// Iterate as f64. Supports Float / Double / Rational.
    pub fn iter_f64(&self, file: &[u8], order: ByteOrder, big_tiff: bool) -> Result<F64Iter> {
        let bytes = self.read_values(file, order, big_tiff)?;
        Ok(F64Iter {
            bytes,
            order,
            dtype: self.dtype,
            count: self.count,
            pos: 0,
        })
    }

    /// ASCII string value (NUL-terminated per TIFF convention; trailing NUL stripped).
    pub fn as_ascii(&self, file: &[u8], order: ByteOrder, big_tiff: bool) -> Result<String> {
        let bytes = self.read_values(file, order, big_tiff)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }
}

#[derive(Debug)]
pub struct U64Iter {
    bytes: Vec<u8>,
    order: ByteOrder,
    dtype: DType,
    count: u64,
    pos: u64,
}

impl Iterator for U64Iter {
    type Item = u64;
    fn next(&mut self) -> Option<u64> {
        if self.pos >= self.count {
            return None;
        }
        let size = self.dtype.size();
        let start = (self.pos as usize) * size;
        let val = match self.dtype {
            DType::Byte | DType::Undefined => self.bytes[start] as u64,
            DType::Short => self.order.u16(&self.bytes[start..start + 2]) as u64,
            DType::Long => self.order.u32(&self.bytes[start..start + 4]) as u64,
            DType::Long8 | DType::Ifd8 => self.order.u64(&self.bytes[start..start + 8]),
            _ => return None,
        };
        self.pos += 1;
        Some(val)
    }
}

#[derive(Debug)]
pub struct F64Iter {
    bytes: Vec<u8>,
    order: ByteOrder,
    dtype: DType,
    count: u64,
    pos: u64,
}

impl Iterator for F64Iter {
    type Item = f64;
    fn next(&mut self) -> Option<f64> {
        if self.pos >= self.count {
            return None;
        }
        let size = self.dtype.size();
        let start = (self.pos as usize) * size;
        let val = match self.dtype {
            DType::Float => self.order.f32(&self.bytes[start..start + 4]) as f64,
            DType::Double => self.order.f64(&self.bytes[start..start + 8]),
            DType::Rational => {
                // 4-byte numerator / 4-byte denominator
                let num = self.order.u32(&self.bytes[start..start + 4]) as f64;
                let den = self.order.u32(&self.bytes[start + 4..start + 8]) as f64;
                if den == 0.0 {
                    0.0
                } else {
                    num / den
                }
            }
            _ => return None,
        };
        self.pos += 1;
        Some(val)
    }
}

/// One Image File Directory — a flat collection of tags + a chain link to
/// the next IFD (used for COG overviews).
#[derive(Debug, Clone)]
pub struct Ifd {
    pub entries: Vec<TagEntry>,
    /// File offset of the next IFD, or 0 if this is the last.
    pub next_offset: u64,
}

impl Ifd {
    pub fn parse(file: &[u8], offset: u64, order: ByteOrder, big_tiff: bool) -> Result<Self> {
        let start = offset as usize;
        let count_size = if big_tiff { 8 } else { 2 };
        if start + count_size > file.len() {
            return Err(GtiffError::Truncated {
                offset,
                needed: count_size as u64,
                total: file.len() as u64,
            });
        }
        let count = if big_tiff {
            order.u64(&file[start..start + 8])
        } else {
            order.u16(&file[start..start + 2]) as u64
        };
        let entry_size = if big_tiff { 20 } else { 12 };
        let entries_start = start + count_size;
        let next_offset_start = entries_start + (count as usize) * entry_size;
        let next_offset_end = next_offset_start + if big_tiff { 8 } else { 4 };
        if next_offset_end > file.len() {
            return Err(GtiffError::Truncated {
                offset: entries_start as u64,
                needed: (count * entry_size as u64) + 4,
                total: file.len() as u64,
            });
        }

        let mut entries = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let e_start = entries_start + i * entry_size;
            let tag = order.u16(&file[e_start..e_start + 2]);
            let dtype = DType::from_u16(order.u16(&file[e_start + 2..e_start + 4]))?;
            let count_val = if big_tiff {
                order.u64(&file[e_start + 4..e_start + 12])
            } else {
                order.u32(&file[e_start + 4..e_start + 8]) as u64
            };
            let value_off = if big_tiff { e_start + 12 } else { e_start + 8 };
            let value_len = if big_tiff { 8 } else { 4 };
            let mut value_bytes = [0u8; 8];
            value_bytes[..value_len].copy_from_slice(&file[value_off..value_off + value_len]);
            entries.push(TagEntry {
                tag,
                dtype,
                count: count_val,
                value_bytes,
            });
        }

        let next_offset = if big_tiff {
            order.u64(&file[next_offset_start..next_offset_start + 8])
        } else {
            order.u32(&file[next_offset_start..next_offset_start + 4]) as u64
        };

        Ok(Self {
            entries,
            next_offset,
        })
    }

    /// Look up a tag by ID. Returns `None` if the tag is absent.
    pub fn tag(&self, id: u16) -> Option<&TagEntry> {
        self.entries.iter().find(|e| e.tag == id)
    }
}

// --- Well-known tag IDs ---------------------------------------------------

pub mod tags {
    /// Image dimensions
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC_INTERPRETATION: u16 = 262;

    /// Stripped layout
    pub const STRIP_OFFSETS: u16 = 273;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;

    /// Sample structure
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const PLANAR_CONFIGURATION: u16 = 284;
    pub const SAMPLE_FORMAT: u16 = 339;

    /// Tiled layout (COG-friendly)
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;

    /// Predictor (1=none, 2=horizontal differencing, 3=floating-point)
    pub const PREDICTOR: u16 = 317;

    /// Nodata (GDAL extension)
    pub const GDAL_NODATA: u16 = 42113;

    /// GeoTIFF tags
    pub const MODEL_PIXEL_SCALE: u16 = 33550;
    pub const MODEL_TIEPOINT: u16 = 33922;
    pub const MODEL_TRANSFORMATION: u16 = 34264;
    pub const GEO_KEY_DIRECTORY: u16 = 34735;
    pub const GEO_DOUBLE_PARAMS: u16 = 34736;
    pub const GEO_ASCII_PARAMS: u16 = 34737;
}

/// TIFF compression codes.
pub mod compression {
    pub const NONE: u16 = 1;
    pub const LZW: u16 = 5;
    pub const JPEG_OLD: u16 = 6;
    pub const JPEG: u16 = 7;
    pub const DEFLATE: u16 = 8;
    pub const DEFLATE_ADOBE: u16 = 32946;
    pub const PACKBITS: u16 = 32773;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_classic_le() {
        // II + magic 42 (LE) + offset 8 (LE)
        let bytes = [b'I', b'I', 42, 0, 8, 0, 0, 0];
        let h = Header::parse(&bytes).unwrap();
        assert_eq!(h.byte_order, ByteOrder::Little);
        assert!(!h.big_tiff);
        assert_eq!(h.first_ifd_offset, 8);
    }

    #[test]
    fn header_classic_be() {
        let bytes = [b'M', b'M', 0, 42, 0, 0, 0, 16];
        let h = Header::parse(&bytes).unwrap();
        assert_eq!(h.byte_order, ByteOrder::Big);
        assert!(!h.big_tiff);
        assert_eq!(h.first_ifd_offset, 16);
    }

    #[test]
    fn header_bigtiff_le() {
        let bytes = [
            b'I', b'I', 43, 0, 8, 0, 0, 0, // header
            16, 0, 0, 0, 0, 0, 0, 0, // first IFD offset
        ];
        let h = Header::parse(&bytes).unwrap();
        assert_eq!(h.byte_order, ByteOrder::Little);
        assert!(h.big_tiff);
        assert_eq!(h.first_ifd_offset, 16);
    }

    #[test]
    fn header_rejects_garbage() {
        assert!(matches!(
            Header::parse(&[1, 2, 3, 4]).unwrap_err(),
            GtiffError::Truncated { .. }
        ));
        assert!(matches!(
            Header::parse(&[b'Z', b'Z', 42, 0, 0, 0, 0, 0]).unwrap_err(),
            GtiffError::NotATiff(_)
        ));
    }

    #[test]
    fn dtype_sizes() {
        assert_eq!(DType::Byte.size(), 1);
        assert_eq!(DType::Short.size(), 2);
        assert_eq!(DType::Long.size(), 4);
        assert_eq!(DType::Double.size(), 8);
        assert_eq!(DType::Long8.size(), 8);
    }

    #[test]
    fn ifd_round_trip_classic() {
        // Build a minimal classic LE TIFF with one IFD containing one tag
        // (ImageWidth = 100), then parse it back.
        let mut file = Vec::new();
        file.extend_from_slice(b"II"); // little-endian
        file.extend_from_slice(&42u16.to_le_bytes()); // magic
        file.extend_from_slice(&8u32.to_le_bytes()); // first IFD at offset 8
                                                     // IFD at 8: 1 entry, then tag (ImageWidth, Short, count=1, value=100)
        file.extend_from_slice(&1u16.to_le_bytes()); // count = 1
        file.extend_from_slice(&tags::IMAGE_WIDTH.to_le_bytes()); // tag id
        file.extend_from_slice(&(DType::Short as u16).to_le_bytes()); // type = Short
        file.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        file.extend_from_slice(&[100, 0, 0, 0]); // inline value
        file.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset = 0

        let header = Header::parse(&file).unwrap();
        let ifd = Ifd::parse(&file, header.first_ifd_offset, header.byte_order, false).unwrap();
        assert_eq!(ifd.entries.len(), 1);
        assert_eq!(ifd.next_offset, 0);
        let tag = ifd.tag(tags::IMAGE_WIDTH).unwrap();
        assert_eq!(
            tag.as_u64_first(&file, header.byte_order, false).unwrap(),
            100
        );
    }
}

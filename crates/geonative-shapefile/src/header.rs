//! The 100-byte file header shared by `.shp` and `.shx`.
//!
//! Layout per Esri J-7855 (July 1998):
//!
//! | Bytes | Field | Endian |
//! | --- | --- | --- |
//! | 0..4 | File code 9994 | Big |
//! | 24..28 | File length in 16-bit words | Big |
//! | 28..32 | Version 1000 | Little |
//! | 32..36 | Shape type | Little |
//! | 36..68 | XY bbox (xmin,ymin,xmax,ymax) | Little |
//! | 68..100 | Z + M bbox | Little |

use crate::bytes::Cursor;
use crate::error::{Result, ShpError};

pub const SHP_FILE_CODE: i32 = 9994;
pub const SHP_VERSION: i32 = 1000;
pub const SHP_HEADER_BYTES: usize = 100;

/// Esri Shapefile shape type codes (2D + Z/M variants). v0.1 of this crate
/// only decodes the four 2D variants (`Point`, `Polyline`, `Polygon`,
/// `Multipoint`); Z/M / MultiPatch return [`ShpError::Unsupported`] at the
/// decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeType {
    Null = 0,
    Point = 1,
    Polyline = 3,
    Polygon = 5,
    Multipoint = 8,
    PointZ = 11,
    PolylineZ = 13,
    PolygonZ = 15,
    MultipointZ = 18,
    PointM = 21,
    PolylineM = 23,
    PolygonM = 25,
    MultipointM = 28,
    Multipatch = 31,
}

impl ShapeType {
    pub fn from_i32(v: i32) -> Result<Self> {
        Ok(match v {
            0 => Self::Null,
            1 => Self::Point,
            3 => Self::Polyline,
            5 => Self::Polygon,
            8 => Self::Multipoint,
            11 => Self::PointZ,
            13 => Self::PolylineZ,
            15 => Self::PolygonZ,
            18 => Self::MultipointZ,
            21 => Self::PointM,
            23 => Self::PolylineM,
            25 => Self::PolygonM,
            28 => Self::MultipointM,
            31 => Self::Multipatch,
            other => return Err(ShpError::malformed(format!("unknown shape type {other}"))),
        })
    }
}

/// Parsed `.shp` / `.shx` file header (the format is byte-identical between
/// the two files; only the records that follow differ).
#[derive(Debug, Clone)]
pub struct ShpHeader {
    pub file_length_words: i32,
    pub shape_type: ShapeType,
    /// `[xmin, ymin, xmax, ymax]`.
    pub bbox_xy: [f64; 4],
    /// `[zmin, zmax]`. Zero for 2D files.
    pub bbox_z: [f64; 2],
    /// `[mmin, mmax]`. Zero / sentinel for non-M files.
    pub bbox_m: [f64; 2],
}

pub fn parse(bytes: &[u8]) -> Result<ShpHeader> {
    if bytes.len() < SHP_HEADER_BYTES {
        return Err(ShpError::malformed(format!(
            "file shorter than 100-byte header (got {})",
            bytes.len()
        )));
    }
    let mut c = Cursor::new(&bytes[..SHP_HEADER_BYTES]);
    let code = c.read_i32_be()?;
    if code != SHP_FILE_CODE {
        return Err(ShpError::malformed(format!(
            "bad file code {code:#x} (expected {SHP_FILE_CODE:#x})"
        )));
    }
    // Skip 5 unused i32s (bytes 4..24).
    c.seek(24)?;
    let file_length_words = c.read_i32_be()?;
    let version = c.read_i32_le()?;
    if version != SHP_VERSION {
        return Err(ShpError::malformed(format!(
            "bad version {version} (expected {SHP_VERSION})"
        )));
    }
    let shape_type = ShapeType::from_i32(c.read_i32_le()?)?;
    let bbox_xy = [
        c.read_f64_le()?,
        c.read_f64_le()?,
        c.read_f64_le()?,
        c.read_f64_le()?,
    ];
    let bbox_z = [c.read_f64_le()?, c.read_f64_le()?];
    let bbox_m = [c.read_f64_le()?, c.read_f64_le()?];
    Ok(ShpHeader {
        file_length_words,
        shape_type,
        bbox_xy,
        bbox_z,
        bbox_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_polygon_header() {
        let mut h = vec![0u8; 100];
        h[0..4].copy_from_slice(&SHP_FILE_CODE.to_be_bytes());
        h[24..28].copy_from_slice(&500i32.to_be_bytes()); // file_length_words
        h[28..32].copy_from_slice(&SHP_VERSION.to_le_bytes());
        h[32..36].copy_from_slice(&5i32.to_le_bytes()); // Polygon
        h[36..44].copy_from_slice(&0.0f64.to_le_bytes());
        h[44..52].copy_from_slice(&0.0f64.to_le_bytes());
        h[52..60].copy_from_slice(&10.0f64.to_le_bytes());
        h[60..68].copy_from_slice(&10.0f64.to_le_bytes());

        let parsed = parse(&h).unwrap();
        assert_eq!(parsed.file_length_words, 500);
        assert_eq!(parsed.shape_type, ShapeType::Polygon);
        assert_eq!(parsed.bbox_xy, [0.0, 0.0, 10.0, 10.0]);
    }

    #[test]
    fn bad_magic_errors() {
        let mut h = vec![0u8; 100];
        h[0..4].copy_from_slice(&1234i32.to_be_bytes());
        h[28..32].copy_from_slice(&SHP_VERSION.to_le_bytes());
        assert!(parse(&h).is_err());
    }

    #[test]
    fn short_input_errors() {
        let h = vec![0u8; 50];
        assert!(parse(&h).is_err());
    }
}

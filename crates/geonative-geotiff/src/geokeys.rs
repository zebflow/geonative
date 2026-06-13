//! Extract `Crs` + `GeoTransform` from GeoTIFF tags.
//!
//! ## The GeoKey directory
//!
//! GeoTIFF stores its CRS information in a parallel "key directory" inside
//! TIFF tag 34735. The directory itself is an array of u16s laid out as:
//!
//! ```text
//! header   : [KeyDirectoryVersion, KeyRevision, MinorRevision, NumberOfKeys]
//! entries  : [KeyID, TIFFTagLocation, Count, Value], repeated NumberOfKeys times
//! ```
//!
//! For each entry:
//! - `TIFFTagLocation == 0`  → the Value is an inline u16
//! - `TIFFTagLocation == 34736` (GeoDoubleParams) → Value is an offset into the doubles array, Count = how many doubles
//! - `TIFFTagLocation == 34737` (GeoAsciiParams)  → Value is an offset into the ASCII array, Count = string length
//!
//! Keys we care about for v0.1:
//! - 1024 `GTModelType` (1=Projected, 2=Geographic, 3=Geocentric)
//! - 2048 `GeographicTypeGeoKey` — EPSG of a geographic CRS (e.g. 4326)
//! - 3072 `ProjectedCSTypeGeoKey` — EPSG of a projected CRS (e.g. 3857)
//!
//! ## The pixel→world affine
//!
//! GeoTIFF encodes the affine via either:
//! - One `ModelTiepoint` (6 doubles: `[i, j, k, x, y, z]`) plus a
//!   `ModelPixelScale` (3 doubles: `[sx, sy, sz]`) — the north-up case
//! - OR a full `ModelTransformation` (16 doubles, 4×4 matrix) for rotated
//!   / skewed rasters
//!
//! We handle the tiepoint+scale form in v0.1; full transformation matrices
//! are a v0.2 add.

use geonative_core::raster::GeoTransform;
use geonative_core::Crs;

use crate::error::{GtiffError, Result};
use crate::format::{tags, ByteOrder, Ifd};

pub const KEY_GT_MODEL_TYPE: u16 = 1024;
pub const KEY_GEOGRAPHIC_TYPE: u16 = 2048;
pub const KEY_PROJECTED_CS_TYPE: u16 = 3072;

pub fn extract_crs(ifd: &Ifd, file: &[u8], order: ByteOrder, big_tiff: bool) -> Result<Crs> {
    let Some(dir_tag) = ifd.tag(tags::GEO_KEY_DIRECTORY) else {
        return Ok(Crs::Unknown);
    };

    let dir_bytes = dir_tag.read_values(file, order, big_tiff)?;
    if dir_bytes.len() < 8 {
        return Err(GtiffError::malformed(
            "GeoKey directory too short for header",
        ));
    }

    // Read the header.
    let num_keys = order.u16(&dir_bytes[6..8]);
    let expected_len = 8 + (num_keys as usize) * 8;
    if dir_bytes.len() < expected_len {
        return Err(GtiffError::malformed(format!(
            "GeoKey directory truncated: declared {} keys, only {} bytes",
            num_keys,
            dir_bytes.len()
        )));
    }

    // Walk entries.
    let mut projected = None;
    let mut geographic = None;
    for i in 0..num_keys as usize {
        let off = 8 + i * 8;
        let key_id = order.u16(&dir_bytes[off..off + 2]);
        let loc = order.u16(&dir_bytes[off + 2..off + 4]);
        // count + value
        let value = order.u16(&dir_bytes[off + 6..off + 8]);

        if loc == 0 {
            // Inline u16 — common case for EPSG codes.
            match key_id {
                KEY_PROJECTED_CS_TYPE => projected = Some(value as u32),
                KEY_GEOGRAPHIC_TYPE => geographic = Some(value as u32),
                _ => {}
            }
        }
        // (locations 34736 / 34737 — double / ascii params — are needed for
        // arbitrary WKT projections; v0.2.)
    }

    // Projected CRS wins over geographic (a projected file IS a CRS).
    if let Some(code) = projected {
        if code != 0 && code != 32767 {
            return Ok(Crs::Epsg(code));
        }
    }
    if let Some(code) = geographic {
        if code != 0 && code != 32767 {
            return Ok(Crs::Epsg(code));
        }
    }
    Ok(Crs::Unknown)
}

pub fn extract_geo_transform(
    ifd: &Ifd,
    file: &[u8],
    order: ByteOrder,
    big_tiff: bool,
) -> Result<Option<GeoTransform>> {
    // Full ModelTransformation (16 doubles 4x4 matrix) takes precedence —
    // but v0.1 doesn't implement it yet; we error if we see one.
    if let Some(t) = ifd.tag(tags::MODEL_TRANSFORMATION) {
        if t.count == 16 {
            return Err(GtiffError::unsupported(
                "ModelTransformation (full 4x4 affine) — use ModelTiepoint + ModelPixelScale for v0.1",
            ));
        }
    }

    let scale_tag = ifd.tag(tags::MODEL_PIXEL_SCALE);
    let tie_tag = ifd.tag(tags::MODEL_TIEPOINT);
    let (Some(s), Some(t)) = (scale_tag, tie_tag) else {
        return Ok(None);
    };

    if s.count < 3 {
        return Err(GtiffError::malformed(format!(
            "ModelPixelScale count {}, expected 3",
            s.count
        )));
    }
    if t.count < 6 {
        return Err(GtiffError::malformed(format!(
            "ModelTiepoint count {}, expected ≥6",
            t.count
        )));
    }

    let scales: Vec<f64> = s.iter_f64(file, order, big_tiff)?.collect();
    let tie: Vec<f64> = t.iter_f64(file, order, big_tiff)?.collect();

    // ModelTiepoint: [i, j, k, x, y, z]. For the universal north-up case
    // i=j=k=0 and x,y is the upper-left corner of pixel (0, 0).
    let (i, j, _k, x, y, _z) = (tie[0], tie[1], tie[2], tie[3], tie[4], tie[5]);
    let (sx, sy, _sz) = (scales[0], scales[1], scales[2]);

    // If i,j are non-zero, the tiepoint isn't at the origin — back-compute
    // the origin from (x,y) − (i*sx, -j*sy).
    let origin_x = x - i * sx;
    let origin_y = y + j * sy; // Y axis flipped for north-up
    Ok(Some(GeoTransform {
        origin: [origin_x, origin_y],
        pixel_size: [sx, -sy], // negative pixel_h = north-up
        rotation: [0.0, 0.0],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{DType, Header, TagEntry};

    fn build_minimal_geo_tiff(crs_epsg: u16, origin: (f64, f64), pixel: (f64, f64)) -> Vec<u8> {
        // Synthesise a tiny TIFF with:
        // - ModelPixelScale at offset X
        // - ModelTiepoint at offset Y
        // - GeoKeyDirectory at offset Z (one ProjectedCSTypeGeoKey entry)
        // - IFD pointing at all three
        // Layout: header(8) + GeoKey(8*2 bytes inline u16s) + scales(24) + tie(48) + IFD(...)
        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // first IFD offset patched below

        // GeoKey directory: header(4 u16) + 1 entry(4 u16) = 8 u16 = 16 bytes
        let geo_key_off = buf.len();
        buf.extend_from_slice(&1u16.to_le_bytes()); // version
        buf.extend_from_slice(&1u16.to_le_bytes()); // revision
        buf.extend_from_slice(&0u16.to_le_bytes()); // minor
        buf.extend_from_slice(&1u16.to_le_bytes()); // num keys
                                                    // Entry: ProjectedCSTypeGeoKey, loc=0, count=1, value=crs_epsg
        buf.extend_from_slice(&KEY_PROJECTED_CS_TYPE.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&crs_epsg.to_le_bytes());

        // ModelPixelScale: 3 doubles
        let scale_off = buf.len();
        buf.extend_from_slice(&pixel.0.to_le_bytes());
        buf.extend_from_slice(&pixel.1.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());

        // ModelTiepoint: 6 doubles [i, j, k, x, y, z]
        let tie_off = buf.len();
        for v in [0.0, 0.0, 0.0, origin.0, origin.1, 0.0] {
            buf.extend_from_slice(&v.to_le_bytes());
        }

        // IFD at the current position
        let ifd_off = buf.len();
        // Three entries: ModelPixelScale, ModelTiepoint, GeoKeyDirectory
        buf.extend_from_slice(&3u16.to_le_bytes());
        // ModelPixelScale (Double × 3) — value is offset
        buf.extend_from_slice(&tags::MODEL_PIXEL_SCALE.to_le_bytes());
        buf.extend_from_slice(&(DType::Double as u16).to_le_bytes());
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&(scale_off as u32).to_le_bytes());
        // ModelTiepoint (Double × 6)
        buf.extend_from_slice(&tags::MODEL_TIEPOINT.to_le_bytes());
        buf.extend_from_slice(&(DType::Double as u16).to_le_bytes());
        buf.extend_from_slice(&6u32.to_le_bytes());
        buf.extend_from_slice(&(tie_off as u32).to_le_bytes());
        // GeoKeyDirectory (Short × 8)
        buf.extend_from_slice(&tags::GEO_KEY_DIRECTORY.to_le_bytes());
        buf.extend_from_slice(&(DType::Short as u16).to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&(geo_key_off as u32).to_le_bytes());
        // Next IFD = 0
        buf.extend_from_slice(&0u32.to_le_bytes());

        // Patch first IFD offset in the header (bytes 4..8).
        let ifd_off_bytes = (ifd_off as u32).to_le_bytes();
        buf[4..8].copy_from_slice(&ifd_off_bytes);
        buf
    }

    #[test]
    fn extracts_projected_crs() {
        let buf = build_minimal_geo_tiff(3857, (0.0, 0.0), (1.0, 1.0));
        let header = Header::parse(&buf).unwrap();
        let ifd = Ifd::parse(&buf, header.first_ifd_offset, header.byte_order, false).unwrap();
        let crs = extract_crs(&ifd, &buf, header.byte_order, false).unwrap();
        assert_eq!(crs, Crs::Epsg(3857));
    }

    #[test]
    fn extracts_geo_transform_north_up() {
        let buf = build_minimal_geo_tiff(3857, (100.0, 200.0), (0.5, 0.5));
        let header = Header::parse(&buf).unwrap();
        let ifd = Ifd::parse(&buf, header.first_ifd_offset, header.byte_order, false).unwrap();
        let gt = extract_geo_transform(&ifd, &buf, header.byte_order, false)
            .unwrap()
            .unwrap();
        assert_eq!(gt.origin, [100.0, 200.0]);
        assert_eq!(gt.pixel_size, [0.5, -0.5]);
        assert_eq!(gt.rotation, [0.0, 0.0]);
        assert!(gt.is_north_up());
    }

    #[test]
    fn missing_geo_returns_none() {
        // TIFF with no ModelPixelScale / ModelTiepoint
        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // 0 entries
        buf.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        let header = Header::parse(&buf).unwrap();
        let ifd = Ifd::parse(&buf, header.first_ifd_offset, header.byte_order, false).unwrap();
        assert!(extract_geo_transform(&ifd, &buf, header.byte_order, false)
            .unwrap()
            .is_none());

        // And no CRS
        assert_eq!(
            extract_crs(&ifd, &buf, header.byte_order, false).unwrap(),
            Crs::Unknown
        );

        // Just to avoid unused warning
        let entry_unused: Option<&TagEntry> = ifd.tag(123);
        assert!(entry_unused.is_none());
    }
}

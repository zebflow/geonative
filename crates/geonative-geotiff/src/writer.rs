//! GeoTIFF / COG writer.
//!
//! Takes a stream of `RasterTile`s (one per (level, x, y) position) and
//! writes a valid tiled GeoTIFF on close. The output is structured for
//! efficient byte-range reads — internal tiling + embedded overview IFDs —
//! which is what makes a TIFF a "COG."
//!
//! ## v0.1 scope
//!
//! - Classic TIFF (BigTIFF for >4 GB outputs deferred to v0.2)
//! - Compression: None, PackBits, DEFLATE, LZW
//! - Multi-band, chunky (interleaved) pixel layout
//! - North-up `GeoTransform` → `ModelPixelScale` + `ModelTiepoint`
//! - EPSG-coded `Crs` → `GeoKeyDirectory` with `ProjectedCSTypeGeoKey` or
//!   `GeographicTypeGeoKey`
//! - Pyramid levels chained via `next_offset` (caller supplies overview
//!   tiles; building overviews is a `geonative-processing` concern in
//!   Sprint 13 Phase D)
//!
//! ## v0.1 layout note
//!
//! Pixel data is written before IFDs. The output is a valid tiled GeoTIFF
//! that mmap-backed readers (including ours) handle perfectly. For strict
//! "IFD-first" COG layout (which matters most for HTTP-range remote reads),
//! see v0.2.
//!
//! ## Usage
//!
//! ```ignore
//! use geonative_geotiff::writer::{Compression, GeoTiffWriter, WriterOptions};
//!
//! let file = std::fs::File::create("output.cog")?;
//! let mut writer = GeoTiffWriter::create(file, &source_profile, WriterOptions {
//!     compression: Compression::Deflate,
//!     ..WriterOptions::default()
//! })?;
//!
//! for y in 0..grid_y {
//!     for x in 0..grid_x {
//!         let tile = source.read_tile(0, x, y)?;
//!         writer.write_tile(0, x, y, &tile)?;
//!     }
//! }
//! writer.close()?;
//! ```

use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};

use geonative_core::raster::{GeoTransform, PixelType, RasterProfile, RasterTile};
use geonative_core::Crs;

use crate::error::{GtiffError, Result};
use crate::format::{compression as cmp, tags, DType};

/// Compression strategy for written tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Uncompressed — fastest write, largest output.
    None,
    /// PackBits RLE — light compression, very fast.
    PackBits,
    /// DEFLATE — best compatibility, the COG default.
    Deflate,
    /// LZW — historical TIFF default; widely supported.
    Lzw,
}

impl Compression {
    fn tag_value(self) -> u16 {
        match self {
            Self::None => cmp::NONE,
            Self::PackBits => cmp::PACKBITS,
            Self::Deflate => cmp::DEFLATE,
            Self::Lzw => cmp::LZW,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub compression: Compression,
    /// DEFLATE compression level (1=fastest, 9=best). Ignored for other codecs.
    pub deflate_level: u32,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            compression: Compression::Deflate,
            deflate_level: 6,
        }
    }
}

/// Buffered GeoTIFF writer.
///
/// `W` is typically `std::fs::File`. Anything `Write + Seek` works; we need
/// `Seek` to patch the header's "first IFD offset" after laying everything
/// out.
pub struct GeoTiffWriter<W: Write + Seek> {
    sink: W,
    options: WriterOptions,
    profile: RasterProfile,
    /// Encoded tile bytes indexed by `(level, y, x)`. Stored sorted so the
    /// final flat layout is deterministic regardless of caller order.
    tiles: BTreeMap<(u8, u32, u32), Vec<u8>>,
}

impl<W: Write + Seek> std::fmt::Debug for GeoTiffWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoTiffWriter")
            .field("options", &self.options)
            .field("profile_dims", &(self.profile.width, self.profile.height))
            .field("tiles_buffered", &self.tiles.len())
            .finish()
    }
}

impl<W: Write + Seek> GeoTiffWriter<W> {
    /// Construct a writer targeting `sink`. Writes the TIFF header
    /// immediately so partial files (post-crash) are still identifiable.
    pub fn create(mut sink: W, profile: &RasterProfile, options: WriterOptions) -> Result<Self> {
        // Header: II + magic 42 + placeholder first-IFD offset (patched on close).
        sink.write_all(b"II")?;
        sink.write_all(&42u16.to_le_bytes())?;
        sink.write_all(&0u32.to_le_bytes())?;
        Ok(Self {
            sink,
            options,
            profile: profile.clone(),
            tiles: BTreeMap::new(),
        })
    }

    /// Buffer one tile for writing. Subsequent calls to the same
    /// `(level, x, y)` overwrite the prior tile.
    pub fn write_tile(&mut self, level: u8, x: u32, y: u32, tile: &RasterTile) -> Result<()> {
        let encoded = self.encode_tile(tile)?;
        self.tiles.insert((level, y, x), encoded);
        Ok(())
    }

    /// Finalize: write all tile pixel data, then write one IFD per
    /// pyramid level (chained via `next_offset`), then patch the header
    /// to point at IFD 0.
    pub fn close(mut self) -> Result<W> {
        // 1) Plan: group tiles by level, in (y, x) order.
        let levels = self.partition_by_level();

        // 2) Write tile pixel data. Record per-level (offsets, byte_counts).
        let mut per_level_offsets: Vec<Vec<u64>> = Vec::with_capacity(levels.len());
        let mut per_level_byte_counts: Vec<Vec<u64>> = Vec::with_capacity(levels.len());

        for level_tiles in &levels {
            let mut offsets = Vec::with_capacity(level_tiles.len());
            let mut byte_counts = Vec::with_capacity(level_tiles.len());
            for tile_bytes in level_tiles {
                offsets.push(self.sink.stream_position()?);
                byte_counts.push(tile_bytes.len() as u64);
                self.sink.write_all(tile_bytes)?;
            }
            per_level_offsets.push(offsets);
            per_level_byte_counts.push(byte_counts);
        }

        // 3) Write IFDs. Each IFD knows the offset of the next; we lay them
        //    out sequentially and patch offsets as we go.
        // (We don't end up needing per-level next-offset placeholders in
        // this version; offsets are computed directly in the layout loop.)

        // First, compute per-IFD positions: we need to know where each IFD
        // starts to write its "next IFD offset" pointer.
        let first_ifd_offset = self.sink.stream_position()?;

        // We write IFDs sequentially. For each IFD i: tag-array data first
        // (so tag entries can reference offsets), then the IFD itself.
        // To compute "next IFD offset" before writing IFD i, we have to
        // know IFD i+1's start. So we pre-compute the layout.
        let mut ifd_starts: Vec<u64> = Vec::with_capacity(levels.len());
        let mut cursor = first_ifd_offset;
        for (i, _level_tiles) in levels.iter().enumerate() {
            let tag_count = ifd_tag_count(i == 0);
            let tag_arrays_size = self.tag_arrays_size_for_level(i)?;
            // Each IFD body = 2 (entry count) + 12*N (entries) + 4 (next ptr).
            let ifd_body = 2 + 12 * tag_count as u64 + 4;
            ifd_starts.push(cursor + tag_arrays_size);
            cursor += tag_arrays_size + ifd_body;
        }

        // Now write everything in order.
        for (i, level_tiles_count) in levels.iter().map(|t| t.len()).enumerate() {
            let _ = level_tiles_count;
            let next_offset = if i + 1 < ifd_starts.len() {
                ifd_starts[i + 1]
            } else {
                0
            };
            self.write_ifd_for_level(
                i as u8,
                &per_level_offsets[i],
                &per_level_byte_counts[i],
                next_offset,
            )?;
        }

        // 4) Patch the header. The header points at the IFD BODY (after the
        //    tag-array data for level 0), not at the tag arrays themselves.
        let header_ifd_target = ifd_starts[0];
        self.sink.seek(SeekFrom::Start(4))?;
        self.sink
            .write_all(&(header_ifd_target as u32).to_le_bytes())?;
        self.sink.seek(SeekFrom::End(0))?;
        self.sink.flush()?;
        let _ = first_ifd_offset; // silence unused warning

        Ok(self.sink)
    }

    /// Group buffered tiles into per-level Vec<Vec<bytes>>, ordered
    /// (y-major) within each level.
    fn partition_by_level(&self) -> Vec<Vec<Vec<u8>>> {
        let mut max_level = 0u8;
        for &(level, _, _) in self.tiles.keys() {
            max_level = max_level.max(level);
        }
        let mut out: Vec<Vec<Vec<u8>>> = (0..=max_level).map(|_| Vec::new()).collect();
        for (&(level, _y, _x), bytes) in &self.tiles {
            out[level as usize].push(bytes.clone());
        }
        out
    }

    fn encode_tile(&self, tile: &RasterTile) -> Result<Vec<u8>> {
        if !tile.is_well_formed() {
            return Err(GtiffError::malformed("tile is not well-formed for writing"));
        }
        // Interleave bands chunky (PlanarConfiguration=1).
        let pixels = (tile.width as usize) * (tile.height as usize);
        let stride: usize = tile
            .bands
            .iter()
            .map(|b| b.descriptor.dtype.size_bytes())
            .sum();
        let mut interleaved = vec![0u8; pixels * stride];
        let band_strides: Vec<usize> = tile
            .bands
            .iter()
            .map(|b| b.descriptor.dtype.size_bytes())
            .collect();
        for px in 0..pixels {
            let mut off = px * stride;
            for (bi, b) in tile.bands.iter().enumerate() {
                let bps = band_strides[bi];
                let src = px * bps;
                interleaved[off..off + bps].copy_from_slice(&b.data[src..src + bps]);
                off += bps;
            }
        }

        match self.options.compression {
            Compression::None => Ok(interleaved),
            Compression::PackBits => Ok(encode_packbits(&interleaved)),
            Compression::Deflate => encode_deflate(&interleaved, self.options.deflate_level),
            Compression::Lzw => encode_lzw(&interleaved),
        }
    }

    fn tag_arrays_size_for_level(&self, level_idx: usize) -> Result<u64> {
        // Extra-data space we'll write before the IFD body. Note: tags whose
        // total payload ≤ 4 bytes are stored INLINE in the IFD entry and do
        // NOT need external space. This mirrors the inline / external split
        // in `write_ifd_for_level`.
        let tiles_count = self.tile_count_for_level(level_idx as u8)?;
        let nbands = self.profile.bands.len();
        let mut size: u64 = 0;
        let to_bytes = 4 * tiles_count as u64;
        let bc_bytes = 4 * tiles_count as u64;
        if to_bytes > 4 {
            size += to_bytes;
        }
        if bc_bytes > 4 {
            size += bc_bytes;
        }
        if nbands > 1 {
            size += 2 * nbands as u64; // BitsPerSample array (always external if >1 band)
        }
        if level_idx == 0 {
            size += 24; // ModelPixelScale
            size += 48; // ModelTiepoint
            size += 16; // GeoKeyDirectory (header 8 bytes + 1 entry 8 bytes)
        }
        Ok(size)
    }

    fn tile_count_for_level(&self, level: u8) -> Result<u32> {
        let scale = 1u32 << level;
        let w = (self.profile.width / scale).max(1);
        let h = (self.profile.height / scale).max(1);
        // Match write_ifd_for_level's per-level tile-size clamping.
        let tw = self.profile.tile_size[0].min(w).max(1);
        let th = self.profile.tile_size[1].min(h).max(1);
        let gx = (w.saturating_add(tw - 1)) / tw;
        let gy = (h.saturating_add(th - 1)) / th;
        Ok(gx * gy)
    }

    fn write_ifd_for_level(
        &mut self,
        level: u8,
        tile_offsets: &[u64],
        tile_byte_counts: &[u64],
        next_offset: u64,
    ) -> Result<()> {
        let order = u16::to_le_bytes;
        let nbands = self.profile.bands.len();
        let scale = 1u32 << level;
        let w = (self.profile.width / scale).max(1);
        let h = (self.profile.height / scale).max(1);
        // Per-level tile dimensions: clamp to the level's actual image size
        // so overview levels smaller than the canonical tile size record
        // the right TileWidth/TileLength (otherwise the reader expects more
        // pixels than the encoded tile actually contains → DEFLATE corrupt).
        let tw = self.profile.tile_size[0].min(w);
        let th = self.profile.tile_size[1].min(h);

        // 1) Write extra-data arrays first, recording their offsets.
        //
        // TIFF tag entries have a 4-byte value/offset field. When the tag's
        // total payload (count × dtype_size) is ≤ 4 bytes, the value lives
        // INLINE in the entry — there's no external array. Skip writing the
        // external array in that case (writing it would still work for the
        // array bytes themselves, but the entry's value/offset field would
        // then incorrectly point at the array's *position* rather than carry
        // the inline value, and the reader treats short tags as inline by
        // spec).
        let to_off = if tile_offsets.len() * 4 > 4 {
            let off = self.sink.stream_position()?;
            for &o in tile_offsets {
                self.sink.write_all(&(o as u32).to_le_bytes())?;
            }
            Some(off)
        } else {
            None
        };
        let bc_off = if tile_byte_counts.len() * 4 > 4 {
            let off = self.sink.stream_position()?;
            for &c in tile_byte_counts {
                self.sink.write_all(&(c as u32).to_le_bytes())?;
            }
            Some(off)
        } else {
            None
        };
        let bps_off = if nbands > 1 {
            let off = self.sink.stream_position()?;
            for band in &self.profile.bands {
                let bits = pixel_type_bits(band.dtype);
                self.sink.write_all(&order(bits))?;
            }
            Some(off)
        } else {
            None
        };

        let (scale_off, tie_off, gk_off) = if level == 0 {
            // ModelPixelScale: [sx, sy, sz]
            let so = self.sink.stream_position()?;
            self.sink
                .write_all(&self.profile.geo_transform.pixel_size[0].to_le_bytes())?;
            self.sink
                .write_all(&self.profile.geo_transform.pixel_size[1].abs().to_le_bytes())?;
            self.sink.write_all(&0.0f64.to_le_bytes())?;
            // ModelTiepoint: [0, 0, 0, origin_x, origin_y, 0]
            let to = self.sink.stream_position()?;
            for v in [
                0.0,
                0.0,
                0.0,
                self.profile.geo_transform.origin[0],
                self.profile.geo_transform.origin[1],
                0.0,
            ] {
                self.sink.write_all(&v.to_le_bytes())?;
            }
            // GeoKeyDirectory: header + 1 entry
            let go = self.sink.stream_position()?;
            let (key_id, code) = match &self.profile.crs {
                Crs::Epsg(c) => {
                    // We can't know whether c is projected or geographic
                    // without an EPSG lookup table. v0.1 heuristic: codes
                    // ≤4999 are usually geographic (4326, 4269, 4979, …)
                    // and 5000+ are usually projected (32633, 3857, 7855…).
                    // Good enough for the dominant case; a proper EPSG
                    // table is a follow-up.
                    if *c <= 4999 {
                        (crate::geokeys::KEY_GEOGRAPHIC_TYPE, *c as u16)
                    } else {
                        (crate::geokeys::KEY_PROJECTED_CS_TYPE, *c as u16)
                    }
                }
                _ => (crate::geokeys::KEY_PROJECTED_CS_TYPE, 0),
            };
            // Header: [1=KeyDirVersion, 1=KeyRevision, 0=MinorRevision, 1=NumKeys]
            for v in [1u16, 1, 0, 1] {
                self.sink.write_all(&order(v))?;
            }
            // Entry: [key_id, loc=0, count=1, value=code]
            for v in [key_id, 0, 1, code] {
                self.sink.write_all(&order(v))?;
            }
            (Some(so), Some(to), Some(go))
        } else {
            (None, None, None)
        };

        // 2) Build the IFD entries.
        let mut entries: Vec<(u16, DType, u32, [u8; 4])> = Vec::new();
        entries.push((tags::IMAGE_WIDTH, DType::Long, 1, w.to_le_bytes()));
        entries.push((tags::IMAGE_LENGTH, DType::Long, 1, h.to_le_bytes()));
        entries.push((
            tags::BITS_PER_SAMPLE,
            DType::Short,
            nbands as u32,
            if nbands == 1 {
                inline_u16(pixel_type_bits(self.profile.bands[0].dtype))
            } else {
                (bps_off.unwrap() as u32).to_le_bytes()
            },
        ));
        entries.push((
            tags::COMPRESSION,
            DType::Short,
            1,
            inline_u16(self.options.compression.tag_value()),
        ));
        entries.push((
            tags::PHOTOMETRIC_INTERPRETATION,
            DType::Short,
            1,
            inline_u16(if nbands >= 3 { 2 } else { 1 }),
        ));
        entries.push((
            tags::SAMPLES_PER_PIXEL,
            DType::Short,
            1,
            inline_u16(nbands as u16),
        ));
        entries.push((tags::TILE_WIDTH, DType::Long, 1, tw.to_le_bytes()));
        entries.push((tags::TILE_LENGTH, DType::Long, 1, th.to_le_bytes()));
        entries.push((
            tags::TILE_OFFSETS,
            DType::Long,
            tile_offsets.len() as u32,
            match to_off {
                Some(off) => (off as u32).to_le_bytes(),
                None => (tile_offsets[0] as u32).to_le_bytes(),
            },
        ));
        entries.push((
            tags::TILE_BYTE_COUNTS,
            DType::Long,
            tile_byte_counts.len() as u32,
            match bc_off {
                Some(off) => (off as u32).to_le_bytes(),
                None => (tile_byte_counts[0] as u32).to_le_bytes(),
            },
        ));
        entries.push((
            tags::SAMPLE_FORMAT,
            DType::Short,
            1,
            inline_u16(pixel_type_sample_format(self.profile.bands[0].dtype)),
        ));
        entries.push((tags::PLANAR_CONFIGURATION, DType::Short, 1, inline_u16(1)));
        if let (Some(so), Some(to), Some(go)) = (scale_off, tie_off, gk_off) {
            entries.push((
                tags::MODEL_PIXEL_SCALE,
                DType::Double,
                3,
                (so as u32).to_le_bytes(),
            ));
            entries.push((
                tags::MODEL_TIEPOINT,
                DType::Double,
                6,
                (to as u32).to_le_bytes(),
            ));
            entries.push((
                tags::GEO_KEY_DIRECTORY,
                DType::Short,
                8,
                (go as u32).to_le_bytes(),
            ));
        }

        // 3) Write the IFD body. Tags must be ascending by tag id.
        entries.sort_by_key(|e| e.0);
        self.sink.write_all(&(entries.len() as u16).to_le_bytes())?;
        for (tag, dtype, count, value) in &entries {
            self.sink.write_all(&order(*tag))?;
            self.sink.write_all(&order(*dtype as u16))?;
            self.sink.write_all(&count.to_le_bytes())?;
            self.sink.write_all(value)?;
        }
        self.sink.write_all(&(next_offset as u32).to_le_bytes())?;
        Ok(())
    }
}

fn pixel_type_bits(t: PixelType) -> u16 {
    match t {
        PixelType::U8 | PixelType::I8 => 8,
        PixelType::U16 | PixelType::I16 => 16,
        PixelType::U32 | PixelType::I32 | PixelType::F32 => 32,
        PixelType::F64 => 64,
        PixelType::Rgb8 => 8,  // 3 bands × 8 bits each
        PixelType::Rgba8 => 8, // 4 bands × 8 bits each
        _ => 8,
    }
}

fn pixel_type_sample_format(t: PixelType) -> u16 {
    match t {
        PixelType::U8 | PixelType::U16 | PixelType::U32 | PixelType::Rgb8 | PixelType::Rgba8 => 1,
        PixelType::I8 | PixelType::I16 | PixelType::I32 => 2,
        PixelType::F32 | PixelType::F64 => 3,
        _ => 1,
    }
}

fn ifd_tag_count(is_level_zero: bool) -> u16 {
    // ImageWidth, ImageLength, BitsPerSample, Compression, PhotometricInterpretation,
    // SamplesPerPixel, TileWidth, TileLength, TileOffsets, TileByteCounts,
    // SampleFormat, PlanarConfiguration = 12
    // Plus MODEL_PIXEL_SCALE + MODEL_TIEPOINT + GEO_KEY_DIRECTORY for level 0 = 15
    if is_level_zero {
        15
    } else {
        12
    }
}

fn inline_u16(v: u16) -> [u8; 4] {
    let mut b = [0u8; 4];
    b[..2].copy_from_slice(&v.to_le_bytes());
    b
}

// --- Codec encoders (mirror the decoders in codec.rs) -------------------

fn encode_packbits(input: &[u8]) -> Vec<u8> {
    // Minimal PackBits: emit runs of identical bytes, otherwise literal copy.
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        // Find run of identical bytes (max 128).
        let cur = input[i];
        let mut run_end = i + 1;
        while run_end < input.len() && run_end - i < 128 && input[run_end] == cur {
            run_end += 1;
        }
        let run_len = run_end - i;
        if run_len >= 3 {
            out.push(1u8.wrapping_sub(run_len as u8));
            out.push(cur);
            i = run_end;
        } else {
            // Emit literal — find longest run of mixed bytes (≤128).
            let lit_start = i;
            let mut k = i;
            while k < input.len() && k - lit_start < 128 {
                // Stop if a long run starts here.
                let rs = k;
                let mut re = k + 1;
                while re < input.len() && re - rs < 3 && input[re] == input[rs] {
                    re += 1;
                }
                if re - rs >= 3 {
                    break;
                }
                k += 1;
            }
            let n = k - lit_start;
            out.push((n as u8) - 1);
            out.extend_from_slice(&input[lit_start..k]);
            i = k;
        }
    }
    out
}

fn encode_deflate(input: &[u8], level: u32) -> Result<Vec<u8>> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression as DflLevel;
    let mut enc = ZlibEncoder::new(Vec::new(), DflLevel::new(level));
    enc.write_all(input)
        .map_err(|e| GtiffError::Deflate(e.to_string()))?;
    enc.finish().map_err(|e| GtiffError::Deflate(e.to_string()))
}

fn encode_lzw(input: &[u8]) -> Result<Vec<u8>> {
    let mut enc = weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8);
    enc.encode(input)
        .map_err(|e| GtiffError::Lzw(format!("{e:?}")))
}

/// Convenience: derive a `RasterProfile` for a single-resolution output
/// from caller-supplied dimensions + geo. Useful when the consumer is
/// converting a non-tiled source.
pub fn profile_for_output(
    width: u32,
    height: u32,
    bands: Vec<geonative_core::raster::BandDescriptor>,
    geo_transform: GeoTransform,
    crs: Crs,
    tile_size: u32,
) -> RasterProfile {
    RasterProfile {
        width,
        height,
        bands,
        geo_transform,
        crs,
        tile_size: [tile_size, tile_size],
        pyramid_levels: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::raster::{Band, BandDescriptor};
    use std::io::Cursor;

    fn small_profile() -> RasterProfile {
        RasterProfile {
            width: 4,
            height: 4,
            bands: vec![BandDescriptor::new(Some("v".into()), PixelType::U8)],
            geo_transform: GeoTransform::north_up(100.0, 200.0, 0.5, 0.5),
            crs: Crs::Epsg(3857),
            tile_size: [2, 2],
            pyramid_levels: 1,
        }
    }

    fn small_tile(fill: u8) -> RasterTile {
        RasterTile {
            width: 2,
            height: 2,
            bands: vec![Band::new(
                BandDescriptor::new(Some("v".into()), PixelType::U8),
                vec![fill; 4],
            )],
            geo_transform: GeoTransform::north_up(100.0, 200.0, 0.5, 0.5),
            crs: Crs::Epsg(3857),
        }
    }

    #[test]
    fn encode_packbits_short_run() {
        let input = b"aaaabc";
        let out = encode_packbits(input);
        // Should produce: repeat-3-a (header -3, byte 'a') + literal "bc"
        assert!(!out.is_empty());
        // Round-trip
        let mut decoded = vec![0u8; input.len()];
        crate::codec::decode_into(cmp::PACKBITS, &out, &mut decoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn encode_lzw_round_trips() {
        let input = b"the quick brown fox jumps over the lazy dog";
        let compressed = encode_lzw(input).unwrap();
        let mut decoded = vec![0u8; input.len()];
        crate::codec::decode_into(cmp::LZW, &compressed, &mut decoded).unwrap();
        assert_eq!(decoded.as_slice(), input);
    }

    #[test]
    fn encode_deflate_round_trips() {
        let input = b"the quick brown fox jumps over the lazy dog";
        let compressed = encode_deflate(input, 6).unwrap();
        let mut decoded = vec![0u8; input.len()];
        crate::codec::decode_into(cmp::DEFLATE, &compressed, &mut decoded).unwrap();
        assert_eq!(decoded.as_slice(), input);
    }

    #[test]
    fn writer_emits_valid_header() {
        let mut buf = Cursor::new(Vec::new());
        let profile = small_profile();
        let mut w = GeoTiffWriter::create(
            &mut buf,
            &profile,
            WriterOptions {
                compression: Compression::None,
                ..WriterOptions::default()
            },
        )
        .unwrap();
        w.write_tile(0, 0, 0, &small_tile(1)).unwrap();
        w.write_tile(0, 1, 0, &small_tile(2)).unwrap();
        w.write_tile(0, 0, 1, &small_tile(3)).unwrap();
        w.write_tile(0, 1, 1, &small_tile(4)).unwrap();
        w.close().unwrap();

        let bytes = buf.into_inner();
        // Header: II + 42 + 4-byte first IFD offset
        assert_eq!(&bytes[0..2], b"II");
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 42);
        let first_ifd = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert!(first_ifd > 8);
        assert!((first_ifd as usize) < bytes.len());
    }
}

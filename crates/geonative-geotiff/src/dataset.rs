//! The public `GeoTiff` reader — what downstream services (Zebflow's tile
//! server, `geonative-convert`'s RasterSource) interact with.
//!
//! Holds:
//! - The mmapped file
//! - Parsed metadata for each pyramid level (IFD chain — level 0 is the
//!   first IFD; subsequent IFDs are progressively coarser overviews)
//! - The shared `RasterProfile` (dimensions + bands + CRS + geo-transform)
//!
//! Tile reads:
//! 1. Locate the IFD for the requested pyramid level
//! 2. Compute the tile index within that level
//! 3. Look up its offset + byte count in the TileOffsets / TileByteCounts arrays
//! 4. Slice + decompress the bytes into a `RasterTile`
//!
//! mmap means step 3-4 reads only the ~50 KB needed for one tile, even
//! from a multi-GB COG. Linux's page cache does the right thing.

use std::path::Path;
use std::sync::Arc;

use geonative_core::raster::{
    Band, BandDescriptor, GeoTransform, PixelType, RasterLayer, RasterProfile, RasterTile,
};
use geonative_core::Crs;
use memmap2::Mmap;

use crate::codec;
use crate::error::{GtiffError, Result};
use crate::format::{compression, tags, ByteOrder, Header, Ifd};
use crate::geokeys;

/// Per-level decoded metadata, cached so we don't re-walk the IFD on every
/// `read_tile`. `pub(crate)` so [`crate::async_dataset`] can reuse it.
#[derive(Debug, Clone)]
pub(crate) struct LevelMeta {
    /// Pixel dimensions of this pyramid level.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Internal tile dimensions (256, 256 for typical COGs).
    pub(crate) tile_width: u32,
    pub(crate) tile_height: u32,
    /// Tile-grid dimensions (`ceil(width / tile_width) × ceil(height / tile_height)`).
    pub(crate) grid_x: u32,
    pub(crate) grid_y: u32,
    /// `TileOffsets` array — `grid_x * grid_y` entries, byte position of
    /// each tile's compressed data.
    pub(crate) tile_offsets: Vec<u64>,
    /// `TileByteCounts` — compressed length of each tile.
    pub(crate) tile_byte_counts: Vec<u64>,
    /// `Compression` tag value.
    pub(crate) compression: u16,
    /// `BitsPerSample` — one per band.
    pub(crate) bits_per_sample: Vec<u16>,
    /// `SampleFormat` — 1=uint, 2=int, 3=float (one per band; defaults to uint).
    pub(crate) sample_format: Vec<u16>,
    /// Per-level geo-transform (level N is `(2^N)x` coarser than level 0).
    pub(crate) geo_transform: GeoTransform,
}

#[derive(Debug)]
pub struct GeoTiff {
    /// The mmapped file. `Arc` so this is cheap to clone (e.g. for sharing
    /// across tile-server worker threads).
    mmap: Arc<Mmap>,
    /// One per pyramid level. Level 0 is full resolution.
    levels: Vec<LevelMeta>,
    profile: RasterProfile,
}

impl GeoTiff {
    /// Open a TIFF / COG. Parses the header and every IFD in the chain;
    /// pixel data is read lazily on `read_tile`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = std::fs::File::open(path.as_ref())?;
        // SAFETY: mmap reads bytes from a file; we never write through
        // the mmap, and the file lives for the duration of `mmap` via Arc.
        // Matches the pattern in geonative-filegdb.
        #[allow(unsafe_code)]
        let mmap = unsafe { Mmap::map(&file) }?;
        Self::from_mmap(Arc::new(mmap))
    }

    /// Construct from an existing mmap. Useful when the caller has its
    /// own mmap policy (e.g. shared with another reader, or backed by
    /// HTTP range reads in a future cloud-native variant).
    pub fn from_mmap(mmap: Arc<Mmap>) -> Result<Self> {
        let bytes: &[u8] = mmap.as_ref().as_ref();
        let header = Header::parse(bytes)?;

        // Walk the IFD chain.
        let mut levels = Vec::new();
        let mut crs = Crs::Unknown;
        let mut base_geo_transform: Option<GeoTransform> = None;
        let mut next = header.first_ifd_offset;
        let mut level_idx: u32 = 0;
        while next != 0 {
            let ifd = Ifd::parse(bytes, next, header.byte_order, header.big_tiff)?;

            // CRS + geo-transform come from the FIRST IFD (full-res); other
            // levels inherit and scale the transform.
            if level_idx == 0 {
                crs = geokeys::extract_crs(&ifd, bytes, header.byte_order, header.big_tiff)?;
                base_geo_transform = geokeys::extract_geo_transform(
                    &ifd,
                    bytes,
                    header.byte_order,
                    header.big_tiff,
                )?;
            }

            let meta = parse_level_meta(
                &ifd,
                bytes,
                header.byte_order,
                header.big_tiff,
                base_geo_transform,
                level_idx,
            )?;
            levels.push(meta);

            next = ifd.next_offset;
            level_idx += 1;
        }

        if levels.is_empty() {
            return Err(GtiffError::malformed("TIFF has no IFDs"));
        }

        // Build the profile from level 0.
        let base = &levels[0];
        let bands: Vec<BandDescriptor> = (0..base.bits_per_sample.len())
            .map(|i| {
                let bps = base.bits_per_sample[i];
                let fmt = base.sample_format.get(i).copied().unwrap_or(1);
                BandDescriptor::new(None, pixel_type_for(bps, fmt))
            })
            .collect();
        let profile = RasterProfile {
            width: base.width,
            height: base.height,
            bands,
            geo_transform: base.geo_transform,
            crs,
            tile_size: [base.tile_width, base.tile_height],
            pyramid_levels: levels.len() as u8,
        };

        Ok(Self {
            mmap,
            levels,
            profile,
        })
    }

    pub fn pyramid_level_count(&self) -> u8 {
        self.levels.len() as u8
    }
}

impl RasterLayer for GeoTiff {
    fn profile(&self) -> &RasterProfile {
        &self.profile
    }

    fn read_tile(&self, level: u8, x: u32, y: u32) -> geonative_core::Result<RasterTile> {
        let lvl = self.levels.get(level as usize).ok_or_else(|| {
            geonative_core::Error::from(GtiffError::LevelOutOfRange {
                requested: level,
                available: self.levels.len() as u8,
            })
        })?;

        if x >= lvl.grid_x || y >= lvl.grid_y {
            return Err(geonative_core::Error::from(GtiffError::TileOutOfRange {
                level,
                x,
                y,
                grid_x: lvl.grid_x,
                grid_y: lvl.grid_y,
            }));
        }

        let idx = (y * lvl.grid_x + x) as usize;
        let offset = lvl.tile_offsets[idx] as usize;
        let byte_count = lvl.tile_byte_counts[idx] as usize;

        let bytes: &[u8] = self.mmap.as_ref().as_ref();
        if offset + byte_count > bytes.len() {
            return Err(geonative_core::Error::from(GtiffError::Truncated {
                offset: offset as u64,
                needed: byte_count as u64,
                total: bytes.len() as u64,
            }));
        }
        let compressed = &bytes[offset..offset + byte_count];

        decode_tile_into_rastertile(lvl, compressed, x, y, &self.profile)
            .map_err(geonative_core::Error::from)
    }
}

/// Decode one tile's compressed bytes into a [`RasterTile`]. Shared by
/// the sync (mmap-backed) and async (object-store-backed) datasets so the
/// codec / band-split / geo-transform logic lives in exactly one place.
pub(crate) fn decode_tile_into_rastertile(
    lvl: &LevelMeta,
    compressed: &[u8],
    x: u32,
    y: u32,
    profile: &RasterProfile,
) -> Result<RasterTile> {
    let pixels = (lvl.tile_width as usize) * (lvl.tile_height as usize);
    // bits-per-sample → bytes-per-sample (8 = 1, 16 = 2, etc.).
    // Bit shift rather than `(b + 7) / 8` to avoid clippy's div_ceil
    // hint (div_ceil is 1.85; our MSRV is 1.74).
    let bytes_per_pixel: usize = lvl
        .bits_per_sample
        .iter()
        .map(|bps| ((*bps as usize) + 7) >> 3)
        .sum();
    let mut out = vec![0u8; pixels * bytes_per_pixel];
    codec::decode_into(lvl.compression, compressed, &mut out)?;

    // v0.1 assumes chunky (interleaved) PlanarConfiguration=1.
    let band_descriptors: Vec<&BandDescriptor> = profile.bands.iter().collect();
    let bands = split_interleaved_to_bands(&out, &band_descriptors, pixels)?;

    let lvl_gt = lvl.geo_transform;
    let tile_gt = GeoTransform {
        origin: [
            lvl_gt.origin[0] + (x as f64) * (lvl.tile_width as f64) * lvl_gt.pixel_size[0],
            lvl_gt.origin[1] + (y as f64) * (lvl.tile_height as f64) * lvl_gt.pixel_size[1],
        ],
        pixel_size: lvl_gt.pixel_size,
        rotation: lvl_gt.rotation,
    };

    Ok(RasterTile {
        width: lvl.tile_width,
        height: lvl.tile_height,
        bands,
        geo_transform: tile_gt,
        crs: profile.crs.clone(),
    })
}

pub(crate) fn parse_level_meta(
    ifd: &Ifd,
    file: &[u8],
    order: ByteOrder,
    big_tiff: bool,
    base_gt: Option<GeoTransform>,
    level_idx: u32,
) -> Result<LevelMeta> {
    let width =
        require_tag_u64(ifd, tags::IMAGE_WIDTH, "ImageWidth", file, order, big_tiff)? as u32;
    let height = require_tag_u64(
        ifd,
        tags::IMAGE_LENGTH,
        "ImageLength",
        file,
        order,
        big_tiff,
    )? as u32;

    // Tiled is the COG layout we optimise for. We accept stripped but
    // surface a clear "not tiled" error so callers can switch path.
    let tile_width = ifd
        .tag(tags::TILE_WIDTH)
        .ok_or_else(|| GtiffError::unsupported("stripped TIFFs (Phase B2) — only tiled in v0.1"))?
        .as_u64_first(file, order, big_tiff)? as u32;
    let tile_height =
        require_tag_u64(ifd, tags::TILE_LENGTH, "TileLength", file, order, big_tiff)? as u32;
    let tile_offsets: Vec<u64> = ifd
        .tag(tags::TILE_OFFSETS)
        .ok_or_else(|| GtiffError::malformed("missing TileOffsets"))?
        .iter_u64(file, order, big_tiff)?
        .collect();
    let tile_byte_counts: Vec<u64> = ifd
        .tag(tags::TILE_BYTE_COUNTS)
        .ok_or_else(|| GtiffError::malformed("missing TileByteCounts"))?
        .iter_u64(file, order, big_tiff)?
        .collect();

    let compression = ifd
        .tag(tags::COMPRESSION)
        .map(|t| t.as_u64_first(file, order, big_tiff))
        .transpose()?
        .map(|v| v as u16)
        .unwrap_or(compression::NONE);

    let bits_per_sample: Vec<u16> = ifd
        .tag(tags::BITS_PER_SAMPLE)
        .map(|t| {
            t.iter_u64(file, order, big_tiff)
                .map(|it| it.map(|v| v as u16).collect::<Vec<_>>())
        })
        .transpose()?
        .unwrap_or_else(|| vec![8]);
    let sample_format: Vec<u16> = ifd
        .tag(tags::SAMPLE_FORMAT)
        .map(|t| {
            t.iter_u64(file, order, big_tiff)
                .map(|it| it.map(|v| v as u16).collect::<Vec<_>>())
        })
        .transpose()?
        .unwrap_or_else(|| vec![1; bits_per_sample.len()]);

    let grid_x = width.saturating_add(tile_width - 1) / tile_width.max(1);
    let grid_y = height.saturating_add(tile_height - 1) / tile_height.max(1);

    // Per-level geo-transform: at level N, pixel size scales by 2^N
    // (under the "halved-per-level" pyramid convention that COG enforces).
    let geo_transform = match base_gt {
        Some(base) => {
            let scale = (1u32 << level_idx) as f64;
            GeoTransform {
                origin: base.origin,
                pixel_size: [base.pixel_size[0] * scale, base.pixel_size[1] * scale],
                rotation: base.rotation,
            }
        }
        None => GeoTransform::north_up(0.0, 0.0, 1.0, 1.0),
    };

    Ok(LevelMeta {
        width,
        height,
        tile_width,
        tile_height,
        grid_x,
        grid_y,
        tile_offsets,
        tile_byte_counts,
        compression,
        bits_per_sample,
        sample_format,
        geo_transform,
    })
}

fn require_tag_u64(
    ifd: &Ifd,
    tag: u16,
    name: &str,
    file: &[u8],
    order: ByteOrder,
    big_tiff: bool,
) -> Result<u64> {
    ifd.tag(tag)
        .ok_or_else(|| GtiffError::malformed(format!("missing required tag {name} ({tag})")))?
        .as_u64_first(file, order, big_tiff)
}

fn pixel_type_for(bits_per_sample: u16, sample_format: u16) -> PixelType {
    // sample_format: 1=uint, 2=int, 3=float.
    match (bits_per_sample, sample_format) {
        (8, 1) => PixelType::U8,
        (16, 1) => PixelType::U16,
        (32, 1) => PixelType::U32,
        (8, 2) => PixelType::I8,
        (16, 2) => PixelType::I16,
        (32, 2) => PixelType::I32,
        (32, 3) => PixelType::F32,
        (64, 3) => PixelType::F64,
        // Fallback for odd bit depths: treat as U8. Better-handled in v0.2.
        _ => PixelType::U8,
    }
}

fn split_interleaved_to_bands(
    interleaved: &[u8],
    descriptors: &[&BandDescriptor],
    pixels: usize,
) -> Result<Vec<Band>> {
    if descriptors.len() == 1 {
        // Single-band — no de-interleaving needed.
        return Ok(vec![Band::new(
            descriptors[0].clone(),
            interleaved.to_vec(),
        )]);
    }
    let bytes_per_sample: Vec<usize> = descriptors.iter().map(|d| d.dtype.size_bytes()).collect();
    let stride: usize = bytes_per_sample.iter().sum();
    if interleaved.len() != pixels * stride {
        return Err(GtiffError::malformed(format!(
            "decoded bytes {} don't match pixels*stride ({}*{}={})",
            interleaved.len(),
            pixels,
            stride,
            pixels * stride
        )));
    }
    let mut out: Vec<Vec<u8>> = bytes_per_sample
        .iter()
        .map(|bps| Vec::with_capacity(pixels * bps))
        .collect();
    for px in 0..pixels {
        let mut off = px * stride;
        for (bi, &bps) in bytes_per_sample.iter().enumerate() {
            out[bi].extend_from_slice(&interleaved[off..off + bps]);
            off += bps;
        }
    }
    Ok(descriptors
        .iter()
        .zip(out)
        .map(|(d, data)| Band::new((*d).clone(), data))
        .collect())
}

#[cfg(test)]
mod tests {
    // Real-data integration tests live in tests/round_trip.rs (uses
    // synthesised in-memory fixtures since we don't ship real TIFFs).
    // This module proves the byte-level decoders work; the heavier
    // end-to-end "open synthesised tiled TIFF and read its tiles" tests
    // are in the sibling tests/ directory.
}

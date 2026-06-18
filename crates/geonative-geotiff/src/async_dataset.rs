//! Async, object-store-backed COG reader.
//!
//! Mirror of [`crate::dataset::GeoTiff`] for the case where the COG lives
//! in S3 / Azure Blob / GCS / R2 / HTTP. Range-fetches the header + all
//! IFDs at open (one or two GETs typically — COG metadata is dense and
//! sits at the start of the file), then per-tile range-fetches at
//! [`AsyncCog::read_tile`].
//!
//! ## Why range-read works for COGs
//!
//! The Cloud Optimized GeoTIFF spec mandates that **all IFDs live before
//! any pixel data**, and that IFDs and their out-of-line tag values are
//! contiguous near the start of the file. So one GET of the first ~256 KB
//! is enough to read metadata for the vast majority of real-world COGs,
//! including all overview IFDs. Tile bytes are then fetched individually
//! by absolute `(offset, length)` from `TileOffsets` / `TileByteCounts`
//! — typically one GET per tile request.
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use object_store::aws::AmazonS3Builder;
//! use geonative_geotiff::AsyncCog;
//!
//! let store: Arc<dyn object_store::ObjectStore> = Arc::new(
//!     AmazonS3Builder::new()
//!         .with_bucket_name("my-bucket")
//!         .with_region("ap-southeast-2")
//!         .build()?
//! );
//! let cog = AsyncCog::open(store, "rasters/dem.cog".into()).await?;
//! let tile = cog.read_tile(0, 12, 7).await?;
//! ```

use std::sync::Arc;

use geonative_core::raster::{BandDescriptor, RasterProfile, RasterTile};
use geonative_core::Crs;
use object_store::{path::Path as OsPath, ObjectStore, ObjectStoreExt};

use crate::dataset::{decode_tile_into_rastertile, parse_level_meta, LevelMeta};
use crate::error::{GtiffError, Result};
use crate::format::{Header, Ifd};
use crate::geokeys;

/// Initial metadata fetch size. Covers headers + IFDs + most out-of-line
/// tag arrays for typical COGs. We extend the buffer lazily if a tag's
/// values lie beyond this region.
const INITIAL_FETCH_BYTES: u64 = 256 * 1024;
/// Slop fetched on each extension so we don't go back to the network for
/// every tiny missing slice.
const EXTEND_SLOP_BYTES: u64 = 64 * 1024;

/// Async, range-fetching COG reader. Holds parsed metadata plus a handle
/// back to the object store for lazy per-tile reads.
pub struct AsyncCog {
    store: Arc<dyn ObjectStore>,
    path: OsPath,
    file_size: u64,
    levels: Vec<LevelMeta>,
    profile: RasterProfile,
}

impl std::fmt::Debug for AsyncCog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncCog")
            .field("path", &self.path.to_string())
            .field("file_size", &self.file_size)
            .field("levels", &self.levels.len())
            .field("size_px", &(self.profile.width, self.profile.height))
            .finish()
    }
}

impl AsyncCog {
    /// Open the COG at `path` in `store`. Issues:
    /// - 1 HEAD to learn file size
    /// - 1 GET of the first `INITIAL_FETCH_BYTES` (or less for small files)
    /// - Possibly 1+ extension GETs if a tag's out-of-line values lie
    ///   beyond the initial prefix
    pub async fn open(store: Arc<dyn ObjectStore>, path: OsPath) -> Result<Self> {
        let meta = store.head(&path).await.map_err(|e| {
            GtiffError::malformed(format!("object_store head: {e}"))
        })?;
        let file_size: u64 = meta.size;

        // Initial fetch.
        let initial_end = INITIAL_FETCH_BYTES.min(file_size);
        let mut bytes = fetch_range(&store, &path, 0, initial_end).await?;

        // Parse header.
        let header = Header::parse(&bytes)?;

        // Walk the IFD chain, extending `bytes` on demand.
        let mut levels = Vec::new();
        let mut crs = Crs::Unknown;
        let mut base_geo_transform = None;
        let mut next = header.first_ifd_offset;
        let mut level_idx: u32 = 0;
        while next != 0 {
            // Ensure we have at least the IFD header bytes (a coarse upper
            // bound — IFDs are bounded by `count * 20 + 16` bytes, but for
            // simplicity we just guarantee 16 KB headroom past `next`).
            let need_for_ifd_header = next.saturating_add(16 * 1024);
            ensure_prefix(&store, &path, &mut bytes, need_for_ifd_header, file_size).await?;

            let ifd = Ifd::parse(&bytes, next, header.byte_order, header.big_tiff)?;

            // Compute the furthest byte any out-of-line tag in this IFD
            // points to, and extend if needed before doing tag reads.
            if let Some(max_tag_end) =
                max_out_of_line_end(&ifd, header.big_tiff, header.byte_order)
            {
                ensure_prefix(&store, &path, &mut bytes, max_tag_end, file_size).await?;
            }

            if level_idx == 0 {
                crs = geokeys::extract_crs(&ifd, &bytes, header.byte_order, header.big_tiff)?;
                base_geo_transform = geokeys::extract_geo_transform(
                    &ifd,
                    &bytes,
                    header.byte_order,
                    header.big_tiff,
                )?;
            }

            let lvl = parse_level_meta(
                &ifd,
                &bytes,
                header.byte_order,
                header.big_tiff,
                base_geo_transform,
                level_idx,
            )?;
            levels.push(lvl);

            next = ifd.next_offset;
            level_idx += 1;
        }

        if levels.is_empty() {
            return Err(GtiffError::malformed("TIFF has no IFDs"));
        }

        // Build profile from level 0 — same shape as the sync constructor.
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
            store,
            path,
            file_size,
            levels,
            profile,
        })
    }

    pub fn profile(&self) -> &RasterProfile {
        &self.profile
    }

    pub fn pyramid_level_count(&self) -> u8 {
        self.levels.len() as u8
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Range-fetch one tile and decode it. Single GET (+ in-memory decode).
    pub async fn read_tile(&self, level: u8, x: u32, y: u32) -> Result<RasterTile> {
        let lvl = self
            .levels
            .get(level as usize)
            .ok_or_else(|| GtiffError::LevelOutOfRange {
                requested: level,
                available: self.levels.len() as u8,
            })?;

        if x >= lvl.grid_x || y >= lvl.grid_y {
            return Err(GtiffError::TileOutOfRange {
                level,
                x,
                y,
                grid_x: lvl.grid_x,
                grid_y: lvl.grid_y,
            });
        }

        let idx = (y * lvl.grid_x + x) as usize;
        let offset = lvl.tile_offsets[idx];
        let byte_count = lvl.tile_byte_counts[idx];
        if byte_count == 0 {
            return Err(GtiffError::malformed(format!(
                "tile ({x},{y}) at level {level} has zero byte_count"
            )));
        }
        let end = offset
            .checked_add(byte_count)
            .ok_or_else(|| GtiffError::malformed("tile offset overflow"))?;
        if end > self.file_size {
            return Err(GtiffError::Truncated {
                offset,
                needed: byte_count,
                total: self.file_size,
            });
        }

        let compressed = fetch_range(&self.store, &self.path, offset, end).await?;
        decode_tile_into_rastertile(lvl, &compressed, x, y, &self.profile)
    }
}

/// Fetch `[start, end)` and return it as a `Vec<u8>`. The conversion costs
/// one copy out of the `Bytes` blob — small relative to the network GET.
async fn fetch_range(
    store: &Arc<dyn ObjectStore>,
    path: &OsPath,
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    if end <= start {
        return Ok(Vec::new());
    }
    let bytes = store
        .get_range(path, start..end)
        .await
        .map_err(|e| GtiffError::malformed(format!("object_store get_range: {e}")))?;
    Ok(bytes.to_vec())
}

/// Ensure `buf` covers `[0..need)`, extending with one extra GET if not.
/// We always grow from the current tail to keep absolute offsets valid.
async fn ensure_prefix(
    store: &Arc<dyn ObjectStore>,
    path: &OsPath,
    buf: &mut Vec<u8>,
    need: u64,
    file_size: u64,
) -> Result<()> {
    let need = need.min(file_size);
    if (buf.len() as u64) >= need {
        return Ok(());
    }
    let start = buf.len() as u64;
    let end = (need + EXTEND_SLOP_BYTES).min(file_size);
    let extra = fetch_range(store, path, start, end).await?;
    buf.extend_from_slice(&extra);
    Ok(())
}

/// Maximum file offset reached by any out-of-line tag value in `ifd`, or
/// `None` if every value fits inline. Used to decide whether we need to
/// extend the metadata buffer before parsing tags.
fn max_out_of_line_end(
    ifd: &Ifd,
    big_tiff: bool,
    order: crate::format::ByteOrder,
) -> Option<u64> {
    let inline_cap: u64 = if big_tiff { 8 } else { 4 };
    let mut max_end: Option<u64> = None;
    for entry in &ifd.entries {
        let total = entry.count.checked_mul(entry.dtype.size() as u64)?;
        if total <= inline_cap {
            continue;
        }
        let value_offset = if big_tiff {
            order.u64(&entry.value_bytes)
        } else {
            order.u32(&entry.value_bytes[0..4]) as u64
        };
        let end = value_offset.checked_add(total)?;
        max_end = Some(max_end.map_or(end, |m| m.max(end)));
    }
    max_end
}

fn pixel_type_for(bits_per_sample: u16, sample_format: u16) -> geonative_core::raster::PixelType {
    use geonative_core::raster::PixelType;
    match (bits_per_sample, sample_format) {
        (8, 1) => PixelType::U8,
        (16, 1) => PixelType::U16,
        (32, 1) => PixelType::U32,
        (8, 2) => PixelType::I8,
        (16, 2) => PixelType::I16,
        (32, 2) => PixelType::I32,
        (32, 3) => PixelType::F32,
        (64, 3) => PixelType::F64,
        _ => PixelType::U8,
    }
}

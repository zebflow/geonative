//! Async, object-store-backed PMTiles reader.
//!
//! Mirror of [`crate::reader::PmTilesReader`] for S3 / Azure Blob / GCS /
//! R2 / HTTP-hosted PMTiles. Range-fetches the 127-byte header at open,
//! then the root directory, then on each tile read fetches just the bytes
//! of that one tile (and lazily the leaf directory if the file uses leaves).
//!
//! For a typical Zebflow-scale tileset (~100 MB PMTiles, ~50k tiles),
//! cold-start tile lookup is **3 GETs total**: header + root + tile. Warm
//! lookups are 1 GET (header + root cached in the reader).
//!
//! ## Usage
//!
//! ```ignore
//! use std::sync::Arc;
//! use object_store::aws::AmazonS3Builder;
//! use geonative_pmtiles::PmTilesAsyncReader;
//!
//! let store: Arc<dyn object_store::ObjectStore> = Arc::new(
//!     AmazonS3Builder::new()
//!         .with_bucket_name("my-bucket").with_region("ap-southeast-2").build()?
//! );
//! let reader = PmTilesAsyncReader::open(store, "tiles/state.pmtiles".into()).await?;
//! let mvt_bytes = reader.get_tile(10, 512, 384).await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use object_store::{path::Path as OsPath, ObjectStore, ObjectStoreExt};

use crate::codec;
use crate::directory::{self, Entry};
use crate::error::{PmtilesError, Result};
use crate::header::{Header, HEADER_LEN};
use crate::tileid::coords_to_tile_id;

/// Async PMTiles reader.
pub struct PmTilesAsyncReader {
    store: Arc<dyn ObjectStore>,
    path: OsPath,
    header: Header,
    root: Vec<Entry>,
    /// Leaf cache shared across `get_tile` calls. Locked behind a Mutex
    /// because `get_tile` takes `&self` to be callable from concurrent
    /// tasks (e.g. a tile server fanning out a batch).
    leaf_cache: Mutex<HashMap<(u64, u32), Vec<Entry>>>,
}

impl std::fmt::Debug for PmTilesAsyncReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PmTilesAsyncReader")
            .field("path", &self.path.to_string())
            .field("root_entries", &self.root.len())
            .field("addressed_tiles", &self.header.addressed_tiles_count)
            .finish()
    }
}

impl PmTilesAsyncReader {
    /// Open the PMTiles archive at `path`. Issues 2 GETs (header + root dir).
    pub async fn open(store: Arc<dyn ObjectStore>, path: OsPath) -> Result<Self> {
        // 1. Header (first 127 bytes).
        let header_bytes = store.get_range(&path, 0..HEADER_LEN as u64).await?.to_vec();
        let header = Header::parse(&header_bytes)?;

        // 2. Root directory.
        let root_end = header.root_dir_offset + header.root_dir_length;
        let root_raw = store
            .get_range(&path, header.root_dir_offset..root_end)
            .await?
            .to_vec();
        let root_bytes = codec::decompress(&root_raw, header.internal_compression)?;
        let root = directory::decode(&root_bytes)?;

        Ok(Self {
            store,
            path,
            header,
            root,
            leaf_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Fetch the optional JSON metadata blob (decompressed).
    pub async fn metadata(&self) -> Result<Vec<u8>> {
        if self.header.json_metadata_length == 0 {
            return Ok(Vec::new());
        }
        let end = self.header.json_metadata_offset + self.header.json_metadata_length;
        let raw = self
            .store
            .get_range(&self.path, self.header.json_metadata_offset..end)
            .await?
            .to_vec();
        codec::decompress(&raw, self.header.internal_compression)
    }

    /// Fetch the bytes for tile `(z, x, y)`, or `None` if absent.
    pub async fn get_tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let tile_id = coords_to_tile_id(z, x, y)?;

        let root_entry = match directory::find_tile(&self.root, tile_id) {
            Some(e) => *e,
            None => return Ok(None),
        };

        if root_entry.run_length > 0 {
            return Ok(Some(self.fetch_tile_bytes(&root_entry).await?));
        }

        // Leaf dir pointer.
        let leaf = self.fetch_leaf(&root_entry).await?;
        let leaf_entry = match directory::find_tile(&leaf, tile_id) {
            Some(e) => *e,
            None => return Ok(None),
        };
        if leaf_entry.run_length == 0 {
            return Err(PmtilesError::unsupported(
                "nested leaf directories — not supported in v0",
            ));
        }
        Ok(Some(self.fetch_tile_bytes(&leaf_entry).await?))
    }

    async fn fetch_tile_bytes(&self, entry: &Entry) -> Result<Vec<u8>> {
        let start = self.header.tile_data_offset + entry.offset;
        let end = start + entry.length as u64;
        let bytes = self.store.get_range(&self.path, start..end).await?;
        Ok(bytes.to_vec())
    }

    async fn fetch_leaf(&self, ptr: &Entry) -> Result<Vec<Entry>> {
        let key = (ptr.offset, ptr.length);
        {
            let cache = self.leaf_cache.lock().await;
            if let Some(cached) = cache.get(&key) {
                return Ok(cached.clone());
            }
        }
        let start = self.header.leaf_dirs_offset + ptr.offset;
        let end = start + ptr.length as u64;
        let raw = self.store.get_range(&self.path, start..end).await?.to_vec();
        let bytes = codec::decompress(&raw, self.header.internal_compression)?;
        let entries = directory::decode(&bytes)?;
        self.leaf_cache.lock().await.insert(key, entries.clone());
        Ok(entries)
    }
}

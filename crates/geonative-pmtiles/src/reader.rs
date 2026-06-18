//! Sync PMTiles v3 reader.
//!
//! Opens a PMTiles file (any `Read + Seek` sink — typically a `File`),
//! parses the 127-byte header + root directory, and serves `get_tile`
//! lookups. Leaf directories are fetched on demand only when a tile_id
//! falls into a leaf-pointer entry in the root dir.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::codec;
use crate::directory::{self, Entry};
use crate::error::{PmtilesError, Result};
use crate::header::{Header, HEADER_LEN};
use crate::tileid::coords_to_tile_id;

/// PMTiles file reader.
pub struct PmTilesReader<R: Read + Seek> {
    inner: R,
    header: Header,
    root: Vec<Entry>,
    /// Cache of leaf directories already pulled. Keyed by `(offset, length)`
    /// because two root-dir pointers could (legally) collide on offset if
    /// the file is hand-crafted; we want exact-match cache hits.
    leaf_cache: HashMap<(u64, u32), Vec<Entry>>,
}

impl<R: Read + Seek> std::fmt::Debug for PmTilesReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PmTilesReader")
            .field("addressed_tiles", &self.header.addressed_tiles_count)
            .field("tile_entries", &self.header.tile_entries_count)
            .field("unique_tiles", &self.header.tile_contents_count)
            .field("root_entries", &self.root.len())
            .field("leaf_cache", &self.leaf_cache.len())
            .finish()
    }
}

impl PmTilesReader<std::fs::File> {
    /// Convenience: open from a filesystem path.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::from_reader(file)
    }
}

impl<R: Read + Seek> PmTilesReader<R> {
    pub fn from_reader(mut inner: R) -> Result<Self> {
        let header = read_header(&mut inner)?;

        let raw_root = read_range(
            &mut inner,
            header.root_dir_offset,
            header.root_dir_length as usize,
        )?;
        let root_bytes = codec::decompress(&raw_root, header.internal_compression)?;
        let root = directory::decode(&root_bytes)?;

        Ok(Self {
            inner,
            header,
            root,
            leaf_cache: HashMap::new(),
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Read the optional JSON metadata blob. Decompressed bytes — empty
    /// vec if the file's metadata is zero-length.
    pub fn metadata(&mut self) -> Result<Vec<u8>> {
        if self.header.json_metadata_length == 0 {
            return Ok(Vec::new());
        }
        let raw = read_range(
            &mut self.inner,
            self.header.json_metadata_offset,
            self.header.json_metadata_length as usize,
        )?;
        codec::decompress(&raw, self.header.internal_compression)
    }

    /// Fetch the bytes for tile `(z, x, y)`, or `None` if the file
    /// doesn't contain that tile.
    pub fn get_tile(&mut self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>> {
        let tile_id = coords_to_tile_id(z, x, y)?;

        // 1. Search root dir.
        let root_entry = match directory::find_tile(&self.root, tile_id) {
            Some(e) => *e,
            None => return Ok(None),
        };

        // 2. If it's a real tile entry, fetch + return.
        if root_entry.run_length > 0 {
            return Ok(Some(self.read_tile_bytes(&root_entry)?));
        }

        // 3. Else it's a leaf-dir pointer. Load (cached) and search again.
        let leaf = self.load_leaf(&root_entry)?;
        let leaf_entry = match directory::find_tile(&leaf, tile_id) {
            Some(e) => *e,
            None => return Ok(None),
        };
        if leaf_entry.run_length == 0 {
            // Nested leaves aren't disallowed by the spec but are extremely
            // rare; v0 sees them as malformed to keep the lookup loop
            // bounded.
            return Err(PmtilesError::unsupported(
                "nested leaf directories — not supported in v0",
            ));
        }
        Ok(Some(self.read_tile_bytes(&leaf_entry)?))
    }

    fn read_tile_bytes(&mut self, entry: &Entry) -> Result<Vec<u8>> {
        let abs_offset = self.header.tile_data_offset + entry.offset;
        read_range(&mut self.inner, abs_offset, entry.length as usize)
    }

    fn load_leaf(&mut self, ptr: &Entry) -> Result<Vec<Entry>> {
        let key = (ptr.offset, ptr.length);
        if let Some(cached) = self.leaf_cache.get(&key) {
            return Ok(cached.clone());
        }
        let abs = self.header.leaf_dirs_offset + ptr.offset;
        let raw = read_range(&mut self.inner, abs, ptr.length as usize)?;
        let bytes = codec::decompress(&raw, self.header.internal_compression)?;
        let entries = directory::decode(&bytes)?;
        self.leaf_cache.insert(key, entries.clone());
        Ok(entries)
    }
}

fn read_header<R: Read + Seek>(inner: &mut R) -> Result<Header> {
    let mut buf = [0u8; HEADER_LEN];
    inner.seek(SeekFrom::Start(0))?;
    inner.read_exact(&mut buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => PmtilesError::Truncated {
            offset: 0,
            needed: HEADER_LEN as u64,
            total: 0,
        },
        _ => PmtilesError::Io(e),
    })?;
    Header::parse(&buf)
}

fn read_range<R: Read + Seek>(inner: &mut R, offset: u64, length: usize) -> Result<Vec<u8>> {
    inner.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; length];
    inner.read_exact(&mut buf)?;
    Ok(buf)
}

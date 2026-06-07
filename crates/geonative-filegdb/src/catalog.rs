//! Geodatabase catalog: enumerate user-facing layers by parsing
//! `a00000001.gdbtable` (GDB_SystemCatalog).
//!
//! ## How a `.gdb` directory names files
//!
//! Every table — system tables and user feature classes alike — is a row in
//! `GDB_SystemCatalog`. The **row's FID determines the physical filename**:
//! FID N → `a{N:08x}.gdbtable` (lower-case hex, 8-digit, zero-padded).
//! GDB_SystemCatalog itself is FID 1 (`a00000001.gdbtable`).
//!
//! The catalog table has three fields: `ID` (OBJECTID), `Name` (String),
//! `FileFormat` (Int32). For v0.1 we use `Name` to identify the layer and
//! the FID to construct the physical path.
//!
//! ## What we filter
//!
//! Names starting with `GDB_` are system tables (`GDB_SystemCatalog`,
//! `GDB_DBTune`, `GDB_SpatialRefs`, `GDB_Items`, etc.). We hide them from
//! the user-layer list. Everything else is exposed as a candidate layer.
//!
//! ## What v0.1 doesn't do
//!
//! Richer metadata — dataset type discriminator (feature class vs table vs
//! relationship class), parent feature-dataset path, declared SRS — lives in
//! `GDB_Items` (`a00000004.gdbtable`) inside an XML `Definition` blob.
//! Parsing that needs an XML dep and is deferred to v0.2.

use std::path::{Path, PathBuf};

use geonative_core::Value;

use crate::error::{GdbError, Result};
use crate::row::{decode_row_blob, slice_row_blob};
use crate::table::Table;
use crate::tablx::Tablx;

/// Lightweight layer descriptor produced by reading the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    /// Friendly name as declared in `GDB_SystemCatalog`. May be e.g.
    /// `FOI_LINE` or `featuredataset\layer` depending on how the layer is
    /// nested.
    pub name: String,
    /// Catalog FID — also the integer used to build the physical filename.
    pub fid: i64,
    /// Filename within the `.gdb` directory, **without** the path prefix.
    /// E.g. `a00000009.gdbtable`.
    pub physical_filename: String,
}

impl LayerInfo {
    /// Resolve the absolute path of the `.gdbtable` file in `gdb_dir`.
    pub fn table_path(&self, gdb_dir: &Path) -> PathBuf {
        gdb_dir.join(&self.physical_filename)
    }

    /// Resolve the absolute path of the `.gdbtablx` row-offset index.
    pub fn tablx_path(&self, gdb_dir: &Path) -> PathBuf {
        let mut p = self.table_path(gdb_dir);
        p.set_extension("gdbtablx");
        p
    }
}

/// Format the `aNNNNNNNN.gdbtable` filename for a given catalog FID.
pub fn physical_filename_for_fid(fid: i64) -> String {
    format!("a{:08x}.gdbtable", fid)
}

/// Parse `GDB_SystemCatalog` bytes and return the list of **user** layers
/// (system `GDB_*` tables filtered out).
///
/// Pass the raw bytes of `a00000001.gdbtable` and `a00000001.gdbtablx`.
pub fn read_catalog(
    catalog_table_bytes: &[u8],
    catalog_tablx_bytes: &[u8],
) -> Result<Vec<LayerInfo>> {
    let table = Table::parse(catalog_table_bytes)?;
    let tx = Tablx::parse(catalog_tablx_bytes)?;

    let name_idx = table
        .field_section
        .fields
        .iter()
        .position(|f| f.name == "Name")
        .ok_or_else(|| GdbError::malformed("GDB_SystemCatalog has no 'Name' field"))?;

    let mut layers = Vec::new();
    for (row_idx, off) in tx.iter_present() {
        let fid = (row_idx as i64) + 1;
        let blob = slice_row_blob(catalog_table_bytes, off)?;
        let row = decode_row_blob(blob, fid, &table.field_section)?;

        let name = match row.values.get(name_idx) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) => continue, // unnamed catalog row — skip
            _ => continue,
        };

        if is_system_table_name(&name) {
            continue;
        }

        layers.push(LayerInfo {
            name,
            fid,
            physical_filename: physical_filename_for_fid(fid),
        });
    }

    Ok(layers)
}

/// Open a `.gdb` directory and read its catalog.
pub fn open_geodatabase(gdb_dir: impl AsRef<Path>) -> Result<Vec<LayerInfo>> {
    let dir = gdb_dir.as_ref();
    let table_bytes = std::fs::read(dir.join("a00000001.gdbtable"))?;
    let tablx_bytes = std::fs::read(dir.join("a00000001.gdbtablx"))?;
    read_catalog(&table_bytes, &tablx_bytes)
}

/// System-table name predicate. Convention is the `GDB_` prefix.
fn is_system_table_name(name: &str) -> bool {
    name.starts_with("GDB_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_formatting() {
        assert_eq!(physical_filename_for_fid(1), "a00000001.gdbtable");
        assert_eq!(physical_filename_for_fid(9), "a00000009.gdbtable");
        assert_eq!(physical_filename_for_fid(37), "a00000025.gdbtable");
        assert_eq!(physical_filename_for_fid(0xABCDEF), "a00abcdef.gdbtable");
    }

    #[test]
    fn system_table_filter() {
        assert!(is_system_table_name("GDB_SystemCatalog"));
        assert!(is_system_table_name("GDB_Items"));
        assert!(is_system_table_name("GDB_DBTune"));
        assert!(!is_system_table_name("FOI_LINE"));
        assert!(!is_system_table_name("RoadCentreline"));
        assert!(!is_system_table_name("")); // empty isn't system, but caller skips empties
    }

    #[test]
    fn layer_info_paths() {
        let li = LayerInfo {
            name: "FOI_LINE".into(),
            fid: 9,
            physical_filename: "a00000009.gdbtable".into(),
        };
        let dir = Path::new("/data/My.gdb");
        assert_eq!(li.table_path(dir), Path::new("/data/My.gdb/a00000009.gdbtable"));
        assert_eq!(li.tablx_path(dir), Path::new("/data/My.gdb/a00000009.gdbtablx"));
    }
}

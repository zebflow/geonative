//! Write a `.geonative.json` sidecar describing a dataset. The sidecar is
//! the JSON serialisation of [`DatasetInspection`] plus a small envelope
//! (generator + spec_version) so downstream tooling can detect breaking
//! shape changes without parsing the body.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::Result;
use crate::inspect::{self, DatasetInspection};

/// What downstream tools see in `.geonative.json`.
#[derive(Debug, Serialize)]
pub struct Sidecar {
    pub generator: &'static str,
    pub generator_version: &'static str,
    /// Sidecar schema version. Bumped only on breaking shape changes.
    pub spec_version: u32,
    #[serde(flatten)]
    pub inspection: DatasetInspection,
}

/// Append `.geonative.json` to the source path.
pub fn default_sidecar_path(source: &Path) -> PathBuf {
    let mut s = source.as_os_str().to_owned();
    s.push(".geonative.json");
    PathBuf::from(s)
}

pub fn build(source: &Path) -> Result<Sidecar> {
    let inspection = inspect::inspect(source)?;
    Ok(Sidecar {
        generator: "geonative-convert",
        generator_version: env!("CARGO_PKG_VERSION"),
        spec_version: 1,
        inspection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_appends() {
        let p = default_sidecar_path(Path::new("/tmp/foo.parquet"));
        assert_eq!(p, PathBuf::from("/tmp/foo.parquet.geonative.json"));
        let p = default_sidecar_path(Path::new("data/x.gdb"));
        assert_eq!(p, PathBuf::from("data/x.gdb.geonative.json"));
    }
}

//! Gzip-only compress/decompress for PMTiles directories + tile bytes.
//!
//! The spec allows None / Gzip / Brotli / Zstd. Gzip is the universal
//! default — every PMTiles in the wild uses it, and it's the only one we
//! ship in v0. Adding Zstd later is a one-line dep + match arm.

use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression as FlateLevel;

use crate::error::{PmtilesError, Result};
use crate::header::Compression;

/// Compress `data` per `kind`. `Compression::Unknown` is treated as `None`
/// (the spec lets readers reject it; here we just pass through for tests).
pub fn compress(data: &[u8], kind: Compression) -> Result<Vec<u8>> {
    match kind {
        Compression::None | Compression::Unknown => Ok(data.to_vec()),
        Compression::Gzip => {
            let mut enc = GzEncoder::new(Vec::new(), FlateLevel::default());
            enc.write_all(data)?;
            Ok(enc.finish()?)
        }
        Compression::Brotli => Err(PmtilesError::unsupported(
            "Brotli compression (PMTiles spec value 3) — add the `brotli` crate dep to enable",
        )),
        Compression::Zstd => Err(PmtilesError::unsupported(
            "Zstd compression (PMTiles spec value 4) — add the `zstd` crate dep to enable",
        )),
    }
}

pub fn decompress(data: &[u8], kind: Compression) -> Result<Vec<u8>> {
    match kind {
        Compression::None | Compression::Unknown => Ok(data.to_vec()),
        Compression::Gzip => {
            let mut dec = GzDecoder::new(data);
            let mut out = Vec::with_capacity(data.len() * 2);
            dec.read_to_end(&mut out)
                .map_err(|e| PmtilesError::Decompress(e.to_string()))?;
            Ok(out)
        }
        Compression::Brotli | Compression::Zstd => Err(PmtilesError::unsupported(format!(
            "{:?} decompression — not built into v0",
            kind
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_roundtrip() {
        let raw = b"the quick brown fox jumps over the lazy dog".repeat(50);
        let comp = compress(&raw, Compression::Gzip).unwrap();
        assert!(comp.len() < raw.len()); // compressed should be smaller
        let back = decompress(&comp, Compression::Gzip).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn none_is_identity() {
        let raw = b"hello";
        assert_eq!(compress(raw, Compression::None).unwrap(), raw);
        assert_eq!(decompress(raw, Compression::None).unwrap(), raw);
    }

    #[test]
    fn brotli_zstd_error_clearly() {
        let raw = b"hello";
        assert!(matches!(
            compress(raw, Compression::Brotli),
            Err(PmtilesError::Unsupported(_))
        ));
        assert!(matches!(
            decompress(raw, Compression::Zstd),
            Err(PmtilesError::Unsupported(_))
        ));
    }
}

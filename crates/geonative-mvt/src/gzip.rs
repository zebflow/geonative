//! Optional gzip helpers — available when the `gzip` feature is enabled.
//!
//! The MVT spec expects tiles to arrive gzip-compressed at the HTTP layer
//! (`Content-Encoding: gzip`). Tile servers therefore almost always wrap the
//! raw protobuf output in gzip. Pulling in `flate2` is opt-in via Cargo
//! feature so users who handle their own compression (e.g. via an HTTP layer
//! that compresses transparently) don't pay for it.

use std::io::Write;

use flate2::write::GzEncoder;
pub use flate2::Compression;

/// Compress `bytes` with gzip at the default compression level (6).
pub fn gzip_compress(bytes: &[u8]) -> Vec<u8> {
    gzip_compress_with(bytes, Compression::default())
}

/// Compress `bytes` with gzip at the given compression level.
///
/// `flate2::Compression::default()` is level 6 — a good size/speed tradeoff.
/// Use [`Compression::fast`] (level 1) for tile rendering hot paths where
/// every millisecond counts.
pub fn gzip_compress_with(bytes: &[u8], level: Compression) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::with_capacity(bytes.len() / 3), level);
    enc.write_all(bytes)
        .expect("gzip write to Vec is infallible");
    enc.finish().expect("gzip finish to Vec is infallible")
}

/// Decompress gzip-encoded bytes. Returns [`MvtError::Unsupported`] on
/// truncated or otherwise invalid input.
pub fn gzip_decompress(bytes: &[u8]) -> crate::Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut dec = GzDecoder::new(bytes);
    let mut out = Vec::with_capacity(bytes.len() * 3);
    dec.read_to_end(&mut out)
        .map_err(|e| crate::MvtError::Unsupported(format!("gzip decode: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_a_small_payload() {
        let payload = b"hello world hello world hello world";
        let gz = gzip_compress(payload);
        // gzip header is 0x1F 0x8B
        assert_eq!(&gz[..2], &[0x1F, 0x8B]);
        let back = gzip_decompress(&gz).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn round_trip_empty() {
        let gz = gzip_compress(b"");
        let back = gzip_decompress(&gz).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn decompress_garbage_errors() {
        let r = gzip_decompress(&[1, 2, 3, 4, 5]);
        assert!(r.is_err());
    }

    #[test]
    fn fast_level_works() {
        let payload = b"the quick brown fox jumps over the lazy dog".repeat(50);
        let gz = gzip_compress_with(&payload, Compression::fast());
        assert!(gz.len() < payload.len());
        assert_eq!(gzip_decompress(&gz).unwrap(), payload);
    }
}

//! TIFF compression codecs.
//!
//! ## Real-world distribution
//!
//! - **DEFLATE / Adobe DEFLATE** (8 / 32946) — dominant in modern COGs;
//!   used by USGS, NASA, Sentinel, and `rio cogeo` output by default
//! - **LZW** (5) — second most common; classic GeoTIFF default
//! - **PackBits** (32773) — simple RLE; legacy / niche
//! - **None** (1) — uncompressed; small / debug files
//!
//! v0.1 covers all four. JPEG-in-TIFF (compression 7) is deferred to v0.2.

use std::io::Read;

use crate::error::{GtiffError, Result};

/// Decode `input` into `out` (preallocated to the expected uncompressed size).
pub fn decode_into(compression: u16, input: &[u8], out: &mut [u8]) -> Result<()> {
    match compression {
        crate::format::compression::NONE => decode_none(input, out),
        crate::format::compression::PACKBITS => decode_packbits(input, out),
        crate::format::compression::LZW => decode_lzw(input, out),
        crate::format::compression::DEFLATE | crate::format::compression::DEFLATE_ADOBE => {
            decode_deflate(input, out)
        }
        other => Err(GtiffError::unsupported(format!(
            "TIFF compression code {other} (only None/PackBits/LZW/DEFLATE in v0.1)"
        ))),
    }
}

fn decode_none(input: &[u8], out: &mut [u8]) -> Result<()> {
    if input.len() != out.len() {
        return Err(GtiffError::malformed(format!(
            "uncompressed tile: got {} bytes, expected {}",
            input.len(),
            out.len()
        )));
    }
    out.copy_from_slice(input);
    Ok(())
}

/// PackBits RLE.
///
/// Per the Apple/TIFF spec: each chunk starts with a header byte n.
/// - 0..=127 (positive)  → copy the next n+1 bytes verbatim
/// - 129..=255 (negative as i8) → repeat the next byte (-n+1) times
/// - 128 → no-op (skip)
fn decode_packbits(input: &[u8], out: &mut [u8]) -> Result<()> {
    let mut written = 0usize;
    let mut i = 0usize;
    while i < input.len() {
        let n = input[i] as i8;
        i += 1;
        if n >= 0 {
            let run = (n as usize) + 1;
            if i + run > input.len() || written + run > out.len() {
                return Err(GtiffError::malformed("PackBits literal run overruns"));
            }
            out[written..written + run].copy_from_slice(&input[i..i + run]);
            i += run;
            written += run;
        } else if n == -128 {
            // no-op
        } else {
            let count = ((-n as i32) + 1) as usize;
            if i >= input.len() || written + count > out.len() {
                return Err(GtiffError::malformed("PackBits repeat run overruns"));
            }
            let byte = input[i];
            i += 1;
            out[written..written + count].fill(byte);
            written += count;
        }
    }
    if written != out.len() {
        return Err(GtiffError::malformed(format!(
            "PackBits short output: {written} of {}",
            out.len()
        )));
    }
    Ok(())
}

fn decode_lzw(input: &[u8], out: &mut [u8]) -> Result<()> {
    // TIFF LZW uses MSB-first bit ordering and starts at 9 bits per code.
    // weezl 0.1 streams: feed input, drain output, repeat until status::Done.
    use weezl::LzwStatus;
    let mut dec = weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8);
    let mut input_pos = 0;
    let mut output_pos = 0;
    loop {
        let res = dec.decode_bytes(&input[input_pos..], &mut out[output_pos..]);
        if let Err(e) = res.status {
            return Err(GtiffError::Lzw(format!("{e:?}")));
        }
        input_pos += res.consumed_in;
        output_pos += res.consumed_out;
        if matches!(res.status, Ok(LzwStatus::Done)) {
            break;
        }
        if res.consumed_in == 0 && res.consumed_out == 0 {
            // No progress — avoid infinite loop on truncated input.
            return Err(GtiffError::malformed(format!(
                "LZW stalled at in={input_pos}/out={output_pos}"
            )));
        }
    }
    if output_pos != out.len() {
        return Err(GtiffError::malformed(format!(
            "LZW short output: {output_pos} of {}",
            out.len()
        )));
    }
    Ok(())
}

fn decode_deflate(input: &[u8], out: &mut [u8]) -> Result<()> {
    let mut dec = flate2::read::ZlibDecoder::new(input);
    dec.read_exact(out)
        .map_err(|e| GtiffError::Deflate(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_round_trips() {
        let raw = b"abcdef";
        let mut out = vec![0u8; 6];
        decode_none(raw, &mut out).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn none_size_mismatch_errors() {
        let raw = b"abcdef";
        let mut out = vec![0u8; 5];
        assert!(decode_none(raw, &mut out).is_err());
    }

    #[test]
    fn packbits_literal_run() {
        // n=2 → copy 3 bytes verbatim
        let input = &[2u8, b'a', b'b', b'c'];
        let mut out = vec![0u8; 3];
        decode_packbits(input, &mut out).unwrap();
        assert_eq!(&out, b"abc");
    }

    #[test]
    fn packbits_repeat_run() {
        // n=-2 (=254) → repeat next byte 3 times
        let input = &[254u8, b'x'];
        let mut out = vec![0u8; 3];
        decode_packbits(input, &mut out).unwrap();
        assert_eq!(&out, b"xxx");
    }

    #[test]
    fn packbits_noop() {
        // n=-128 (=128) → skip; then n=255 (=-1) → repeat next byte 2 times
        let input = &[128u8, 255, b'a'];
        let mut out = vec![0u8; 2];
        decode_packbits(input, &mut out).unwrap();
        assert_eq!(&out, b"aa");
    }

    #[test]
    fn packbits_mixed() {
        // literal "ab" + repeat 'c'×3 + literal "d"
        let input = &[1u8, b'a', b'b', 254, b'c', 0, b'd'];
        let mut out = vec![0u8; 6];
        decode_packbits(input, &mut out).unwrap();
        assert_eq!(&out, b"abcccd");
    }

    #[test]
    fn deflate_round_trip() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"the quick brown fox jumps over the lazy dog";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let compressed = enc.finish().unwrap();

        let mut out = vec![0u8; original.len()];
        decode_deflate(&compressed, &mut out).unwrap();
        assert_eq!(&out, original);
    }

    #[test]
    fn lzw_round_trip() {
        let original = b"the quick brown fox jumps over the lazy dog";
        let mut enc = weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8);
        let compressed = enc.encode(original).unwrap();
        let mut out = vec![0u8; original.len()];
        decode_lzw(&compressed, &mut out).unwrap();
        assert_eq!(&out, original);
    }

    #[test]
    fn unsupported_compression_errors() {
        let mut out = vec![0u8; 4];
        assert!(matches!(
            decode_into(99, &[1, 2, 3, 4], &mut out).unwrap_err(),
            GtiffError::Unsupported(_)
        ));
    }
}

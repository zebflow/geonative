//! Pixel value codec — convert raw band bytes ↔ `f64` for resampling math.
//!
//! Every resampling kernel does its arithmetic in `f64`, regardless of the
//! source dtype. This module is the bridge: read one pixel as `f64`, do
//! the math, write back as the dtype's raw bytes (clamping to the dtype's
//! valid range).
//!
//! ## Why f64
//!
//! - All numeric pixel types fit losslessly into f64 (even u32 and f32)
//! - Bilinear / cubic / lanczos kernels need fractional math anyway
//! - The per-pixel f64 ↔ raw cost is dominated by the kernel evaluation, not
//!   the conversion

use geonative_core::raster::{Band, PixelType};

/// Read one pixel from a band at `(col, row)` as `f64`. Returns `None` if
/// the band's dtype isn't supported by the resamplers (Rgb8/Rgba8 need
/// per-channel handling that the caller does manually) or if the
/// coordinate is out of range.
pub fn read(band: &Band, col: usize, row: usize, width: usize) -> Option<f64> {
    let idx = row * width + col;
    let dtype = band.descriptor.dtype;
    let bpp = dtype.size_bytes();
    let off = idx * bpp;
    if off + bpp > band.data.len() {
        return None;
    }
    let bytes = &band.data[off..off + bpp];
    Some(match dtype {
        PixelType::U8 => bytes[0] as f64,
        PixelType::I8 => (bytes[0] as i8) as f64,
        PixelType::U16 => u16::from_le_bytes([bytes[0], bytes[1]]) as f64,
        PixelType::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f64,
        PixelType::U32 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
        PixelType::I32 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
        PixelType::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
        PixelType::F64 => f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        // Rgb8/Rgba8 are interleaved multi-component; callers must split
        // into per-channel bands before resampling.
        _ => return None,
    })
}

/// Write `value` into a band at `(col, row)`. Clamps to the dtype's valid
/// range; rounds-to-nearest for integer types. Returns `false` if the
/// coordinate is out of range OR the dtype isn't supported.
pub fn write(band: &mut Band, col: usize, row: usize, width: usize, value: f64) -> bool {
    let idx = row * width + col;
    let dtype = band.descriptor.dtype;
    let bpp = dtype.size_bytes();
    let off = idx * bpp;
    if off + bpp > band.data.len() {
        return false;
    }
    match dtype {
        PixelType::U8 => {
            band.data[off] = value.round().clamp(0.0, u8::MAX as f64) as u8;
        }
        PixelType::I8 => {
            band.data[off] = (value.round().clamp(i8::MIN as f64, i8::MAX as f64) as i8) as u8;
        }
        PixelType::U16 => {
            let v = value.round().clamp(0.0, u16::MAX as f64) as u16;
            band.data[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }
        PixelType::I16 => {
            let v = value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            band.data[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }
        PixelType::U32 => {
            let v = value.round().clamp(0.0, u32::MAX as f64) as u32;
            band.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        PixelType::I32 => {
            let v = value.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
            band.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        PixelType::F32 => {
            band.data[off..off + 4].copy_from_slice(&(value as f32).to_le_bytes());
        }
        PixelType::F64 => {
            band.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::raster::BandDescriptor;

    fn u8_band(width: usize, height: usize) -> Band {
        Band::new(
            BandDescriptor::new(Some("v".into()), PixelType::U8),
            vec![0u8; width * height],
        )
    }

    #[test]
    fn round_trip_u8() {
        let mut b = u8_band(4, 4);
        assert!(write(&mut b, 1, 2, 4, 42.0));
        assert_eq!(read(&b, 1, 2, 4), Some(42.0));
    }

    #[test]
    fn u8_clamps() {
        let mut b = u8_band(2, 2);
        write(&mut b, 0, 0, 2, 300.0); // over max
        assert_eq!(read(&b, 0, 0, 2), Some(255.0));
        write(&mut b, 1, 0, 2, -5.0); // under min
        assert_eq!(read(&b, 1, 0, 2), Some(0.0));
    }

    #[test]
    fn round_trip_f32() {
        let mut b = Band::new(
            BandDescriptor::new(Some("v".into()), PixelType::F32),
            vec![0u8; 4 * 4 * 4],
        );
        write(&mut b, 2, 3, 4, std::f64::consts::PI);
        let got = read(&b, 2, 3, 4).unwrap();
        assert!((got - std::f64::consts::PI).abs() < 1e-6);
    }

    #[test]
    fn round_trip_i16_negative() {
        let mut b = Band::new(
            BandDescriptor::new(Some("v".into()), PixelType::I16),
            vec![0u8; 2 * 2 * 2],
        );
        write(&mut b, 0, 0, 2, -1234.0);
        assert_eq!(read(&b, 0, 0, 2), Some(-1234.0));
    }
}

//! [`ImageRaster`] — the public reader. Decodes a JPG or PNG, parses the
//! adjacent world file, exposes the whole image as a single `RasterTile`
//! via the `RasterLayer` trait.

use std::path::Path;

use geonative_core::raster::{
    Band, BandDescriptor, PixelType, RasterLayer, RasterProfile, RasterTile,
};
use geonative_core::{Crs, Result as CoreResult};

use crate::error::{ImageError, Result};
use crate::worldfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
}

impl ImageKind {
    pub fn from_extension(ext: &str) -> Result<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            other => Err(ImageError::unsupported(format!(
                "extension .{other} (v0.1 supports .jpg / .jpeg / .png)"
            ))),
        }
    }
}

/// An opened raster image with georeferencing from a world file.
///
/// The entire image is decoded into memory on `open`. Subsequent
/// `read_tile` calls slice from the in-memory buffer.
#[derive(Debug)]
pub struct ImageRaster {
    /// Owned pixel data, interleaved chunky per `bands_meta`.
    pixels: Vec<u8>,
    /// Schema-like description.
    profile: RasterProfile,
}

impl ImageRaster {
    /// Open the image at `image_path`. Looks for a sidecar world file
    /// (`.jgw` / `.pgw` / `.wld`) next to it; errors with
    /// [`ImageError::MissingWorldFile`] if none found.
    ///
    /// CRS defaults to `Crs::Unknown`; use [`Self::open_with_crs`] to set
    /// one explicitly (world files don't carry CRS info).
    pub fn open(image_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_crs(image_path, Crs::Unknown)
    }

    /// Same as [`Self::open`] but with an explicit CRS — the typical
    /// pattern is the caller specifying it via UI / config.
    pub fn open_with_crs(image_path: impl AsRef<Path>, crs: Crs) -> Result<Self> {
        let path = image_path.as_ref();
        let ext = path.extension().and_then(|s| s.to_str()).ok_or_else(|| {
            ImageError::malformed(format!("image path has no extension: {}", path.display()))
        })?;
        let kind = ImageKind::from_extension(ext)?;

        // 1) Decode the image bytes.
        let image_bytes = std::fs::read(path)?;
        let decoded = match kind {
            ImageKind::Jpeg => decode_jpeg(&image_bytes)?,
            ImageKind::Png => decode_png(&image_bytes)?,
        };

        // 2) Locate + parse the world file.
        let wf_path = worldfile::find_sidecar(path)?;
        let wf_text = std::fs::read_to_string(&wf_path)?;
        let geo_transform = worldfile::parse(&wf_text)?;

        // 3) Build the profile + return.
        let bands: Vec<BandDescriptor> = decoded
            .band_names()
            .into_iter()
            .map(|name| BandDescriptor::new(Some(name.into()), PixelType::U8))
            .collect();
        let profile = RasterProfile {
            width: decoded.width,
            height: decoded.height,
            bands,
            geo_transform,
            crs,
            tile_size: [decoded.width, decoded.height], // single-tile, whole-image
            pyramid_levels: 1,
        };

        Ok(Self {
            pixels: decoded.pixels,
            profile,
        })
    }

    /// Hand back the raw pixel buffer (chunky interleaved). Useful for
    /// downstream code that wants to encode the same pixels into another
    /// format without going through `RasterTile` (which would copy).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

impl RasterLayer for ImageRaster {
    fn profile(&self) -> &RasterProfile {
        &self.profile
    }

    fn read_tile(&self, level: u8, x: u32, y: u32) -> CoreResult<RasterTile> {
        if level != 0 {
            return Err(ImageError::Unsupported(format!(
                "single-image raster has only level 0; got {level}"
            ))
            .into());
        }
        if x != 0 || y != 0 {
            return Err(ImageError::Unsupported(format!(
                "single-image raster has only tile (0, 0); got ({x}, {y})"
            ))
            .into());
        }
        // Split interleaved bytes into per-band buffers.
        let nbands = self.profile.bands.len();
        let pixels = (self.profile.width as usize) * (self.profile.height as usize);
        let stride = nbands; // all bands are U8 in v0.1
        let mut bands = Vec::with_capacity(nbands);
        for bi in 0..nbands {
            let mut data = Vec::with_capacity(pixels);
            for p in 0..pixels {
                data.push(self.pixels[p * stride + bi]);
            }
            bands.push(Band::new(self.profile.bands[bi].clone(), data));
        }
        Ok(RasterTile {
            width: self.profile.width,
            height: self.profile.height,
            bands,
            geo_transform: self.profile.geo_transform,
            crs: self.profile.crs.clone(),
        })
    }
}

/// Internal type used while decoding. Stays out of the public API.
struct Decoded {
    width: u32,
    height: u32,
    /// Chunky-interleaved pixels.
    pixels: Vec<u8>,
    /// 1 (Greyscale), 3 (Rgb), or 4 (Rgba)
    nbands: u8,
}

impl Decoded {
    fn band_names(&self) -> Vec<&'static str> {
        match self.nbands {
            1 => vec!["grey"],
            3 => vec!["red", "green", "blue"],
            4 => vec!["red", "green", "blue", "alpha"],
            _ => Vec::new(),
        }
    }
}

fn decode_jpeg(bytes: &[u8]) -> Result<Decoded> {
    let mut dec = jpeg_decoder::Decoder::new(bytes);
    let pixels = dec
        .decode()
        .map_err(|e| ImageError::Jpeg(format!("{e:?}")))?;
    let info = dec
        .info()
        .ok_or_else(|| ImageError::Jpeg("missing JPEG info after decode".into()))?;
    let nbands = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => 1,
        jpeg_decoder::PixelFormat::RGB24 => 3,
        jpeg_decoder::PixelFormat::CMYK32 => {
            return Err(ImageError::unsupported(
                "CMYK JPEGs (use RGB output instead)",
            ))
        }
        // L16 / RGB48 etc are 16-bit; v0.1 is U8-only
        _ => {
            return Err(ImageError::unsupported(format!(
                "JPEG pixel format {:?} (v0.1 supports L8 + RGB24)",
                info.pixel_format
            )))
        }
    };
    Ok(Decoded {
        width: info.width as u32,
        height: info.height as u32,
        pixels,
        nbands,
    })
}

fn decode_png(bytes: &[u8]) -> Result<Decoded> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| ImageError::Png(e.to_string()))?;
    let info = reader.info().clone();

    // We need 8-bit channel depth for v0.1.
    if info.bit_depth != png::BitDepth::Eight {
        return Err(ImageError::unsupported(format!(
            "PNG bit depth {:?} (v0.1 supports 8-bit)",
            info.bit_depth
        )));
    }

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| ImageError::Png(e.to_string()))?;
    buf.truncate(frame.buffer_size());

    let nbands = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Indexed => {
            return Err(ImageError::unsupported(
                "indexed (palette) PNGs — decode to RGB before upload",
            ))
        }
    };

    Ok(Decoded {
        width: info.width,
        height: info.height,
        pixels: buf,
        nbands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workdir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("imgraster_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Encode a tiny RGB PNG using the `png` crate so we have a real
    /// fixture to test against (jpeg-decoder is decode-only — JPEG paths
    /// are exercised only via documented `decode_jpeg` shape, not via a
    /// round-trip).
    fn write_test_png(path: &Path, width: u32, height: u32, fill: [u8; 3]) {
        let file = std::fs::File::create(path).unwrap();
        let buf = std::io::BufWriter::new(file);
        let mut enc = png::Encoder::new(buf, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        let pixels: Vec<u8> = (0..(width * height))
            .flat_map(|_| fill.iter().copied())
            .collect();
        writer.write_image_data(&pixels).unwrap();
    }

    #[test]
    fn opens_png_with_world_file() {
        let dir = workdir("png_with_wf");
        let img = dir.join("ortho.png");
        write_test_png(&img, 4, 4, [10, 20, 30]);
        let wld = dir.join("ortho.pgw");
        std::fs::write(&wld, "0.5\n0\n0\n-0.5\n144\n-37\n").unwrap();

        let r = ImageRaster::open_with_crs(&img, Crs::Epsg(4326)).unwrap();
        let p = r.profile();
        assert_eq!(p.width, 4);
        assert_eq!(p.height, 4);
        assert_eq!(p.bands.len(), 3);
        assert_eq!(p.crs, Crs::Epsg(4326));
        // Half-pixel shift: centre (144, -37) + pixel (0.5, -0.5) → upper-left (143.75, -36.75)
        assert!((p.geo_transform.origin[0] - 143.75).abs() < 1e-9);
        assert!((p.geo_transform.origin[1] - (-36.75)).abs() < 1e-9);
    }

    #[test]
    fn read_tile_returns_full_image_as_one_tile() {
        let dir = workdir("read_tile");
        let img = dir.join("ortho.png");
        write_test_png(&img, 4, 4, [10, 20, 30]);
        let wld = dir.join("ortho.pgw");
        std::fs::write(&wld, "0.5\n0\n0\n-0.5\n144\n-37\n").unwrap();

        let r = ImageRaster::open_with_crs(&img, Crs::Epsg(4326)).unwrap();
        let t = r.read_tile(0, 0, 0).unwrap();
        assert_eq!(t.width, 4);
        assert_eq!(t.height, 4);
        assert_eq!(t.bands.len(), 3);
        // Each band should be all the same value (since fill was constant)
        assert!(t.bands[0].data.iter().all(|&v| v == 10));
        assert!(t.bands[1].data.iter().all(|&v| v == 20));
        assert!(t.bands[2].data.iter().all(|&v| v == 30));
    }

    #[test]
    fn missing_world_file_errors_clearly() {
        let dir = workdir("missing_wf");
        let img = dir.join("noworld.png");
        write_test_png(&img, 2, 2, [0, 0, 0]);

        let err = ImageRaster::open(&img).unwrap_err();
        assert!(matches!(err, ImageError::MissingWorldFile { .. }));
    }

    #[test]
    fn unsupported_extension_errors() {
        let dir = workdir("bad_ext");
        let img = dir.join("file.gif");
        std::fs::write(&img, b"").unwrap();
        let err = ImageRaster::open(&img).unwrap_err();
        assert!(matches!(err, ImageError::Unsupported(_)));
    }

    #[test]
    fn read_tile_rejects_non_zero_tile() {
        let dir = workdir("bad_tile");
        let img = dir.join("ortho.png");
        write_test_png(&img, 2, 2, [0, 0, 0]);
        std::fs::write(dir.join("ortho.pgw"), "0.5\n0\n0\n-0.5\n0\n0\n").unwrap();
        let r = ImageRaster::open(&img).unwrap();
        assert!(r.read_tile(0, 1, 0).is_err());
        assert!(r.read_tile(1, 0, 0).is_err());
    }
}

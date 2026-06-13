//! One channel of a multi-band raster.
//!
//! A multi-spectral satellite image is multi-band (red, green, blue, NIR,
//! …). A DEM is single-band (elevation). An RGB photo can be modelled as
//! either three U8 bands or one packed `Rgb8` band — the choice depends on
//! the source format and what downstream ops need.

use std::fmt;

/// The numeric type a band's pixel data is interpreted as. Constrained
/// subset of `ValueType` — rasters never store String/Binary/Guid/Xml/
/// DateTime per-pixel; floats and integers cover every practical case
/// (imagery = U8 or U16, DEMs = F32 or I16, scientific = F32 or F64,
/// landcover/classification = U8/U16 with categorical interpretation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelType {
    U8,
    U16,
    U32,
    I8,
    I16,
    I32,
    F32,
    F64,
    /// 3 bytes per pixel (R, G, B). Stored interleaved; saves the
    /// 3-separate-bands bookkeeping for the very common RGB-photo case.
    Rgb8,
    /// 4 bytes per pixel (R, G, B, A).
    Rgba8,
}

impl PixelType {
    /// Bytes per pixel for this type.
    pub fn size_bytes(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::F64 => 8,
            Self::Rgb8 => 3,
            Self::Rgba8 => 4,
            // Future-variant guard. `#[non_exhaustive]` opens us up to new
            // pixel types (e.g. F16, RGBA16) without breaking SemVer.
            #[allow(unreachable_patterns)]
            _ => 0,
        }
    }

    /// Whether the type encodes an integer or floating-point value.
    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
}

/// Description of a band's structure, without the actual pixel bytes —
/// equivalent of [`crate::FieldDef`] on the vector side. Lives in the
/// [`RasterProfile`](crate::raster::RasterProfile) (the "schema" for a
/// raster layer); same `BandDescriptor` is reused as each tile is read,
/// so we don't allocate per-tile metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct BandDescriptor {
    /// Human-readable name (`"red"`, `"elevation"`, `"ndvi"`, etc.).
    /// `None` for unnamed bands.
    pub name: Option<String>,

    /// The pixel encoding of this band.
    pub dtype: PixelType,

    /// Value used by the source to mean "no data here" (clouds, ocean
    /// outside DEM, masked-out pixel). Stored as `f64` so any pixel type
    /// can express it; readers compare against the band's raw bytes via
    /// `dtype`-aware reinterpretation.
    pub nodata: Option<f64>,

    /// Optional unit (`"meters"`, `"DN"`, `"reflectance"`). Surfaced from
    /// TIFF's `ImageDescription` / GDAL metadata when available.
    pub unit: Option<String>,

    /// Optional scale + offset for converting raw pixel values to physical
    /// values: `physical = raw * scale + offset`. Common in scientific
    /// formats; `None` means raw values are already in physical units.
    pub scale: Option<f64>,
    pub offset: Option<f64>,
}

impl BandDescriptor {
    pub fn new(name: Option<String>, dtype: PixelType) -> Self {
        Self {
            name,
            dtype,
            nodata: None,
            unit: None,
            scale: None,
            offset: None,
        }
    }

    pub fn with_nodata(mut self, nodata: f64) -> Self {
        self.nodata = Some(nodata);
        self
    }
}

/// One band's worth of pixel data for a single [`RasterTile`](super::RasterTile).
///
/// Pixels are stored as **raw bytes** in row-major order, interpreted via
/// `dtype`. `data.len()` must equal `width * height * dtype.size_bytes()`.
/// We don't carry width/height on the band itself — they live on the
/// owning `RasterTile` and the tile's bands all share dimensions (multi-
/// resolution within one tile is not a real-world thing).
#[derive(Clone, PartialEq)]
pub struct Band {
    pub descriptor: BandDescriptor,
    pub data: Vec<u8>,
}

impl Band {
    pub fn new(descriptor: BandDescriptor, data: Vec<u8>) -> Self {
        Self { descriptor, data }
    }

    pub fn pixel_count(&self) -> usize {
        let bpp = self.descriptor.dtype.size_bytes();
        if bpp == 0 {
            0
        } else {
            self.data.len() / bpp
        }
    }
}

impl fmt::Debug for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Band")
            .field("descriptor", &self.descriptor)
            .field("data_bytes", &self.data.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_type_sizes() {
        assert_eq!(PixelType::U8.size_bytes(), 1);
        assert_eq!(PixelType::U16.size_bytes(), 2);
        assert_eq!(PixelType::F32.size_bytes(), 4);
        assert_eq!(PixelType::F64.size_bytes(), 8);
        assert_eq!(PixelType::Rgb8.size_bytes(), 3);
        assert_eq!(PixelType::Rgba8.size_bytes(), 4);
    }

    #[test]
    fn pixel_type_float_classification() {
        assert!(PixelType::F32.is_float());
        assert!(PixelType::F64.is_float());
        assert!(!PixelType::U8.is_float());
        assert!(!PixelType::Rgb8.is_float());
    }

    #[test]
    fn band_pixel_count_matches_data_size() {
        let desc = BandDescriptor::new(Some("red".into()), PixelType::U8);
        let band = Band::new(desc, vec![0u8; 256 * 256]);
        assert_eq!(band.pixel_count(), 256 * 256);
    }

    #[test]
    fn band_descriptor_builder() {
        let desc =
            BandDescriptor::new(Some("elevation".into()), PixelType::F32).with_nodata(-9999.0);
        assert_eq!(desc.nodata, Some(-9999.0));
        assert!(desc.dtype.is_float());
    }
}

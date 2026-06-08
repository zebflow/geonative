//! Parser for the `.gdbtable` 40-byte header and the field-description section.
//!
//! Layout reference: rouault/dump_gdbtable FGDB-Spec. Verified against real
//! FileGDB data (GDB_SystemCatalog from a VicMap export).

use crate::bytes::LeReader;
use crate::error::{GdbError, Result};

/// `.gdbtable` format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableVersion {
    /// 32-bit OBJECTIDs (FGDB 10.x and earlier).
    V3,
    /// 64-bit OBJECTIDs (ArcGIS Pro 3.2+).
    V4,
}

/// The 40-byte `.gdbtable` header.
#[derive(Debug, Clone)]
pub struct TableHeader {
    pub version: TableVersion,
    /// Number of currently-valid (non-deleted) rows. Source of truth for
    /// iteration upper bound (combined with `.gdbtablx`).
    pub valid_record_count: i64,
    /// Maximum row size in bytes. Useful as a sanity-check upper bound.
    pub max_row_size: u32,
    /// Reported file size in bytes.
    pub file_size: u64,
    /// Byte offset at which the field-description section starts.
    /// Usually 40 (i.e. right after the header).
    pub field_desc_offset: u64,
}

/// Parse the 40-byte `.gdbtable` header. Reader must be at byte 0.
pub fn parse_table_header(r: &mut LeReader) -> Result<TableHeader> {
    let version_raw = r.read_i32()?;
    let version = match version_raw {
        3 => TableVersion::V3,
        4 => TableVersion::V4,
        v => {
            return Err(GdbError::malformed(format!(
                "unknown .gdbtable version {v} (expected 3 or 4)"
            )))
        }
    };

    let v3_record_count = r.read_i32()?; // bytes 4..8 (in v4 this is the delete flag)
    let max_row_size = r.read_i32()? as u32; // bytes 8..12
    let _const_5 = r.read_i32()?; // bytes 12..16 — documented as constant 5, role unknown
    let bytes_16_24 = r.read_i64()?; // bytes 16..24 — in v4 this is the i64 valid-row-count
    let file_size = r.read_i64()? as u64; // bytes 24..32
    let field_desc_offset = r.read_i64()? as u64; // bytes 32..40

    let valid_record_count = match version {
        TableVersion::V3 => v3_record_count as i64,
        TableVersion::V4 => bytes_16_24,
    };

    Ok(TableHeader {
        version,
        valid_record_count,
        max_row_size,
        file_size,
        field_desc_offset,
    })
}

/// Per-layer flags packed into a single u32. Decoded at access time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerFlags(pub u32);

impl LayerFlags {
    /// Low 8 bits: geometry type code (0 none, 1 point, 2 multipoint,
    /// 3 polyline, 4 polygon, 5 rectangle, 9 multipatch, …).
    pub fn geometry_type_code(&self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// True if strings in this layer are UTF-8; false → UTF-16-LE.
    pub fn strings_utf8(&self) -> bool {
        self.0 & (1 << 8) != 0
    }

    pub fn has_m(&self) -> bool {
        self.0 & (1 << 30) != 0
    }

    pub fn has_z(&self) -> bool {
        self.0 & (1 << 31) != 0
    }
}

/// FileGDB field type code as stored in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTypeCode {
    Int16,
    Int32,
    Float32,
    Float64,
    String,
    DateTime,
    ObjectId,
    Geometry,
    Binary,
    Raster,
    Guid,
    GlobalId,
    Xml,
    /// ArcGIS Pro 3.2+
    Int64,
    /// ArcGIS Pro 3.2+
    DateOnly,
    /// ArcGIS Pro 3.2+
    TimeOnly,
    /// ArcGIS Pro 3.2+
    DateTimeWithOffset,
}

impl FieldTypeCode {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => Self::Int16,
            1 => Self::Int32,
            2 => Self::Float32,
            3 => Self::Float64,
            4 => Self::String,
            5 => Self::DateTime,
            6 => Self::ObjectId,
            7 => Self::Geometry,
            8 => Self::Binary,
            9 => Self::Raster,
            10 => Self::Guid,
            11 => Self::GlobalId,
            12 => Self::Xml,
            13 => Self::Int64,
            14 => Self::DateOnly,
            15 => Self::TimeOnly,
            16 => Self::DateTimeWithOffset,
            other => {
                return Err(GdbError::malformed(format!(
                    "unknown field type code {other}"
                )))
            }
        })
    }
}

/// Spec-derived per-field flag byte. bit0 = nullable, bit1 = required,
/// bit2 = editable.
fn flag_nullable(flag: u8) -> bool {
    flag & 0x01 != 0
}

/// Geometry-field-specific metadata. Holds dequantization parameters needed
/// by the shape-buffer decoder.
///
/// The Z/M story has two independent axes:
/// - **Origin-scale-tolerance presence** (from the sub-flags byte right after
///   the WKT): whether `morigin`/`mscale`/`mtolerance` and
///   `zorigin`/`zscale`/`ztolerance` are stored in the metadata.
/// - **Layer-level has_z / has_m** (from the layer flags u32 bits 30/31):
///   whether the layer actually carries Z/M ordinates. This controls whether
///   `zmin/zmax` and `mmin/mmax` are present in the extent.
///
/// A layer can declare origin/scale precision for Z/M without those ordinates
/// being present in the features (the inverse should never happen).
#[derive(Debug, Clone)]
pub struct GeomFieldMeta {
    pub srs_wkt: String,
    /// Sub-flags bit 1: are m origin/scale/tolerance stored in this metadata?
    pub has_m_origin_scale_tolerance: bool,
    /// Sub-flags bit 2: are z origin/scale/tolerance stored in this metadata?
    pub has_z_origin_scale_tolerance: bool,
    /// Layer flags bit 30: does the layer carry M ordinates?
    pub layer_has_m: bool,
    /// Layer flags bit 31: does the layer carry Z ordinates?
    pub layer_has_z: bool,
    pub xorigin: f64,
    pub yorigin: f64,
    pub xyscale: f64,
    pub morigin: Option<f64>,
    pub mscale: Option<f64>,
    pub zorigin: Option<f64>,
    pub zscale: Option<f64>,
    pub xytolerance: f64,
    pub mtolerance: Option<f64>,
    pub ztolerance: Option<f64>,
    /// `[xmin, ymin, xmax, ymax]`. Always present for Geometry fields.
    /// May contain NaN.
    pub extent_xy: [f64; 4],
    /// `[zmin, zmax]`. Present only when `layer_has_z`.
    pub extent_z: Option<[f64; 2]>,
    /// `[mmin, mmax]`. Present only when `layer_has_m`.
    pub extent_m: Option<[f64; 2]>,
    pub grid_resolutions: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub alias: Option<String>,
    pub ty: FieldTypeCode,
    pub nullable: bool,
    /// Spec-stored width hint (string max-length, OBJECTID width 4/8, fixed-
    /// type width 2/4/8). `None` for Geometry and other untyped-width fields.
    pub width: Option<u32>,
    /// Raw default-value bytes (driver decodes lazily per type). Empty if no
    /// default declared.
    pub default_raw: Vec<u8>,
    /// Present iff `ty == FieldTypeCode::Geometry`.
    pub geometry: Option<GeomFieldMeta>,
}

#[derive(Debug, Clone)]
pub struct FieldSection {
    pub fields: Vec<Field>,
    pub flags: LayerFlags,
    /// Inner format version (3 = FGDB 9.x, 4 = FGDB 10.x, 6 = Pro 3.2 extended).
    pub format_version: u32,
}

impl FieldSection {
    /// Index of the geometry field, if any.
    pub fn geometry_field_index(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|f| f.ty == FieldTypeCode::Geometry)
    }

    /// Index of the OBJECTID field, if any.
    pub fn objectid_field_index(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|f| f.ty == FieldTypeCode::ObjectId)
    }
}

/// Parse the field-description section. Reader must be positioned at
/// `TableHeader::field_desc_offset`.
pub fn parse_field_section(r: &mut LeReader) -> Result<FieldSection> {
    let _section_size = r.read_i32()?; // size of section excluding this i32
    let format_version = r.read_u32()?;
    let flags = LayerFlags(r.read_u32()?);
    let n_fields = r.read_i16()? as usize;

    let mut fields = Vec::with_capacity(n_fields);
    for _ in 0..n_fields {
        fields.push(parse_field_descriptor(r, &flags)?);
    }

    Ok(FieldSection {
        fields,
        flags,
        format_version,
    })
}

fn read_name_str(r: &mut LeReader, n_chars: usize) -> Result<String> {
    // Field names and aliases are UTF-16-LE in every FGDB sample we've seen —
    // independent of the layer's payload string encoding flag.
    r.read_utf16_le(n_chars)
}

fn parse_field_descriptor(r: &mut LeReader, layer_flags: &LayerFlags) -> Result<Field> {
    let name_len = r.read_u8()? as usize;
    let name = read_name_str(r, name_len)?;
    let alias_len = r.read_u8()? as usize;
    let alias = if alias_len > 0 {
        Some(read_name_str(r, alias_len)?)
    } else {
        None
    };

    let type_byte = r.read_u8()?;
    let ty = FieldTypeCode::from_u8(type_byte)?;

    // Per-type encoding of width / flag / default.
    let (width, nullable, default_raw, geom) = match ty {
        FieldTypeCode::Geometry => {
            let g = parse_geometry_field_meta(r, layer_flags)?;
            (None, true, Vec::new(), Some(g))
        }
        FieldTypeCode::String | FieldTypeCode::Xml => {
            let max_len = r.read_u32()?;
            let flag = r.read_u8()?;
            let default = read_optional_default(r)?;
            (Some(max_len), flag_nullable(flag), default, None)
        }
        FieldTypeCode::ObjectId => {
            let width = r.read_u8()? as u32;
            let _flag = r.read_u8()?; // documented as constant 2
                                      // OBJECTID has no default value section.
            (Some(width), false, Vec::new(), None)
        }
        FieldTypeCode::Binary
        | FieldTypeCode::Raster
        | FieldTypeCode::Guid
        | FieldTypeCode::GlobalId => {
            let flag = r.read_u8()?;
            // These types are not documented to carry a varuint default;
            // be conservative and accept either form.
            let default = read_optional_default(r).unwrap_or_default();
            (None, flag_nullable(flag), default, None)
        }
        // Fixed-width "other" types: Int16/32/64, Float32/64, DateTime,
        // DateOnly, TimeOnly, DateTimeWithOffset.
        FieldTypeCode::Int16
        | FieldTypeCode::Int32
        | FieldTypeCode::Int64
        | FieldTypeCode::Float32
        | FieldTypeCode::Float64
        | FieldTypeCode::DateTime
        | FieldTypeCode::DateOnly
        | FieldTypeCode::TimeOnly
        | FieldTypeCode::DateTimeWithOffset => {
            let width = r.read_u8()? as u32;
            let flag = r.read_u8()?;
            let default = read_optional_default(r)?;
            (Some(width), flag_nullable(flag), default, None)
        }
    };

    Ok(Field {
        name,
        alias,
        ty,
        nullable,
        width,
        default_raw,
        geometry: geom,
    })
}

/// Read the varuint-length-prefixed default value bytes. Returns empty if 0.
fn read_optional_default(r: &mut LeReader) -> Result<Vec<u8>> {
    let n = r.read_varuint()? as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    Ok(r.read_bytes(n)?.to_vec())
}

fn parse_geometry_field_meta(r: &mut LeReader, layer_flags: &LayerFlags) -> Result<GeomFieldMeta> {
    // ubyte (=0), ubyte flag (6 or 7)
    let _zero = r.read_u8()?;
    let _flag = r.read_u8()?;

    // u16 WKT length **in BYTES** (verified against GDAL openfilegdb source),
    // then UTF-16-LE WKT string.
    let wkt_byte_len = r.read_u16()? as usize;
    if wkt_byte_len % 2 != 0 {
        return Err(GdbError::malformed(format!(
            "WKT byte length {wkt_byte_len} is odd; UTF-16 expected"
        )));
    }
    let srs_wkt = read_name_str(r, wkt_byte_len / 2)?;

    // Sub-flags byte right after WKT. bit0 = always set; bit1 = has m
    // origin/scale/tolerance; bit2 = has z origin/scale/tolerance.
    // These are INDEPENDENT of whether the layer itself carries Z/M.
    let sub_flags = r.read_u8()?;
    let has_m_ost = sub_flags & 0x02 != 0;
    let has_z_ost = sub_flags & 0x04 != 0;

    let xorigin = r.read_f64()?;
    let yorigin = r.read_f64()?;
    let xyscale = r.read_f64()?;
    if xyscale == 0.0 {
        return Err(GdbError::malformed("geometry xyscale is zero"));
    }

    let (morigin, mscale) = if has_m_ost {
        (Some(r.read_f64()?), Some(r.read_f64()?))
    } else {
        (None, None)
    };
    let (zorigin, zscale) = if has_z_ost {
        (Some(r.read_f64()?), Some(r.read_f64()?))
    } else {
        (None, None)
    };

    let xytolerance = r.read_f64()?;
    let mtolerance = if has_m_ost { Some(r.read_f64()?) } else { None };
    let ztolerance = if has_z_ost { Some(r.read_f64()?) } else { None };

    // Extent XY is ALWAYS present for Geometry fields.
    let extent_xy = [r.read_f64()?, r.read_f64()?, r.read_f64()?, r.read_f64()?];
    // Z/M extent is gated on layer-level has_z / has_m, not the sub-flags.
    let layer_has_z = layer_flags.has_z();
    let layer_has_m = layer_flags.has_m();
    let extent_z = if layer_has_z {
        Some([r.read_f64()?, r.read_f64()?])
    } else {
        None
    };
    let extent_m = if layer_has_m {
        Some([r.read_f64()?, r.read_f64()?])
    } else {
        None
    };

    // Spatial-index grid: zero byte + uint32 grid count (1..=3) + that many f64s.
    let _zero2 = r.read_u8()?;
    let grid_count = r.read_u32()?;
    if grid_count == 0 || grid_count > 3 {
        return Err(GdbError::malformed(format!(
            "geometry field declares {grid_count} spatial-index grids (expected 1..=3)"
        )));
    }
    let mut grid_resolutions = Vec::with_capacity(grid_count as usize);
    for _ in 0..grid_count {
        grid_resolutions.push(r.read_f64()?);
    }

    Ok(GeomFieldMeta {
        srs_wkt,
        has_m_origin_scale_tolerance: has_m_ost,
        has_z_origin_scale_tolerance: has_z_ost,
        layer_has_m,
        layer_has_z,
        xorigin,
        yorigin,
        xyscale,
        morigin,
        mscale,
        zorigin,
        zscale,
        xytolerance,
        mtolerance,
        ztolerance,
        extent_xy,
        extent_z,
        extent_m,
        grid_resolutions,
    })
}

/// High-level wrapper: parse header + seek to field section + parse fields.
#[derive(Debug, Clone)]
pub struct Table {
    pub header: TableHeader,
    pub field_section: FieldSection,
}

impl Table {
    /// Parse the table from its raw bytes (e.g. the contents of an
    /// `aNNNNNNNN.gdbtable` file).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut r = LeReader::new(bytes);
        let header = parse_table_header(&mut r)?;
        r.seek(header.field_desc_offset as usize)?;
        let field_section = parse_field_section(&mut r)?;
        Ok(Self {
            header,
            field_section,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-crafted minimal v3 header + a 1-field (Int32) section.
    fn synth_v3_with_one_int32() -> Vec<u8> {
        let mut buf = Vec::new();

        // ── header (40 bytes) ──────────────────────────────────────────
        buf.extend_from_slice(&3i32.to_le_bytes()); // version 3
        buf.extend_from_slice(&5i32.to_le_bytes()); // 5 valid rows
        buf.extend_from_slice(&20i32.to_le_bytes()); // max_row_size = 20
        buf.extend_from_slice(&5i32.to_le_bytes()); // const 5
        buf.extend_from_slice(&0i64.to_le_bytes()); // bytes 16..24 unused
        buf.extend_from_slice(&999i64.to_le_bytes()); // file_size
        buf.extend_from_slice(&40i64.to_le_bytes()); // field_desc_offset = 40

        // ── field section ──────────────────────────────────────────────
        // section_size placeholder
        let section_size_pos = buf.len();
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes()); // format_version
        buf.extend_from_slice(&0x100u32.to_le_bytes()); // flags: UTF-8 strings
        buf.extend_from_slice(&1i16.to_le_bytes()); // 1 field

        // field 0: "X" (Int32, nullable=false)
        buf.push(1); // name length = 1
        buf.push(b'X');
        buf.push(0); // X in UTF-16 LE high byte
        buf.push(0); // alias length = 0
        buf.push(1); // type = Int32
        buf.push(4); // width = 4
        buf.push(4); // flag = 4 (not nullable, regular)
        buf.push(0); // default length varuint = 0

        // back-patch section_size
        let sz = (buf.len() - section_size_pos - 4) as i32;
        buf[section_size_pos..section_size_pos + 4].copy_from_slice(&sz.to_le_bytes());

        buf
    }

    #[test]
    fn header_v3_roundtrip() {
        let bytes = synth_v3_with_one_int32();
        let mut r = LeReader::new(&bytes);
        let h = parse_table_header(&mut r).unwrap();
        assert_eq!(h.version, TableVersion::V3);
        assert_eq!(h.valid_record_count, 5);
        assert_eq!(h.max_row_size, 20);
        assert_eq!(h.file_size, 999);
        assert_eq!(h.field_desc_offset, 40);
    }

    #[test]
    fn field_section_v3_roundtrip() {
        let bytes = synth_v3_with_one_int32();
        let table = Table::parse(&bytes).unwrap();
        assert_eq!(table.field_section.fields.len(), 1);
        assert!(table.field_section.flags.strings_utf8());
        assert_eq!(table.field_section.format_version, 4);

        let f = &table.field_section.fields[0];
        assert_eq!(f.name, "X");
        assert_eq!(f.alias, None);
        assert_eq!(f.ty, FieldTypeCode::Int32);
        assert!(!f.nullable);
        assert_eq!(f.width, Some(4));
        assert!(f.default_raw.is_empty());
    }

    #[test]
    fn header_rejects_unknown_version() {
        let mut buf = vec![0u8; 40];
        buf[..4].copy_from_slice(&7i32.to_le_bytes());
        assert!(parse_table_header(&mut LeReader::new(&buf)).is_err());
    }

    #[test]
    fn field_type_codes_round_trip_through_from_u8() {
        for code in 0u8..=16 {
            FieldTypeCode::from_u8(code).unwrap();
        }
        assert!(FieldTypeCode::from_u8(99).is_err());
    }

    #[test]
    fn layer_flags_decode_bits() {
        let f = LayerFlags(0x0000_0100); // bit 8 = UTF-8
        assert!(f.strings_utf8());
        assert!(!f.has_z());
        assert!(!f.has_m());

        let f = LayerFlags(0x4000_0003); // polyline + has_m
        assert_eq!(f.geometry_type_code(), 3);
        assert!(!f.strings_utf8());
        assert!(f.has_m());
        assert!(!f.has_z());

        let f = LayerFlags(0x8000_0004); // polygon + has_z
        assert_eq!(f.geometry_type_code(), 4);
        assert!(f.has_z());
        assert!(!f.has_m());
    }
}

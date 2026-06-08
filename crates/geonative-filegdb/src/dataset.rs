//! High-level public API: [`Geodatabase`] and [`Layer`].
//!
//! Ties together the lower-level pieces ([`crate::catalog`],
//! [`crate::table`], [`crate::tablx`], [`crate::row`], [`crate::geometry`])
//! and emits `geonative_core::Feature` values through a sequential iterator.

use std::path::{Path, PathBuf};

use geonative_core::{Crs, Feature, FieldDef, GeomField, GeometryType, Schema, ValueType};
use memmap2::Mmap;

use crate::catalog::{open_geodatabase, LayerInfo};
use crate::error::{GdbError, Result};
use crate::geometry::decode_shape_buffer;
use crate::row::{decode_row_blob, slice_row_blob};
use crate::table::{FieldSection, FieldTypeCode, LayerFlags, Table};
use crate::tablx::Tablx;

/// An opened FileGDB directory. Cheap to clone the [`LayerInfo`] list, but
/// keep the [`Geodatabase`] handle around for [`layer()`](Self::layer).
#[derive(Debug)]
pub struct Geodatabase {
    dir: PathBuf,
    layers: Vec<LayerInfo>,
}

impl Geodatabase {
    /// Open a `.gdb` directory and read its catalog. Returns immediately —
    /// individual layers are not loaded until [`layer()`](Self::layer) is called.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let dir = path.as_ref().to_path_buf();
        let layers = open_geodatabase(&dir)?;
        Ok(Self { dir, layers })
    }

    /// List the user-facing layers in this geodatabase.
    pub fn layers(&self) -> &[LayerInfo] {
        &self.layers
    }

    /// Open a layer by its friendly name (as it appears in `layers()`).
    pub fn layer(&self, name: &str) -> Result<Layer> {
        let info = self
            .layers
            .iter()
            .find(|l| l.name == name)
            .ok_or_else(|| GdbError::malformed(format!("layer '{name}' not found")))?;
        Layer::open(&self.dir, info)
    }
}

/// One open layer (feature class or attribute table).
///
/// The `.gdbtable` is **memory-mapped** (`memmap2::Mmap`), so peak resident
/// set size stays constant at < ~100 MB regardless of the underlying file
/// size — the OS pages bytes into RAM on demand and evicts them under
/// pressure. We hold the `Mmap` for the layer's lifetime so the OS knows
/// the mapping is still live.
///
/// The `.gdbtablx` is small (≤ a few MB even for very large layers) and is
/// fully read into a `Vec` — no mmap there.
#[derive(Debug)]
pub struct Layer {
    name: String,
    /// Memory map of the `.gdbtable` file. Derefs to `&[u8]` for the same
    /// slice-based access pattern as a `Vec<u8>`.
    mmap: Mmap,
    tablx: Tablx,
    table: Table,
    schema: Schema,
    /// Index of the geometry field within `table.field_section.fields`, if any.
    geom_field_idx: Option<usize>,
    /// Index of the OBJECTID field within `table.field_section.fields`, if any.
    oid_field_idx: Option<usize>,
}

impl Layer {
    fn open(dir: &Path, info: &LayerInfo) -> Result<Self> {
        let table_path = info.table_path(dir);
        let table_file = std::fs::File::open(&table_path)
            .map_err(|e| GdbError::malformed(format!("opening {}: {e}", table_path.display())))?;
        // SAFETY: standard mmap caveats — if the file is truncated or modified
        // by another process while mapped, accessing bytes may SIGBUS. We use
        // this read-only on local disk for a process-private view; that's the
        // canonical safe scenario for `memmap2::Mmap::map`.
        #[allow(unsafe_code)]
        let mmap = unsafe {
            Mmap::map(&table_file)
                .map_err(|e| GdbError::malformed(format!("mmap {}: {e}", table_path.display())))?
        };
        let tablx_bytes = std::fs::read(info.tablx_path(dir))?;

        let table = Table::parse(&mmap)?;
        let tablx = Tablx::parse(&tablx_bytes)?;

        let geom_field_idx = table.field_section.geometry_field_index();
        let oid_field_idx = table.field_section.objectid_field_index();
        let schema = build_schema(&table.field_section)?;

        Ok(Self {
            name: info.name.clone(),
            mmap,
            tablx,
            table,
            schema,
            geom_field_idx,
            oid_field_idx,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Number of valid (non-deleted) rows declared by the table header.
    pub fn feature_count(&self) -> i64 {
        self.table.header.valid_record_count
    }

    /// Iterate features in row order. Errors during decoding are surfaced
    /// per-feature; the iterator does not stop on the first error.
    pub fn read(&self) -> FeatureIter<'_> {
        FeatureIter {
            layer: self,
            inner: Box::new(self.tablx.iter_present()),
        }
    }
}

/// Iterator over the layer's features.
pub struct FeatureIter<'a> {
    layer: &'a Layer,
    inner: Box<dyn Iterator<Item = (u64, u64)> + 'a>,
}

impl<'a> std::fmt::Debug for FeatureIter<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureIter").finish()
    }
}

impl<'a> Iterator for FeatureIter<'a> {
    type Item = Result<Feature>;

    fn next(&mut self) -> Option<Self::Item> {
        let (row_idx, offset) = self.inner.next()?;
        Some(decode_one(self.layer, row_idx, offset))
    }
}

fn decode_one(layer: &Layer, row_idx: u64, offset: u64) -> Result<Feature> {
    let fid = (row_idx as i64) + 1;
    let blob = slice_row_blob(&layer.mmap, offset)?;
    let row = decode_row_blob(blob, fid, &layer.table.field_section)?;

    let geometry = match (row.geometry_blob.as_deref(), layer.geom_field_idx) {
        (Some(blob), Some(gidx)) => {
            let meta = layer.table.field_section.fields[gidx]
                .geometry
                .as_ref()
                .expect("geometry field index must point at a Geometry field with meta");
            Some(decode_shape_buffer(blob, meta)?)
        }
        _ => None,
    };

    // Build the user-facing attribute vector: drop OBJECTID and geometry slots.
    let mut attributes = Vec::with_capacity(row.values.len());
    for (i, v) in row.values.into_iter().enumerate() {
        if Some(i) == layer.oid_field_idx || Some(i) == layer.geom_field_idx {
            continue;
        }
        attributes.push(v);
    }

    Ok(Feature {
        fid: Some(fid),
        geometry,
        attributes,
    })
}

/// Build the user-facing [`Schema`] from a parsed field section. OBJECTID and
/// the geometry field are pulled out into [`Schema::geometry`] and (implicitly)
/// [`Feature::fid`]; everything else becomes a [`FieldDef`] in
/// [`Schema::fields`] in declaration order.
fn build_schema(fs: &FieldSection) -> Result<Schema> {
    let mut fields = Vec::new();
    let mut geometry: Option<GeomField> = None;
    let mut crs = Crs::Unknown;

    for f in &fs.fields {
        match f.ty {
            FieldTypeCode::ObjectId => continue,
            FieldTypeCode::Geometry => {
                if let Some(meta) = &f.geometry {
                    let extent = Some(extent_array_from_meta(
                        meta.extent_xy,
                        meta.extent_z,
                        meta.extent_m,
                    ));
                    geometry = Some(GeomField {
                        name: f.name.clone(),
                        kind: layer_geometry_type(&fs.flags),
                        has_z: meta.layer_has_z,
                        has_m: meta.layer_has_m,
                        extent,
                    });
                    if !meta.srs_wkt.is_empty() {
                        crs = Crs::Wkt(meta.srs_wkt.clone());
                    }
                }
            }
            other => {
                let value_type = field_type_to_value_type(other)?;
                let mut def = FieldDef::new(f.name.clone(), value_type, f.nullable);
                if let Some(alias) = f.alias.clone() {
                    def = def.with_alias(alias);
                }
                if let Some(w) = f.width {
                    def = def.with_width(w);
                }
                fields.push(def);
            }
        }
    }

    Ok(Schema::new(fields, geometry, crs))
}

/// Map the layer-flags geometry type code (low byte of layer flags) to the
/// abstract [`GeometryType`] declared by the schema. FileGDB layers are
/// inherently multi-part, so polyline/polygon layers expose
/// `MultiLineString` / `MultiPolygon` even when individual features have a
/// single part — this matches GDAL/OGR convention.
fn layer_geometry_type(flags: &LayerFlags) -> GeometryType {
    match flags.geometry_type_code() {
        1 => GeometryType::Point,
        2 => GeometryType::MultiPoint,
        3 => GeometryType::MultiLineString,
        4 => GeometryType::MultiPolygon,
        // 9 = multipatch — closest analogue is MultiPolygon for v0.1.
        9 => GeometryType::MultiPolygon,
        // Fallback: treat anything else (incl. 5 = rectangle / 0 = none) as
        // Polygon. We don't have a NoGeometry variant in the IR.
        _ => GeometryType::Polygon,
    }
}

fn extent_array_from_meta(xy: [f64; 4], z: Option<[f64; 2]>, _m: Option<[f64; 2]>) -> [f64; 6] {
    let (zmin, zmax) = z.map(|z| (z[0], z[1])).unwrap_or((f64::NAN, f64::NAN));
    [xy[0], xy[1], zmin, xy[2], xy[3], zmax]
}

fn field_type_to_value_type(t: FieldTypeCode) -> Result<ValueType> {
    use FieldTypeCode::*;
    Ok(match t {
        Int16 => ValueType::Int16,
        Int32 => ValueType::Int32,
        Int64 => ValueType::Int64,
        Float32 => ValueType::Float32,
        Float64 => ValueType::Float64,
        String => ValueType::String,
        DateTime | DateOnly | TimeOnly | DateTimeWithOffset => ValueType::DateTime,
        Binary => ValueType::Binary,
        Guid | GlobalId => ValueType::Guid,
        Xml => ValueType::Xml,
        ObjectId => {
            return Err(GdbError::malformed(
                "ObjectId should be handled by the API, not mapped to a ValueType",
            ))
        }
        Geometry => {
            return Err(GdbError::malformed(
                "Geometry should be handled separately, not mapped to a ValueType",
            ))
        }
        Raster => return Err(GdbError::unsupported("Raster fields not yet supported")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_geometry_type_mapping() {
        let mut f = LayerFlags(0);
        f.0 = 1;
        assert_eq!(layer_geometry_type(&f), GeometryType::Point);
        f.0 = 2;
        assert_eq!(layer_geometry_type(&f), GeometryType::MultiPoint);
        f.0 = 3;
        assert_eq!(layer_geometry_type(&f), GeometryType::MultiLineString);
        f.0 = 4;
        assert_eq!(layer_geometry_type(&f), GeometryType::MultiPolygon);
    }

    #[test]
    fn field_type_mapping_handles_all_attribute_types() {
        for t in [
            FieldTypeCode::Int16,
            FieldTypeCode::Int32,
            FieldTypeCode::Int64,
            FieldTypeCode::Float32,
            FieldTypeCode::Float64,
            FieldTypeCode::String,
            FieldTypeCode::DateTime,
            FieldTypeCode::DateOnly,
            FieldTypeCode::TimeOnly,
            FieldTypeCode::DateTimeWithOffset,
            FieldTypeCode::Binary,
            FieldTypeCode::Guid,
            FieldTypeCode::GlobalId,
            FieldTypeCode::Xml,
        ] {
            field_type_to_value_type(t).unwrap_or_else(|_| panic!("{t:?} should map"));
        }
        assert!(field_type_to_value_type(FieldTypeCode::ObjectId).is_err());
        assert!(field_type_to_value_type(FieldTypeCode::Geometry).is_err());
        assert!(field_type_to_value_type(FieldTypeCode::Raster).is_err());
    }
}

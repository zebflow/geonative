//! Format-agnostic dataset inspection: open any supported source, walk the
//! catalog, return a structured report.
//!
//! ## What it returns
//!
//! For each layer in the dataset:
//! - name (single-layer formats use `"default"`)
//! - declared feature count (if cheap)
//! - geometry kind + CRS + declared extent
//! - field definitions (name, type, nullable, width)
//!
//! ## What it does NOT return (yet)
//!
//! - Computed extent (use [`geonative_processing::profile`] for that)
//! - Per-field null counts / cardinality (also `profile`)
//! - Sample rows (`profile`)

use std::path::Path;

use geonative_core::{Crs, FieldDef, GeometryType, Schema, ValueType};
use serde::Serialize;

use crate::error::Result;

#[derive(Debug, Serialize)]
pub struct DatasetInspection {
    pub source: String,
    pub format: &'static str,
    pub layers: Vec<LayerInspection>,
}

#[derive(Debug, Serialize)]
pub struct LayerInspection {
    pub name: String,
    pub feature_count: Option<i64>,
    pub geometry: Option<GeometryInspection>,
    pub crs: CrsInspection,
    pub fields: Vec<FieldInspection>,
}

#[derive(Debug, Serialize)]
pub struct GeometryInspection {
    pub field_name: String,
    pub kind: String,
    pub has_z: bool,
    pub has_m: bool,
    /// `[xmin, ymin, zmin, xmax, ymax, zmax]` per geonative-core convention.
    /// May contain NaN if the source didn't declare an extent.
    pub declared_extent: Option<[f64; 6]>,
}

#[derive(Debug, Serialize)]
pub struct FieldInspection {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub nullable: bool,
    pub width: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CrsInspection {
    Unknown,
    Epsg { code: u32 },
    Wkt { wkt: String },
    Projjson { projjson: String },
}

impl From<&Crs> for CrsInspection {
    fn from(c: &Crs) -> Self {
        match c {
            Crs::Unknown => Self::Unknown,
            Crs::Epsg(n) => Self::Epsg { code: *n },
            Crs::Wkt(s) => Self::Wkt { wkt: s.clone() },
            Crs::Projjson(s) => Self::Projjson {
                projjson: s.clone(),
            },
            _ => Self::Unknown,
        }
    }
}

/// Top-level entry. Format is detected from extension; FileGDB walks every
/// layer, single-layer formats produce one `LayerInspection` named `"default"`.
pub fn inspect(source: &Path) -> Result<DatasetInspection> {
    use crate::io::Format;
    match Format::from_path(source)? {
        Format::FileGdb => inspect_filegdb(source),
        Format::Shapefile => inspect_shapefile(source),
        Format::GeoParquet => inspect_geoparquet(source),
        Format::GeoJson => inspect_geojson(source),
        // Raster has different shape (no layers/features/fields); use
        // RasterSource::open(path)?.profile_cloned() instead. A polymorphic
        // Inspection type that covers both modalities is a follow-up.
        Format::GeoTiff => Err(crate::error::ConvertError::invalid(
            "inspect doesn't support raster formats yet; call RasterSource::open + profile_cloned() instead",
        )),
    }
}

fn inspect_filegdb(source: &Path) -> Result<DatasetInspection> {
    let gdb = geonative_filegdb::open(source)?;
    let mut layers = Vec::with_capacity(gdb.layers().len());
    for info in gdb.layers() {
        let layer = gdb.layer(&info.name)?;
        layers.push(layer_inspection_from(
            &info.name,
            layer.schema(),
            Some(layer.feature_count()),
        ));
    }
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "filegdb",
        layers,
    })
}

fn inspect_shapefile(source: &Path) -> Result<DatasetInspection> {
    let shp = geonative_shapefile::open(source)?;
    let layer = layer_inspection_from("default", shp.schema(), Some(shp.feature_count() as i64));
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "shapefile",
        layers: vec![layer],
    })
}

fn inspect_geoparquet(source: &Path) -> Result<DatasetInspection> {
    let reader = geonative_geoparquet::GeoParquetReader::open(source)?;
    // GeoParquet 1.x is single-layer per file. Feature count would require
    // summing per-row-group counts from the parquet metadata; the reader
    // doesn't yet surface that, so we return None.
    let layer = layer_inspection_from("default", reader.schema(), None);
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "geoparquet",
        layers: vec![layer],
    })
}

fn inspect_geojson(source: &Path) -> Result<DatasetInspection> {
    let r = geonative_geojson::GeoJsonReader::open(source)?;
    let layer = layer_inspection_from("default", r.schema(), Some(r.feature_count() as i64));
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "geojson",
        layers: vec![layer],
    })
}

fn layer_inspection_from(
    name: &str,
    schema: &Schema,
    feature_count: Option<i64>,
) -> LayerInspection {
    LayerInspection {
        name: name.to_string(),
        feature_count,
        geometry: schema.geometry.as_ref().map(|g| GeometryInspection {
            field_name: g.name.clone(),
            kind: geometry_kind_name(g.kind).to_string(),
            has_z: g.has_z,
            has_m: g.has_m,
            declared_extent: g.extent,
        }),
        crs: (&schema.crs).into(),
        fields: schema.fields.iter().map(field_inspection_from).collect(),
    }
}

fn field_inspection_from(f: &FieldDef) -> FieldInspection {
    FieldInspection {
        name: f.name.clone(),
        ty: value_type_name(f.ty).to_string(),
        nullable: f.nullable,
        width: f.width,
    }
}

fn geometry_kind_name(t: GeometryType) -> &'static str {
    match t {
        GeometryType::Point => "Point",
        GeometryType::LineString => "LineString",
        GeometryType::Polygon => "Polygon",
        GeometryType::MultiPoint => "MultiPoint",
        GeometryType::MultiLineString => "MultiLineString",
        GeometryType::MultiPolygon => "MultiPolygon",
        GeometryType::GeometryCollection => "GeometryCollection",
        _ => "Geometry",
    }
}

fn value_type_name(t: ValueType) -> &'static str {
    match t {
        ValueType::Bool => "Bool",
        ValueType::Int16 => "Int16",
        ValueType::Int32 => "Int32",
        ValueType::Int64 => "Int64",
        ValueType::Float32 => "Float32",
        ValueType::Float64 => "Float64",
        ValueType::String => "String",
        ValueType::Binary => "Binary",
        ValueType::DateTime => "DateTime",
        ValueType::Guid => "Guid",
        ValueType::Xml => "Xml",
        _ => "Unknown",
    }
}

//! Format-agnostic dataset inspection: open any supported source, walk the
//! catalog, emit a JSON report.
//!
//! ## What it returns
//!
//! For each layer in the dataset:
//! - name (layers in single-layer formats use `"default"`)
//! - declared feature count (if cheap)
//! - geometry kind + CRS + declared extent
//! - field definitions (name, type, nullable, width)
//!
//! ## What it does NOT return (yet)
//!
//! - Computed extent (would require a full scan)
//! - Per-field null counts / cardinality (that's `profile`, not `inspect`)
//! - Sample rows (could add via `--sample N` flag; deferred)

use std::path::Path;

use geonative_core::{Crs, FieldDef, GeometryType, Schema, ValueType};
use serde::Serialize;

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
    /// `[xmin, ymin, zmin, xmax, ymax, zmax]` per the geonative-core convention.
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

/// Top-level entry: open `source` (format auto-detected from extension)
/// and walk its layers.
pub fn inspect(source: &Path) -> Result<DatasetInspection, String> {
    let ext = source
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            format!(
                "could not determine format from path: {} (extension required)",
                source.display()
            )
        })?;

    match ext.as_str() {
        "gdb" => inspect_filegdb(source),
        "shp" => inspect_shapefile(source),
        "parquet" => inspect_geoparquet(source),
        other => Err(format!(
            "unsupported input extension '.{other}' (supported: .gdb, .shp, .parquet)"
        )),
    }
}

fn inspect_filegdb(source: &Path) -> Result<DatasetInspection, String> {
    let gdb = geonative_filegdb::open(source).map_err(|e| format!("open .gdb: {e}"))?;
    let mut layers = Vec::with_capacity(gdb.layers().len());
    for info in gdb.layers() {
        let layer = gdb
            .layer(&info.name)
            .map_err(|e| format!("open layer '{}': {e}", info.name))?;
        layers.push(layer_inspection_from(&info.name, layer.schema(), Some(layer.feature_count())));
    }
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "filegdb",
        layers,
    })
}

fn inspect_shapefile(source: &Path) -> Result<DatasetInspection, String> {
    let shp =
        geonative_shapefile::Shapefile::open(source).map_err(|e| format!("open shapefile: {e}"))?;
    let layer = layer_inspection_from(
        "default",
        shp.schema(),
        Some(shp.feature_count() as i64),
    );
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "shapefile",
        layers: vec![layer],
    })
}

fn inspect_geoparquet(source: &Path) -> Result<DatasetInspection, String> {
    let reader =
        geonative_geoparquet::GeoParquetReader::open(source).map_err(|e| format!("open parquet: {e}"))?;
    // GeoParquet 1.x is single-layer per file (the spec doesn't have a
    // layer concept inside a parquet); we surface the schema as the
    // sole `"default"` layer. Feature count requires a row-group sum which
    // the parquet metadata exposes — but the reader doesn't yet expose it,
    // so we return None for now.
    let layer = layer_inspection_from("default", reader.schema(), None);
    Ok(DatasetInspection {
        source: source.display().to_string(),
        format: "geoparquet",
        layers: vec![layer],
    })
}

fn layer_inspection_from(name: &str, schema: &Schema, feature_count: Option<i64>) -> LayerInspection {
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

//! Top-level public API: `Shapefile`, feature iterator.
//!
//! A shapefile is a single-layer dataset (unlike a FileGDB), so the type
//! exposes layer-level methods directly. Mmap-backed for the `.shp`
//! payload so multi-GB shapefiles stay bounded in app-private memory.

use std::path::{Path, PathBuf};

use geonative_core::{Crs, Feature, GeomField, GeometryType, Schema, Value};
use memmap2::Mmap;

use crate::dbf::{build_schema, decode_field, parse_header, DbfHeader};
use crate::error::{Result, ShpError};
use crate::header::ShapeType;
use crate::shape;
use crate::shx;

/// One opened shapefile (`.shp` + `.shx` + `.dbf` + optional `.prj`).
#[derive(Debug)]
pub struct Shapefile {
    /// Mmapped `.shp` (bounded RAM regardless of file size).
    shp_mmap: Mmap,
    shx: shx::Shx,
    dbf_bytes: Vec<u8>,
    dbf_header: DbfHeader,
    schema: Schema,
}

impl Shapefile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let base = strip_shp_ext(path.as_ref());
        let shp_path = base.with_extension("shp");
        let shx_path = base.with_extension("shx");
        let dbf_path = base.with_extension("dbf");
        let prj_path = base.with_extension("prj");

        let shp_file = std::fs::File::open(&shp_path)
            .map_err(|e| ShpError::MissingFile(format!("{}: {e}", shp_path.display())))?;
        // SAFETY: standard mmap caveats — file truncation/modification while
        // mapped may SIGBUS. Read-only, process-private view of a local file.
        #[allow(unsafe_code)]
        let shp_mmap = unsafe { Mmap::map(&shp_file)? };

        let shx_bytes = std::fs::read(&shx_path)
            .map_err(|e| ShpError::MissingFile(format!("{}: {e}", shx_path.display())))?;
        let shx = shx::parse(&shx_bytes)?;

        let dbf_bytes = std::fs::read(&dbf_path)
            .map_err(|e| ShpError::MissingFile(format!("{}: {e}", dbf_path.display())))?;
        let dbf_header = parse_header(&dbf_bytes)?;

        let prj = std::fs::read_to_string(&prj_path).ok();
        let crs = match prj {
            Some(s) if !s.trim().is_empty() => Crs::Wkt(s.trim().to_string()),
            _ => Crs::Unknown,
        };

        let geom_field = GeomField {
            name: "geometry".to_string(),
            kind: shape_type_to_geometry_type(shx.header.shape_type),
            has_z: false,
            has_m: false,
            extent: Some([
                shx.header.bbox_xy[0],
                shx.header.bbox_xy[1],
                shx.header.bbox_z[0],
                shx.header.bbox_xy[2],
                shx.header.bbox_xy[3],
                shx.header.bbox_z[1],
            ]),
        };
        let schema = build_schema(&dbf_header, geom_field, crs);

        Ok(Self {
            shp_mmap,
            shx,
            dbf_bytes,
            dbf_header,
            schema,
        })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn feature_count(&self) -> usize {
        // Trust the .dbf record count (matches .shx record count for well-
        // formed files; we don't repair mismatches in v0.1).
        self.dbf_header.n_records as usize
    }

    pub fn shape_type(&self) -> ShapeType {
        self.shx.header.shape_type
    }

    /// Iterate features in record order. Lazy — each step does one shape
    /// decode + one DBF row decode.
    pub fn read(&self) -> FeatureIter<'_> {
        FeatureIter {
            shp: self,
            index: 0,
        }
    }
}

fn strip_shp_ext(p: &Path) -> PathBuf {
    if p.extension().and_then(|s| s.to_str()).is_some() {
        p.with_extension("")
    } else {
        p.to_path_buf()
    }
}

fn shape_type_to_geometry_type(s: ShapeType) -> GeometryType {
    match s {
        ShapeType::Point | ShapeType::PointZ | ShapeType::PointM => GeometryType::Point,
        ShapeType::Polyline | ShapeType::PolylineZ | ShapeType::PolylineM => {
            GeometryType::MultiLineString
        }
        ShapeType::Polygon | ShapeType::PolygonZ | ShapeType::PolygonM => {
            GeometryType::MultiPolygon
        }
        ShapeType::Multipoint | ShapeType::MultipointZ | ShapeType::MultipointM => {
            GeometryType::MultiPoint
        }
        _ => GeometryType::Polygon, // multipatch / null — treat as polygon best-effort
    }
}

#[derive(Debug)]
pub struct FeatureIter<'a> {
    shp: &'a Shapefile,
    index: usize,
}

impl<'a> Iterator for FeatureIter<'a> {
    type Item = Result<Feature>;

    fn next(&mut self) -> Option<Self::Item> {
        let total = self.shp.shx.records.len();
        while self.index < total {
            let i = self.index;
            self.index += 1;

            match decode_one(self.shp, i) {
                Ok(Some(f)) => return Some(Ok(f)),
                Ok(None) => continue, // deleted row
                Err(e) => return Some(Err(e)),
            }
        }
        None
    }
}

fn decode_one(shp: &Shapefile, i: usize) -> Result<Option<Feature>> {
    let rec = &shp.shx.records[i];
    let start = rec.offset_bytes as usize;
    let end = start + 8 + rec.content_len_bytes as usize;
    if end > shp.shp_mmap.len() {
        return Err(ShpError::malformed(format!(
            "record {i} runs past .shp EOF (offset {start} + content {})",
            rec.content_len_bytes
        )));
    }
    let content = &shp.shp_mmap[start + 8..end];
    let geom = shape::decode_record_content(content)?;

    // DBF row: skip header, then i × record_len bytes.
    let dbf_off = shp.dbf_header.header_len as usize + i * shp.dbf_header.record_len as usize;
    if dbf_off + shp.dbf_header.record_len as usize > shp.dbf_bytes.len() {
        return Ok(None); // ran past the table
    }
    let row = &shp.dbf_bytes[dbf_off..dbf_off + shp.dbf_header.record_len as usize];
    if row.first() == Some(&0x2A) {
        return Ok(None); // deleted row
    }

    let mut attrs: Vec<Value> = Vec::with_capacity(shp.dbf_header.fields.len());
    for f in &shp.dbf_header.fields {
        attrs.push(decode_field(row, f));
    }

    Ok(Some(Feature {
        fid: Some((i as i64) + 1),
        geometry: Some(geom),
        attributes: attrs,
    }))
}

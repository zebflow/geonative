//! GeoParquet **reader** — the inverse of [`crate::writer::GeoParquetWriter`].
//!
//! ## What this module is
//!
//! Reads a GeoParquet file written by any spec-compliant 1.0/1.1 writer
//! (ours, GDAL, GeoPandas, DuckDB, …) and emits a stream of
//! `geonative_core::Feature`s + a reconstructed `Schema`.
//!
//! ## How it sits in the architecture
//!
//! - Parses the parquet file's `geo` key in file-level metadata for the
//!   primary geometry column name and CRS (via [`crate::meta::parse_geo_metadata`]).
//! - Reconstructs a `Schema` by walking the Arrow schema, mapping each
//!   Arrow `DataType` back to `ValueType`, and recognising the geometry
//!   column + bbox covering columns (which we **drop** from user attributes).
//! - Decodes geometry per row via `Geometry::from_wkb` (shipped in core).
//! - Decodes attributes per column with type-tagged dispatch — the inverse
//!   of the writer's `AttrBuilder`.
//!
//! ## Clever bits
//!
//! - **Reads `bbox` covering columns ONCE for filter, then hides them** —
//!   xmin/ymin/xmax/ymax aren't user attributes; they're emitted by our own
//!   writer for predicate pushdown and re-detected here so round-trip with
//!   our writer produces the same `Schema` as the original layer.
//! - **Resilient to missing `geo` metadata.** Files written by tools that
//!   don't follow the spec still load — geometry column is guessed from
//!   conventional names (`geometry`, `geom`, `SHAPE`, `wkb_geometry`, `the_geom`).
//! - **Lazy per-batch decode.** The reader materialises one `RecordBatch`
//!   at a time from parquet; features within the batch are produced lazily
//!   on `.next()`. Peak memory ≈ one row group worth.

use std::fs::File;
use std::path::Path;

use arrow::array::{
    Array, BinaryArray, BooleanArray, FixedSizeBinaryArray, Float32Array, Float64Array, Int16Array,
    Int32Array, Int64Array, RecordBatch, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, SchemaRef, TimeUnit};
use geonative_core::{
    Crs, Feature, FieldDef, GeomField, Geometry, GeometryType, Schema as CoreSchema, Value,
    ValueType,
};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::file::reader::{FileReader, SerializedFileReader};

use crate::error::{GeoParquetError, Result};
use crate::meta::parse_geo_metadata;

const BBOX_COL_NAMES: &[&str] = &["xmin", "ymin", "xmax", "ymax"];
const GEOMETRY_COL_FALLBACKS: &[&str] = &[
    "geometry",
    "geom",
    "SHAPE",
    "wkb_geometry",
    "the_geom",
    "shape",
];

/// Open and iterate a GeoParquet file.
pub struct GeoParquetReader {
    arrow_schema: SchemaRef,
    core_schema: CoreSchema,
    geom_col_idx: usize,
    /// Indices of attribute columns in arrow order — excludes geometry + bbox cols.
    attr_col_indices: Vec<usize>,
    /// File path kept so we can rebuild the parquet reader for `read()`.
    /// (parquet's reader-builder consumes itself on `build`.)
    inner_reader: ParquetRecordBatchReader,
}

impl GeoParquetReader {
    /// Open a parquet file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file_for_meta = File::open(&path)?;
        let geo_json = file_geo_metadata(&file_for_meta)?;
        drop(file_for_meta);

        let file_for_read = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file_for_read)?;
        let arrow_schema = builder.schema().clone();
        let inner_reader = builder.build()?;

        let (core_schema, geom_col_idx, attr_col_indices) =
            reconstruct_schema(&arrow_schema, geo_json.as_deref())?;

        Ok(Self {
            arrow_schema,
            core_schema,
            geom_col_idx,
            attr_col_indices,
            inner_reader,
        })
    }

    pub fn schema(&self) -> &CoreSchema {
        &self.core_schema
    }

    pub fn arrow_schema(&self) -> &SchemaRef {
        &self.arrow_schema
    }

    /// Consume the reader and produce a feature iterator. One pass.
    pub fn into_features(self) -> FeatureReadIter {
        FeatureReadIter {
            inner: self.inner_reader,
            geom_col_idx: self.geom_col_idx,
            attr_col_indices: self.attr_col_indices,
            current_batch: None,
            cursor: 0,
        }
    }
}

/// Returns the `geo` metadata JSON string, if present.
fn file_geo_metadata(file: &File) -> Result<Option<String>> {
    let reader = SerializedFileReader::new(file.try_clone()?)?;
    let md = reader.metadata();
    Ok(md
        .file_metadata()
        .key_value_metadata()
        .as_ref()
        .and_then(|kvs| kvs.iter().find(|kv| kv.key == "geo"))
        .and_then(|kv| kv.value.clone()))
}

/// From the Arrow schema and the (optional) `geo` JSON, produce:
/// - the reconstructed user-facing `Schema`
/// - the Arrow-column index of the geometry column
/// - the Arrow-column indices of the user attribute columns (skipping bbox cols)
pub(crate) fn reconstruct_schema(
    arrow: &SchemaRef,
    geo_json: Option<&str>,
) -> Result<(CoreSchema, usize, Vec<usize>)> {
    let parsed = geo_json.map(parse_geo_metadata).unwrap_or_default();

    // Resolve geometry column name → arrow index.
    let geom_col_idx = if let Some(name) = parsed.primary_column.as_deref() {
        arrow.index_of(name).map_err(|_| {
            GeoParquetError::schema(format!("geo primary_column '{name}' not in arrow schema"))
        })?
    } else {
        let mut found = None;
        for cand in GEOMETRY_COL_FALLBACKS {
            if let Ok(i) = arrow.index_of(cand) {
                if matches!(
                    arrow.field(i).data_type(),
                    DataType::Binary | DataType::LargeBinary
                ) {
                    found = Some(i);
                    break;
                }
            }
        }
        found.ok_or_else(|| {
            GeoParquetError::schema("no geo metadata and no conventional geometry column found")
        })?
    };

    // Walk arrow fields → build core fields, skipping geometry + bbox cols.
    //
    // Bbox covering columns come in two shapes depending on the writer:
    //   - Flat: four siblings named xmin/ymin/xmax/ymax (Float32 OR Float64).
    //     This is what our own writer emits and what GDAL+geopandas use.
    //   - Struct: a single column (typically named `bbox`) of
    //     Struct{xmin,ymin,xmax,ymax} — used by DuckDB's spatial extension
    //     and PostGIS COPY exports. Either form is valid per GeoParquet 1.1.
    // Both are auxiliary metadata for predicate pushdown, not user attributes,
    // so we hide them from the reconstructed schema either way.
    let mut fields = Vec::new();
    let mut attr_col_indices = Vec::new();
    for (i, f) in arrow.fields().iter().enumerate() {
        if i == geom_col_idx {
            continue;
        }
        if is_flat_bbox_column(f.name(), f.data_type()) {
            continue;
        }
        if is_bbox_struct_column(f.data_type()) {
            continue;
        }
        let ty = value_type_for_arrow(f.data_type())?;
        fields.push(FieldDef::new(f.name(), ty, f.is_nullable()));
        attr_col_indices.push(i);
    }

    let geom_arrow = arrow.field(geom_col_idx);
    let kind = guess_geometry_type_from_metadata(geo_json).unwrap_or(GeometryType::Polygon);
    let geom_field = GeomField {
        name: geom_arrow.name().clone(),
        kind,
        has_z: false,
        has_m: false,
        extent: None,
    };

    let crs = match parsed.epsg_code {
        Some(n) => Crs::Epsg(n),
        None => Crs::Unknown,
    };

    let schema = CoreSchema::new(fields, Some(geom_field), crs);
    Ok((schema, geom_col_idx, attr_col_indices))
}

fn is_flat_bbox_column(name: &str, dt: &DataType) -> bool {
    BBOX_COL_NAMES.contains(&name) && matches!(dt, DataType::Float32 | DataType::Float64)
}

/// True for an Arrow Struct that exposes (xmin, ymin, xmax, ymax) sub-fields
/// of any float width. The struct's outer name varies by writer (`bbox` for
/// DuckDB, sometimes `bounds` or arbitrary) so we identify by shape, not
/// name — false-positives on a user-named bbox-shaped struct are vanishingly
/// rare in real schemas.
fn is_bbox_struct_column(dt: &DataType) -> bool {
    let DataType::Struct(children) = dt else {
        return false;
    };
    if children.len() != 4 {
        return false;
    }
    let names: std::collections::HashSet<&str> =
        children.iter().map(|c| c.name().as_str()).collect();
    let required = ["xmin", "ymin", "xmax", "ymax"];
    if !required.iter().all(|r| names.contains(r)) {
        return false;
    }
    children
        .iter()
        .all(|c| matches!(c.data_type(), DataType::Float32 | DataType::Float64))
}

/// Best-effort: look for `"geometry_types":["..."]` and take the first one.
fn guess_geometry_type_from_metadata(json: Option<&str>) -> Option<GeometryType> {
    let s = json?;
    let key = "\"geometry_types\":[";
    let pos = s.find(key)?;
    let after = &s[pos + key.len()..];
    let open = after.find('"')?;
    let body = &after[open + 1..];
    let close = body.find('"')?;
    let name = &body[..close];
    Some(match name {
        "Point" => GeometryType::Point,
        "LineString" => GeometryType::LineString,
        "Polygon" => GeometryType::Polygon,
        "MultiPoint" => GeometryType::MultiPoint,
        "MultiLineString" => GeometryType::MultiLineString,
        "MultiPolygon" => GeometryType::MultiPolygon,
        "GeometryCollection" => GeometryType::GeometryCollection,
        _ => return None,
    })
}

fn value_type_for_arrow(dt: &DataType) -> Result<ValueType> {
    Ok(match dt {
        DataType::Boolean => ValueType::Bool,
        DataType::Int16 => ValueType::Int16,
        DataType::Int32 => ValueType::Int32,
        DataType::Int64 => ValueType::Int64,
        DataType::Float32 => ValueType::Float32,
        DataType::Float64 => ValueType::Float64,
        DataType::Utf8 | DataType::LargeUtf8 => ValueType::String,
        DataType::Binary | DataType::LargeBinary => ValueType::Binary,
        DataType::Timestamp(TimeUnit::Microsecond, _) => ValueType::DateTime,
        DataType::FixedSizeBinary(16) => ValueType::Guid,
        other => {
            return Err(GeoParquetError::unsupported(format!(
                "Arrow type {other:?} has no ValueType mapping (v0.1)"
            )))
        }
    })
}

/// One Microsecond-since-epoch back to GDB-style days since 1899-12-30.
/// Inverse of `gdb_days_to_unix_micros`.
fn unix_micros_to_gdb_days(us: i64) -> f64 {
    const EPOCH_OFFSET_DAYS: f64 = 25569.0;
    const MICROS_PER_DAY: f64 = 86_400.0 * 1_000_000.0;
    (us as f64) / MICROS_PER_DAY + EPOCH_OFFSET_DAYS
}

/// Iterator over features from a GeoParquet file.
pub struct FeatureReadIter {
    inner: ParquetRecordBatchReader,
    geom_col_idx: usize,
    attr_col_indices: Vec<usize>,
    current_batch: Option<RecordBatch>,
    cursor: usize,
}

impl Iterator for FeatureReadIter {
    type Item = Result<Feature>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Need to (re)fill the current batch?
            let needs_refill = match &self.current_batch {
                None => true,
                Some(b) => self.cursor >= b.num_rows(),
            };
            if needs_refill {
                match self.inner.next() {
                    None => return None,
                    Some(Err(e)) => return Some(Err(GeoParquetError::Arrow(e))),
                    Some(Ok(batch)) => {
                        self.current_batch = Some(batch);
                        self.cursor = 0;
                        // Loop back: re-check needs_refill so an empty batch
                        // is skipped instead of triggering an out-of-bounds
                        // decode.
                        continue;
                    }
                }
            }

            let batch = self.current_batch.as_ref().expect("non-empty after refill");
            let row = self.cursor;
            self.cursor += 1;
            return Some(decode_row(
                batch,
                row,
                self.geom_col_idx,
                &self.attr_col_indices,
            ));
        }
    }
}

pub(crate) fn decode_row(
    batch: &RecordBatch,
    row: usize,
    geom_col_idx: usize,
    attr_col_indices: &[usize],
) -> Result<Feature> {
    // Geometry.
    let geom_col = batch.column(geom_col_idx);
    let geometry = if geom_col.is_null(row) {
        None
    } else {
        let bytes = geom_col
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or_else(|| GeoParquetError::schema("geometry column is not Binary"))?
            .value(row);
        Some(Geometry::from_wkb(bytes)?)
    };

    // Attributes.
    let mut attrs = Vec::with_capacity(attr_col_indices.len());
    for &ci in attr_col_indices {
        let col = batch.column(ci);
        if col.is_null(row) {
            attrs.push(Value::Null);
            continue;
        }
        let v = match col.data_type() {
            DataType::Boolean => Value::Bool(
                col.as_any()
                    .downcast_ref::<BooleanArray>()
                    .unwrap()
                    .value(row),
            ),
            DataType::Int16 => Value::Int16(
                col.as_any()
                    .downcast_ref::<Int16Array>()
                    .unwrap()
                    .value(row),
            ),
            DataType::Int32 => Value::Int32(
                col.as_any()
                    .downcast_ref::<Int32Array>()
                    .unwrap()
                    .value(row),
            ),
            DataType::Int64 => Value::Int64(
                col.as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap()
                    .value(row),
            ),
            DataType::Float32 => Value::Float32(
                col.as_any()
                    .downcast_ref::<Float32Array>()
                    .unwrap()
                    .value(row),
            ),
            DataType::Float64 => Value::Float64(
                col.as_any()
                    .downcast_ref::<Float64Array>()
                    .unwrap()
                    .value(row),
            ),
            DataType::Utf8 => Value::String(
                col.as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap()
                    .value(row)
                    .to_string(),
            ),
            DataType::Binary => Value::Binary(
                col.as_any()
                    .downcast_ref::<BinaryArray>()
                    .unwrap()
                    .value(row)
                    .to_vec(),
            ),
            DataType::Timestamp(TimeUnit::Microsecond, _) => {
                let us = col
                    .as_any()
                    .downcast_ref::<TimestampMicrosecondArray>()
                    .unwrap()
                    .value(row);
                Value::DateTime(unix_micros_to_gdb_days(us))
            }
            DataType::FixedSizeBinary(16) => {
                let bytes = col
                    .as_any()
                    .downcast_ref::<FixedSizeBinaryArray>()
                    .unwrap()
                    .value(row);
                let mut buf = [0u8; 16];
                buf.copy_from_slice(bytes);
                Value::Guid(buf)
            }
            other => {
                return Err(GeoParquetError::unsupported(format!(
                    "row decode: Arrow type {other:?} not yet supported"
                )))
            }
        };
        attrs.push(v);
    }

    Ok(Feature {
        fid: None,
        geometry,
        attributes: attrs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::{Crs, FieldDef, GeomField, Geometry, GeometryType, LineString, Schema};

    use crate::writer::{GeoParquetWriter, WriterOptions};

    fn make_schema() -> Schema {
        Schema::new(
            vec![
                FieldDef::new("id", ValueType::Int32, false),
                FieldDef::new("name", ValueType::String, true),
            ],
            Some(GeomField::new("geometry", GeometryType::LineString)),
            Crs::Epsg(4326),
        )
    }

    fn make_features() -> Vec<Feature> {
        (0i32..5)
            .map(|i| {
                Feature::new(
                    Some(i as i64 + 1),
                    Some(Geometry::LineString(LineString::new(vec![
                        geonative_core::Coord::xy(i as f64, i as f64),
                        geonative_core::Coord::xy((i + 1) as f64, (i + 1) as f64),
                    ]))),
                    vec![Value::Int32(i + 1), Value::String(format!("name-{i}"))],
                )
            })
            .collect()
    }

    #[test]
    fn round_trip_five_features() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let schema = make_schema();
        let features = make_features();

        let file = std::fs::File::create(&path).unwrap();
        let mut w = GeoParquetWriter::create(file, &schema, WriterOptions::default()).unwrap();
        for f in &features {
            w.write(f).unwrap();
        }
        w.close().unwrap();

        let reader = GeoParquetReader::open(&path).unwrap();
        // Schema should round-trip on names + ValueTypes (FID is not stored in parquet).
        let read_schema = reader.schema().clone();
        assert_eq!(read_schema.fields.len(), 2);
        assert_eq!(read_schema.fields[0].name, "id");
        assert_eq!(read_schema.fields[1].name, "name");
        assert!(read_schema.geometry.is_some());
        assert_eq!(read_schema.crs, Crs::Epsg(4326));

        let read_features: Vec<Feature> =
            reader.into_features().collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(read_features.len(), features.len());
        // FID is not persisted in GeoParquet, so compare only geometry + attrs.
        for (i, (orig, back)) in features.iter().zip(&read_features).enumerate() {
            assert_eq!(orig.geometry, back.geometry, "geom row {i}");
            assert_eq!(orig.attributes, back.attributes, "attrs row {i}");
        }
    }

    #[test]
    fn reader_sees_canonical_geometry_name_by_default() {
        // Default writer policy (since v0.3.1): canonicalise any source
        // geometry name to "geometry". The reader should see exactly that
        // back, via the `primary_column` we wrote into geo metadata.
        let schema = Schema::new(
            vec![FieldDef::new("id", ValueType::Int32, false)],
            Some(GeomField::new("geom", GeometryType::Point)),
            Crs::Unknown,
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut w = GeoParquetWriter::create(file, &schema, WriterOptions::default()).unwrap();
        w.write(&Feature::new(
            Some(1),
            Some(Geometry::Point(geonative_core::Coord::xy(1.0, 2.0))),
            vec![Value::Int32(42)],
        ))
        .unwrap();
        w.close().unwrap();

        let reader = GeoParquetReader::open(tmp.path()).unwrap();
        let geom_field = reader.schema().geometry.as_ref().unwrap();
        assert_eq!(geom_field.name, "geometry"); // canonicalised from "geom"
    }

    #[test]
    fn reader_sees_preserved_geometry_name_when_opted_in() {
        let schema = Schema::new(
            vec![FieldDef::new("id", ValueType::Int32, false)],
            Some(GeomField::new("geom", GeometryType::Point)),
            Crs::Unknown,
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut w = GeoParquetWriter::create(
            file,
            &schema,
            WriterOptions {
                preserve_source_geometry_name: true,
                ..WriterOptions::default()
            },
        )
        .unwrap();
        w.write(&Feature::new(
            Some(1),
            Some(Geometry::Point(geonative_core::Coord::xy(1.0, 2.0))),
            vec![Value::Int32(42)],
        ))
        .unwrap();
        w.close().unwrap();

        let reader = GeoParquetReader::open(tmp.path()).unwrap();
        let geom_field = reader.schema().geometry.as_ref().unwrap();
        assert_eq!(geom_field.name, "geom"); // preserved by opt-in
    }
}

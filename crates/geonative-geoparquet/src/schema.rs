//! Map a [`geonative_core::Schema`] to an [`arrow::datatypes::Schema`].
//!
//! Output column order:
//! 1. `geometry` (WKB, [`DataType::Binary`])
//! 2. each attribute field in [`geonative_core::Schema::fields`] order
//! 3. optional bbox columns (`xmin`, `ymin`, `xmax`, `ymax` as `Float64`)
//!    when [`SchemaMapOptions::add_bbox_columns`] is true — used by
//!    GeoParquet 1.1 readers for row-group pruning. Zebflow's
//!    `geoparquet_optimize.rs` adds exactly these.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use geonative_core::{Schema as CoreSchema, ValueType};

use crate::error::{GeoParquetError, Result};

#[derive(Debug, Clone)]
pub struct SchemaMapOptions {
    /// Canonical name to write for the geometry column. Defaults to
    /// `"geometry"` — the convention GeoParquet 1.1 examples and most
    /// downstream tools (DuckDB, Polars, PostGIS COPY, GovEyes-style web
    /// stacks) expect.
    pub geometry_column_name: String,
    /// If `true`, keep whatever the source schema's `Schema.geometry.name`
    /// says (e.g. `"SHAPE"` for FileGDB-origin files, `"geom"` for some
    /// PostGIS dumps). Default `false` — i.e. **canonicalize on write**.
    ///
    /// Enable this only when you need round-trip identity (e.g. you're
    /// rewriting an existing GeoParquet and don't want downstream tools
    /// to see a schema change).
    pub preserve_source_geometry_name: bool,
    pub add_bbox_columns: bool,
}

impl Default for SchemaMapOptions {
    fn default() -> Self {
        Self {
            geometry_column_name: "geometry".into(),
            preserve_source_geometry_name: false,
            add_bbox_columns: true,
        }
    }
}

/// Result of mapping a core schema to an Arrow schema.
#[derive(Debug, Clone)]
pub struct MappedSchema {
    /// Arrow schema in column order: geometry, attributes, [bbox cols].
    pub arrow: Arc<ArrowSchema>,
    /// Index of the geometry column within `arrow.fields`. Always 0 for now.
    pub geometry_col_index: usize,
    /// Indices of the bbox columns `(xmin, ymin, xmax, ymax)` in
    /// `arrow.fields`, if they were added.
    pub bbox_col_indices: Option<(usize, usize, usize, usize)>,
    /// Index into `arrow.fields` for each core attribute field, in core order.
    pub attribute_col_indices: Vec<usize>,
}

impl MappedSchema {
    /// Number of Arrow columns total (geometry + attributes + maybe bbox).
    pub fn n_cols(&self) -> usize {
        self.arrow.fields().len()
    }
}

pub fn map_schema(core: &CoreSchema, opts: &SchemaMapOptions) -> Result<MappedSchema> {
    let mut fields: Vec<Field> = Vec::with_capacity(1 + core.fields.len() + 4);

    // Geometry column. WKB-encoded.
    //
    // Naming policy: the canonical `opts.geometry_column_name` wins by
    // default. The source's declared name is honoured only when the
    // caller explicitly opted in to preservation. Either way, the chosen
    // name flows through to GeoParquet `geo.primary_column` metadata so
    // spec-aware readers stay correct.
    let geom_name = if opts.preserve_source_geometry_name {
        core.geometry
            .as_ref()
            .map(|g| g.name.clone())
            .unwrap_or_else(|| opts.geometry_column_name.clone())
    } else {
        opts.geometry_column_name.clone()
    };

    // Collision check: if an attribute field already has this name, fail
    // loudly rather than silently produce a malformed Arrow schema.
    if let Some(clash) = core.fields.iter().find(|f| f.name == geom_name) {
        return Err(GeoParquetError::Schema(format!(
            "geometry column name '{geom_name}' collides with attribute field '{}'. \
             Either rename the attribute upstream or set \
             SchemaMapOptions::geometry_column_name to a non-conflicting value.",
            clash.name
        )));
    }

    fields.push(Field::new(&geom_name, DataType::Binary, true));
    let geometry_col_index = 0;

    // Attribute columns.
    let mut attribute_col_indices = Vec::with_capacity(core.fields.len());
    for f in &core.fields {
        let dt = arrow_type_for(f.ty)?;
        let arrow_field = Field::new(&f.name, dt, f.nullable);
        attribute_col_indices.push(fields.len());
        fields.push(arrow_field);
    }

    // Optional bbox covering columns.
    let bbox_col_indices = if opts.add_bbox_columns {
        let i0 = fields.len();
        fields.push(Field::new("xmin", DataType::Float64, true));
        fields.push(Field::new("ymin", DataType::Float64, true));
        fields.push(Field::new("xmax", DataType::Float64, true));
        fields.push(Field::new("ymax", DataType::Float64, true));
        Some((i0, i0 + 1, i0 + 2, i0 + 3))
    } else {
        None
    };

    Ok(MappedSchema {
        arrow: Arc::new(ArrowSchema::new(fields)),
        geometry_col_index,
        bbox_col_indices,
        attribute_col_indices,
    })
}

/// Map a core [`ValueType`] to its Arrow [`DataType`].
///
/// `DateTime` is mapped to **Timestamp(Microsecond, UTC)**. Geonative stores
/// GDB datetimes as `f64` days since 1899-12-30; the writer converts to
/// microseconds-since-epoch (UTC) at row-build time so downstream tools
/// (DataFusion, DuckDB, Polars) can use it idiomatically.
pub fn arrow_type_for(t: ValueType) -> Result<DataType> {
    Ok(match t {
        ValueType::Bool => DataType::Boolean,
        ValueType::Int16 => DataType::Int16,
        ValueType::Int32 => DataType::Int32,
        ValueType::Int64 => DataType::Int64,
        ValueType::Float32 => DataType::Float32,
        ValueType::Float64 => DataType::Float64,
        ValueType::String => DataType::Utf8,
        ValueType::Binary => DataType::Binary,
        ValueType::DateTime => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        ValueType::Guid => DataType::FixedSizeBinary(16),
        ValueType::Xml => DataType::Utf8,
        // Future ValueType variants — refuse rather than silently coerce.
        // Callers can extend this map when they upgrade geonative-core and
        // need first-class support for the new variant.
        _ => {
            return Err(GeoParquetError::unsupported(format!(
                "unrecognized ValueType {t:?} — geonative-core may be newer than this crate"
            )))
        }
    })
}

/// Convert a GDB-style datetime (f64 days since 1899-12-30 00:00:00 UTC) to
/// microseconds since 1970-01-01 00:00:00 UTC.
pub fn gdb_days_to_unix_micros(days: f64) -> i64 {
    // 1970-01-01 minus 1899-12-30 = 25569 days.
    const EPOCH_OFFSET_DAYS: f64 = 25569.0;
    const MICROS_PER_DAY: f64 = 86_400.0 * 1_000_000.0;
    ((days - EPOCH_OFFSET_DAYS) * MICROS_PER_DAY) as i64
}

#[allow(dead_code)] // schema-relative diagnostic; will be wired into the writer in 2c.
pub(crate) fn schema_mapping_summary(m: &MappedSchema) -> String {
    let mut out = String::new();
    for (i, f) in m.arrow.fields().iter().enumerate() {
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(&format!("{i}:{}:{}", f.name(), f.data_type()));
    }
    out
}

impl From<GeoParquetError> for arrow::error::ArrowError {
    fn from(_e: GeoParquetError) -> Self {
        // Bridge for places that need a plain ArrowError; preserve the message.
        arrow::error::ArrowError::ExternalError(Box::new(_e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::{Crs, FieldDef, GeomField, GeometryType};

    fn build_core_schema() -> CoreSchema {
        CoreSchema::new(
            vec![
                FieldDef::new("UFI", ValueType::Int32, false),
                FieldDef::new("NAME", ValueType::String, true),
                FieldDef::new("CREATED", ValueType::DateTime, true),
            ],
            Some(GeomField::new("SHAPE", GeometryType::MultiLineString)),
            Crs::Epsg(7844),
        )
    }

    #[test]
    fn maps_geometry_first_then_attributes_then_bbox_canonicalised_by_default() {
        // Default policy: source said "SHAPE" but we canonicalise to "geometry"
        // for downstream tools that expect the GeoParquet 1.1 convention.
        let core = build_core_schema();
        let m = map_schema(&core, &SchemaMapOptions::default()).unwrap();
        let names: Vec<&str> = m.arrow.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec!["geometry", "UFI", "NAME", "CREATED", "xmin", "ymin", "xmax", "ymax"]
        );
        assert_eq!(m.geometry_col_index, 0);
        assert_eq!(m.attribute_col_indices, vec![1, 2, 3]);
        assert_eq!(m.bbox_col_indices, Some((4, 5, 6, 7)));
    }

    #[test]
    fn preserves_source_geometry_name_when_opted_in() {
        // preserve_source_geometry_name=true → keep "SHAPE" exactly as
        // the source declared it. Round-trip identity.
        let core = build_core_schema();
        let m = map_schema(
            &core,
            &SchemaMapOptions {
                preserve_source_geometry_name: true,
                ..Default::default()
            },
        )
        .unwrap();
        let names: Vec<&str> = m.arrow.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names[0], "SHAPE");
    }

    #[test]
    fn custom_canonical_name_honoured() {
        // Callers can pick a different canonical name (e.g. "geom" for
        // some PostGIS-friendly conventions).
        let core = build_core_schema();
        let m = map_schema(
            &core,
            &SchemaMapOptions {
                geometry_column_name: "geom".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(m.arrow.field(0).name(), "geom");
    }

    #[test]
    fn collision_with_attribute_name_errors_clearly() {
        // If a source attribute is already named "geometry", canonicalising
        // would create two columns with the same name — refuse loudly.
        let core = CoreSchema::new(
            vec![
                FieldDef::new("geometry", ValueType::String, true), // ← attribute clash
                FieldDef::new("name", ValueType::String, true),
            ],
            Some(GeomField::new("SHAPE", GeometryType::Point)),
            Crs::Epsg(4326),
        );
        let err = map_schema(&core, &SchemaMapOptions::default()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("collides"),
            "expected collision error, got: {msg}"
        );
        assert!(
            msg.contains("geometry"),
            "should name the conflicting column"
        );
    }

    #[test]
    fn fallback_canonical_name_when_source_has_no_geometry_field() {
        // Schema with no geometry field (e.g. attribute-only layer that
        // somehow still asks for a parquet write) → canonical name applies.
        let core = CoreSchema::new(
            vec![FieldDef::new("name", ValueType::String, true)],
            None,
            Crs::Unknown,
        );
        let m = map_schema(&core, &SchemaMapOptions::default()).unwrap();
        assert_eq!(m.arrow.field(0).name(), "geometry");
    }

    #[test]
    fn opt_out_of_bbox_columns() {
        let core = build_core_schema();
        let m = map_schema(
            &core,
            &SchemaMapOptions {
                add_bbox_columns: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(m.n_cols(), 4); // geom + 3 attrs
        assert!(m.bbox_col_indices.is_none());
    }

    #[test]
    fn arrow_types_correct_for_each_value_type() {
        assert_eq!(arrow_type_for(ValueType::Bool).unwrap(), DataType::Boolean);
        assert_eq!(arrow_type_for(ValueType::Int16).unwrap(), DataType::Int16);
        assert_eq!(arrow_type_for(ValueType::Int32).unwrap(), DataType::Int32);
        assert_eq!(arrow_type_for(ValueType::Int64).unwrap(), DataType::Int64);
        assert_eq!(
            arrow_type_for(ValueType::Float32).unwrap(),
            DataType::Float32
        );
        assert_eq!(
            arrow_type_for(ValueType::Float64).unwrap(),
            DataType::Float64
        );
        assert_eq!(arrow_type_for(ValueType::String).unwrap(), DataType::Utf8);
        assert_eq!(arrow_type_for(ValueType::Binary).unwrap(), DataType::Binary);
        assert_eq!(
            arrow_type_for(ValueType::DateTime).unwrap(),
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        );
        assert_eq!(
            arrow_type_for(ValueType::Guid).unwrap(),
            DataType::FixedSizeBinary(16)
        );
    }

    #[test]
    fn datetime_conversion_anchors() {
        // 1899-12-30 → exactly -25569 days from unix epoch
        let epoch_in_gdb_days = 25569.0;
        let micros = gdb_days_to_unix_micros(epoch_in_gdb_days);
        assert_eq!(micros, 0, "1970-01-01 should be 0 micros since epoch");

        // 1900-01-01 in GDB days = 2 (1899-12-30 + 2 days)
        // = 1900-01-01 → unix = -2208988800 seconds → micros
        let micros_1900 = gdb_days_to_unix_micros(2.0);
        let expected_seconds: i64 = -2_208_988_800;
        assert_eq!(micros_1900, expected_seconds * 1_000_000);
    }
}

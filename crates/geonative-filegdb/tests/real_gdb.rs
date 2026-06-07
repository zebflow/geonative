//! Integration tests against real FileGDB files.
//!
//! Set `GEONATIVE_FIXTURE_GDB` to a `.gdb` directory path to enable these
//! tests. Without it they are skipped (`assume!`-style: they assert nothing
//! and pass).
//!
//! Recommended fixture: a VicMap "Melbourne Water" export — small, single
//! layer, well-formed. Set:
//!     export GEONATIVE_FIXTURE_GDB=".../Melbourne Water-0/VMFEAT.gdb"

use geonative_filegdb::{
    decode_row_blob, decode_shape_buffer, open_geodatabase, slice_row_blob, FieldTypeCode,
    Table, Tablx,
};
use geonative_core::{Geometry, Value};

fn fixture_path() -> Option<std::path::PathBuf> {
    std::env::var_os("GEONATIVE_FIXTURE_GDB").map(std::path::PathBuf::from)
}

#[test]
fn parse_system_catalog_header_and_fields() {
    let Some(gdb) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    let catalog_path = gdb.join("a00000001.gdbtable");
    let bytes = std::fs::read(&catalog_path)
        .unwrap_or_else(|e| panic!("read {catalog_path:?}: {e}"));

    let table = Table::parse(&bytes).expect("parse a00000001.gdbtable");

    // The system catalog is a v3 (32-bit OID) attribute-only table.
    assert_eq!(
        table.header.file_size as usize,
        bytes.len(),
        "header file_size disagrees with on-disk length"
    );
    assert_eq!(table.header.field_desc_offset, 40);

    // Should be exactly 3 fields: ID (OBJECTID), Name (String), FileFormat (Int32).
    let fields = &table.field_section.fields;
    assert_eq!(fields.len(), 3, "expected 3 fields, got {fields:?}");

    assert_eq!(fields[0].name, "ID");
    assert_eq!(fields[0].ty, FieldTypeCode::ObjectId);

    assert_eq!(fields[1].name, "Name");
    assert_eq!(fields[1].ty, FieldTypeCode::String);

    assert_eq!(fields[2].name, "FileFormat");
    assert_eq!(fields[2].ty, FieldTypeCode::Int32);
}

#[test]
fn parse_feature_class_table_with_geometry_field() {
    let Some(gdb) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    // The feature class is the largest .gdbtable in the directory (not a
    // system table). Find it: the largest `aNNNNNNNN.gdbtable` file.
    let mut largest: Option<(std::path::PathBuf, u64)> = None;
    for entry in std::fs::read_dir(&gdb).expect("read .gdb dir").flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("gdbtable") {
            continue;
        }
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if largest.as_ref().map_or(true, |(_, l)| len > *l) {
            largest = Some((p, len));
        }
    }
    let Some((path, _)) = largest else {
        panic!("no .gdbtable files in {gdb:?}");
    };

    let bytes = std::fs::read(&path).unwrap();
    let table = Table::parse(&bytes).expect(&format!("parse {path:?}"));

    // A feature-class table should have a Geometry field with populated
    // dequantization parameters.
    let geom_idx = table
        .field_section
        .geometry_field_index()
        .expect("feature class should have a Geometry field");
    let geom = table.field_section.fields[geom_idx]
        .geometry
        .as_ref()
        .expect("geometry field should have GeomFieldMeta");

    assert!(geom.xyscale > 0.0, "xyscale must be positive");
    assert!(
        !geom.srs_wkt.is_empty(),
        "feature class should declare an SRS WKT"
    );
    assert!(
        table.field_section.objectid_field_index().is_some(),
        "feature class should have an OBJECTID field"
    );

    println!(
        "{}: {} fields, geom xyscale={:.0} srs_wkt={}…",
        path.file_name().unwrap().to_string_lossy(),
        table.field_section.fields.len(),
        geom.xyscale,
        &geom.srs_wkt[..geom.srs_wkt.len().min(40)]
    );
}

#[test]
fn parse_system_catalog_tablx_row_offsets() {
    let Some(gdb) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    // GDB_SystemCatalog row index. 9 valid rows in our fixture.
    let table_bytes = std::fs::read(gdb.join("a00000001.gdbtable")).unwrap();
    let table = Table::parse(&table_bytes).unwrap();

    let tablx_bytes = std::fs::read(gdb.join("a00000001.gdbtablx")).unwrap();
    let tx = Tablx::parse(&tablx_bytes).expect("parse a00000001.gdbtablx");

    assert_eq!(tx.header.total_record_count as i64, table.header.valid_record_count);
    assert!(tx.header.offset_size >= 4 && tx.header.offset_size <= 6);

    // Every "present" row offset must fall within the .gdbtable file size
    // and point to a row blob (which starts with an i32 length).
    let mut present_count = 0u64;
    for (row_idx, off) in tx.iter_present() {
        assert!(
            (off as usize) < table_bytes.len(),
            "row {row_idx} offset {off} >= .gdbtable size {}",
            table_bytes.len()
        );
        // Sanity: the next i32 at that offset should be a plausible row size
        // (positive, less than max_row_size from the header).
        let row_len_bytes = &table_bytes[off as usize..(off as usize + 4)];
        let row_len = i32::from_le_bytes(row_len_bytes.try_into().unwrap());
        assert!(
            row_len > 0 && (row_len as u32) <= table.header.max_row_size,
            "row {row_idx} at off {off}: implausible row size {row_len} (max={})",
            table.header.max_row_size
        );
        present_count += 1;
    }
    assert_eq!(present_count as i64, table.header.valid_record_count);

    println!(
        "GDB_SystemCatalog: {present_count} present rows, offset_size={}b, blocks={}",
        tx.header.offset_size, tx.header.n_1024_blocks_present
    );
}

#[test]
fn decode_every_row_of_feature_class_attributes() {
    let Some(gdb) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    // Pick the largest .gdbtable (feature class with geometry).
    let mut largest: Option<(std::path::PathBuf, u64)> = None;
    for entry in std::fs::read_dir(&gdb).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("gdbtable") {
            continue;
        }
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if largest.as_ref().map_or(true, |(_, l)| len > *l) {
            largest = Some((p, len));
        }
    }
    let (table_path, _) = largest.expect("no .gdbtable in fixture");
    let tablx_path = table_path.with_extension("gdbtablx");

    let table_bytes = std::fs::read(&table_path).unwrap();
    let tablx_bytes = std::fs::read(&tablx_path).unwrap();
    let table = Table::parse(&table_bytes).unwrap();
    let tx = Tablx::parse(&tablx_bytes).unwrap();

    // Iterate every present row, decode it, count by field type.
    let mut row_count = 0u64;
    let mut total_geometry_blob_bytes = 0u64;
    let mut sample_first: Option<String> = None;

    for (row_idx, offset) in tx.iter_present() {
        let fid = (row_idx as i64) + 1;
        let blob = slice_row_blob(&table_bytes, offset).expect("slice row blob");
        let row = decode_row_blob(blob, fid, &table.field_section)
            .unwrap_or_else(|e| panic!("decode row {fid} (off {offset}): {e}"));

        assert_eq!(row.values.len(), table.field_section.fields.len());
        // The OBJECTID slot must equal fid.
        if let Some(oid_idx) = table.field_section.objectid_field_index() {
            assert_eq!(row.values[oid_idx], Value::Int64(fid));
        }
        // The geometry slot itself is Null in this phase; geometry_blob holds the bytes.
        if let Some(_) = table.field_section.geometry_field_index() {
            // Just check that we captured *some* geometry for a row that ogrinfo
            // reports as having geometry. Allow null too — some rows may not
            // have geometry assigned.
            if let Some(g) = row.geometry_blob.as_ref() {
                total_geometry_blob_bytes += g.len() as u64;
            }
        }

        if sample_first.is_none() {
            // Build a one-line summary of the first row's non-null attrs.
            let parts: Vec<String> = table
                .field_section
                .fields
                .iter()
                .zip(&row.values)
                .filter(|(_, v)| !matches!(v, Value::Null))
                .map(|(f, v)| format!("{}={:?}", f.name, v))
                .take(6)
                .collect();
            sample_first = Some(parts.join("  "));
        }

        row_count += 1;
    }

    assert_eq!(
        row_count as i64,
        table.header.valid_record_count,
        "row count mismatch"
    );

    println!(
        "{}: decoded {row_count} rows, geom blob bytes total = {total_geometry_blob_bytes}",
        table_path.file_name().unwrap().to_string_lossy()
    );
    if let Some(sample) = sample_first {
        println!("  first row sample: {sample}");
    }
}

#[test]
fn decode_all_geometries_of_feature_class() {
    let Some(gdb) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    // Find the largest .gdbtable (feature class with geometry)
    let mut largest: Option<(std::path::PathBuf, u64)> = None;
    for entry in std::fs::read_dir(&gdb).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("gdbtable") {
            continue;
        }
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if largest.as_ref().map_or(true, |(_, l)| len > *l) {
            largest = Some((p, len));
        }
    }
    let (table_path, _) = largest.expect("no .gdbtable in fixture");
    let tablx_path = table_path.with_extension("gdbtablx");

    let table_bytes = std::fs::read(&table_path).unwrap();
    let tablx_bytes = std::fs::read(&tablx_path).unwrap();
    let table = Table::parse(&table_bytes).unwrap();
    let tx = Tablx::parse(&tablx_bytes).unwrap();

    let geom_field_idx = table
        .field_section
        .geometry_field_index()
        .expect("no geometry field");
    let meta = table.field_section.fields[geom_field_idx]
        .geometry
        .as_ref()
        .expect("geometry field has no meta");

    let mut decoded = 0u64;
    let mut total_coords = 0u64;
    let mut variant_counts = std::collections::HashMap::<&str, u32>::new();
    // Victoria, Australia bounds for sanity-checking GDA2020 lat/lon:
    let lon_range = 140.0..150.0;
    let lat_range = -40.0..-33.0;

    for (row_idx, offset) in tx.iter_present() {
        let fid = (row_idx as i64) + 1;
        let blob = slice_row_blob(&table_bytes, offset).unwrap();
        let row = decode_row_blob(blob, fid, &table.field_section).unwrap();
        let Some(geom_blob) = row.geometry_blob.as_deref() else {
            continue;
        };
        let geom = decode_shape_buffer(geom_blob, meta)
            .unwrap_or_else(|e| panic!("decode geometry for fid {fid}: {e}"));

        // Walk every coord; assert it falls in Victoria.
        let coords = collect_coords(&geom);
        for c in &coords {
            assert!(
                lon_range.contains(&c.x),
                "fid {fid}: lon {} out of Victoria range",
                c.x
            );
            assert!(
                lat_range.contains(&c.y),
                "fid {fid}: lat {} out of Victoria range",
                c.y
            );
        }
        total_coords += coords.len() as u64;

        let key = match geom {
            Geometry::LineString(_) => "LineString",
            Geometry::MultiLineString(_) => "MultiLineString",
            Geometry::Point(_) => "Point",
            Geometry::MultiPoint(_) => "MultiPoint",
            Geometry::Polygon(_) => "Polygon",
            Geometry::MultiPolygon(_) => "MultiPolygon",
            Geometry::GeometryCollection(_) => "GeometryCollection",
            Geometry::Empty(_) => "Empty",
        };
        *variant_counts.entry(key).or_default() += 1;
        decoded += 1;
    }

    assert_eq!(
        decoded as i64,
        table.header.valid_record_count,
        "geometry count mismatch"
    );

    let mut v: Vec<(&&str, &u32)> = variant_counts.iter().collect();
    v.sort();
    println!(
        "decoded {decoded} geoms, {total_coords} total coords, variants: {:?}",
        v
    );
}

#[test]
fn end_to_end_public_api_returns_core_features() {
    let Some(gdb_path) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    let gdb = geonative_filegdb::open(&gdb_path).expect("open .gdb");
    let layer = gdb.layer("FOI_LINE").expect("open FOI_LINE");

    // Schema sanity
    let schema = layer.schema();
    assert!(schema.geometry.is_some(), "schema should declare geometry");
    let geom_field = schema.geometry.as_ref().unwrap();
    assert_eq!(geom_field.kind, geonative_core::GeometryType::MultiLineString);
    assert!(matches!(&schema.crs, geonative_core::Crs::Wkt(s) if s.starts_with("GEOGCS[\"GDA2020\"")));
    assert_eq!(layer.feature_count(), 75);

    // The user-facing attribute list should NOT contain OBJECTID or the
    // geometry slot — both are surfaced via Feature.fid / Feature.geometry.
    let attr_names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(!attr_names.iter().any(|n| *n == "OBJECTID"));
    assert!(!attr_names.iter().any(|n| *n == "SHAPE"));
    assert!(attr_names.contains(&"UFI"));
    assert!(attr_names.contains(&"NAME"));
    assert_eq!(
        schema.fields.len(),
        25,
        "expected 25 attribute fields (27 raw - OBJECTID - SHAPE), got {:?}",
        attr_names
    );

    // Read every feature.
    let mut count = 0u64;
    let mut first_feature: Option<geonative_core::Feature> = None;
    for f in layer.read() {
        let f = f.expect("decode feature");
        assert!(f.fid.is_some());
        assert_eq!(f.attributes.len(), schema.fields.len());
        if first_feature.is_none() {
            first_feature = Some(f);
            continue;
        }
        count += 1;
    }
    count += 1; // counted first_feature separately
    assert_eq!(count as i64, layer.feature_count());

    // Spot-check the first feature: FID=1, attribute order matches schema,
    // geometry is a MultiLineString.
    let f = first_feature.unwrap();
    assert_eq!(f.fid, Some(1));
    assert!(matches!(
        f.geometry,
        Some(geonative_core::Geometry::MultiLineString(_))
    ));
    // Find UFI by name and check its decoded value.
    let ufi_idx = schema.field_index("UFI").expect("UFI in schema");
    assert_eq!(
        f.attributes[ufi_idx],
        geonative_core::Value::Int32(64536814),
        "FID 1 UFI should match ogrinfo"
    );
    let name_idx = schema.field_index("NAME").expect("NAME in schema");
    assert_eq!(
        f.attributes[name_idx],
        geonative_core::Value::String("BASS GAS - LEONGARTHA AND WONTHAGGI".into()),
    );
}

#[test]
fn enumerate_layers_via_catalog() {
    let Some(gdb) = fixture_path() else {
        eprintln!("(skipped: set GEONATIVE_FIXTURE_GDB to enable)");
        return;
    };

    let layers = open_geodatabase(&gdb).expect("open .gdb");

    // VMFEAT.gdb's only user-facing layer is FOI_LINE.
    assert!(
        !layers.is_empty(),
        "expected at least one user layer in {gdb:?}"
    );
    assert!(
        layers.iter().any(|l| l.name == "FOI_LINE"),
        "FOI_LINE not found among layers: {:?}",
        layers.iter().map(|l| &l.name).collect::<Vec<_>>()
    );
    assert!(
        layers.iter().all(|l| !l.name.starts_with("GDB_")),
        "system tables leaked into user layer list"
    );

    // Each declared physical file must actually exist.
    for l in &layers {
        assert!(
            l.table_path(&gdb).exists(),
            "table file missing for layer {}: {:?}",
            l.name,
            l.table_path(&gdb)
        );
        assert!(
            l.tablx_path(&gdb).exists(),
            "tablx file missing for layer {}: {:?}",
            l.name,
            l.tablx_path(&gdb)
        );
    }

    println!(
        "discovered {} user layers: {:?}",
        layers.len(),
        layers.iter().map(|l| (&l.name, l.fid)).collect::<Vec<_>>()
    );
}

fn collect_coords(g: &Geometry) -> Vec<geonative_core::Coord> {
    use Geometry::*;
    match g {
        Point(c) => vec![*c],
        MultiPoint(v) => v.clone(),
        LineString(ls) => ls.coords.clone(),
        MultiLineString(v) => v.iter().flat_map(|ls| ls.coords.iter().copied()).collect(),
        Polygon(p) => p
            .exterior
            .coords
            .iter()
            .chain(p.holes.iter().flat_map(|h| h.coords.iter()))
            .copied()
            .collect(),
        MultiPolygon(v) => v
            .iter()
            .flat_map(|p| {
                p.exterior
                    .coords
                    .iter()
                    .chain(p.holes.iter().flat_map(|h| h.coords.iter()))
                    .copied()
            })
            .collect(),
        GeometryCollection(v) => v.iter().flat_map(collect_coords).collect(),
        Empty(_) => vec![],
    }
}

//! Integration tests against real FileGDB files.
//!
//! Set `GEONATIVE_FIXTURE_GDB` to a `.gdb` directory path to enable these
//! tests. Without it they are skipped (`assume!`-style: they assert nothing
//! and pass).
//!
//! Recommended fixture: a VicMap "Melbourne Water" export — small, single
//! layer, well-formed. Set:
//!     export GEONATIVE_FIXTURE_GDB=".../Melbourne Water-0/VMFEAT.gdb"

use geonative_filegdb::{FieldTypeCode, Table, Tablx};

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

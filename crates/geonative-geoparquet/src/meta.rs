//! Build the GeoParquet 1.1 `geo` metadata JSON string.
//!
//! Spec: <https://geoparquet.org/releases/v1.1.0/>
//!
//! The `geo` key sits in the parquet file's key-value metadata. v0.1 emits:
//!
//! - `version`: `"1.1.0"`
//! - `primary_column`: the WKB geometry column name
//! - `columns.<name>.encoding`: `"WKB"`
//! - `columns.<name>.geometry_types`: declared-from-schema (single entry in
//!   v0.1; we don't probe per-feature)
//! - `columns.<name>.crs`: PROJJSON object if derivable from the source CRS,
//!   else `null` (treated as OGC:CRS84 / longitude-latitude per spec)
//! - `columns.<name>.edges`: `"planar"`
//! - `columns.<name>.covering.bbox`: present when bbox columns were emitted,
//!   pointing at `xmin/ymin/xmax/ymax` for row-group predicate pushdown
//!
//! The JSON is hand-built (no `serde_json` dep) — the document shape is tiny
//! and fixed, and zebflow's `geoparquet.rs` consumes it as a string.

use geonative_core::{Crs, GeometryType};

#[derive(Debug, Clone)]
pub struct GeoMetadataInput<'a> {
    pub primary_column: &'a str,
    pub layer_geometry_type: GeometryType,
    pub crs: &'a Crs,
    pub include_bbox_covering: bool,
}

pub fn build_geo_metadata_json(input: &GeoMetadataInput<'_>) -> String {
    let name = json_escape(input.primary_column);
    let geom_type_str = geometry_type_string(input.layer_geometry_type);

    // CRS: either a PROJJSON object literal, or the JSON null literal.
    let crs_json = match input.crs.to_projjson() {
        Some(s) => s,
        None => "null".to_string(),
    };

    let covering_block = if input.include_bbox_covering {
        r#","covering":{"bbox":{"xmin":["xmin"],"ymin":["ymin"],"xmax":["xmax"],"ymax":["ymax"]}}"#
    } else {
        ""
    };

    format!(
        r#"{{"version":"1.1.0","primary_column":"{name}","columns":{{"{name}":{{"encoding":"WKB","geometry_types":["{geom_type_str}"],"crs":{crs_json},"edges":"planar"{covering_block}}}}}}}"#
    )
}

fn geometry_type_string(t: GeometryType) -> &'static str {
    match t {
        GeometryType::Point => "Point",
        GeometryType::LineString => "LineString",
        GeometryType::Polygon => "Polygon",
        GeometryType::MultiPoint => "MultiPoint",
        GeometryType::MultiLineString => "MultiLineString",
        GeometryType::MultiPolygon => "MultiPolygon",
        GeometryType::GeometryCollection => "GeometryCollection",
    }
}

fn json_escape(s: &str) -> String {
    // GeoParquet column names are typically simple identifiers; do the minimal
    // JSON escape (backslash + quote) and leave the rest as-is. If users name
    // a column with arbitrary unicode we still emit valid JSON because all
    // other byte sequences are passed through unchanged.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_metadata_shape_with_epsg_and_bbox() {
        let crs = Crs::Epsg(7844);
        let s = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: "geometry",
            layer_geometry_type: GeometryType::MultiLineString,
            crs: &crs,
            include_bbox_covering: true,
        });
        assert!(s.contains(r#""version":"1.1.0""#));
        assert!(s.contains(r#""primary_column":"geometry""#));
        assert!(s.contains(r#""encoding":"WKB""#));
        assert!(s.contains(r#""geometry_types":["MultiLineString"]"#));
        assert!(s.contains(r#""authority":"EPSG","code":7844"#));
        assert!(s.contains(r#""edges":"planar""#));
        assert!(s.contains(r#""covering":{"bbox":{"xmin":["xmin"],"ymin":["ymin"],"xmax":["xmax"],"ymax":["ymax"]}}"#));
    }

    #[test]
    fn unknown_crs_yields_null() {
        let crs = Crs::Unknown;
        let s = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: "geometry",
            layer_geometry_type: GeometryType::Point,
            crs: &crs,
            include_bbox_covering: false,
        });
        assert!(s.contains(r#""crs":null"#));
        assert!(!s.contains("covering"));
    }

    #[test]
    fn wkt_crs_with_epsg_authority_extracted_into_projjson() {
        let crs = Crs::Wkt(r#"GEOGCS["WGS 84",AUTHORITY["EPSG","4326"]]"#.into());
        let s = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: "geometry",
            layer_geometry_type: GeometryType::Point,
            crs: &crs,
            include_bbox_covering: false,
        });
        assert!(s.contains(r#""code":4326"#));
    }

    #[test]
    fn output_is_valid_single_line_json_shape() {
        let crs = Crs::Epsg(4326);
        let s = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: "geometry",
            layer_geometry_type: GeometryType::Point,
            crs: &crs,
            include_bbox_covering: false,
        });
        // Count braces — should be balanced.
        let opens = s.bytes().filter(|&b| b == b'{').count();
        let closes = s.bytes().filter(|&b| b == b'}').count();
        assert_eq!(opens, closes, "unbalanced braces: {s}");
    }
}

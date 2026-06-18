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
        // Future GeometryType variants: emit "Geometry" — the GeoParquet
        // spec's generic catch-all that signals "could be any subtype".
        _ => "Geometry",
    }
}

/// Tiny "good enough" parser for the bits of GeoParquet `geo` metadata our
/// reader cares about: the primary geometry column name and, if present, the
/// EPSG code of its CRS. Avoids pulling in `serde_json` for a few dozen bytes
/// of well-structured input.
#[derive(Debug, Default, Clone)]
pub struct GeoMetadataParsed {
    pub primary_column: Option<String>,
    pub epsg_code: Option<u32>,
}

pub fn parse_geo_metadata(json: &str) -> GeoMetadataParsed {
    GeoMetadataParsed {
        primary_column: extract_str_value(json, "\"primary_column\""),
        epsg_code: extract_epsg_code(json).or_else(|| extract_epsg_from_name(json)),
    }
}

/// Find `"<key>":"<value>"` in `s` (skipping any whitespace between key,
/// colon, and value) and return `value`.
fn extract_str_value(s: &str, quoted_key: &str) -> Option<String> {
    let key_pos = s.find(quoted_key)?;
    let rest = &s[key_pos + quoted_key.len()..];
    let after_colon = rest.find(':')?;
    let after_colon_slice = &rest[after_colon + 1..];
    let open_quote = after_colon_slice.find('"')?;
    let body = &after_colon_slice[open_quote + 1..];
    let close_quote = body.find('"')?;
    Some(body[..close_quote].to_string())
}

/// Look for an `"id":{"authority":"EPSG","code":<n>}` clause in the JSON and
/// return `<n>`. Handles minor whitespace variation; only the first match
/// (the primary column's CRS) is returned.
fn extract_epsg_code(json: &str) -> Option<u32> {
    let authority_marker = "\"authority\":\"EPSG\"";
    let pos = json.find(authority_marker)?;
    let after = &json[pos + authority_marker.len()..];
    let code_marker = "\"code\":";
    let cpos = after.find(code_marker)?;
    let after_code = &after[cpos + code_marker.len()..];
    let trimmed = after_code.trim_start();
    let end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if end == 0 {
        return None;
    }
    trimmed[..end].parse::<u32>().ok()
}

/// Fallback for files that only declare CRS via `"name":"…"` instead of the
/// canonical `id.authority`/`id.code` clause. Recognises the two shorthand
/// forms seen in the wild from DuckDB, GeoPandas, GeoServer exports:
///
/// - `"name":"EPSG:3857"` (with optional whitespace after the colon)
/// - `"name":"urn:ogc:def:crs:EPSG::3857"` (OGC URN form)
///
/// Bare datum-name strings (`"name":"WGS 84"`) aren't matched here — that
/// path requires the inline-name table in `geonative-core`'s `Crs::epsg_code`,
/// which only kicks in for `Crs::Wkt`. Adding that here would couple two
/// crates' parsers; we keep this scope tight.
fn extract_epsg_from_name(json: &str) -> Option<u32> {
    let name = extract_str_value(json, "\"name\"")?;
    epsg_from_name_string(&name)
}

fn epsg_from_name_string(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    // URN form: "urn:ogc:def:crs:EPSG::3857" — the double colon is required.
    if lower.starts_with("urn:ogc:def:crs:epsg:") {
        let (_, code_str) = name.split_once("::")?;
        return code_str.trim().parse::<u32>().ok();
    }
    // Prefix form: "EPSG:3857" (case-insensitive, optional space after colon).
    if let Some(rest) = lower.strip_prefix("epsg:") {
        return rest.trim().parse::<u32>().ok();
    }
    None
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
    fn parse_round_trips_with_build() {
        let crs = Crs::Epsg(4326);
        let json = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: "SHAPE",
            layer_geometry_type: GeometryType::MultiPolygon,
            crs: &crs,
            include_bbox_covering: true,
        });
        let parsed = parse_geo_metadata(&json);
        assert_eq!(parsed.primary_column.as_deref(), Some("SHAPE"));
        assert_eq!(parsed.epsg_code, Some(4326));
    }

    #[test]
    fn parse_epsg_prefix_in_name_field() {
        // DuckDB / GeoPandas style: minimal PROJJSON with only "name".
        let json = r#"{"version":"1.1.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Polygon"],"crs":{"name":"EPSG:3857"},"edges":"planar"}}}"#;
        let parsed = parse_geo_metadata(json);
        assert_eq!(parsed.primary_column.as_deref(), Some("geometry"));
        assert_eq!(parsed.epsg_code, Some(3857));
    }

    #[test]
    fn parse_epsg_prefix_lowercase_and_spaced() {
        let json = r#"{"crs":{"name":"epsg: 4326"}}"#;
        assert_eq!(parse_geo_metadata(json).epsg_code, Some(4326));
    }

    #[test]
    fn parse_urn_form_in_name_field() {
        // OGC URN form as seen in older GeoServer / WFS exports.
        let json = r#"{"crs":{"name":"urn:ogc:def:crs:EPSG::7844"}}"#;
        assert_eq!(parse_geo_metadata(json).epsg_code, Some(7844));
    }

    #[test]
    fn parse_prefers_authority_over_name_when_both_present() {
        // If a writer is generous and emits both, the canonical authority
        // clause wins (matches the order GDAL trusts).
        let json = r#"{"crs":{"name":"EPSG:9999","id":{"authority":"EPSG","code":4326}}}"#;
        assert_eq!(parse_geo_metadata(json).epsg_code, Some(4326));
    }

    #[test]
    fn parse_unrecognised_name_falls_through_to_none() {
        // Bare datum names (no EPSG prefix) aren't matched here — that path
        // would need the WKT name table from geonative-core.
        let json = r#"{"crs":{"name":"WGS 84 / Pseudo-Mercator"}}"#;
        assert_eq!(parse_geo_metadata(json).epsg_code, None);
    }

    #[test]
    fn parse_handles_missing_crs() {
        let crs = Crs::Unknown;
        let json = build_geo_metadata_json(&GeoMetadataInput {
            primary_column: "geometry",
            layer_geometry_type: GeometryType::Point,
            crs: &crs,
            include_bbox_covering: false,
        });
        let parsed = parse_geo_metadata(&json);
        assert_eq!(parsed.primary_column.as_deref(), Some("geometry"));
        assert_eq!(parsed.epsg_code, None);
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

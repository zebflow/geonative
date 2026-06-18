//! Byte-level streaming scanner for GeoJSON FeatureCollections.
//!
//! Walks the top-level object header looking for the `"features"` key,
//! then yields one balanced JSON value per call from inside the
//! `[...]` array — without holding the rest of the file in RAM.
//!
//! Why bytes-level and not `serde_json::Deserializer`: `serde_json`'s
//! `StreamDeserializer` only supports whitespace-separated top-level
//! values. The comma separator between FeatureCollection entries makes
//! it unusable here. A small string-aware brace-depth walker does the
//! job in ~200 lines without adding a streaming-JSON dependency.

use std::io::{BufRead, BufReader};

use serde_json::{Map as JsonMap, Value as Json};

use crate::error::{GeoJsonError, Result};

/// One whole top-level "thing" found at the start of the file: either
/// the FeatureCollection envelope (header info + positioned at the
/// start of the features array), a bare Feature, or a bare Geometry.
#[derive(Debug)]
pub(crate) enum TopLevel<R: BufRead> {
    /// Streamed FeatureCollection: the reader is positioned **inside**
    /// the `features` array, ready to yield entries.
    Collection {
        reader: R,
        /// Other top-level keys we already consumed (e.g. `crs`, `bbox`).
        /// We keep them parsed so the higher-level code can extract CRS
        /// without re-walking the file.
        header_keys: JsonMap<String, Json>,
    },
    /// Bare Feature object — fits in RAM by definition (one feature).
    BareFeature(Json),
    /// Bare Geometry object.
    BareGeometry(Json),
}

/// Parse the top-level structure. For a FeatureCollection, leaves
/// `reader` positioned right after the opening `[` of the features
/// array so [`next_feature_value`] can stream from there.
pub(crate) fn open_top_level<R: BufRead>(mut reader: R) -> Result<TopLevel<R>> {
    skip_ws(&mut reader)?;
    let first = peek_byte(&mut reader)?
        .ok_or_else(|| GeoJsonError::malformed("empty input"))?;
    if first != b'{' {
        return Err(GeoJsonError::malformed(format!(
            "GeoJSON root must be a JSON object (got first byte {first:#x})"
        )));
    }
    // Consume the `{`.
    consume(&mut reader, 1)?;

    let mut header_keys: JsonMap<String, Json> = JsonMap::new();
    let mut declared_type: Option<String> = None;

    loop {
        skip_ws(&mut reader)?;
        let nxt = peek_byte(&mut reader)?
            .ok_or_else(|| GeoJsonError::malformed("unexpected EOF in top-level object"))?;
        if nxt == b'}' {
            // Object ended without a "features" key — must have been
            // a Feature or Geometry (or malformed). Reconstruct.
            consume(&mut reader, 1)?;
            return wrap_bare(declared_type.as_deref(), header_keys);
        }
        if nxt == b',' {
            consume(&mut reader, 1)?;
            continue;
        }
        if nxt != b'"' {
            return Err(GeoJsonError::malformed(format!(
                "expected object key (string) or '}}', got byte {nxt:#x}"
            )));
        }
        // Read a string token using the same balanced-value reader,
        // which handles escape sequences correctly.
        let key_bytes = read_one_balanced_value(&mut reader)?
            .ok_or_else(|| GeoJsonError::malformed("EOF while reading key"))?;
        let key: String = serde_json::from_slice(&key_bytes)?;

        skip_ws(&mut reader)?;
        // Expect ':'
        let colon = peek_byte(&mut reader)?
            .ok_or_else(|| GeoJsonError::malformed("EOF after key"))?;
        if colon != b':' {
            return Err(GeoJsonError::malformed(format!(
                "expected ':' after key '{key}', got byte {colon:#x}"
            )));
        }
        consume(&mut reader, 1)?;
        skip_ws(&mut reader)?;

        if key == "features" {
            // Expect '['.
            let lb = peek_byte(&mut reader)?
                .ok_or_else(|| GeoJsonError::malformed("EOF before features array"))?;
            if lb != b'[' {
                return Err(GeoJsonError::malformed(format!(
                    "expected '[' after \"features\", got byte {lb:#x}"
                )));
            }
            consume(&mut reader, 1)?;
            // Reader is now positioned inside the features array.
            return Ok(TopLevel::Collection {
                reader,
                header_keys,
            });
        }

        // Other top-level key — slurp its value and remember.
        let val_bytes = read_one_balanced_value(&mut reader)?
            .ok_or_else(|| GeoJsonError::malformed(format!("EOF while reading value for key '{key}'")))?;
        let val: Json = serde_json::from_slice(&val_bytes)?;
        if key == "type" {
            declared_type = val.as_str().map(str::to_string);
        }
        header_keys.insert(key, val);
    }
}

/// Reconstruct a bare Feature / Geometry from the partial top-level
/// keys we've already consumed (we never saw `"features"`, so the
/// object must have been the feature or geometry itself).
fn wrap_bare<R: BufRead>(
    declared_type: Option<&str>,
    keys: JsonMap<String, Json>,
) -> Result<TopLevel<R>> {
    let ty = declared_type
        .ok_or_else(|| GeoJsonError::malformed("GeoJSON root missing 'type'"))?;
    let obj = Json::Object(keys);
    match ty {
        "Feature" => Ok(TopLevel::BareFeature(obj)),
        "Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString" | "MultiPolygon"
        | "GeometryCollection" => Ok(TopLevel::BareGeometry(obj)),
        other => Err(GeoJsonError::unsupported(format!(
            "top-level type '{other}'"
        ))),
    }
}

/// Pull the next feature value out of an `InArray` reader. Returns
/// `Ok(None)` when the array is exhausted (saw `]`).
pub(crate) fn next_feature_value<R: BufRead>(reader: &mut R) -> Result<Option<Json>> {
    loop {
        skip_ws(reader)?;
        let nxt = peek_byte(reader)?;
        let Some(b) = nxt else {
            return Err(GeoJsonError::malformed(
                "EOF inside features array (no closing ']')",
            ));
        };
        if b == b']' {
            consume(reader, 1)?;
            return Ok(None);
        }
        if b == b',' {
            consume(reader, 1)?;
            continue;
        }
        let bytes = read_one_balanced_value(reader)?
            .ok_or_else(|| GeoJsonError::malformed("EOF mid-feature"))?;
        let value: Json = serde_json::from_slice(&bytes)?;
        return Ok(Some(value));
    }
}

// ---------------------------------------------------------------------------
// Low-level byte helpers
// ---------------------------------------------------------------------------

fn skip_ws<R: BufRead>(r: &mut R) -> Result<()> {
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(());
        }
        let mut consumed = 0;
        for &b in buf {
            if b.is_ascii_whitespace() {
                consumed += 1;
            } else {
                break;
            }
        }
        if consumed == 0 {
            return Ok(());
        }
        r.consume(consumed);
    }
}

fn peek_byte<R: BufRead>(r: &mut R) -> Result<Option<u8>> {
    let buf = r.fill_buf()?;
    Ok(buf.first().copied())
}

fn consume<R: BufRead>(r: &mut R, n: usize) -> Result<()> {
    let mut remaining = n;
    while remaining > 0 {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Err(GeoJsonError::malformed("unexpected EOF while consuming"));
        }
        let take = remaining.min(buf.len());
        r.consume(take);
        remaining -= take;
    }
    Ok(())
}

/// Read one balanced JSON value (object, array, string, number, bool,
/// null) from `r`. Returns the raw bytes of the value so the caller
/// can hand them to `serde_json::from_slice`.
///
/// Tracks brace/bracket depth and string state with escape-awareness
/// so a `}` inside a string doesn't terminate the value early.
fn read_one_balanced_value<R: BufRead>(r: &mut R) -> Result<Option<Vec<u8>>> {
    skip_ws(r)?;
    let Some(first) = peek_byte(r)? else {
        return Ok(None);
    };

    let mut out: Vec<u8> = Vec::with_capacity(256);
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;

    // Track whether we're inside a structured value (object/array/string)
    // or a bare scalar (number, true, false, null). Scalars end at
    // whitespace or a JSON-structural character.
    let starts_structured = matches!(first, b'{' | b'[' | b'"');

    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            // End of input: scalar values are OK to terminate here,
            // structured ones must have closed already.
            if !starts_structured && !out.is_empty() && depth == 0 && !in_string {
                return Ok(Some(out));
            }
            return Err(GeoJsonError::malformed(
                "unexpected EOF mid-value (unterminated object/array/string?)",
            ));
        }
        let mut consumed = 0;
        let mut finished = false;
        for (i, &b) in buf.iter().enumerate() {
            consumed = i + 1;

            if !starts_structured && depth == 0 && !in_string && !out.is_empty() {
                // We're in a scalar — terminate on the first
                // structural character or whitespace.
                if b.is_ascii_whitespace() || matches!(b, b',' | b'}' | b']' | b':') {
                    consumed = i; // don't consume the terminator
                    finished = true;
                    break;
                }
            }

            out.push(b);

            if escape {
                escape = false;
                continue;
            }
            if in_string {
                if b == b'\\' {
                    escape = true;
                } else if b == b'"' {
                    in_string = false;
                    if depth == 0 && starts_structured && first == b'"' {
                        // Top-level string just closed.
                        finished = true;
                        break;
                    }
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth < 0 {
                        return Err(GeoJsonError::malformed(format!(
                            "unbalanced '{}' at value start (got {:#x})",
                            b as char, b
                        )));
                    }
                    if depth == 0 && starts_structured {
                        finished = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        r.consume(consumed);
        if finished {
            return Ok(Some(out));
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience for opening a file as a buffered streamer.
// ---------------------------------------------------------------------------

pub(crate) fn buf_reader_for_file(path: &std::path::Path) -> Result<BufReader<std::fs::File>> {
    let file = std::fs::File::open(path)?;
    // 64 KiB is enough to cover small features in a single fill_buf()
    // call most of the time, keeping the brace walker's per-call cost
    // down. Larger features just span multiple fills cleanly.
    Ok(BufReader::with_capacity(64 * 1024, file))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn open_str(s: &str) -> TopLevel<BufReader<&[u8]>> {
        open_top_level(BufReader::new(s.as_bytes())).unwrap()
    }

    #[test]
    fn streams_simple_feature_collection() {
        let s = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{"a":1}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[3,4]},"properties":{"a":2}}
        ]}"#;
        let TopLevel::Collection { mut reader, header_keys } = open_str(s) else {
            panic!("expected Collection");
        };
        assert_eq!(header_keys.get("type").and_then(Json::as_str), Some("FeatureCollection"));
        let f1 = next_feature_value(&mut reader).unwrap().unwrap();
        let f2 = next_feature_value(&mut reader).unwrap().unwrap();
        assert!(next_feature_value(&mut reader).unwrap().is_none());
        assert_eq!(f1["properties"]["a"], Json::from(1));
        assert_eq!(f2["properties"]["a"], Json::from(2));
    }

    #[test]
    fn skips_leading_crs_and_bbox_keys() {
        let s = r#"{
          "type":"FeatureCollection",
          "crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::3857"}},
          "bbox":[0,0,1,1],
          "features":[{"type":"Feature","geometry":null,"properties":{}}]
        }"#;
        let TopLevel::Collection { mut reader, header_keys } = open_str(s) else {
            panic!("expected Collection");
        };
        assert!(header_keys.contains_key("crs"));
        assert!(header_keys.contains_key("bbox"));
        assert!(next_feature_value(&mut reader).unwrap().is_some());
        assert!(next_feature_value(&mut reader).unwrap().is_none());
    }

    #[test]
    fn strings_with_braces_dont_confuse_walker() {
        let s = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":null,"properties":{"raw":"value with }]{ and \"quotes\" inside"}},
            {"type":"Feature","geometry":null,"properties":{}}
        ]}"#;
        let TopLevel::Collection { mut reader, .. } = open_str(s) else { panic!() };
        let f1 = next_feature_value(&mut reader).unwrap().unwrap();
        let f2 = next_feature_value(&mut reader).unwrap().unwrap();
        assert_eq!(
            f1["properties"]["raw"].as_str().unwrap(),
            r#"value with }]{ and "quotes" inside"#
        );
        assert!(f2.is_object());
        assert!(next_feature_value(&mut reader).unwrap().is_none());
    }

    #[test]
    fn empty_features_array_returns_none_immediately() {
        let s = r#"{"type":"FeatureCollection","features":[]}"#;
        let TopLevel::Collection { mut reader, .. } = open_str(s) else { panic!() };
        assert!(next_feature_value(&mut reader).unwrap().is_none());
    }

    #[test]
    fn bare_feature_is_detected() {
        let s = r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}"#;
        match open_str(s) {
            TopLevel::BareFeature(v) => assert_eq!(v["type"], Json::from("Feature")),
            _ => panic!("expected BareFeature"),
        }
    }

    #[test]
    fn bare_geometry_is_detected() {
        let s = r#"{"type":"Point","coordinates":[10,20]}"#;
        match open_str(s) {
            TopLevel::BareGeometry(v) => assert_eq!(v["type"], Json::from("Point")),
            _ => panic!("expected BareGeometry"),
        }
    }

    #[test]
    fn truncated_array_errors_cleanly() {
        let s = r#"{"type":"FeatureCollection","features":["#;
        let result = open_top_level(BufReader::new(s.as_bytes()));
        let TopLevel::Collection { mut reader, .. } = result.unwrap() else { panic!() };
        assert!(next_feature_value(&mut reader).is_err());
    }
}

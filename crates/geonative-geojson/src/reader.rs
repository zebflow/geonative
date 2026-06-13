//! GeoJSON reader. Eager (loads the whole file into memory and parses to a
//! [`Feature`] vector). v0.1 — fine for files up to a few hundred MB.
//!
//! ## Top-level shapes accepted
//!
//! - `FeatureCollection` with `features: []`
//! - Bare `Feature`
//! - Bare geometry object (wrapped into a single fid-less, property-less feature)
//!
//! ## CRS handling
//!
//! RFC 7946 mandates WGS84 (EPSG:4326) and removes the legacy 2008-era `crs`
//! member. We default to `Crs::Epsg(4326)` but honour an explicit legacy
//! `crs.properties.name` URN like `urn:ogc:def:crs:EPSG::3857` if present —
//! plenty of real-world feeds still carry one.

use std::path::Path;

use geonative_core::{Crs, Feature, GeomField, Geometry, GeometryType, Schema, Value};
use serde_json::{Map as JsonMap, Value as Json};

use crate::error::{GeoJsonError, Result};
use crate::geometry::from_json as geom_from_json;
use crate::properties::{infer_fields, json_to_value};

#[derive(Debug)]
pub struct GeoJsonReader {
    schema: Schema,
    features: Vec<Feature>,
}

impl GeoJsonReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let root: Json = serde_json::from_slice(bytes)?;
        Self::from_value(root)
    }

    pub fn from_value(root: Json) -> Result<Self> {
        let obj = root
            .as_object()
            .ok_or_else(|| GeoJsonError::malformed("GeoJSON root must be a JSON object"))?;
        let ty = obj
            .get("type")
            .and_then(Json::as_str)
            .ok_or_else(|| GeoJsonError::malformed("GeoJSON root missing 'type'"))?;
        let crs = extract_crs(obj);

        let raw_features: Vec<RawFeature> = match ty {
            "FeatureCollection" => {
                let arr = obj
                    .get("features")
                    .and_then(Json::as_array)
                    .ok_or_else(|| {
                        GeoJsonError::malformed("FeatureCollection missing 'features'")
                    })?;
                arr.iter()
                    .map(parse_feature_or_geometry)
                    .collect::<Result<Vec<_>>>()?
            }
            "Feature" => vec![parse_feature(obj)?],
            // Bare geometry object: wrap into a featureless feature.
            "Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString"
            | "MultiPolygon" | "GeometryCollection" => vec![RawFeature {
                fid: None,
                geometry: Some(geom_from_json(&root)?),
                properties: None,
            }],
            other => {
                return Err(GeoJsonError::unsupported(format!(
                    "top-level type '{other}'"
                )))
            }
        };

        // Schema inference over the property maps.
        let all_props: Vec<Option<&JsonMap<String, Json>>> =
            raw_features.iter().map(|f| f.properties.as_ref()).collect();
        let fields = infer_fields(&all_props);

        // Geometry kind: homogeneous wins; mixed falls back to GeometryCollection.
        let geom_kind = detect_geom_kind(&raw_features);
        let geom_field = geom_kind.map(|k| GeomField::new("geometry", k));

        let schema = Schema::new(fields.clone(), geom_field, crs);

        // Materialise Feature rows with attributes in schema order.
        let features = raw_features
            .into_iter()
            .enumerate()
            .map(|(i, raw)| {
                let attrs: Vec<Value> = fields
                    .iter()
                    .map(|f| match raw.properties.as_ref() {
                        Some(props) => json_to_value(props.get(&f.name), f.ty),
                        None => Value::Null,
                    })
                    .collect();
                Feature::new(raw.fid.or(Some(i as i64)), raw.geometry, attrs)
            })
            .collect();

        Ok(Self { schema, features })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn feature_count(&self) -> usize {
        self.features.len()
    }

    pub fn features(&self) -> &[Feature] {
        &self.features
    }

    pub fn into_features(self) -> std::vec::IntoIter<Feature> {
        self.features.into_iter()
    }

    /// Iterate as `Result<Feature, E>` for code paths that want to treat
    /// every reader uniformly (matches the iterator shape of the other
    /// readers in the workspace, which can return per-feature errors).
    pub fn iter_results(&self) -> impl Iterator<Item = Result<Feature>> + '_ {
        self.features.iter().cloned().map(Ok)
    }
}

#[derive(Debug)]
struct RawFeature {
    fid: Option<i64>,
    geometry: Option<Geometry>,
    properties: Option<JsonMap<String, Json>>,
}

fn parse_feature_or_geometry(v: &Json) -> Result<RawFeature> {
    let obj = v
        .as_object()
        .ok_or_else(|| GeoJsonError::malformed("feature must be a JSON object"))?;
    let ty = obj
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| GeoJsonError::malformed("feature missing 'type'"))?;
    match ty {
        "Feature" => parse_feature(obj),
        // Some feeds put bare geometries in a FeatureCollection's array. Accept.
        "Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString" | "MultiPolygon"
        | "GeometryCollection" => Ok(RawFeature {
            fid: None,
            geometry: Some(geom_from_json(v)?),
            properties: None,
        }),
        other => Err(GeoJsonError::unsupported(format!(
            "feature-array element type '{other}'"
        ))),
    }
}

fn parse_feature(obj: &JsonMap<String, Json>) -> Result<RawFeature> {
    let geometry = match obj.get("geometry") {
        Some(Json::Null) | None => None,
        Some(other) => Some(geom_from_json(other)?),
    };
    let properties = match obj.get("properties") {
        Some(Json::Null) | None => None,
        Some(Json::Object(map)) => Some(map.clone()),
        Some(_) => {
            return Err(GeoJsonError::malformed(
                "feature 'properties' must be object or null",
            ))
        }
    };
    let fid = obj.get("id").and_then(json_id_to_i64);
    Ok(RawFeature {
        fid,
        geometry,
        properties,
    })
}

fn json_id_to_i64(j: &Json) -> Option<i64> {
    if let Some(n) = j.as_i64() {
        return Some(n);
    }
    if let Some(s) = j.as_str() {
        return s.parse::<i64>().ok();
    }
    None
}

fn detect_geom_kind(features: &[RawFeature]) -> Option<GeometryType> {
    let mut found: Option<GeometryType> = None;
    for f in features {
        if let Some(g) = &f.geometry {
            let k = geom_type_of(g);
            match found {
                None => found = Some(k),
                Some(existing) if existing == k => {}
                Some(_) => return Some(GeometryType::GeometryCollection),
            }
        }
    }
    found
}

fn geom_type_of(g: &Geometry) -> GeometryType {
    match g {
        Geometry::Point(_) => GeometryType::Point,
        Geometry::LineString(_) => GeometryType::LineString,
        Geometry::Polygon(_) => GeometryType::Polygon,
        Geometry::MultiPoint(_) => GeometryType::MultiPoint,
        Geometry::MultiLineString(_) => GeometryType::MultiLineString,
        Geometry::MultiPolygon(_) => GeometryType::MultiPolygon,
        Geometry::GeometryCollection(_) => GeometryType::GeometryCollection,
        _ => GeometryType::GeometryCollection,
    }
}

/// Honour either RFC 7946 (4326 always) or legacy 2008 `crs` member.
fn extract_crs(obj: &JsonMap<String, Json>) -> Crs {
    if let Some(crs) = obj.get("crs") {
        if let Some(name) = crs
            .get("properties")
            .and_then(|p| p.get("name"))
            .and_then(Json::as_str)
        {
            // Common URN forms:
            //   urn:ogc:def:crs:EPSG::3857
            //   urn:ogc:def:crs:OGC:1.3:CRS84  (≈ 4326 axis order lng/lat)
            //   EPSG:3857
            if let Some(code) = parse_epsg_urn(name) {
                return Crs::Epsg(code);
            }
            if name.contains("CRS84") {
                return Crs::Epsg(4326);
            }
            return Crs::Wkt(name.to_string());
        }
    }
    Crs::Epsg(4326)
}

fn parse_epsg_urn(s: &str) -> Option<u32> {
    let lower = s.to_ascii_lowercase();
    // Find the last "epsg" then read trailing digits.
    let idx = lower.rfind("epsg")?;
    let tail = &s[idx + 4..];
    let digits: String = tail.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::ValueType;

    #[test]
    fn reads_feature_collection() {
        let json = br#"
        {
          "type": "FeatureCollection",
          "features": [
            { "type": "Feature", "id": 1, "geometry": {"type":"Point","coordinates":[1,2]}, "properties": {"name": "a", "rank": 10}},
            { "type": "Feature", "id": 2, "geometry": {"type":"Point","coordinates":[3,4]}, "properties": {"name": "b", "rank": 20}}
          ]
        }"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.feature_count(), 2);
        assert_eq!(r.schema().fields.len(), 2);
        assert_eq!(r.features()[0].fid, Some(1));
    }

    #[test]
    fn reads_bare_feature() {
        let json = br#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.feature_count(), 1);
    }

    #[test]
    fn reads_bare_geometry() {
        let json = br#"{"type":"Point","coordinates":[10,20]}"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.feature_count(), 1);
        assert!(r.features()[0].geometry.is_some());
    }

    #[test]
    fn mixed_geometry_kinds_become_collection() {
        let json = br#"{
            "type":"FeatureCollection",
            "features":[
                {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}},
                {"type":"Feature","geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]},"properties":{}}
            ]
        }"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(
            r.schema().geometry.as_ref().unwrap().kind,
            GeometryType::GeometryCollection
        );
    }

    #[test]
    fn honours_legacy_epsg_urn() {
        let json = br#"{
            "type":"FeatureCollection",
            "crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::3857"}},
            "features":[]
        }"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.schema().crs, Crs::Epsg(3857));
    }

    #[test]
    fn defaults_to_epsg_4326() {
        let json = br#"{"type":"FeatureCollection","features":[]}"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.schema().crs, Crs::Epsg(4326));
    }

    #[test]
    fn null_geometry_allowed() {
        let json = br#"{
            "type":"FeatureCollection",
            "features":[
                {"type":"Feature","geometry":null,"properties":{"x":1}}
            ]
        }"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.feature_count(), 1);
        assert!(r.features()[0].geometry.is_none());
    }

    #[test]
    fn rejects_non_object_root() {
        assert!(GeoJsonReader::from_bytes(b"[]").is_err());
        assert!(GeoJsonReader::from_bytes(b"42").is_err());
    }

    #[test]
    fn rejects_missing_type() {
        assert!(GeoJsonReader::from_bytes(b"{}").is_err());
    }

    #[test]
    fn string_id_parses_to_fid() {
        let json = br#"{"type":"Feature","id":"42","geometry":null,"properties":{}}"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.features()[0].fid, Some(42));
    }

    #[test]
    fn schema_widens_int_to_float64() {
        let json = br#"{
            "type":"FeatureCollection",
            "features":[
                {"type":"Feature","geometry":null,"properties":{"v":1}},
                {"type":"Feature","geometry":null,"properties":{"v":2.5}}
            ]
        }"#;
        let r = GeoJsonReader::from_bytes(json).unwrap();
        assert_eq!(r.schema().fields[0].ty, ValueType::Float64);
        // Values must coerce — int → float on read.
        match &r.features()[0].attributes[0] {
            Value::Float64(n) => assert_eq!(*n, 1.0),
            other => panic!("expected Float64, got {other:?}"),
        }
    }
}

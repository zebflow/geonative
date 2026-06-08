//! Coordinate reference system. Carried through verbatim from the source so
//! each writer can serialize it in its own preferred form.

/// Coordinate reference system. Marked `#[non_exhaustive]` because future
/// versions may add `Wkt2(String)`, a structured `Authority { name, code }`,
/// or pre-parsed PROJJSON without that counting as a SemVer-breaking change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Crs {
    /// CRS unknown or unspecified.
    #[default]
    Unknown,
    /// An EPSG authority code (e.g. 4326, 7844).
    Epsg(u32),
    /// Well-Known Text (ESRI-WKT or OGC-WKT). Stored verbatim.
    Wkt(String),
    /// PROJJSON. Stored verbatim (GeoParquet's preferred form).
    Projjson(String),
}

impl Crs {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Crs::Unknown)
    }

    /// Return the CRS as an EPSG authority code, if it can be cheaply
    /// determined. Resolution order:
    /// 1. [`Crs::Epsg`] returns directly.
    /// 2. [`Crs::Wkt`] looks for `AUTHORITY["EPSG","NNNN"]` (WKT1) or
    ///    `ID["EPSG",NNNN]` (WKT2) on the outermost CRS node.
    /// 3. If no authority is present, fall back to a small inline lookup of
    ///    common datum/CRS names ("GDA2020", "WGS 84", "NAD83", "Web
    ///    Mercator", etc.) — handles the ESRI File-Geodatabase case where
    ///    the WKT has just a name and a datum, no AUTHORITY.
    ///
    /// Returns `None` for [`Crs::Unknown`], [`Crs::Projjson`], or WKT we
    /// can't resolve. Full WKT → EPSG resolution (every CRS) requires PROJ
    /// and is the job of the future `geonative-proj` crate.
    pub fn epsg_code(&self) -> Option<u32> {
        match self {
            Crs::Epsg(n) => Some(*n),
            Crs::Wkt(s) => extract_trailing_epsg(s).or_else(|| epsg_from_wkt_name(s)),
            Crs::Unknown | Crs::Projjson(_) => None,
        }
    }

    /// Render the CRS as PROJJSON — the form GeoParquet stores in its `geo`
    /// metadata. v0.1 produces a minimal PROJJSON that just references an
    /// EPSG code when one is detectable:
    ///
    /// ```json
    /// { "$schema": "...", "type": "GeographicCRS", "id": { "authority": "EPSG", "code": 4326 } }
    /// ```
    ///
    /// Returns `None` if no EPSG code is detectable. Full WKT → PROJJSON
    /// conversion (preserving every parameter) requires PROJ and is deferred
    /// to the optional `geonative-proj` crate; until then, the GeoParquet
    /// spec also accepts WKT in the `crs` field as a string fallback.
    pub fn to_projjson(&self) -> Option<String> {
        let code = self.epsg_code()?;
        Some(format!(
            r#"{{"$schema":"https://proj.org/schemas/v0.7/projjson.schema.json","id":{{"authority":"EPSG","code":{code}}}}}"#
        ))
    }
}

/// Find an `AUTHORITY["EPSG","NNNN"]` (or `ID["EPSG",NNNN]` in WKT2) clause
/// that terminates the WKT, returning the numeric code. Scans from the right
/// to prefer the outermost authority over any inner ones (e.g. an inner
/// datum's authority).
fn extract_trailing_epsg(wkt: &str) -> Option<u32> {
    // Look for both forms; AUTHORITY is WKT1 (most ESRI .prj sidecars), ID is WKT2.
    let candidates = [
        find_clause_value(wkt, "AUTHORITY[\"EPSG\",\""),
        find_clause_value(wkt, "ID[\"EPSG\","),
    ];
    candidates.into_iter().flatten().last()
}

fn find_clause_value(wkt: &str, opener: &str) -> Option<u32> {
    // Find the LAST occurrence of `opener`, then read digits up to the next
    // `"` or `]`. Iterating from the right ensures we match the outer authority.
    let pos = wkt.rfind(opener)?;
    let rest = &wkt[pos + opener.len()..];
    let end = rest.find(['"', ']', ',', ' ']).unwrap_or(rest.len());
    rest[..end].parse::<u32>().ok()
}

/// Best-effort EPSG lookup by recognising the outermost CRS name in a WKT
/// that has no AUTHORITY clause (typical of ESRI File-Geodatabase WKTs).
///
/// Extracts the first quoted string after `GEOGCS[`, `GEOGCRS[`, `PROJCS[`,
/// or `PROJCRS[` and matches it against a small hardcoded table covering the
/// CRSes most commonly seen in Australian and global data.
fn epsg_from_wkt_name(wkt: &str) -> Option<u32> {
    let name = extract_outer_crs_name(wkt)?;
    epsg_for_common_name(&name)
}

fn extract_outer_crs_name(wkt: &str) -> Option<String> {
    // Find the earliest occurrence of any CRS opener (the outer one comes first).
    let openers = ["PROJCS[\"", "PROJCRS[\"", "GEOGCS[\"", "GEOGCRS[\""];
    let opener_pos = openers
        .iter()
        .filter_map(|op| wkt.find(op).map(|p| (p + op.len(), op)))
        .min_by_key(|(p, _)| *p)?;
    let start = opener_pos.0;
    let rest = &wkt[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn epsg_for_common_name(name: &str) -> Option<u32> {
    // Match-on-trimmed: some ESRI WKTs use underscores instead of spaces.
    let normalized = name.trim().replace('_', " ").to_ascii_uppercase();
    Some(match normalized.as_str() {
        // GDA2020 (Australia)
        "GDA2020" | "GCS GDA 2020" | "GDA 2020" => 7844,
        // GDA94 (Australia, older)
        "GDA94" | "GCS GDA 1994" | "GDA 1994" => 4283,
        // WGS 84
        "WGS 84" | "WGS84" | "WGS 1984" | "GCS WGS 1984" => 4326,
        // NAD83 (North America)
        "NAD83" | "NAD 83" | "GCS NORTH AMERICAN 1983" => 4269,
        // NAD27
        "NAD27" | "NAD 27" | "GCS NORTH AMERICAN 1927" => 4267,
        // Web Mercator (the projection web maps use)
        "WGS 84 / PSEUDO-MERCATOR" | "WGS 1984 WEB MERCATOR AUXILIARY SPHERE" | "WEB MERCATOR" => {
            3857
        }
        // British National Grid
        "OSGB 1936 / BRITISH NATIONAL GRID"
        | "BRITISH NATIONAL GRID"
        | "OSGB36 / BRITISH NATIONAL GRID" => 27700,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epsg_code_from_epsg_variant() {
        assert_eq!(Crs::Epsg(4326).epsg_code(), Some(4326));
        assert_eq!(Crs::Epsg(7844).epsg_code(), Some(7844));
    }

    #[test]
    fn epsg_code_from_wkt_authority_clause() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(Crs::Wkt(wkt.into()).epsg_code(), Some(4326));
    }

    #[test]
    fn epsg_code_from_wkt2_id_clause() {
        let wkt = r#"GEOGCRS["GDA2020",DATUM["GDA2020"],PRIMEM["Greenwich",0],ID["EPSG",7844]]"#;
        assert_eq!(Crs::Wkt(wkt.into()).epsg_code(), Some(7844));
    }

    #[test]
    fn epsg_code_none_for_unknown_or_projjson() {
        assert_eq!(Crs::Unknown.epsg_code(), None);
        assert_eq!(Crs::Projjson("{}".into()).epsg_code(), None);
        // WKT without AUTHORITY clause and an unrecognized name
        assert_eq!(Crs::Wkt("LOCAL_CS[\"custom\"]".into()).epsg_code(), None);
    }

    #[test]
    fn epsg_code_from_wkt_name_when_no_authority() {
        // The exact WKT we get from VicMap's FileGDB — no AUTHORITY, just the name.
        let wkt = r#"GEOGCS["GDA2020",DATUM["GDA2020",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(Crs::Wkt(wkt.into()).epsg_code(), Some(7844));

        // WGS84 variant ESRI sometimes emits with underscores.
        let wkt = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(Crs::Wkt(wkt.into()).epsg_code(), Some(4326));

        // GDA94
        let wkt = r#"GEOGCS["GCS_GDA_1994",DATUM["D_GDA_1994",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        assert_eq!(Crs::Wkt(wkt.into()).epsg_code(), Some(4283));
    }

    #[test]
    fn authority_takes_precedence_over_name_lookup() {
        // If both are present, the trailing AUTHORITY wins (matches GDAL semantics).
        let wkt = r#"GEOGCS["GDA2020",DATUM["GDA2020"],AUTHORITY["EPSG","9999"]]"#;
        assert_eq!(Crs::Wkt(wkt.into()).epsg_code(), Some(9999));
    }

    #[test]
    fn projjson_minimal_form() {
        let s = Crs::Epsg(4326).to_projjson().unwrap();
        assert!(s.contains("\"authority\":\"EPSG\""));
        assert!(s.contains("\"code\":4326"));
        assert!(s.contains("$schema"));
    }
}

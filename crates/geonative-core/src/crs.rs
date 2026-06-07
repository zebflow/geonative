//! Coordinate reference system. Carried through verbatim from the source so
//! each writer can serialize it in its own preferred form.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Crs {
    /// CRS unknown or unspecified.
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
}

impl Default for Crs {
    fn default() -> Self {
        Crs::Unknown
    }
}

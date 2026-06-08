//! The [`Transformer`] handle — constructed once, applied to many coords.
//!
//! ## Pipeline
//!
//! Every transformation flows through **EPSG:4326 lat/lng** as the canonical
//! pivot:
//!
//! ```text
//! source → (inverse op) → 4326 → (forward op) → target
//! ```
//!
//! If the source IS 4326 (or 7844, which we treat as equivalent in v0.1),
//! the inverse op is identity. Same for target → forward. If both are 4326,
//! the whole thing is identity and is essentially free.
//!
//! Specifics:
//! - **4326 ↔ 3857**: spherical Web Mercator forward / inverse.
//! - **4326 ↔ UTM/MGA**: Krüger n-series TM forward / inverse.
//! - **Two projected CRSes** (e.g. 7855 → 32754): inverse TM(GRS80) →
//!   4326 → forward TM(WGS84). Both legs use the same shared TM engine.

use geonative_core::{Coord, Crs, Geometry};

use crate::epsg::{kind_for, CrsKind};
use crate::error::{ProjError, Result};
use crate::spherical_mercator;
use crate::tm;

/// Constructed once, applied to many coords. Internally a (source-leg,
/// target-leg) pair operating through the 4326 lat/lng pivot.
#[derive(Debug, Clone)]
pub struct Transformer {
    source: Crs,
    target: Crs,
    inverse: Op,
    forward: Op,
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Identity,
    SphericalMercatorForward,
    SphericalMercatorInverse,
    TmForward(tm::TmParams),
    TmInverse(tm::TmParams),
}

impl Transformer {
    /// Construct from EPSG codes — the common case.
    pub fn from_epsg(from: u32, to: u32) -> Result<Self> {
        Self::build(Crs::Epsg(from), Crs::Epsg(to))
    }

    /// Construct from `core::Crs`. v0.1 supports `Crs::Epsg(_)` only;
    /// `Wkt` / `Projjson` / `Unknown` return [`ProjError::UnsupportedCrs`].
    pub fn from_crs(from: &Crs, to: &Crs) -> Result<Self> {
        match (from, to) {
            (Crs::Epsg(f), Crs::Epsg(t)) => Self::build(Crs::Epsg(*f), Crs::Epsg(*t)),
            (Crs::Unknown, _) | (_, Crs::Unknown) => Err(ProjError::UnsupportedCrs(
                "CRS::Unknown cannot be transformed; declare an EPSG code".into(),
            )),
            (Crs::Wkt(_), _) | (_, Crs::Wkt(_)) => Err(ProjError::UnsupportedCrs(
                "CRS::Wkt — WKT parsing arrives in v0.2".into(),
            )),
            (Crs::Projjson(_), _) | (_, Crs::Projjson(_)) => Err(ProjError::UnsupportedCrs(
                "CRS::Projjson — PROJJSON parsing arrives in v0.2".into(),
            )),
            _ => Err(ProjError::UnsupportedCrs(format!(
                "unsupported CRS pair: {from:?} → {to:?}"
            ))),
        }
    }

    fn build(source: Crs, target: Crs) -> Result<Self> {
        let src_code = epsg_code(&source)?;
        let tgt_code = epsg_code(&target)?;
        let inverse = inverse_op(kind_for(src_code)?);
        let forward = forward_op(kind_for(tgt_code)?);
        Ok(Self {
            source,
            target,
            inverse,
            forward,
        })
    }

    pub fn source_crs(&self) -> &Crs {
        &self.source
    }

    pub fn target_crs(&self) -> &Crs {
        &self.target
    }

    /// Identity check — returns true when source EPSG == target EPSG (or
    /// when both are 4326-equivalent).
    pub fn is_identity(&self) -> bool {
        matches!(self.inverse, Op::Identity) && matches!(self.forward, Op::Identity)
    }

    /// Transform one coord in place.
    pub fn transform(&self, c: &mut Coord) -> Result<()> {
        apply(self.inverse, c)?;
        apply(self.forward, c)?;
        Ok(())
    }

    /// Transform a slice of coords in place.
    pub fn transform_slice(&self, coords: &mut [Coord]) -> Result<()> {
        for c in coords {
            self.transform(c)?;
        }
        Ok(())
    }

    /// Walk a `Geometry` and transform every coord in place. Z/M are
    /// preserved (we're 2D-only in v0.1; vertical datums come later).
    pub fn transform_geometry(&self, g: &mut Geometry) -> Result<()> {
        visit_coords(g, &mut |c| self.transform(c))
    }
}

fn epsg_code(crs: &Crs) -> Result<u32> {
    match crs {
        Crs::Epsg(n) => Ok(*n),
        other => Err(ProjError::UnsupportedCrs(format!("{other:?}"))),
    }
}

fn inverse_op(kind: CrsKind) -> Op {
    match kind {
        CrsKind::Geographic4326 => Op::Identity,
        CrsKind::WebMercator => Op::SphericalMercatorInverse,
        CrsKind::TransverseMercator(p) => Op::TmInverse(p),
    }
}

fn forward_op(kind: CrsKind) -> Op {
    match kind {
        CrsKind::Geographic4326 => Op::Identity,
        CrsKind::WebMercator => Op::SphericalMercatorForward,
        CrsKind::TransverseMercator(p) => Op::TmForward(p),
    }
}

fn apply(op: Op, c: &mut Coord) -> Result<()> {
    match op {
        Op::Identity => {}
        Op::SphericalMercatorForward => spherical_mercator::forward(c),
        Op::SphericalMercatorInverse => spherical_mercator::inverse(c),
        Op::TmForward(p) => tm::forward(&p, c),
        Op::TmInverse(p) => tm::inverse(&p, c),
    }
    Ok(())
}

fn visit_coords<F>(g: &mut Geometry, f: &mut F) -> Result<()>
where
    F: FnMut(&mut Coord) -> Result<()>,
{
    use geonative_core::Geometry as G;
    match g {
        G::Point(c) => f(c),
        G::LineString(ls) => {
            for c in &mut ls.coords {
                f(c)?;
            }
            Ok(())
        }
        G::Polygon(p) => {
            for c in &mut p.exterior.coords {
                f(c)?;
            }
            for ring in &mut p.holes {
                for c in &mut ring.coords {
                    f(c)?;
                }
            }
            Ok(())
        }
        G::MultiPoint(pts) => {
            for c in pts {
                f(c)?;
            }
            Ok(())
        }
        G::MultiLineString(lines) => {
            for ls in lines {
                for c in &mut ls.coords {
                    f(c)?;
                }
            }
            Ok(())
        }
        G::MultiPolygon(polys) => {
            for p in polys {
                for c in &mut p.exterior.coords {
                    f(c)?;
                }
                for ring in &mut p.holes {
                    for c in &mut ring.coords {
                        f(c)?;
                    }
                }
            }
            Ok(())
        }
        G::GeometryCollection(items) => {
            for item in items {
                visit_coords(item, f)?;
            }
            Ok(())
        }
        // Future variants pass through unchanged — we don't know how to
        // transform unknown geometry shapes.
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geonative_core::LineString;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn identity_4326_to_4326() {
        let t = Transformer::from_epsg(4326, 4326).unwrap();
        assert!(t.is_identity());
        let mut c = Coord::xy(145.0, -37.0);
        t.transform(&mut c).unwrap();
        assert_eq!(c.x, 145.0);
        assert_eq!(c.y, -37.0);
    }

    #[test]
    fn identity_gda2020_and_4326_equivalent() {
        // v0.1: 7844 ≈ 4326 for visualisation.
        let t = Transformer::from_epsg(7844, 4326).unwrap();
        assert!(t.is_identity());
    }

    #[test]
    fn web_mercator_round_trip_via_pipeline() {
        let to_3857 = Transformer::from_epsg(4326, 3857).unwrap();
        let to_4326 = Transformer::from_epsg(3857, 4326).unwrap();
        let mut c = Coord::xy(144.9631, -37.8136);
        to_3857.transform(&mut c).unwrap();
        to_4326.transform(&mut c).unwrap();
        assert!(approx(c.x, 144.9631, 1e-9));
        assert!(approx(c.y, -37.8136, 1e-9));
    }

    #[test]
    fn mga55_to_4326_melbourne_round_trip() {
        // Vicmap MGA Zone 55. Round-trip 4326 → 7855 → 4326. With my Krüger
        // implementation this recovers to nanometre precision.
        let to_mga = Transformer::from_epsg(4326, 7855).unwrap();
        let to_wgs = Transformer::from_epsg(7855, 4326).unwrap();
        let original = Coord::xy(144.9631, -37.8136);
        let mut c = original;
        to_mga.transform(&mut c).unwrap();
        to_wgs.transform(&mut c).unwrap();
        // 1e-12° ≈ 0.1 µm — recovery is essentially float-precision-limited.
        assert!(approx(c.x, original.x, 1e-12), "got {}", c.x);
        assert!(approx(c.y, original.y, 1e-12), "got {}", c.y);
    }

    #[test]
    fn mga56_to_utm56_two_projected_crses() {
        // Two projected CRSes — must pipe through 4326. MGA-56 (GRS80) and
        // WGS84/UTM-56 share central meridian, scale, false offsets; only
        // ellipsoid differs (GRS80 vs WGS84, identical to ~0.1 mm).
        let to_utm56 = Transformer::from_epsg(7856, 32756).unwrap();
        let to_mga56 = Transformer::from_epsg(32756, 7856).unwrap();
        let original = Coord::xy(500_000.0, 6_250_000.0);
        let mut c = original;
        to_utm56.transform(&mut c).unwrap();
        to_mga56.transform(&mut c).unwrap();
        // Round-trip through two projected CRSes via 4326: cm-precision.
        assert!(approx(c.x, original.x, 0.01), "got {}", c.x);
        assert!(approx(c.y, original.y, 0.01), "got {}", c.y);
    }

    #[test]
    fn transform_geometry_walks_polygon() {
        let mut g = Geometry::LineString(LineString::new(vec![
            Coord::xy(144.0, -37.0),
            Coord::xy(145.0, -38.0),
            Coord::xy(146.0, -37.0),
        ]));
        let t = Transformer::from_epsg(4326, 3857).unwrap();
        t.transform_geometry(&mut g).unwrap();
        if let Geometry::LineString(ls) = g {
            // After Web Mercator forward, all x values should be in metres
            // (~16M for these lngs) and finite.
            for c in &ls.coords {
                assert!(c.x.is_finite() && c.x.abs() > 1e6);
                assert!(c.y.is_finite() && c.y.abs() > 1e6);
            }
        } else {
            panic!("type changed");
        }
    }

    #[test]
    fn unsupported_epsg_errors() {
        assert!(matches!(
            Transformer::from_epsg(99999, 4326).unwrap_err(),
            ProjError::UnsupportedEpsg(99999)
        ));
    }

    #[test]
    fn unknown_crs_errors() {
        assert!(matches!(
            Transformer::from_crs(&Crs::Unknown, &Crs::Epsg(4326)).unwrap_err(),
            ProjError::UnsupportedCrs(_)
        ));
    }
}

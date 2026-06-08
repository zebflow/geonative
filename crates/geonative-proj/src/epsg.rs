//! EPSG code → projection parameters lookup.
//!
//! Covers the v0.1 supported set:
//!
//! - **4326** — WGS84 geographic (pivot)
//! - **3857** — Web Mercator (spherical)
//! - **7844** — GDA2020 geographic (treated as identity with 4326; ~1.5 m
//!   caveat for visualisation)
//! - **32601..=32660** — WGS84/UTM zones 1–60 North
//! - **32701..=32760** — WGS84/UTM zones 1–60 South
//! - **7846..=7859** — GDA2020/MGA zones 46–59 (TM on GRS80, southern
//!   hemisphere false-northing)

use crate::ellipsoid::{GRS80, WGS84};
use crate::error::ProjError;
use crate::tm::TmParams;

/// What kind of operation an EPSG code represents.
#[derive(Debug, Clone, Copy)]
pub enum CrsKind {
    /// Geographic lat/lng on WGS84-equivalent ellipsoid. Treated as the
    /// canonical pivot CRS in the pipeline.
    Geographic4326,
    /// Spherical Web Mercator.
    WebMercator,
    /// Transverse Mercator with the embedded zone params.
    TransverseMercator(TmParams),
}

pub fn kind_for(epsg: u32) -> Result<CrsKind, ProjError> {
    match epsg {
        4326 => Ok(CrsKind::Geographic4326),
        // GDA2020 geographic — for v0.1 we treat as identical to 4326 for
        // visualisation use (positions differ by ~1.5 m due to plate motion
        // since the WGS84 reference epoch). Survey-grade Helmert transform
        // is on the v0.2 roadmap.
        7844 => Ok(CrsKind::Geographic4326),
        3857 => Ok(CrsKind::WebMercator),
        32601..=32660 => Ok(CrsKind::TransverseMercator(TmParams::utm_north(
            (epsg - 32600) as u8,
            WGS84,
        ))),
        32701..=32760 => Ok(CrsKind::TransverseMercator(TmParams::utm_south(
            (epsg - 32700) as u8,
            WGS84,
        ))),
        // GDA2020/MGA: EPSG code = 7800 + zone_number.
        7846..=7859 => Ok(CrsKind::TransverseMercator(TmParams::utm_south(
            (epsg - 7800) as u8,
            GRS80,
        ))),
        other => Err(ProjError::UnsupportedEpsg(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_resolve() {
        assert!(matches!(kind_for(4326).unwrap(), CrsKind::Geographic4326));
        assert!(matches!(kind_for(7844).unwrap(), CrsKind::Geographic4326));
        assert!(matches!(kind_for(3857).unwrap(), CrsKind::WebMercator));
        assert!(matches!(kind_for(32754).unwrap(), CrsKind::TransverseMercator(_)));
        assert!(matches!(kind_for(7855).unwrap(), CrsKind::TransverseMercator(_)));
    }

    #[test]
    fn mga_zone_55_central_meridian_is_147() {
        // EPSG:7855 is MGA Zone 55, central meridian 147°E. Sanity check the
        // zone-to-meridian formula stays right.
        if let CrsKind::TransverseMercator(p) = kind_for(7855).unwrap() {
            assert_eq!(p.lon0_deg, 147.0);
            assert_eq!(p.false_northing, 10_000_000.0);
        } else {
            panic!("expected TM");
        }
    }

    #[test]
    fn utm_zone_30_north_central_meridian_is_neg_3() {
        if let CrsKind::TransverseMercator(p) = kind_for(32630).unwrap() {
            assert_eq!(p.lon0_deg, -3.0);
            assert_eq!(p.false_northing, 0.0);
        } else {
            panic!("expected TM");
        }
    }

    #[test]
    fn unsupported_returns_error() {
        assert!(matches!(kind_for(99999).unwrap_err(), ProjError::UnsupportedEpsg(99999)));
    }
}

//! Reference ellipsoid parameters.
//!
//! WGS84 (used by EPSG:4326 + UTM zones) and GRS80 (used by GDA2020 + MGA
//! zones) are functionally identical to ~0.1 mm — same semi-major axis,
//! inverse-flattening differs only in the 11th significant digit. We model
//! them as distinct types anyway so future datum-shift code can keep them
//! straight.

#[derive(Debug, Clone, Copy)]
pub struct Ellipsoid {
    /// Semi-major axis, metres.
    pub a: f64,
    /// Inverse flattening (1/f).
    pub inv_f: f64,
}

impl Ellipsoid {
    /// Semi-minor axis `b = a * (1 - f)`.
    pub fn b(&self) -> f64 {
        self.a * (1.0 - 1.0 / self.inv_f)
    }

    /// First eccentricity squared `e² = 2f - f²`.
    pub fn e2(&self) -> f64 {
        let f = 1.0 / self.inv_f;
        2.0 * f - f * f
    }

    /// Third flattening `n = f / (2 - f) = (a-b)/(a+b)`.
    pub fn n(&self) -> f64 {
        let f = 1.0 / self.inv_f;
        f / (2.0 - f)
    }
}

/// WGS84 — EPSG:7030, the ellipsoid for EPSG:4326 and the WGS84 UTM family
/// (EPSG:32601–32760).
pub const WGS84: Ellipsoid = Ellipsoid {
    a: 6_378_137.0,
    inv_f: 298.257_223_563,
};

/// GRS80 — EPSG:7019, the ellipsoid for GDA2020 (EPSG:7844) and the MGA
/// family (EPSG:7846–7859).
pub const GRS80: Ellipsoid = Ellipsoid {
    a: 6_378_137.0,
    inv_f: 298.257_222_101,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgs84_constants_within_tolerance() {
        // Reference: e² ≈ 0.00669437999014133...
        assert!((WGS84.e2() - 0.006_694_379_990_141_3).abs() < 1e-15);
        // n ≈ 0.001679220395...
        assert!((WGS84.n() - 0.001_679_220_386_383_5).abs() < 1e-12);
    }

    #[test]
    fn grs80_and_wgs84_essentially_identical() {
        // Same a; inv_f differs by ~1.5e-6 in absolute terms. e² depends on
        // ~2/inv_f, so its diff is ~3e-11. Anything ≤ 1e-10 is "identical".
        assert!((WGS84.e2() - GRS80.e2()).abs() < 1e-10);
        assert_eq!(WGS84.a, GRS80.a);
    }
}

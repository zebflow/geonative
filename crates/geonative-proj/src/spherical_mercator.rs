//! EPSG:4326 ↔ EPSG:3857 — spherical Web Mercator.
//!
//! Web Mercator is *defined* to use a spherical projection on a
//! WGS84-radius sphere (`a = 6_378_137 m`, no flattening), even though the
//! input lat/lng is on the WGS84 ellipsoid. This is the official OGC choice
//! and matches what every slippy-map tile server uses. Treating it as
//! ellipsoidal would diverge from Google/OSM/Mapbox tiles by ~20 km.

use std::f64::consts::PI;

use geonative_core::Coord;

/// Earth radius used by Web Mercator, metres. By spec, the WGS84 semi-major
/// axis treated as a sphere.
pub const R: f64 = 6_378_137.0;

/// Maximum representable latitude in Web Mercator. Above this, the
/// projection diverges; slippy-map convention is to clamp.
pub const MAX_LATITUDE_DEG: f64 = 85.051_128_779_806_59;

/// 4326 → 3857. `lng` in degrees becomes easting in metres; `lat` in degrees
/// becomes northing in metres.
pub fn forward(c: &mut Coord) {
    let lat = c.y.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG);
    let lng = c.x;
    c.x = R * lng.to_radians();
    let lat_r = lat.to_radians();
    c.y = R * ((PI * 0.25 + lat_r * 0.5).tan()).ln();
}

/// 3857 → 4326. Inverse of [`forward`].
pub fn inverse(c: &mut Coord) {
    let x = c.x;
    let y = c.y;
    c.x = (x / R).to_degrees();
    c.y = (2.0 * (y / R).exp().atan() - PI * 0.5).to_degrees();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn origin_maps_to_origin() {
        let mut c = Coord::xy(0.0, 0.0);
        forward(&mut c);
        assert!(approx_eq(c.x, 0.0, 1e-9));
        assert!(approx_eq(c.y, 0.0, 1e-9));
        inverse(&mut c);
        assert!(approx_eq(c.x, 0.0, 1e-9));
        assert!(approx_eq(c.y, 0.0, 1e-9));
    }

    #[test]
    fn round_trip_melbourne() {
        // Melbourne: 144.9631°E, -37.8136°S
        let original = Coord::xy(144.9631, -37.8136);
        let mut c = original;
        forward(&mut c);
        inverse(&mut c);
        assert!(approx_eq(c.x, original.x, 1e-9));
        assert!(approx_eq(c.y, original.y, 1e-9));
    }

    #[test]
    fn forward_melbourne_matches_formula() {
        // Spherical Mercator is exact: x = R * lng_rad, y = R * ln(tan(π/4 + lat_rad/2)).
        // For Melbourne (144.9631°E, -37.8136°S), this is a closed-form
        // expectation, not an external reference. Tolerance is sub-µm.
        let mut c = Coord::xy(144.9631, -37.8136);
        forward(&mut c);
        let expected_x = R * 144.9631_f64.to_radians();
        let lat_r = (-37.8136_f64).to_radians();
        let expected_y = R * ((PI * 0.25 + lat_r * 0.5).tan()).ln();
        assert!(approx_eq(c.x, expected_x, 1e-6));
        assert!(approx_eq(c.y, expected_y, 1e-6));
        // Sanity: magnitude is in the ~16M m range expected for ~145°E.
        assert!(c.x > 16_000_000.0 && c.x < 16_200_000.0);
        // And southern hemisphere → negative northing.
        assert!(c.y < 0.0);
    }

    #[test]
    fn forward_pole_is_clamped() {
        // Lat 89° is past the Web Mercator limit (85.05°). Should clamp.
        let mut c = Coord::xy(0.0, 89.0);
        forward(&mut c);
        let mut c2 = Coord::xy(0.0, MAX_LATITUDE_DEG);
        forward(&mut c2);
        assert!(approx_eq(c.y, c2.y, 1e-6));
    }

    #[test]
    fn antimeridian_is_finite() {
        let mut c = Coord::xy(180.0, 0.0);
        forward(&mut c);
        assert!(c.x.is_finite());
        assert!(approx_eq(c.x, R * PI, 1e-3));
    }
}

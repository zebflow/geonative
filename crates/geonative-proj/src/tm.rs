//! Ellipsoidal Transverse Mercator via the **Krüger n-series** (6th order).
//!
//! Single implementation shared by UTM (WGS84 ellipsoid) and MGA (GRS80
//! ellipsoid). At 6th order, accuracy is sub-millimetre out to ±4° from the
//! central meridian — well past the ±3° standard zone width.
//!
//! ## Reference
//!
//! Karney, C. F. F. (2011). *Transverse Mercator with an accuracy of a few
//! nanometers.* Journal of Geodesy. The α (forward) and β (inverse) series
//! coefficients below are exactly from that paper, expanded to powers of
//! the third flattening `n = f / (2 - f)`.

use std::f64::consts::PI;

use geonative_core::Coord;

use crate::ellipsoid::Ellipsoid;

/// Parameters for one TM projection (one zone, one ellipsoid).
#[derive(Debug, Clone, Copy)]
pub struct TmParams {
    pub ellipsoid: Ellipsoid,
    /// Central meridian, degrees.
    pub lon0_deg: f64,
    /// Scale factor on the central meridian. UTM/MGA = 0.9996.
    pub k0: f64,
    /// False easting, metres. UTM/MGA = 500_000.
    pub false_easting: f64,
    /// False northing, metres. UTM/MGA northern = 0; southern = 10_000_000.
    pub false_northing: f64,
}

impl TmParams {
    /// UTM zone N (1–60) for the northern hemisphere on the given ellipsoid.
    pub const fn utm_north(zone: u8, ellipsoid: Ellipsoid) -> Self {
        Self {
            ellipsoid,
            lon0_deg: (zone as f64) * 6.0 - 183.0,
            k0: 0.9996,
            false_easting: 500_000.0,
            false_northing: 0.0,
        }
    }

    /// UTM zone N (1–60) for the southern hemisphere on the given ellipsoid.
    pub const fn utm_south(zone: u8, ellipsoid: Ellipsoid) -> Self {
        Self {
            ellipsoid,
            lon0_deg: (zone as f64) * 6.0 - 183.0,
            k0: 0.9996,
            false_easting: 500_000.0,
            false_northing: 10_000_000.0,
        }
    }
}

/// `(lng_deg, lat_deg) → (easting, northing)`. In-place.
pub fn forward(p: &TmParams, c: &mut Coord) {
    let coeffs = Coeffs::new(p.ellipsoid.n());
    let lat_rad = c.y.to_radians();
    let dlon_rad = (c.x - p.lon0_deg).to_radians();

    // Conformal latitude tau (Karney calls it 'sinh^{-1} of t').
    let sin_phi = lat_rad.sin();
    let two_n_sqrt_over_one_plus_n = 2.0 * coeffs.n.sqrt() / (1.0 + coeffs.n);
    let t = ((lat_rad.tan()).asinh()
        - two_n_sqrt_over_one_plus_n * (two_n_sqrt_over_one_plus_n * sin_phi).atanh())
    .sinh();

    let cos_dlon = dlon_rad.cos();
    let xi_prime = (t / cos_dlon).atan().rem_pi();
    // Karney 2011 eq. (8): η' = atanh(sin(Δλ) / √(t² + cos²(Δλ))).
    // Earlier (buggy) form used √(1 + t²) which is only correct on the
    // central meridian and drifts ~500 m at ±2° dlon.
    let eta_prime = (dlon_rad.sin() / (t * t + cos_dlon * cos_dlon).sqrt()).atanh();

    // Sum the Krüger series.
    let (xi, eta) = sum_series(&coeffs.alpha, xi_prime, eta_prime);

    c.x = p.false_easting + p.k0 * coeffs.a_hat * eta;
    c.y = p.false_northing + p.k0 * coeffs.a_hat * xi;
}

/// `(easting, northing) → (lng_deg, lat_deg)`. In-place.
pub fn inverse(p: &TmParams, c: &mut Coord) {
    let coeffs = Coeffs::new(p.ellipsoid.n());
    let xi = (c.y - p.false_northing) / (p.k0 * coeffs.a_hat);
    let eta = (c.x - p.false_easting) / (p.k0 * coeffs.a_hat);

    let (xi_prime, eta_prime) = sum_series_neg(&coeffs.beta, xi, eta);

    // Exact inverse of forward auxiliary (ξ', η'):
    //   sin(χ)  = sin(ξ') / √(1 + tanh²(η'))    [from tan(χ) = tan(ξ')·cos(Δλ)]
    //   tan(Δλ) = tanh(η') / cos(ξ')            [from inverting η' = atanh(sin(Δλ)/√(t²+cos²(Δλ)))]
    let tanh_eta = eta_prime.tanh();
    let chi = (xi_prime.sin() / (1.0 + tanh_eta * tanh_eta).sqrt()).asin();
    let dlon = tanh_eta.atan2(xi_prime.cos());

    // χ → φ via Snyder's iterative inverse (Karney 2011 eq. 30, Snyder 3-5):
    //   φ_{k+1} = atan(sinh( atanh(sin(χ)) + e · atanh(e · sin(φ_k)) ))
    // Converges in 3–5 iterations to machine precision for any reasonable lat.
    let e = (p.ellipsoid.e2()).sqrt();
    let target_atanh_sin_chi = chi.sin().atanh();
    let mut phi = chi;
    for _ in 0..15 {
        let sigma = target_atanh_sin_chi + e * (e * phi.sin()).atanh();
        let new_phi = sigma.sinh().atan();
        if (new_phi - phi).abs() < 1e-15 {
            phi = new_phi;
            break;
        }
        phi = new_phi;
    }

    c.x = p.lon0_deg + dlon.to_degrees();
    c.y = phi.to_degrees();
}

/// Sum xi = xi' + Σ a_i sin(2i xi') cosh(2i eta')
///     eta = eta' + Σ a_i cos(2i xi') sinh(2i eta')
fn sum_series(a: &[f64; 6], xi_p: f64, eta_p: f64) -> (f64, f64) {
    let mut xi = xi_p;
    let mut eta = eta_p;
    for (i, &ai) in a.iter().enumerate() {
        let k = 2.0 * (i + 1) as f64;
        xi += ai * (k * xi_p).sin() * (k * eta_p).cosh();
        eta += ai * (k * xi_p).cos() * (k * eta_p).sinh();
    }
    (xi, eta)
}

/// Same as `sum_series` but with subtracted series (used by inverse with
/// β coefficients).
fn sum_series_neg(b: &[f64; 6], xi: f64, eta: f64) -> (f64, f64) {
    let mut xi_p = xi;
    let mut eta_p = eta;
    for (i, &bi) in b.iter().enumerate() {
        let k = 2.0 * (i + 1) as f64;
        xi_p -= bi * (k * xi).sin() * (k * eta).cosh();
        eta_p -= bi * (k * xi).cos() * (k * eta).sinh();
    }
    (xi_p, eta_p)
}

/// Cached series coefficients for a given ellipsoid `n`.
struct Coeffs {
    n: f64,
    /// A-hat = a / (1 + n) * (1 + n²/4 + n⁴/64 + n⁶/256) — the meridional arc scale.
    a_hat: f64,
    alpha: [f64; 6],
    beta: [f64; 6],
}

impl Coeffs {
    fn new(n: f64) -> Self {
        let n2 = n * n;
        let n3 = n2 * n;
        let n4 = n3 * n;
        let n5 = n4 * n;
        let n6 = n5 * n;

        // a_hat = (a / (1 + n)) * (1 + n²/4 + n⁴/64 + n⁶/256)
        // The "a" is the semi-major axis, baked in by the caller via TmParams.
        // We absorb it into a_hat here using the WGS84/GRS80 value since both
        // share a = 6_378_137. If we ever support an ellipsoid with a different
        // a, this needs to be parameterised.
        let a = 6_378_137.0_f64;
        let a_hat = (a / (1.0 + n)) * (1.0 + n2 / 4.0 + n4 / 64.0 + n6 / 256.0);

        let alpha = [
            n / 2.0 - 2.0 / 3.0 * n2 + 5.0 / 16.0 * n3 + 41.0 / 180.0 * n4
                - 127.0 / 288.0 * n5
                - 7891.0 / 37_800.0 * n6,
            13.0 / 48.0 * n2 - 3.0 / 5.0 * n3 + 557.0 / 1440.0 * n4 + 281.0 / 630.0 * n5
                - 1_983_433.0 / 1_935_360.0 * n6,
            61.0 / 240.0 * n3 - 103.0 / 140.0 * n4
                + 15_061.0 / 26_880.0 * n5
                + 167_603.0 / 181_440.0 * n6,
            49_561.0 / 161_280.0 * n4 - 179.0 / 168.0 * n5 + 6_601_661.0 / 7_257_600.0 * n6,
            34_729.0 / 80_640.0 * n5 - 3_418_889.0 / 1_995_840.0 * n6,
            212_378_941.0 / 319_334_400.0 * n6,
        ];

        let beta = [
            n / 2.0 - 2.0 / 3.0 * n2 + 37.0 / 96.0 * n3 - n4 / 360.0 - 81.0 / 512.0 * n5
                + 96_199.0 / 604_800.0 * n6,
            n2 / 48.0 + n3 / 15.0 - 437.0 / 1440.0 * n4 + 46.0 / 105.0 * n5
                - 1_118_711.0 / 3_870_720.0 * n6,
            17.0 / 480.0 * n3 - 37.0 / 840.0 * n4 - 209.0 / 4480.0 * n5 + 5569.0 / 90_720.0 * n6,
            4397.0 / 161_280.0 * n4 - 11.0 / 504.0 * n5 - 830_251.0 / 7_257_600.0 * n6,
            4583.0 / 161_280.0 * n5 - 108_847.0 / 3_991_680.0 * n6,
            20_648_693.0 / 638_668_800.0 * n6,
        ];

        Self {
            n,
            a_hat,
            alpha,
            beta,
        }
    }
}

/// Trait-method-style remainder for the `atan` result so we keep |xi'| ≤ π/2.
trait RemPi {
    fn rem_pi(self) -> f64;
}

impl RemPi for f64 {
    fn rem_pi(self) -> f64 {
        // `atan` already returns in (-π/2, π/2). Keep this for forward-compat
        // in case we widen the input via a different branch.
        let half = PI * 0.5;
        if self > half {
            self - PI
        } else if self < -half {
            self + PI
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ellipsoid::{GRS80, WGS84};

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn equator_one_degree_east_of_cm() {
        // At (lat=0, lng=CM+1°), northing must equal false_northing exactly
        // (xi=0 because tan(0)=0). Easting reflects k0·a_hat·(arc + tiny α correction)
        // which for WGS84/GRS80 + 1° dlon comes to E ≈ 611_298 m.
        let params = TmParams::utm_south(56, GRS80);
        let mut c = Coord::xy(154.0, 0.0); // CM (153°) + 1°
        forward(&params, &mut c);
        assert!(approx_eq(c.x, 611_298.0, 5.0), "easting got {}", c.x);
        assert!(approx_eq(c.y, 10_000_000.0, 1e-6), "northing got {}", c.y);
    }

    #[test]
    fn wgs84_utm_melbourne_natural_zone_55_forward() {
        // Melbourne CBD: lng=144.9631°E, lat=-37.8136°S → natural zone 55
        // (CM=147°E, Melbourne is 2.04° west). Easting must be < 500_000 m
        // on the negative side of CM. Northing for lat=-37.8 should be around
        // 5.81M (true distance from equator scaled by k0).
        let params = TmParams::utm_south(55, WGS84);
        let mut c = Coord::xy(144.9631, -37.8136);
        forward(&params, &mut c);
        assert!(c.x < 500_000.0, "easting {} should be < false_easting", c.x);
        assert!(c.x > 300_000.0, "easting {} too far from CM", c.x);
        assert!(approx_eq(c.y, 5_813_000.0, 5_000.0), "northing got {}", c.y);
    }

    #[test]
    fn wgs84_utm_round_trip_melbourne() {
        let params = TmParams::utm_south(55, WGS84);
        let original = Coord::xy(144.9631, -37.8136);
        let mut c = original;
        forward(&params, &mut c);
        inverse(&params, &mut c);
        // Round-trip should recover to sub-mm.
        assert!(approx_eq(c.x, original.x, 1e-8), "got {}", c.x);
        assert!(approx_eq(c.y, original.y, 1e-8), "got {}", c.y);
    }

    #[test]
    fn grs80_mga_zone_56_sydney_forward() {
        // Sydney: lng=151.2093°E, lat=-33.8688°S → MGA Zone 56 (CM=153°E,
        // Sydney is 1.79° west). Easting < false_easting (west of CM);
        // northing ~6.25M for lat=-33.87°.
        let params = TmParams::utm_south(56, GRS80);
        let mut c = Coord::xy(151.2093, -33.8688);
        forward(&params, &mut c);
        assert!(c.x < 500_000.0 && c.x > 300_000.0, "easting got {}", c.x);
        assert!(approx_eq(c.y, 6_251_000.0, 2_000.0), "northing got {}", c.y);
    }

    #[test]
    fn grs80_mga_round_trip_sydney() {
        let params = TmParams::utm_south(56, GRS80);
        let original = Coord::xy(151.2093, -33.8688);
        let mut c = original;
        forward(&params, &mut c);
        inverse(&params, &mut c);
        assert!(approx_eq(c.x, original.x, 1e-8));
        assert!(approx_eq(c.y, original.y, 1e-8));
    }

    #[test]
    fn northern_hemisphere_utm() {
        // London: lng=-0.1276°E, lat=51.5074°N. Zone 30N (cm 3°W).
        let params = TmParams::utm_north(30, WGS84);
        let original = Coord::xy(-0.1276, 51.5074);
        let mut c = original;
        forward(&params, &mut c);
        inverse(&params, &mut c);
        assert!(approx_eq(c.x, original.x, 1e-8));
        assert!(approx_eq(c.y, original.y, 1e-8));
    }

    #[test]
    fn central_meridian_easting_is_false_easting() {
        // On the central meridian, easting must equal false_easting exactly.
        let params = TmParams::utm_south(55, WGS84); // CM at 147°
        let mut c = Coord::xy(147.0, -30.0);
        forward(&params, &mut c);
        assert!(approx_eq(c.x, 500_000.0, 1e-3));
    }
}

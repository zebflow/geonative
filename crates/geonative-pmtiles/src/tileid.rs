//! PMTiles tile-ID encoding.
//!
//! PMTiles indexes every tile in the archive by a single `u64` "tile_id"
//! that combines the zoom level and a Hilbert-curve traversal of the
//! `(x, y)` tile grid at that zoom. This buys two huge wins:
//!
//! - **Spatially-near tiles get consecutive IDs.** Sorting by tile_id
//!   physically clusters them in the file, which is what makes range-read
//!   over object storage fast — fetching one tile usually warms up the
//!   next few you'll ask for.
//! - **One 8-byte key per tile** instead of three numbers, so the per-tile
//!   directory entries compress beautifully under varint+delta.
//!
//! Spec: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md#tileid>

use crate::error::{PmtilesError, Result};

/// Convert `(z, x, y)` to PMTiles tile_id.
///
/// Errors if `x` or `y` is out of range for zoom `z` (i.e. ≥ 2^z), or if
/// `z` is so deep that `4^z` would overflow u64 (z > 31).
pub fn coords_to_tile_id(z: u8, x: u32, y: u32) -> Result<u64> {
    if z > 31 {
        return Err(PmtilesError::malformed(format!(
            "zoom level {z} exceeds u64-safe range (max 31)"
        )));
    }
    let dim: u64 = 1u64 << z;
    if (x as u64) >= dim || (y as u64) >= dim {
        return Err(PmtilesError::malformed(format!(
            "tile ({z},{x},{y}) out of range for 2^{z}={dim}"
        )));
    }
    let offset = zoom_offset(z);
    let pos = xy_to_hilbert(dim, x as u64, y as u64);
    Ok(offset + pos)
}

/// Inverse of [`coords_to_tile_id`]. Returns `(z, x, y)`.
pub fn tile_id_to_coords(tile_id: u64) -> Result<(u8, u32, u32)> {
    // Find the largest zoom whose offset ≤ tile_id.
    let mut z: u8 = 0;
    while z < 32 && zoom_offset(z + 1) <= tile_id {
        z += 1;
    }
    let offset = zoom_offset(z);
    let pos = tile_id - offset;
    let dim: u64 = 1u64 << z;
    let (x, y) = hilbert_to_xy(dim, pos);
    Ok((z, x as u32, y as u32))
}

/// The first tile_id at zoom `z`. `((4^z) - 1) / 3` per spec — the sum of
/// tiles at all shallower zooms.
fn zoom_offset(z: u8) -> u64 {
    if z == 0 {
        return 0;
    }
    // (4^z - 1) / 3, computed without overflow up to z=31 (4^31 fits in u64).
    let four_z: u64 = 1u64 << (2 * z as u32);
    (four_z - 1) / 3
}

/// Encode (x, y) on a 2^z × 2^z grid as a Hilbert-curve distance.
/// Standard "rot/quad" algorithm — same one Wikipedia documents.
fn xy_to_hilbert(n: u64, mut x: u64, mut y: u64) -> u64 {
    let mut d: u64 = 0;
    let mut s: u64 = n / 2;
    while s > 0 {
        let rx: u64 = if (x & s) > 0 { 1 } else { 0 };
        let ry: u64 = if (y & s) > 0 { 1 } else { 0 };
        d = d.wrapping_add(s.wrapping_mul(s).wrapping_mul((3 * rx) ^ ry));
        // rotate the quadrant
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// Inverse of `xy_to_hilbert`.
fn hilbert_to_xy(n: u64, mut t: u64) -> (u64, u64) {
    let mut x: u64 = 0;
    let mut y: u64 = 0;
    let mut s: u64 = 1;
    while s < n {
        let rx: u64 = 1 & (t / 2);
        let ry: u64 = 1 & (t ^ rx);
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        x = x.wrapping_add(s.wrapping_mul(rx));
        y = y.wrapping_add(s.wrapping_mul(ry));
        t /= 4;
        s *= 2;
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_offsets_match_spec() {
        // Spec: z0=0, z1=1, z2=5, z3=21, z4=85, z5=341
        assert_eq!(zoom_offset(0), 0);
        assert_eq!(zoom_offset(1), 1);
        assert_eq!(zoom_offset(2), 5);
        assert_eq!(zoom_offset(3), 21);
        assert_eq!(zoom_offset(4), 85);
        assert_eq!(zoom_offset(5), 341);
    }

    #[test]
    fn z0_single_tile_is_tile_id_zero() {
        assert_eq!(coords_to_tile_id(0, 0, 0).unwrap(), 0);
        assert_eq!(tile_id_to_coords(0).unwrap(), (0, 0, 0));
    }

    #[test]
    fn z1_four_tiles_cover_ids_one_through_four() {
        // At z1 the Hilbert order is (0,0)→(0,1)→(1,1)→(1,0), so:
        // (1,0,0)=1, (1,0,1)=2, (1,1,1)=3, (1,1,0)=4
        assert_eq!(coords_to_tile_id(1, 0, 0).unwrap(), 1);
        assert_eq!(coords_to_tile_id(1, 0, 1).unwrap(), 2);
        assert_eq!(coords_to_tile_id(1, 1, 1).unwrap(), 3);
        assert_eq!(coords_to_tile_id(1, 1, 0).unwrap(), 4);
    }

    #[test]
    fn roundtrip_across_zooms() {
        for z in 0u8..=10 {
            let dim: u32 = 1u32 << z;
            // Test the four corners + middle of each zoom
            let cases = [
                (0, 0),
                (dim - 1, 0),
                (0, dim - 1),
                (dim - 1, dim - 1),
                (dim / 2, dim / 2),
            ];
            for (x, y) in cases {
                let id = coords_to_tile_id(z, x, y).unwrap();
                let (z2, x2, y2) = tile_id_to_coords(id).unwrap();
                assert_eq!((z, x, y), (z2, x2, y2), "roundtrip z={z} x={x} y={y}");
            }
        }
    }

    #[test]
    fn out_of_range_xy_errors() {
        assert!(coords_to_tile_id(1, 2, 0).is_err());
        assert!(coords_to_tile_id(1, 0, 2).is_err());
        assert!(coords_to_tile_id(0, 1, 0).is_err());
    }

    #[test]
    fn nearby_tiles_have_nearby_ids_away_from_folds() {
        // Hilbert locality is tightest away from the fold points (powers
        // of 2 in either axis). Picking (50, 50) at z=8 sits comfortably
        // inside one sub-quadrant — a 5×5 neighbourhood there should map
        // to a small ID window. A regression in the rotation arithmetic
        // would blow this up by orders of magnitude.
        let z = 8u8;
        let cx = 50u32;
        let cy = 50u32;
        let center_id = coords_to_tile_id(z, cx, cy).unwrap();
        let mut spread = 0i64;
        for dx in -2i32..=2 {
            for dy in -2i32..=2 {
                let id = coords_to_tile_id(z, (cx as i32 + dx) as u32, (cy as i32 + dy) as u32)
                    .unwrap();
                spread = spread.max((id as i64 - center_id as i64).abs());
            }
        }
        assert!(
            spread < 200,
            "neighbourhood spread {spread} too big at non-fold point — Hilbert math broken?"
        );
    }
}

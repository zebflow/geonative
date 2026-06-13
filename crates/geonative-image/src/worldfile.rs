//! World file (.jgw / .pgw / .tfw / .wld) parser.
//!
//! ## Layout
//!
//! Six numeric lines, one number per line:
//!
//! ```text
//! 0.5         ← line 1: pixel width (world units)
//! 0           ← line 2: rotation y (axis 2)
//! 0           ← line 3: rotation x (axis 1)
//! -0.5        ← line 4: pixel height (negative for north-up)
//! 144.96      ← line 5: world x at CENTRE of pixel (0, 0)
//! -37.81      ← line 6: world y at CENTRE of pixel (0, 0)
//! ```
//!
//! ## The half-pixel shift
//!
//! Lines 5 + 6 describe the world coords of the **centre** of pixel (0, 0).
//! GDAL's `GeoTransform` convention (which we follow in `core::raster`)
//! uses the **upper-left corner** of the image as origin. We convert at
//! parse time by shifting the origin by half a pixel.

use geonative_core::raster::GeoTransform;

use crate::error::{ImageError, Result};

/// Parse the 6-line world-file ASCII into a [`GeoTransform`].
pub fn parse(text: &str) -> Result<GeoTransform> {
    let mut nums = [0.0f64; 6];
    let mut count = 0usize;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if count >= 6 {
            return Err(ImageError::world_file(format!(
                "world file has more than 6 numeric lines (extra on line {})",
                i + 1
            )));
        }
        nums[count] = trimmed
            .parse::<f64>()
            .map_err(|e| ImageError::world_file(format!("line {} ('{}'): {e}", i + 1, trimmed)))?;
        count += 1;
    }
    if count != 6 {
        return Err(ImageError::world_file(format!(
            "world file needs 6 numeric lines, got {count}"
        )));
    }

    let pixel_w = nums[0];
    let rot_y = nums[1];
    let rot_x = nums[2];
    let pixel_h = nums[3];
    let centre_x = nums[4];
    let centre_y = nums[5];

    // Shift from pixel-centre to upper-left-corner: subtract half a pixel.
    let origin_x = centre_x - pixel_w * 0.5;
    let origin_y = centre_y - pixel_h * 0.5;

    Ok(GeoTransform {
        origin: [origin_x, origin_y],
        pixel_size: [pixel_w, pixel_h],
        rotation: [rot_x, rot_y],
    })
}

/// Look up the conventional world-file extension for an image. JGW for .jpg,
/// PGW for .png, etc. Also returns `.wld` as a universal fallback.
pub fn extensions_for(image_ext: &str) -> &'static [&'static str] {
    match image_ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => &["jgw", "jpgw", "wld"],
        "png" => &["pgw", "pngw", "wld"],
        "tif" | "tiff" => &["tfw", "tifw", "wld"],
        "bmp" => &["bpw", "wld"],
        "gif" => &["gfw", "wld"],
        _ => &["wld"],
    }
}

/// Locate a world-file sidecar next to an image path.
pub fn find_sidecar(image_path: &std::path::Path) -> Result<std::path::PathBuf> {
    let stem = image_path.file_stem().ok_or_else(|| {
        ImageError::malformed(format!("image path has no stem: {}", image_path.display()))
    })?;
    let parent = image_path.parent().unwrap_or(std::path::Path::new("."));
    let image_ext = image_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    for ext in extensions_for(image_ext) {
        for candidate in [
            parent.join(format!("{}.{ext}", stem.to_string_lossy())),
            parent.join(format!("{}.{}", stem.to_string_lossy(), ext.to_uppercase())),
        ] {
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(ImageError::MissingWorldFile {
        image: image_path.display().to_string(),
        expected: format!(
            "{}/{}.{{{}}}",
            parent.display(),
            stem.to_string_lossy(),
            extensions_for(image_ext).join(",")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_north_up() {
        let text = "0.5\n0\n0\n-0.5\n144.96\n-37.81\n";
        let gt = parse(text).unwrap();
        assert_eq!(gt.pixel_size, [0.5, -0.5]);
        assert_eq!(gt.rotation, [0.0, 0.0]);
        // Half-pixel shift: centre at (144.96, -37.81) → upper-left at
        // (144.96 - 0.25, -37.81 - (-0.25)) = (144.71, -37.56)
        assert!((gt.origin[0] - (144.96 - 0.25)).abs() < 1e-9);
        assert!((gt.origin[1] - (-37.81 - (-0.25))).abs() < 1e-9);
        assert!(gt.is_north_up());
    }

    #[test]
    fn tolerates_blank_lines() {
        let text = "0.5\n\n0\n0\n-0.5\n144\n-37\n\n";
        let gt = parse(text).unwrap();
        assert_eq!(gt.pixel_size[0], 0.5);
    }

    #[test]
    fn rejects_too_few_numbers() {
        let text = "0.5\n0\n0\n";
        assert!(parse(text).is_err());
    }

    #[test]
    fn rejects_garbage() {
        let text = "not_a_number\n0\n0\n0\n0\n0\n";
        assert!(parse(text).is_err());
    }

    #[test]
    fn extensions_for_jpg() {
        let exts = extensions_for("jpg");
        assert!(exts.contains(&"jgw"));
    }

    #[test]
    fn extensions_for_png() {
        let exts = extensions_for("png");
        assert!(exts.contains(&"pgw"));
    }

    #[test]
    fn find_sidecar_works() {
        let dir = std::env::temp_dir().join(format!("wf_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let img = dir.join("ortho.jpg");
        std::fs::write(&img, b"").unwrap();
        let wld = dir.join("ortho.jgw");
        std::fs::write(&wld, "0.5\n0\n0\n-0.5\n144\n-37\n").unwrap();

        let found = find_sidecar(&img).unwrap();
        assert_eq!(found, wld);
    }

    #[test]
    fn find_sidecar_missing_errors() {
        let dir = std::env::temp_dir().join(format!("wf_test_miss_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("noworld.jpg");
        std::fs::write(&img, b"").unwrap();
        let err = find_sidecar(&img).unwrap_err();
        assert!(matches!(err, ImageError::MissingWorldFile { .. }));
    }
}

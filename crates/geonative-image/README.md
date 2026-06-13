# geonative-image

Pure-Rust reader for image + world-file rasters — the "JPG/PNG with a 6-line `.jgw`/`.pgw` next to it" upload case.

## What v0.1 covers

- **JPEG** (`.jpg` / `.jpeg`) decode via [`jpeg-decoder`](https://crates.io/crates/jpeg-decoder)
- **PNG** decode via [`png`](https://crates.io/crates/png)
- **World file sidecars** (`.jgw` / `.pgw` / `.wld`) — 6-line ASCII format:
  ```
  0.5        ← pixel width
  0          ← rotation y
  0          ← rotation x
  -0.5       ← pixel height (typically negative for north-up)
  144.96     ← world x of centre of pixel (0, 0)
  -37.81     ← world y of centre of pixel (0, 0)
  ```
- **CRS hint** via `with_crs()` builder — world files don't carry a CRS,
  so the caller specifies it (or defaults to `Crs::Unknown`)
- Implements `geonative_core::raster::RasterLayer` — same trait that
  `geonative-geotiff::GeoTiff` implements, so `geonative-convert` treats
  them interchangeably

## v0.1 scope cuts

- **No pyramid** — these are single-resolution images; for multi-level
  output you convert to COG via `geonative-geotiff`
- **JPEG2000** (`.jp2`) — needs `jpeg2000-decoder`; deferred
- **WebP** — common in modern web maps; deferred
- **TIFF** — covered by `geonative-geotiff`, not here
- **Writer** — these formats are inputs only

## Pi-friendly resource use

Single-image decode loads the full pixel array (the whole `RasterTile`).
For a 4096×4096 RGB image that's ~50 MB peak RAM. For Pi serving, the
recommended workflow is:

```bash
# 1. User uploads big_drone_ortho.jpg (multi-MB)
# 2. Convert it to COG once, off the hot path
geonative convert big_drone_ortho.jpg ortho.cog
# 3. Tile server reads from ortho.cog via mmap — Pi-friendly RAM forever
```

For tiny inputs (< 50 MB images) the raw image path is fine; for large
inputs, convert to COG first.

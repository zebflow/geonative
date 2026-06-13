# geonative-geotiff

Pure-Rust GeoTIFF + Cloud Optimized GeoTIFF (COG) reader for the [`geonative`](https://geonative.zebflow.com) geospatial library.

## What v0.1 covers

- **Classic TIFF** + **BigTIFF** (≥4 GB files) headers
- **Compression codecs**: None, PackBits, LZW (via `weezl`), DEFLATE (via `flate2`)
- **Tiled layouts** (TileWidth / TileLength / TileOffsets / TileByteCounts) — the COG-friendly arrangement
- **Stripped layouts** (StripOffsets / StripByteCounts) — for legacy / non-COG TIFFs
- **GeoTIFF tags** (ModelPixelScale, ModelTiepoint, GeoKeyDirectory) for CRS + affine
- **EPSG-coded CRS** via `ProjectedCSTypeGeoKey` / `GeographicTypeGeoKey`
- **mmap-backed** file access — multi-GB COGs read tile-by-tile without loading the full image into RAM
- Implements `geonative_core::raster::RasterLayer`

## v0.1 scope cuts

- **JPEG-in-TIFF** (compression 7) — deferred to v0.2 (used by some legacy aerial)
- **WebP-in-TIFF** (compression 50001) — modern but rare; deferred
- **Predictor 2/3** (horizontal differencing) — deferred; most COGs use predictor=1
- **GeoKey lookup beyond EPSG** — WKT parsing for arbitrary projections deferred
- **Writer** — Phase C of Sprint 13

## Pi-friendly resource use

The reader mmaps the file. Linux's page cache fetches only the bytes you actually touch:

- 50 GB COG, request one 256×256 tile → ~50 KB of I/O, ~0.5 MB RAM
- Multi-TB sources work on a Raspberry Pi 4

Same philosophy as `geonative-filegdb`'s `.gdbtable` reader.

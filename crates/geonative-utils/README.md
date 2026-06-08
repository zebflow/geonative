# geonative-utils

Geometric algorithms for the [geonative](https://geonative.zebflow.com) geospatial library.

Currently:
- **Douglas-Peucker line simplification** — preserves ring closure for polygons, dispatches across all geometry variants (Point passes through unchanged).
- **`tolerance_for_zoom(zoom)`** — preset tolerance table for typical web-map LOD pipelines.

Pure-Rust, no deps beyond `geonative-core`.

# geonative

A lightweight, pure-Rust geospatial library — built from scratch, zero C/C++ dependencies.

> **Status: pre-release.** The crates published under `geonative-*` on crates.io are currently placeholders to reserve the namespace. Real implementation is in active development.

## Architecture

`geonative` is a Cargo workspace of focused crates. The meta-crate `geonative` re-exports them behind feature flags so users opt in only to what they need.

| Crate | Purpose |
| --- | --- |
| `geonative-core` | Common data model (`Feature`, `Geometry`, `Schema`) and driver traits (`Dataset`, `Layer`, `LayerWriter`) |
| `geonative-filegdb` | Esri File Geodatabase (`.gdb`) reader |
| `geonative-shapefile` | Shapefile (`.shp` / `.shx` / `.dbf`) reader and writer |
| `geonative-geojson` | GeoJSON reader and writer |
| `geonative-geoparquet` | GeoParquet reader and writer (WKB-encoded) |
| `geonative-processing` | Geoprocessing algorithms (buffer, clip, reproject, …) |
| `geonative-convert` | `ogr2ogr`-style format conversion pipeline |
| `geonative-cli` | Command-line interface (installs the `geonative` binary) |
| `geonative` | Meta-crate; pulls in sub-crates via Cargo features |

## Planned usage

```toml
[dependencies]
geonative = { version = "0.1", features = ["file-gdb", "file-geoparquet", "convert"] }
```

```rust
use geonative::filegdb;

let src = filegdb::open("vicmap.gdb")?;
let layer = src.layer("roads")?;
for feature in layer.read()? { /* … */ }
```

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

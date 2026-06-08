# geonative — TODO

Living checklist. Status as of `81a73ef` on `main`.

---

## Done

### Workspace + FileGDB reader (v0.1)
- [x] Workspace scaffold, 9 placeholder crates, dual MIT/Apache-2.0 licensing — `fbc2e3d`
- [x] `geonative-core` IR — `Coord`, `Geometry`, `Value`, `Schema`, `Feature`, `Crs`, `Error` — `f1f14d0`
- [x] `geonative-core` `geo-types` feature-flagged interop (lossy 2D) — `f1f14d0`
- [x] `geonative-filegdb` byte primitives — LE/varuint/varint(FileGDB-flavor) — `f1f14d0`
- [x] `.gdbtable` header + field-description section parser (GDAL-verified) — `f1f14d0`
- [x] `.gdbtablx` row-offset index with sparse-block bitmap — `f1f14d0`
- [x] Row decoder (null bitmap, all attribute types) — `ba3a945`
- [x] Catalog enumeration via `GDB_SystemCatalog` (no XML dep in v0.1) — `ba3a945`
- [x] 2D geometry decoder (Point/MultiPoint/Polyline/Polygon + General variants, curve descriptions silently dropped) — `ba3a945`
- [x] Public API: `open()` / `Geodatabase` / `Layer` / `FeatureIter` returning `core::Feature` — `ba3a945`
- [x] End-to-end validated on Vicmap VMFEAT, FID 1 byte-perfect vs `ogrinfo` — `ba3a945`

### GeoParquet writer (v0.1)
- [x] `Geometry::to_wkb()` — OGC SF, 2D LE — `a1f156e`
- [x] `Geometry::bbox()` — `a1f156e`
- [x] `Crs::epsg_code()` with WKT-name lookup for GDA2020/WGS84/NAD83/Web Mercator/BNG — `a1f156e`
- [x] `Crs::to_projjson()` — minimal `{authority, code}` PROJJSON — `a1f156e`
- [x] `geonative-geoparquet` crate on `arrow + parquet 58.3.0` — `a1f156e`
- [x] Schema mapper (geom + attrs + optional bbox cols) — `a1f156e`
- [x] `RecordBatchBuilder` (WKB-encodes geometry → Arrow batches) — `a1f156e`
- [x] GeoParquet 1.1 `geo` metadata builder + `GeoParquetWriter` — `a1f156e`
- [x] Hilbert sort flag (off by default; cuts row-group spread ~2x in tests) — `81a73ef`
- [x] Per-row-group bbox stats (free from parquet's column statistics) — `81a73ef`
- [x] `examples/convert.rs` — one-shot layer → parquet + timing — `81a73ef`

### Validation
- [x] Stress test: VMPROP V_PROPERTY_MP_ADDRESS — 3.05M MultiPolygons in 26s @ 124K feat/sec — `81a73ef`
- [x] Stress test: VMVEG TREE_URBAN — 10.56M Points in 19s @ 587K feat/sec — `81a73ef`
- [x] Stress test: VMVEG TREE_DENSITY — 64.8K MultiPolygons in 1.5s — `81a73ef`

---

### WKB codec round-trip (Phase 3)
- [x] **`Geometry::from_wkb(&[u8])`** in `geonative-core` — full OGC SF decoder, LE+BE per-nested, Z/M rejected as Unsupported, 12 unit tests for every variant + edge cases
- [x] **`wkb::bbox_from_bytes(&[u8])`** in `geonative-core` — alloc-free bbox walker over raw WKB; matches `Geometry::bbox()` for every variant (3 unit tests)
- [x] **Real-data round-trip integration test** — 75/75 Vicmap FOI_LINE MultiLineStrings survive GDB → WKB → parquet → WKB → Geometry with `==` equality
- [x] Comprehensive module-level rustdoc on `wkb.rs` (purpose, architecture, clever bits)

### GeoParquet reader + CLI (Phase 4)
- [x] **`GeoParquetReader`** in `geonative-geoparquet` — opens any spec-compliant 1.0/1.1 file, reconstructs `Schema` from `geo` metadata (with fallback to conventional column names), iterates `Feature` stream lazily per RecordBatch
- [x] **`parse_geo_metadata`** — hand-rolled extractor for primary_column + EPSG code from the `geo` JSON (avoids `serde_json` dep)
- [x] Inverse Arrow → `Value` decoder for every supported type (Bool, Int16/32/64, Float32/64, String, Binary, Timestamp(µs)→DateTime, FixedSizeBinary(16)→Guid)
- [x] Hides bbox covering columns + geometry column when reconstructing user attributes — our writer + reader round-trip to identical `Schema`
- [x] **End-to-end real-data test**: 75 fixture features round-trip GDB → writer → reader → `Feature` exactly (DateTime within µs tolerance per Arrow encoding)
- [x] **`geonative convert` CLI subcommand** — clap-based, `geonative convert <input.gdb> <output.parquet> [--layer NAME] [--hilbert] [--batch-size N] [--no-bbox-columns]`, auto-detects single user layer, lists available when ambiguous, format-detection by extension with helpful errors
- [x] Smoke-tested release build on single-layer + multi-layer + Hilbert + bad-extension paths

### mmap-backed .gdbtable (Phase 5)
- [x] `Layer` switched from `Vec<u8>` to `memmap2::Mmap` — peak application memory dropped 7–12× on big stress fixtures (1.36 GB → 195 MB on the 3M-polygon fixture; 2.15 GB → 176 MB on the 10.5M-point fixture)
- [x] OS-managed mmap'd pages are evicted under memory pressure, so the app can now process arbitrarily-large `.gdbtable` files on a low-RAM device
- [x] Crate-level lint relaxed `forbid(unsafe_code)` → `deny + #[allow]` scoped to the single `Mmap::map` call with documented SAFETY note
- [x] 126 workspace tests all green; throughput roughly preserved (sequential scan is mmap-friendly)

### Tile coord math + MVT encoder (Phase 7)
- [x] **new crate `geonative-tile`** — `TileCoord`/`LngLat` types, lng/lat ↔ Web Mercator XYZ, tile bbox, integer-pixel projection within a tile, metatile grouping. 11 unit tests. Foundation for MVT + future WMS/WMTS/raster tiles.
- [x] **new crate `geonative-mvt`** — hand-rolled MVT 2.1 protobuf encoder (no `prost` dep). Modules: `proto` (varint/zigzag/tag/length-delim), `geom` (Geometry → command stream with MoveTo/LineTo/ClosePath + cursor accumulator), `builder::LayerBuilder` (per-layer key/value interning + feature body emission), `lib` (`TileBuilder` + one-shot helpers + multi-layer assembly). 21 unit tests.
- [x] Both crates added to workspace + `[workspace.dependencies]`; placeholder pins not needed (these are new crates, no crates.io publish yet)

### Tag-driven crates.io publish setup (Phase 6)
- [x] Workspace version bumped 0.0.1 → 0.1.0 for implemented crates; placeholders (shapefile/geojson/processing/convert/meta) pinned to 0.0.1 so they don't accidentally ship as empty 0.1.0 shells
- [x] Inter-crate deps moved to `[workspace.dependencies]`; per-crate manifests use `name = { workspace = true }` (avoids the `sed`-the-Cargo.toml hack used by some sibling projects)
- [x] `.github/workflows/release.yml` — triggers on `v*` tag push; runs test + clippy then publishes core → filegdb → geoparquet → cli sequentially with 30s waits for crates.io indexing; creates GitHub Release with auto-generated notes
- [x] `.github/workflows/ci.yml` — fmt + clippy + test on push/PR across ubuntu/macos/windows
- [x] First `cargo fmt --all` pass + all `clippy -D warnings` issues resolved (workspace builds clippy-clean)
- [ ] **You: add `CARGO_REGISTRY_TOKEN` secret in GitHub Settings → Secrets and variables → Actions** (from your crates.io account API tokens)
- [ ] **You: `git tag v0.1.0 && git push origin v0.1.0`** to kick off the first real release

---

## In progress

(nothing currently in flight)

---

## Next up (priority order)

- [ ] **Add module-level rustdoc** (`//!` blocks) to every remaining `.rs` file in the workspace following the pattern set in `wkb.rs` (one-line purpose + architecture + clever bits).
- [ ] **`geonative-shapefile`** reader — byte specs already researched (deep-research-3 + compass-3 in `~/Downloads`).
- [ ] Extract `Dataset` / `Layer` / `LayerWriter` traits in `geonative-core` once a second format reader lands.

---

## Backlog

### Migrating zebflow spatial code into geonative
Per audit, ~55 spatial functions in zebflow are candidates. Migration order:

- [ ] `geonative-core`: bbox intersects predicate (zebflow `bbox_overlaps`, `feature_intersects_bbox`, `normalize_bbox`)
- [ ] **new crate** `geonative-utils`:
  - [ ] Douglas-Peucker simplification (5 fns from zebflow `simplify.rs`)
  - [ ] Hilbert curve (move from `geonative-geoparquet/src/hilbert.rs`)
  - [ ] Ring signed-area / orientation helpers
  - [ ] Distance / length / area
- [ ] **new crate** `geonative-tile`: slippy-map XYZ ↔ lng/lat, metatile grouping, pixel projection within tile bbox
- [ ] **new crate** `geonative-mvt`: full MVT encoder (~13 fns; protobuf wire format, varint, zigzag, geom commands)
- [ ] **new crate** `geonative-render` (optional, depends on `tiny-skia`): PNG rasterization, path building
- [ ] Move zebflow's `geoparquet_optimize.rs` logic into `geonative-geoparquet::optimize` (some of this already done via Hilbert sort)

### New format crates
- [ ] **`geonative-shapefile`** reader — byte specs already researched (deep-research-3 + compass-3 in `~/Downloads`)
- [ ] `geonative-gpkg` reader/writer (depends on `rusqlite`)
- [ ] `geonative-geojson` reader/writer
- [ ] `geonative-flatgeobuf` reader/writer

### Extracting traits
- [ ] When we have ≥2 format readers, extract `Dataset` / `Layer` / `LayerWriter` traits into `geonative-core` and re-wire both implementations. Defer until then to avoid premature abstraction.

### FileGDB v0.2+
- [ ] Z/M ordinate decoding (currently 2D-only)
- [ ] Curve linearization or preservation (currently silently dropped)
- [ ] MultiPatch geometry (codes 31, 32, 54)
- [ ] Sparse v4 64-bit OID tables (currently warns + best-effort)
- [ ] Parse `GDB_Items` XML for richer layer metadata (needs `quick-xml`)
- [ ] `.spx` spatial index reader for fast bbox filters
- [ ] `.atx` attribute index reader

### CRS / Reprojection
- [ ] **new crate** `geonative-proj` (optional) wrapping the `proj` crate (PROJ binding) for full WKT→PROJJSON conversion and CRS transforms
- [ ] **new crate** `geonative-proj-pure` (optional) — pure-Rust transforms for common cases (4326↔3857, UTM, basic Mercator)
- [ ] Expand `Crs::epsg_code()` name lookup table as needed

### Quality / infra
- [ ] CI workflow (`.github/workflows/ci.yml`): cargo check + fmt + clippy + test on PR
- [ ] Publish v0.1.0 to crates.io once API stabilizes
- [ ] `cargo deny` + license audit
- [ ] Fuzz targets for byte parsers (varuint, varint, table header, tablx header, shape buffer)
- [ ] Benchmarks (criterion) for decoder + encoder hot paths

### Docs (the request from this session)
- [ ] Add module-level rustdoc (`//!`) to every `.rs` file with:
  - One-line purpose
  - Where it fits in the architecture
  - Any "clever ways" / non-obvious design decisions
- [ ] README per crate (currently minimal placeholders)
- [ ] Top-level architecture diagram in repo README

---

## Roadmap notes

- **v0.1.x**: stabilize the current scope (FileGDB read, GeoParquet write) + WKB decoder + reader
- **v0.2**: Z/M support, GPKG, shapefile, MVT, tile math
- **v0.3**: traits extraction, GeoArrow native encoding, GeoParquet 1.1 covering-as-struct
- **v1.0**: API freeze, full reprojection story

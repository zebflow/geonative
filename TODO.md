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

---

## In progress

(nothing currently in flight)

---

## Next up (priority order)

- [ ] **GeoParquet reader** in `geonative-geoparquet` — uses the new `from_wkb` to materialize a `Feature` stream from a parquet file. Closes the round-trip loop end-to-end.
- [ ] **`geonative-cli convert`** subcommand wiring the `convert.rs` example into the binary.
- [ ] **`memmap2`-backed `.gdbtable`** reader — cut peak RSS from ~source-size to <100MB constant on multi-GB files.
- [ ] **Add module-level rustdoc** (`//!` blocks) to every remaining `.rs` file in the workspace following the pattern set in `wkb.rs` (one-line purpose + architecture + clever bits).

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

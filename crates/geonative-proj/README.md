# geonative-proj

Pure-Rust projection engine for the [`geonative`](https://geonative.zebflow.com) geospatial library. No PROJ, no C dep.

## v0.1 coverage

- **Spherical Mercator**: EPSG:4326 ↔ EPSG:3857
- **Transverse Mercator (Krüger n-series, 6th order)** — shared between:
  - **WGS84/UTM zones 1–60 N+S** (EPSG:32601–32760)
  - **GDA2020/MGA zones 46–59** (EPSG:7846–7859) — TM on GRS80
- **GDA2020 geographic** (EPSG:7844) treated as identity with EPSG:4326 — ~1.5 m caveat for visualisation; full Helmert 7-parameter transform deferred to v0.2.

## Usage

```rust
use geonative_proj::Transformer;
use geonative_core::Coord;

let t = Transformer::from_epsg(7855, 4326)?;   // Vicmap MGA Zone 55 → WGS84
let mut c = Coord::xy(500000.0, 5800000.0);
t.transform(&mut c)?;
println!("{}, {}", c.x, c.y);  // ~144.96, -37.81
# Ok::<(), geonative_proj::ProjError>(())
```

## Roadmap

- Lambert Conformal Conic, Albers Equal Area, Polar Stereographic
- WKT / PROJJSON parsing for `Crs::Wkt` and `Crs::Projjson`
- NTv2 grid shifts (optional `geonative-proj-grids` sibling crate)
- Full Helmert 7-param datum transforms (4326 ↔ 7844 survey-grade)

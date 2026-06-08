# Stability principles for geonative

geonative is intended to become a foundational geospatial library that other
Rust projects depend on for the long term. Every `pub` symbol is a stability
commitment. These principles are how we keep that commitment honest as the
library grows.

## SemVer regime

| Phase | Version | Rule |
|---|---|---|
| Pre-1.0 (now) | `0.x.y` | **Breaking changes allowed across minor `x` bumps.** Patch bumps must be non-breaking. Treat 0.1 → 0.2 as a real version negotiation; document the breaks in the commit body. |
| 1.0+ | `≥1.0.0` | **Strict SemVer.** Breaking changes only across major bumps. We aim to stay at 1.x for a long time. |

## Module organization

- **Domain crates** (`geonative-core`, `geonative-tile`, `geonative-filegdb`):
  the crate root *should* re-export the primary types users reach for
  constantly — `Geometry`, `Feature`, `Schema`, `TileCoord`, etc. These are
  the names that define the crate.
- **Utility crates** (`geonative-utils`, anything that's a collection of
  domain-grouped functions): the crate root should **not** flatten
  re-exports. Force callers to write
  `geonative_utils::simplify::douglas_peucker(...)` so the domain stays
  visible at every call site. Mirrors how `std` (`std::fmt`, `std::iter`,
  `std::collections`) and the `geo` crate organize their algorithm surfaces.

## `#[non_exhaustive]`

Apply to **growable enums** — variants we expect to add later:
- `geonative_core::Geometry` (curves, surfaces, Z/M variants coming)
- `geonative_core::GeometryType` (mirrors `Geometry`)
- `geonative_core::Value` (Decimal, JSON, DateTimeOffset coming)
- `geonative_core::ValueType` (mirrors `Value`)
- `geonative_core::Crs` (structured Authority, Wkt2, pre-parsed PROJJSON)

Do **not** apply to fixed-shape data structs (`Coord`, `LineString`,
`Polygon`, `TileCoord`, `LngLat`, `Metatile`). Their shape is part of their
contract.

Downstream crates that `match` on a `#[non_exhaustive]` enum **must** include
a `_ => ...` wildcard arm. Pick the wildcard behavior carefully — silent
pass-through for cosmetic operations (e.g. `simplify_geometry` clones the
unknown variant unchanged), explicit "unsupported" errors for everything else.

## Field visibility

- **Coordinate-style data** (`Coord { x, y, z, m }`, `TileCoord { z, x, y }`):
  `pub` fields are fine. Their fields are part of the type's mathematical
  definition; nobody benefits from a getter.
- **Schema-style data** (`FieldDef`, `Schema`, `GeomField`): `pub` fields
  are fine **today** because the shapes are tightly tied to formats we
  read/write. If a field becomes "computed" (e.g. an index built from
  another field), private it + add a method.
- **Builder / accumulator state** (`LayerBuilder`, `GeoParquetWriter`):
  **always private**. The whole point of a builder is to hide invariants
  from the caller.

## Dependencies leaking through the public API

- **No `arrow::*` types in `geonative-core` or `geonative-tile` public
  signatures.** Arrow versions break across SemVer; pinning core to one
  Arrow version would force the whole ecosystem to upgrade in lockstep.
- **OK to have `arrow::*` in `geonative-geoparquet` public signatures** —
  that crate explicitly opts users into arrow + parquet 58.x.
- **OK to have `geonative_core::Geometry` everywhere** — that's the whole
  point of the IR.

Rule of thumb: if a crate's `Cargo.toml` lists a dep, that dep's types
*may* appear in its public API. Otherwise, no.

## Re-export discipline

Every `pub use` is a permanent name in the crate's public surface. Reach for
`pub(crate)` first; promote to `pub` only when an external caller actually
needs it.

For the meta-crate `geonative` (when we finally wire it up):
- `pub use geonative_core::*;` is acceptable as the documented "always
  available" surface
- `#[cfg(feature = "...")] pub mod xyz { pub use geonative_xyz::*; }` for
  feature-gated sub-crate exposure
- Anything else added later requires a SemVer review

## What "stable" actually means here

Pre-1.0: we are honest that the API may change in 0.X+1. We commit to:
- documenting every break in the commit body
- never re-purposing an existing name (don't rename `Crs::Epsg` to mean
  something else; add a new variant instead)
- never silently changing a function's semantics

Post-1.0: we commit to true SemVer — additions in minor, breakage in major,
yanked versions get a new patch with a deprecation pointer.

## Reviewer checklist (per PR / per commit)

Before adding a new `pub` symbol:
- [ ] Is it actually needed externally, or could it be `pub(crate)`?
- [ ] If it's an enum that might grow, is `#[non_exhaustive]` on it?
- [ ] Does its signature leak a third-party type that the crate's
      `Cargo.toml` doesn't already commit to?
- [ ] Does it have a doc comment explaining purpose + non-obvious bits?
- [ ] If it replaces an existing symbol, is the old one kept + deprecated
      (pre-1.0 only) or held for a major bump (post-1.0)?

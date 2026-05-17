# proj-rust

[![proj-core crates.io](https://img.shields.io/crates/v/proj-core.svg)](https://crates.io/crates/proj-core)
[![proj-core docs.rs](https://docs.rs/proj-core/badge.svg)](https://docs.rs/proj-core)
[![proj-wkt crates.io](https://img.shields.io/crates/v/proj-wkt.svg)](https://crates.io/crates/proj-wkt)
[![proj-wkt docs.rs](https://docs.rs/proj-wkt/badge.svg)](https://docs.rs/proj-wkt)

Pure-Rust coordinate transformation library. No C libraries, no build scripts, no unsafe code.

This workspace currently contains:

- `proj-core`: transform engine, CRS registry, projection math, and datum shifts
- `proj-wkt`: parsing and compatibility helpers for EPSG codes, WKT, PROJ strings, and PROJJSON

## Release Scope

`proj-rust` is intended for production use within its supported CRS and projection set. It is not a full implementation of all PROJ capabilities.

Current non-goals for the `0.6` release line include:

- packaged vertical grid assets, broad vertical operation selection, cross-datum vertical, or time-dependent CRS transformation operations
- arbitrary user-defined PROJ pipeline parsing/execution beyond the supported CRS and operation model
- full EPSG/PROJ registry coverage outside the implemented projection families and embedded operation set
- full custom CRS coverage for arbitrary axis-order, prime-meridian, and geographic angular-unit semantics
- grid ecosystems beyond the supported embedded NTv2 resources and caller-supplied NTv2/GTX resources

## Usage

```rust
use proj_core::{Bounds, Transform};

// WGS84 geographic (degrees) -> Web Mercator (meters)
let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
let (x, y) = t.convert((-74.006, 40.7128)).unwrap();

// Inverse: Web Mercator -> WGS84
let inv = t.inverse().unwrap();
let (lon, lat) = inv.convert((x, y)).unwrap();

// Works with geo_types::Coord<f64> (with `geo-types` feature)
let coord = geo_types::Coord { x: -74.006, y: 40.7128 };
let projected: geo_types::Coord<f64> = t.convert(coord).unwrap();

// Batch transforms
let coords: Vec<(f64, f64)> = vec![(-74.006, 40.7128); 1000];
let results = t.convert_batch(&coords).unwrap();

// 3D transforms preserve or unit-convert the third ordinate when vertical semantics are compatible
let (x, y, h) = t.convert_3d((-74.006, 40.7128, 15.0)).unwrap();
assert_eq!(h, 15.0);

// Reproject an extent by densifying its perimeter
let bounds = Bounds::new(-74.3, 40.45, -73.65, 40.95);
let projected_bounds = t.transform_bounds(bounds, 8).unwrap();
assert!(projected_bounds.max_x > projected_bounds.min_x);
```

Coordinates use the CRS's native units: degrees for geographic CRS, and the CRS's declared linear unit for projected CRS (for example meters or US survey feet).
For `convert_3d()`, the `z` component is preserved unchanged when neither CRS declares an explicit vertical component or both CRS definitions declare the same vertical component in the same unit. When both CRS definitions declare the same vertical reference frame with different linear units, `z` is converted between those units. Grid/geoid-backed ellipsoidal-to-gravity height transforms require an explicit `VerticalGridOperation` and caller-supplied grid resources; otherwise they are rejected.

## Supported Input Formats

With `proj-core`, transforms can be created from registry-backed EPSG codes such as `"EPSG:4326"` and `"EPSG:3857"`.

With `proj-wkt`, the following CRS definition formats are supported:

- EPSG authority codes and bare EPSG numbers
- OGC `CRS:84` aliases and EPSG URNs
- common PROJ strings for the implemented projection families, including legacy `+init=epsg:XXXX`
- WKT1 and the supported WKT2 projected/geographic CRS forms, including top-level EPSG `ID[...]` and compound CRS definitions with explicit vertical components
- basic PROJJSON geographic, projected, and compound CRS definitions for the implemented methods

Custom WKT, PROJJSON, and PROJ string definitions are only accepted when they map cleanly onto this workspace's native CRS model:
2D longitude/latitude geographic coordinates in degrees with a Greenwich prime meridian, and projected coordinates in native linear units with easting/northing axis order.
Definitions that require unsupported axis-order, prime-meridian, or geographic angular-unit semantics are rejected instead of being silently degraded.

## Supported CRS

| Projection | Status | EPSG |
|---|---|---|
| Geographic (WGS84, NAD83, NAD27, ETRS89, etc.) | Implemented | 4326, 4269, 4267, 4258, ... |
| 3D geographic / compound CRS with compatible vertical component | Modelled for z preservation and same-reference unit conversion | 4979, custom WKT/PROJJSON |
| Vertical CRS metadata | Implemented for common height CRS lookup and compound parsing | 3855, 5702, 5703, 5773, 6360 |
| Web Mercator | Implemented | 3857 |
| Transverse Mercator / UTM | Implemented | 32601-32660, 32701-32760 |
| Polar Stereographic | Implemented | 3413, 3031, 3995, 32661, 32761 |
| Lambert Conformal Conic | Implemented | 2154, 3347 |
| Albers Equal Area | Implemented | 5070, 3005 |
| Lambert Azimuthal Equal Area | Implemented | 3035, 3408, 6931, 9311 |
| Oblique Stereographic | Implemented | 28992, 2953 |
| Hotine Oblique Mercator / RSO | Implemented | 2056, 3078, 3375 |
| Cassini-Soldner | Implemented | 30200, 3377 |
| Mercator | Implemented | 3395 |
| Equidistant Cylindrical | Implemented | 32662 |

Custom CRS definitions can be constructed and passed to `Transform::from_crs_defs()`. Use `Transform::from_crs_defs_with_selection_options()` when custom definitions reference external grid resources through a `GridProvider`. Use `Transform::from_horizontal_components()` for XY-only workflows that need to accept compound CRS inputs while deliberately ignoring vertical semantics. The companion `proj-wkt` crate parses EPSG codes, a subset of WKT/PROJ strings, and basic PROJJSON inputs into `CrsDef` values, including PROJ `+nadgrids` lists for supported NTv2 horizontal grid shifts and compound CRS definitions whose compatible vertical component can be preserved or unit-converted.

## Operation Selection And Grids

`proj-core` includes embedded coordinate-operation metadata, default operation selection, and explicit operation execution. `Transform::new()` and `Transform::from_crs_defs()` choose the best supported operation for the CRS pair, while `Transform::with_selection_options()` lets callers supply an area of interest or require grid-backed or exact-area matches.
Use `SelectionOptions::new()` with fluent builders such as `.with_area_of_interest(...)`, `.require_grids()`, `.require_exact_area_match()`, `.with_grid_provider(...)`, and `.with_vertical_grid_operation(...)` for advanced operation and grid selection.

The default `SelectionPolicy::BestAvailable` does not synthesize approximate Helmert datum-shift fallbacks. If a custom CRS pair has no supported registry/grid operation and an approximate Helmert shift derived from datum metadata is acceptable, opt in with `.allow_approximate_helmert_fallback()`. Approximate fallback operations are reported with `approximate = true` in operation metadata and selection diagnostics.

Use `Transform::selected_operation()`, `Transform::selection_diagnostics()`, `Transform::vertical_diagnostics()`, `registry::operation_candidates_between()`, and `lookup_operation()` when you need deterministic operation inspection including operation direction. NTv2 horizontal grid-backed transforms are supported through the embedded registry, parsed PROJ `+nadgrids` custom CRS definitions, `EmbeddedGridProvider`, `FilesystemGridProvider`, and custom `GridProvider` implementations. Vertical same-reference unit conversion is supported. NOAA/VDatum binary GTX vertical grids are supported through `FilesystemGridProvider` or a custom `GridProvider` when the caller supplies an explicit `VerticalGridOperation`; vertical grid selection honors the operation area of use, falls back across candidate grids after coverage misses, and reports resolved grid SHA-256 checksums in diagnostics. Packaged geoid grid assets and broad vertical operation selection remain outside the default registry.

With the default `geo-types` feature, `Transform::convert_geometry()` transforms whole 2D `geo-types` geometries including points, lines, polygons, multi-geometries, rectangles, geometry collections, and `Geometry` enum values. Geometry transforms stop at the first coordinate error and do not return partial results.

## Compatibility Surface

`proj-wkt` also exposes a lightweight `Proj` compatibility facade for downstream code that currently uses the common `new_known_crs` / `new` / `create_crs_to_crs_from_pj` / `convert` flow. These constructors honor compatibility area strings such as `lon,lat` or `west,south,east,north`, accept simple option strings including `require_grids`, `operation=<id>`, and `allow_approximate_helmert_fallback`, and provide `*_with_selection_options` variants for the full typed selection API. The facade is intentionally narrow and only covers the supported CRS semantics in this workspace.

`proj-core::Transform` and `proj-wkt::Proj` also expose inverse-transform construction and sampled bounds reprojection via `inverse()` and `transform_bounds()`.

## Feature Flags

| Flag | Default | Description |
|---|---|---|
| `rayon` | yes | Parallel batch transforms via `convert_batch_parallel()` |
| `geo-types` | yes | `From`/`Into` conversions for `geo_types::Coord<f64>` |
| `c-proj-compat` | no | Optional reference-compatibility integration against bundled C PROJ |

## Testing

```sh
cargo test --workspace
cargo test -p proj-core --no-default-features  # core crate without rayon/geo-types
cargo test -p proj-core --features c-proj-compat
./scripts/run-reference-parity.sh
./scripts/check-registry-generation.sh
./scripts/verify-release-packaging.sh --offline
cargo clippy --workspace --all-targets -- -D warnings
./scripts/run-reference-benchmarks.sh
```

The embedded EPSG registry is generated from the pinned bundled PROJ `proj.db` with:

```sh
cargo run --manifest-path gen-reference/Cargo.toml --bin gen-registry
```

That command writes `proj-core/data/epsg.bin` and deterministic provenance in `proj-core/data/epsg.provenance.json`, also exposed at runtime through `proj_core::registry::embedded_registry_provenance_json()`. CI runs `./scripts/check-registry-generation.sh` to rebuild in memory and fail if either artifact no longer matches the pinned PROJ database.

Prefer `convert_batch()` for small and medium batch sizes.
`convert_batch_parallel()` uses Rayon for larger batches and falls back to the sequential path when that is likely to be faster.

For reference comparisons and current benchmark results against bundled C PROJ,
see [docs/benchmark-report.md](docs/benchmark-report.md).

## Publishing

Release this workspace as a `0.x` line with scoped claims: production-ready for the supported projection families and CRS formats above, but not a claim of full PROJ parity.

Publish order matters because `proj-wkt` depends on `proj-core` as a separately published crate:

```sh
./scripts/verify-release-packaging.sh
cargo publish -p proj-core

# wait for crates.io to index the new proj-core version

cargo package -p proj-wkt --allow-dirty
cargo publish -p proj-wkt
```

`proj-core` packages and verifies independently. `proj-wkt` will not package for upload until the matching `proj-core` version is available in the crates.io index.

## License

MIT OR Apache-2.0

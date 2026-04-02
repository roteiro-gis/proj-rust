# proj-rust

Pure-Rust coordinate transformation library. No C libraries, no build scripts, no unsafe code.

This workspace currently contains:

- `proj-core`: transform engine, CRS registry, projection math, and datum shifts
- `proj-wkt`: parsing and compatibility helpers for EPSG codes, WKT, PROJ strings, and PROJJSON

## Release Scope

`proj-rust` is intended for production use within its supported CRS and projection set. It is not a full implementation of all PROJ capabilities.

Current non-goals for the `0.2` release line include:

- grid-shift based datum transforms
- vertical or time-dependent CRS operations
- full PROJ pipeline semantics and operation selection by area of use
- complete axis-order coverage and full arbitrary angular/unit-model coverage across all CRS definitions

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

// 3D transforms preserve the third ordinate unchanged
let (x, y, h) = t.convert_3d((-74.006, 40.7128, 15.0)).unwrap();
assert_eq!(h, 15.0);

// Reproject an extent by densifying its perimeter
let bounds = Bounds::new(-74.3, 40.45, -73.65, 40.95);
let projected_bounds = t.transform_bounds(bounds, 8).unwrap();
assert!(projected_bounds.max_x > projected_bounds.min_x);
```

Coordinates use the CRS's native units: degrees for geographic CRS, and the CRS's declared linear unit for projected CRS (for example meters or US survey feet).
For `convert_3d()`, the `z` component is preserved unchanged because the current CRS model is horizontal-only.

## Supported Input Formats

With `proj-core`, transforms can be created from registry-backed EPSG codes such as `"EPSG:4326"` and `"EPSG:3857"`.

With `proj-wkt`, the following CRS definition formats are supported:

- EPSG authority codes and bare EPSG numbers
- OGC `CRS:84` aliases and EPSG URNs
- common PROJ strings for the implemented projection families, including legacy `+init=epsg:XXXX`
- WKT1 and the supported WKT2 projected/geographic CRS forms, including top-level EPSG `ID[...]`
- basic PROJJSON geographic and projected CRS definitions for the implemented methods

Custom WKT, PROJJSON, and PROJ string definitions are only accepted when they map cleanly onto this workspace's native CRS model:
2D longitude/latitude geographic coordinates in degrees with a Greenwich prime meridian, and projected coordinates in native linear units with easting/northing axis order.
Definitions that require unsupported axis-order, prime-meridian, or geographic angular-unit semantics are rejected instead of being silently degraded.

## Supported CRS

| Projection | Status | EPSG |
|---|---|---|
| Geographic (WGS84, NAD83, NAD27, ETRS89, etc.) | Implemented | 4326, 4269, 4267, 4258, ... |
| Web Mercator | Implemented | 3857 |
| Transverse Mercator / UTM | Implemented | 32601-32660, 32701-32760 |
| Polar Stereographic | Implemented | 3413, 3031, 3995, 32661, 32761 |
| Lambert Conformal Conic | Implemented | 2154, 3347 |
| Albers Equal Area | Implemented | 5070, 3005 |
| Mercator | Implemented | 3395 |
| Equidistant Cylindrical | Implemented | 32662 |

Custom CRS definitions can be constructed and passed to `Transform::from_crs_defs()`. The companion `proj-wkt` crate parses EPSG codes, a subset of WKT/PROJ strings, and basic PROJJSON inputs into `CrsDef` values.

## Compatibility Surface

`proj-wkt` also exposes a lightweight `Proj` compatibility facade for downstream code that currently uses the common `new_known_crs` / `new` / `create_crs_to_crs_from_pj` / `convert` flow. It is intentionally narrow and only covers the supported CRS semantics in this workspace.

`proj-core::Transform` and `proj-wkt::Proj` also expose inverse-transform construction and sampled bounds reprojection via `inverse()` and `transform_bounds()`.

## Feature Flags

| Flag | Default | Description |
|---|---|---|
| `rayon` | yes | Parallel batch transforms via `convert_batch_parallel()` |
| `geo-types` | yes | `From`/`Into` conversions for `geo_types::Coord<f64>` |
| `c-proj-compat` | no | Optional reference-compatibility integration against bundled C PROJ |

## Testing

```sh
cargo test                        # all tests
cargo test -p proj-core --no-default-features  # core crate without rayon/geo-types
./scripts/run-reference-parity.sh
./scripts/verify-release-packaging.sh
cargo clippy --all-targets -- -D warnings
```

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

# proj-rust

Pure-Rust coordinate transformation library. No C libraries, no build scripts, no unsafe code.

This workspace currently contains:

- `proj-core`: transform engine, CRS registry, projection math, and datum shifts
- `proj-wkt`: parsing and compatibility helpers for EPSG codes, WKT, PROJ strings, and PROJJSON

## Release Scope

`proj-rust` is intended for production use within its supported CRS and projection set. It is not a full implementation of all PROJ capabilities.

Current non-goals for `v0.1.0` include:

- grid-shift based datum transforms
- vertical or time-dependent CRS operations
- full PROJ pipeline semantics and operation selection by area of use
- complete axis-order and unit-model coverage across arbitrary CRS definitions

## Usage

```rust
use proj_core::Transform;

// WGS84 geographic (degrees) -> Web Mercator (meters)
let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
let (x, y) = t.convert((-74.006, 40.7128)).unwrap();

// Inverse: Web Mercator -> WGS84
let inv = Transform::new("EPSG:3857", "EPSG:4326").unwrap();
let (lon, lat) = inv.convert((x, y)).unwrap();

// Works with geo_types::Coord<f64> (with `geo-types` feature)
let coord = geo_types::Coord { x: -74.006, y: 40.7128 };
let projected: geo_types::Coord<f64> = t.convert(coord).unwrap();

// Batch transforms (parallel with `rayon` feature)
let coords: Vec<(f64, f64)> = vec![(-74.006, 40.7128); 1000];
let results = t.convert_batch_parallel(&coords).unwrap();
```

Coordinates use the CRS's native units: degrees for geographic CRS, meters for projected CRS.

## Supported Input Formats

With `proj-core`, transforms can be created from registry-backed EPSG codes such as `"EPSG:4326"` and `"EPSG:3857"`.

With `proj-wkt`, the following CRS definition formats are supported:

- EPSG authority codes and bare EPSG numbers
- OGC `CRS:84` aliases and EPSG URNs
- common PROJ strings for the implemented projection families
- WKT1 and the supported WKT2 projected/geographic CRS forms
- basic PROJJSON geographic and projected CRS definitions for the implemented methods

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
cargo clippy --all-targets -- -D warnings
cargo package -p proj-core --allow-dirty
```

For reference comparisons and current benchmark results against bundled C PROJ,
see [docs/benchmark-report.md](docs/benchmark-report.md).

## Publishing

Release this workspace as a `0.x` line with scoped claims: production-ready for the supported projection families and CRS formats above, but not a claim of full PROJ parity.

Publish order matters because `proj-wkt` depends on `proj-core` as a separately published crate:

```sh
cargo package -p proj-core --allow-dirty
cargo publish -p proj-core

# wait for crates.io to index the new proj-core version

cargo package -p proj-wkt --allow-dirty
cargo publish -p proj-wkt
```

`proj-core` packages and verifies independently. `proj-wkt` will not package for upload until the matching `proj-core` version is available in the crates.io index.

## License

MIT OR Apache-2.0

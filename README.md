# proj-rust

[![proj-core crates.io](https://img.shields.io/crates/v/proj-core.svg)](https://crates.io/crates/proj-core)
[![proj-core docs.rs](https://docs.rs/proj-core/badge.svg)](https://docs.rs/proj-core)
[![proj-wkt crates.io](https://img.shields.io/crates/v/proj-wkt.svg)](https://crates.io/crates/proj-wkt)
[![proj-wkt docs.rs](https://docs.rs/proj-wkt/badge.svg)](https://docs.rs/proj-wkt)

Pure-Rust coordinate transformation library. The default library surface has no
C libraries, no build scripts, and no unsafe code.

## Crates

- `proj-core`: CRS definitions, operation selection, projection math, datum shifts, grid sampling, and transforms.
- `proj-wkt`: parsing for EPSG codes, WKT, PROJ strings, PROJJSON, and a small `Proj` compatibility facade.

## Usage

```rust
use proj_core::{Bounds, Transform};

let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
let (x, y) = t.convert((-74.006, 40.7128)).unwrap();

let inv = t.inverse().unwrap();
let (lon, lat) = inv.convert((x, y)).unwrap();

let coords = vec![(-74.006, 40.7128); 1000];
let projected = t.convert_batch(&coords).unwrap();

let (x3, y3, z3) = t.convert_3d((-74.006, 40.7128, 15.0)).unwrap();

let bounds = Bounds::new(-74.3, 40.45, -73.65, 40.95);
let projected_bounds = t.transform_bounds(bounds, 8).unwrap();
```

Coordinates use CRS-native units:

- Geographic CRS coordinates are longitude/latitude in degrees.
- Projected CRS coordinates use the CRS linear unit, such as metres or US survey feet.
- `convert_3d()` preserves `z` when no explicit vertical CRS is present, preserves it for matching vertical components, and converts it when matching vertical reference frames use different linear units.

With the default `geo-types` feature, `Transform` also supports `geo_types::Coord<f64>` and full 2D geometry conversion through `convert_geometry()`.

## CRS Input

`proj-core` accepts registry-backed EPSG codes such as `"EPSG:4326"` and `"EPSG:3857"`.

`proj-wkt` accepts:

- EPSG authority codes, bare EPSG numbers, EPSG URNs, and OGC `CRS:84`.
- Common PROJ strings for the implemented projection families, including legacy `+init=epsg:XXXX`.
- WKT1 and supported WKT2 geographic/projected CRS definitions.
- WKT2 compound CRS definitions with explicit vertical components.
- Basic PROJJSON geographic, projected, and compound CRS definitions.

Custom definitions are accepted only when they map to this library's CRS model: longitude/east, latitude/north geographic axes in degrees with a Greenwich prime meridian, and projected easting/northing axes in a single linear unit. Unsupported axis order, prime meridian, angular unit, projection, or vertical transformation semantics return errors.

## Supported CRS

| CRS or projection | EPSG examples |
|---|---|
| Geographic CRS and datum identity | EPSG:4326, EPSG:4269, EPSG:4267, EPSG:4258 |
| 3D geographic and compatible compound CRS | EPSG:4979 |
| Generated vertical CRS metadata and same-reference unit conversion | EPSG:3855, EPSG:5702, EPSG:5703, EPSG:5773, EPSG:6360, EPSG:5709, and other supported EPSG vertical CRS records |
| Grid-based 3D compound (with `geotiff`) | EPSG:7415 (RD New + NAP, RDNAPTRANS2018) |
| Web Mercator | EPSG:3857 |
| Transverse Mercator / UTM | EPSG:32601-32660, EPSG:32701-32760 |
| Polar Stereographic | EPSG:3413, EPSG:3031, EPSG:3995, EPSG:32661, EPSG:32761 |
| Lambert Conformal Conic | EPSG:2154, EPSG:3347 |
| Albers Equal Area | EPSG:5070, EPSG:3005 |
| Lambert Azimuthal Equal Area | EPSG:3035, EPSG:3408, EPSG:6931, EPSG:9311 |
| Oblique Stereographic | EPSG:28992, EPSG:2953 |
| Hotine Oblique Mercator / RSO | EPSG:2056, EPSG:3078, EPSG:3375 |
| Cassini-Soldner | EPSG:30200, EPSG:3377 |
| Mercator | EPSG:3395 |
| Equidistant Cylindrical | EPSG:32662 |

`Transform::new()` and `Transform::from_crs_defs()` select the best supported operation for a CRS pair. Use `Transform::with_selection_options()` or `Transform::from_crs_defs_with_selection_options()` to set an area of interest, require grid-backed operations, require exact area matches, provide a `GridProvider`, or select an explicit operation.

Approximate Helmert datum-shift fallbacks are opt-in through `SelectionOptions::allow_approximate_helmert_fallback()`.

## Grids

Horizontal NTv2 grid shifts are supported through embedded registry operations, parsed PROJ `+nadgrids` definitions, `EmbeddedGridProvider`, `FilesystemGridProvider`, and custom `GridProvider` implementations.

Vertical GTX geoid operations are supported for registry-backed ellipsoidal-to-gravity height pairs and explicit `VerticalGridOperation` values. Grid files are resolved through a caller-provided `FilesystemGridProvider` or custom `GridProvider`; geoid grid files are not bundled.

With the `geotiff` feature, PROJ-format GeoTIFF/COG grids (`.tif`, as distributed on the PROJ CDN) are decoded for both horizontal (NTv2-equivalent latitude/longitude offsets, including nested subgrids) and vertical (geoid undulation) shifts. This enables grid-based transforms such as **RDNAPTRANS™2018** — ETRS89/WGS 84 3D (`EPSG:4979`) to RD New + NAP height (`EPSG:7415`) — matching PROJ to sub-millimetre. Supply the `nl_nsgi_rdtrans2018.tif` and `nl_nsgi_nlgeo2018.tif` grids through a `FilesystemGridProvider`:

```rust
use std::sync::Arc;
use proj_core::{FilesystemGridProvider, SelectionOptions, Transform};

let provider = Arc::new(FilesystemGridProvider::new(vec!["/path/to/grids".into()]));
let options = SelectionOptions::new().with_grid_provider(provider);
let t = Transform::with_selection_options("EPSG:4979", "EPSG:7415", options).unwrap();
let (rd_x, rd_y, nap) = t.convert_3d((6.605585, 53.294378, 50.0)).unwrap();
```

Grid diagnostics expose selected operation metadata and resolved grid SHA-256 checksums.

## Feature Flags

| Flag | Default | Description |
|---|---|---|
| `rayon` | yes | Parallel batch transforms via `convert_batch_parallel()` |
| `geo-types` | yes | `geo_types::Coord<f64>` conversions and geometry transforms |
| `geotiff` | no | Decode PROJ-format GeoTIFF/COG datum-shift and geoid grids (e.g. RDNAPTRANS2018) via the pure-Rust `geotiff-reader` crate |
| `c-proj-compat` | no | Reference-compatibility tests against bundled C PROJ |

## Development

```sh
cargo test --workspace
cargo test -p proj-core --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
```

The optional `c-proj-compat` feature and `gen-reference` tool intentionally use
bundled C PROJ/sqlite for reference comparisons and registry generation; they
are not part of the default library surface.

The embedded EPSG registry is generated from the bundled PROJ `proj.db`:

```sh
cargo run --manifest-path gen-reference/Cargo.toml --bin gen-registry
./scripts/check-registry-generation.sh
```

Reference comparisons and benchmark results are in [docs/benchmark-report.md](docs/benchmark-report.md).

## License

MIT OR Apache-2.0

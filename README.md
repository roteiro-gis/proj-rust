# proj-rust

Pure-Rust coordinate transformation library. No C libraries, no build scripts, no unsafe code.

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

## Feature Flags

| Flag | Default | Description |
|---|---|---|
| `rayon` | yes | Parallel batch transforms via `convert_batch_parallel()` |
| `geo-types` | yes | `From`/`Into` conversions for `geo_types::Coord<f64>` |

## Testing

```sh
cargo test                        # all tests
cargo test -p proj-core --no-default-features  # core crate without rayon/geo-types
cargo clippy --all-targets -- -D warnings
```

## License

MIT OR Apache-2.0

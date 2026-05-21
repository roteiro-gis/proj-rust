# Benchmark Report

Date: 2026-05-05

This historical report summarizes the parity and benchmark suite for
`proj-rust` against bundled C PROJ at an earlier `0.6.0` release-candidate
state. It has not been refreshed for the final `0.6.0` release candidate.

## System Under Test

- Machine: Apple M1
- CPU topology: 8 logical CPUs
- Memory: 16 GiB
- OS: macOS 13.0
- Architecture: `arm64`
- Rust toolchain: `rustc 1.92.0`

These measurements reflect this machine and should not be read as universal
throughput claims.

## Scope

- Live parity against bundled C PROJ using the checked-in 161-value reference corpus
- Transform-construction timing for:
  - `EPSG:4326 -> 3857`
  - `EPSG:4267 -> 4326`
- Single-point comparisons for:
  - `EPSG:4326 -> 3857`
  - `EPSG:4326 -> 32618`
  - `EPSG:4326 -> 3413`
  - `EPSG:4267 -> 4326`
- Single-point 3D comparisons for:
  - `EPSG:4326 -> 3857`
  - `EPSG:4267 -> 4326`
- Batch comparison for 10,000 points in `EPSG:4326 -> 3857`
- Batch 3D timing for 10,000 points in `EPSG:4326 -> 3857`

## Methodology

Commands used for this report:

```sh
cargo test -p proj-core --features c-proj-compat
./scripts/run-reference-benchmarks.sh
```

Notes:

- The parity run passed both live C PROJ tests.
- The 3D parity run passed the live C PROJ 3D cases.
- The parity corpus currently contains 161 reference values.
- Criterion is used for all timing.
- Rust-versus-C rows are evaluated by same-run relative ratio only. Historical
  Criterion baseline deltas and absolute wall-clock changes are too noisy for
  release decisions on this host.
- Absolute estimates are retained only to make the ratio calculation auditable.
- The batch benchmark reports element throughput for 10,000 coordinate pairs.
- The benchmarked 3D cases preserve height. Vertical unit conversion and
  grid-backed operations are exercised by targeted transform tests rather than
  this microbenchmark suite.

## Current Results

### Parity

- `live_c_proj_parity`: 2 tests passed
- `live_c_proj_parity_3d`: 1 test passed
- The 161-value corpus remained in sync with live bundled C PROJ
- `proj-core` matched live bundled C PROJ for all supported corpus cases
- `proj-core` matched live bundled C PROJ for all covered 3D cases

### Construction Diagnostics

These rows do not have a same-run C PROJ control, so they are recorded as
diagnostics rather than release performance gates.

| workload | proj-rust |
| --- | ---: |
| `construct 4326 -> 3857` | 0.83 us |
| `construct 4267 -> 4326` | 33.95 us |

### Single-Point Summary

| workload | proj-rust | C PROJ | same-run result |
| --- | ---: | ---: | --- |
| `4326 -> 3857` | 25.46 ns | 72.99 ns | `proj-rust` 2.87x faster |
| `4326 -> 32618` | 40.14 ns | 130.26 ns | `proj-rust` 3.25x faster |
| `4326 -> 3413` | 55.71 ns | 94.24 ns | `proj-rust` 1.69x faster |
| `4267 -> 4326` | 159.79 ns | 282.16 ns | `proj-rust` 1.77x faster |

### Single-Point 3D Summary

| workload | proj-rust | C PROJ | same-run result |
| --- | ---: | ---: | --- |
| `3D 4326 -> 3857` | 28.88 ns | 71.72 ns | `proj-rust` 2.48x faster |
| `3D 4267 -> 4326` | 162.56 ns | 266.63 ns | `proj-rust` 1.64x faster |

### Batch Summary

| workload | proj-rust | C PROJ | same-run result |
| --- | ---: | ---: | --- |
| `10K 4326 -> 3857` sequential | 391.65 us | 771.63 us | `proj-rust` 1.97x faster |
| `10K 4326 -> 3857` throughput | 25.5 Melem/s | 13.0 Melem/s | `proj-rust` 1.97x higher throughput |
| `10K 4326 -> 3857` parallel | 409.49 us | 771.63 us | `proj-rust` 1.88x faster |
| `10K 4326 -> 3857` parallel throughput | 24.4 Melem/s | 13.0 Melem/s | `proj-rust` 1.88x higher throughput |

### Batch 3D Diagnostics

These rows are Rust-only diagnostics for the height-preserving 3D path.

| workload | proj-rust | result |
| --- | ---: | --- |
| `10K 3D 4326 -> 3857` sequential | 309.51 us | 32.3 Melem/s |
| `10K 3D 4326 -> 3857` parallel | 302.91 us | 33.0 Melem/s |

## Interpretation

- `proj-rust` is faster than bundled C PROJ in every same-run Rust-versus-C case in this suite.
- The strongest same-run wins are Web Mercator, UTM, and 10K Web Mercator batch throughput.
- Construction timing is useful for spotting large local changes, but it is not treated as a release gate without a same-run control.
- On this host and at 10K elements, the adaptive parallel path remains effectively flat with the sequential path for the covered 2D workload, which is the intended crossover behavior.
- The height-preserving 3D benchmark path stays close to the 2D fast path after avoiding diagnostics work on non-diagnostic conversions.
- The live parity suite remains the strongest correctness signal because it checks both corpus drift and current Rust-versus-C behavior.

## Limits

- This report reflects one machine and should be interpreted by same-run relative
  comparisons, not absolute timing movement.
- The benchmark suite is representative, not exhaustive across the full CRS registry.
- The batch comparison uses one 10K Web Mercator workload; different sizes or
  thread topologies may shift the parallel-versus-sequential crossover point.

# Benchmark Report

Date: 2026-04-16

This report summarizes the current parity and benchmark suite for `proj-rust`
against bundled C PROJ. It captures both the current Rust-versus-C performance
shape and the current transform-construction cost for the `0.3.0` release
state.

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

- Live parity against bundled C PROJ using the checked-in 135-point reference corpus
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
- The parity corpus currently contains 135 reference points.
- Criterion is used for all timing.
- The batch benchmark reports element throughput for 10,000 coordinate pairs.
- The current 3D API preserves the third ordinate unchanged because the CRS model remains horizontal-only.

## Current Results

### Parity

- `live_c_proj_parity`: 2 tests passed
- `live_c_proj_parity_3d`: 1 test passed
- The 135-point corpus remained in sync with live bundled C PROJ
- `proj-core` matched live bundled C PROJ for all supported corpus cases
- `proj-core` matched live bundled C PROJ for all covered 3D cases

### Construction Summary

| workload | proj-rust |
| --- | ---: |
| `construct 4326 -> 3857` | 687.42 ns |
| `construct 4267 -> 4326` | 31.24 us |

### Single-Point Summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `4326 -> 3857` | 27.25 ns | 72.77 ns | `proj-rust` 2.67x faster |
| `4326 -> 32618` | 41.18 ns | 130.88 ns | `proj-rust` 3.18x faster |
| `4326 -> 3413` | 59.30 ns | 92.52 ns | `proj-rust` 1.56x faster |
| `4267 -> 4326` | 160.37 ns | 276.96 ns | `proj-rust` 1.73x faster |

### Single-Point 3D Summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `3D 4326 -> 3857` | 25.70 ns | 82.27 ns | `proj-rust` 3.20x faster |
| `3D 4267 -> 4326` | 153.14 ns | 281.22 ns | `proj-rust` 1.84x faster |

### Batch Summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `10K 4326 -> 3857` sequential | 293.75 us | 778.47 us | `proj-rust` 2.65x faster |
| `10K 4326 -> 3857` throughput | 34.0 Melem/s | 12.8 Melem/s | `proj-rust` 2.65x higher throughput |
| `10K 4326 -> 3857` parallel | 292.19 us | 778.47 us | `proj-rust` 2.66x faster |
| `10K 4326 -> 3857` parallel throughput | 34.2 Melem/s | 12.8 Melem/s | `proj-rust` 2.66x higher throughput |

### Batch 3D Summary

| workload | proj-rust | result |
| --- | ---: | --- |
| `10K 3D 4326 -> 3857` sequential | 258.73 us | 38.6 Melem/s |
| `10K 3D 4326 -> 3857` parallel | 259.91 us | 38.5 Melem/s |

## Interpretation

- `proj-rust` remains ahead of bundled C PROJ in every measured Rust-versus-C case in this suite.
- Construction is now sub-microsecond for simple registry-backed projected transforms and roughly 31 microseconds for the covered datum-shifted pair.
- Simple projected single-point transforms still show the largest relative wins.
- On this host and at 10K elements, the adaptive parallel path is essentially flat with the sequential path for the covered workloads, which is the intended crossover behavior.
- The current 3D path stays close to the 2D fast path because the third ordinate is preserved unchanged.
- The live parity suite remains the strongest correctness signal because it checks both corpus drift and current Rust-versus-C behavior.

## Limits

- This report reflects one machine.
- The benchmark suite is representative, not exhaustive across the full CRS registry.
- The batch comparison uses one 10K Web Mercator workload; different sizes or
  thread topologies may shift the parallel-versus-sequential crossover point.

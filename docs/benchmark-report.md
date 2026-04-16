# Benchmark Report

Date: 2026-04-14

This report summarizes the current parity and benchmark suite for `proj-rust`
against bundled C PROJ. It captures both the current Rust-versus-C performance
shape and the new transform-construction cost after the indexed operation
selection work in the embedded registry.

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
./scripts/run-reference-parity.sh
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
| `construct 4326 -> 3857` | 1.51 us |
| `construct 4267 -> 4326` | 77.48 us |

### Single-Point Summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `4326 -> 3857` | 25.82 ns | 77.11 ns | `proj-rust` 2.99x faster |
| `4326 -> 32618` | 42.72 ns | 126.13 ns | `proj-rust` 2.95x faster |
| `4326 -> 3413` | 57.19 ns | 90.64 ns | `proj-rust` 1.58x faster |
| `4267 -> 4326` | 162.23 ns | 265.20 ns | `proj-rust` 1.63x faster |

### Single-Point 3D Summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `3D 4326 -> 3857` | 25.26 ns | 73.08 ns | `proj-rust` 2.89x faster |
| `3D 4267 -> 4326` | 149.08 ns | 270.40 ns | `proj-rust` 1.81x faster |

### Batch Summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `10K 4326 -> 3857` sequential | 285.69 us | 778.53 us | `proj-rust` 2.73x faster |
| `10K 4326 -> 3857` throughput | 35.0 Melem/s | 12.8 Melem/s | `proj-rust` 2.73x higher throughput |
| `10K 4326 -> 3857` parallel | 294.68 us | 778.53 us | `proj-rust` 2.64x faster |
| `10K 4326 -> 3857` parallel throughput | 33.9 Melem/s | 12.8 Melem/s | `proj-rust` 2.64x higher throughput |

### Batch 3D Summary

| workload | proj-rust | result |
| --- | ---: | --- |
| `10K 3D 4326 -> 3857` sequential | 316.96 us | 31.5 Melem/s |
| `10K 3D 4326 -> 3857` parallel | 268.78 us | 37.2 Melem/s |

## Interpretation

- `proj-rust` remains ahead of bundled C PROJ in every measured Rust-versus-C case in this suite.
- The indexed operation-selection refactor keeps construction costs low for simple transforms while preserving acceptable construction costs for datum-shifted pairs.
- Simple projected single-point transforms still show the largest relative wins.
- On this host and at 10K elements, the adaptive parallel path is now competitive with and slightly faster than the sequential path for the covered workloads.
- The current 3D path stays close to the 2D fast path because the third ordinate is preserved unchanged.
- The live parity suite remains the strongest correctness signal because it checks both corpus drift and current Rust-versus-C behavior.

## Limits

- This report reflects one machine.
- The benchmark suite is representative, not exhaustive across the full CRS registry.
- The batch comparison uses one 10K Web Mercator workload; different sizes or
  thread topologies may shift the parallel-versus-sequential crossover point.

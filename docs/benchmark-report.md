# Benchmark Report

Date: 2026-03-21

This document summarizes the current parity and comparison benchmark suite for
`proj-rust` against bundled C PROJ. The goal is to capture the current
performance shape for representative single-point transforms, a batch workload,
and the live parity status against the reference implementation.

## System Under Test

- Machine: same local Apple M1 host used for the current `netcdf-rust` report
- CPU topology: 8 logical CPUs
- Memory: 16 GiB
- OS: macOS 13.0
- Architecture: `arm64`
- Rust toolchain: `rustc 1.92.0`

These numbers are local measurements for this machine, not universal
throughput claims.

## Scope

- Live parity test against bundled C PROJ using the checked-in 135-point
  reference corpus
- Single-point comparisons for:
  - `EPSG:4326 -> 3857`
  - `EPSG:4326 -> 32618`
  - `EPSG:4326 -> 3413`
  - `EPSG:4267 -> 4326`
- Batch comparison for 10,000 points in `EPSG:4326 -> 3857`

## Methodology

Commands used for this report:

```sh
./scripts/run-reference-parity.sh

cargo bench -p proj-core --features c-proj-compat \
  --bench transform_compare_bench -- --noplot
```

Notes:

- The parity run passed both live C PROJ tests.
- The parity corpus currently contains 135 reference points.
- Criterion is used for the comparison benchmarks.
- The batch benchmark reports element throughput for 10,000 coordinate pairs.

## Current Results

### Parity

- `live_c_proj_parity`: 2 tests passed
- The 135-point corpus remained in sync with live bundled C PROJ
- `proj-core` matched live bundled C PROJ for all supported corpus cases

### Single-point summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `4326 -> 3857` | 19.2 ns | 71.5 ns | `proj-rust` 3.72x faster |
| `4326 -> 32618` | 35.3 ns | 127.8 ns | `proj-rust` 3.62x faster |
| `4326 -> 3413` | 49.3 ns | 99.2 ns | `proj-rust` 2.01x faster |
| `4267 -> 4326` | 169.2 ns | 296.5 ns | `proj-rust` 1.75x faster |

### Batch summary

| workload | proj-rust | C PROJ | result |
| --- | ---: | ---: | --- |
| `10K 4326 -> 3857` sequential | 250.9 us | 875.2 us | `proj-rust` 3.49x faster |
| `10K 4326 -> 3857` throughput | 39.9 Melem/s | 11.4 Melem/s | `proj-rust` 3.49x higher throughput |
| `10K 4326 -> 3857` parallel | 587.5 us | 875.2 us | `proj-rust` 1.49x faster |
| `10K 4326 -> 3857` parallel throughput | 17.0 Melem/s | 11.4 Melem/s | `proj-rust` 1.49x higher throughput |

## Interpretation

- `proj-rust` is ahead of bundled C PROJ on every measured case in this suite.
- The largest wins are the simple projected single-point transforms and the
  sequential 10K Web Mercator batch.
- On this host and at this batch size, `convert_batch_parallel()` is slower
  than `convert_batch()` due to parallel overhead, even though it still remains
  ahead of the C PROJ baseline.
- The live parity suite gives a tighter signal than the frozen JSON alone,
  because it verifies both corpus drift and current Rust-vs-C behavior in the
  same run.

## Limits

- This report reflects one machine.
- The benchmark suite is representative, not exhaustive across the full CRS
  registry.
- The batch comparison uses one 10K Web Mercator workload; different sizes or
  thread topologies may shift the parallel/sequential crossover point.

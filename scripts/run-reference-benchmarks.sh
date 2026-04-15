#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo bench -p proj-core --bench transform_bench -- --noplot
cargo bench -p proj-core --features c-proj-compat --bench transform_compare_bench -- --noplot

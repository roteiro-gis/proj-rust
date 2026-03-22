#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo test -p proj-core --features c-proj-compat --test live_c_proj_parity

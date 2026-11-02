#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

if ! pkg-config --atleast-version=9.8.0 proj; then
  echo "Live coordinate parity requires PROJ 9.8+; install it and set PKG_CONFIG_PATH to its lib/pkgconfig directory." >&2
  exit 1
fi

cargo test -p proj-core --features c-proj-compat \
  --test live_c_proj_helmert_parity \
  --test live_c_proj_parity \
  --test live_c_proj_parity_3d

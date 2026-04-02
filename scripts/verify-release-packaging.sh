#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/verify-release-packaging.sh [--offline]

Verifies the release packaging flow by:
1. packaging and verifying `proj-core`
2. staging the `proj-wkt` package file set in an isolated temporary workspace
3. testing staged `proj-wkt` against the packaged `proj-core` contents via a crates.io patch
EOF
}

cargo_flags=()
while (($#)); do
  case "$1" in
    --offline)
      cargo_flags+=(--offline)
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

cd "$(dirname "$0")/.."
workspace_root="$(pwd)"

workspace_field() {
  local key="$1"
  awk -F'"' -v key="$key" '$1 == key " = " { print $2; exit }' "$workspace_root/Cargo.toml"
}

copy_proj_wkt_package_files() {
  local stage_dir="$1"
  local rel src
  local package_files=()

  mapfile -t package_files < <(cargo package -p proj-wkt --list --allow-dirty "${cargo_flags[@]}")

  for rel in "${package_files[@]}"; do
    case "$rel" in
      .cargo_vcs_info.json|Cargo.toml.orig)
        continue
        ;;
    esac

    src="$workspace_root/proj-wkt/$rel"
    if [[ ! -e "$src" ]]; then
      src="$workspace_root/$rel"
    fi
    if [[ ! -e "$src" ]]; then
      echo "missing source file for packaged proj-wkt path: $rel" >&2
      exit 1
    fi

    mkdir -p "$stage_dir/proj-wkt/$(dirname "$rel")"
    cp "$src" "$stage_dir/proj-wkt/$rel"
  done
}

workspace_version="$(workspace_field version)"
workspace_edition="$(workspace_field edition)"
workspace_rust_version="$(workspace_field rust-version)"
workspace_license="$(workspace_field license)"
workspace_repository="$(workspace_field repository)"
workspace_homepage="$(workspace_field homepage)"

cargo package -p proj-core --allow-dirty "${cargo_flags[@]}"

packaged_proj_core_dir="$workspace_root/target/package/proj-core-$workspace_version"
if [[ ! -d "$packaged_proj_core_dir" ]]; then
  echo "missing packaged proj-core directory: $packaged_proj_core_dir" >&2
  exit 1
fi

stage_dir="$(mktemp -d "${TMPDIR:-/tmp}/proj-rust-release-check.XXXXXX")"
trap 'rm -rf "$stage_dir"' EXIT

copy_proj_wkt_package_files "$stage_dir"
cp "$workspace_root/Cargo.lock" "$stage_dir/Cargo.lock"
cp "$workspace_root/README.md" "$stage_dir/README.md"

cat > "$stage_dir/Cargo.toml" <<EOF
[workspace]
members = ["proj-wkt"]
resolver = "2"

[workspace.package]
version = "$workspace_version"
edition = "$workspace_edition"
rust-version = "$workspace_rust_version"
license = "$workspace_license"
repository = "$workspace_repository"
homepage = "$workspace_homepage"

[workspace.dependencies]
proj-core = { version = "$workspace_version" }

[patch.crates-io]
proj-core = { path = "$packaged_proj_core_dir" }
EOF

cargo test -p proj-wkt --manifest-path "$stage_dir/Cargo.toml" "${cargo_flags[@]}"

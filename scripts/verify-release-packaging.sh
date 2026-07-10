#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/verify-release-packaging.sh [--offline]

Verifies the release packaging flow by:
1. packaging and verifying `proj-epsg-format`
2. packaging `proj-core` (verified via step 3, since its unpublished
   `proj-epsg-format` dependency cannot resolve in cargo's isolated verify)
3. staging the `proj-wkt` package file set in an isolated temporary workspace
   and testing it against the packaged `proj-core` and `proj-epsg-format`
   contents via crates.io patches
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

copy_crate_package_files() {
  local crate="$1"
  local stage_dir="$2"
  local rel src
  local package_files=()

  mapfile -t package_files < <(cargo package -p "$crate" --list --allow-dirty "${cargo_flags[@]}")

  for rel in "${package_files[@]}"; do
    case "$rel" in
      .cargo_vcs_info.json|Cargo.toml.orig)
        continue
        ;;
    esac

    src="$workspace_root/$crate/$rel"
    if [[ ! -e "$src" ]]; then
      src="$workspace_root/$rel"
    fi
    if [[ ! -e "$src" ]]; then
      echo "missing source file for packaged $crate path: $rel" >&2
      exit 1
    fi

    mkdir -p "$stage_dir/$crate/$(dirname "$rel")"
    cp "$src" "$stage_dir/$crate/$rel"
  done
}

workspace_version="$(workspace_field version)"
workspace_edition="$(workspace_field edition)"
workspace_rust_version="$(workspace_field rust-version)"
workspace_license="$(workspace_field license)"
workspace_repository="$(workspace_field repository)"
workspace_homepage="$(workspace_field homepage)"

cargo package -p proj-epsg-format --allow-dirty "${cargo_flags[@]}"

packaged_proj_epsg_format_dir="$workspace_root/target/package/proj-epsg-format-$workspace_version"
if [[ ! -d "$packaged_proj_epsg_format_dir" ]]; then
  echo "missing packaged proj-epsg-format directory: $packaged_proj_epsg_format_dir" >&2
  exit 1
fi

stage_dir="$(mktemp -d "${TMPDIR:-/tmp}/proj-rust-release-check.XXXXXX")"
trap 'rm -rf "$stage_dir"' EXIT

# proj-core depends on the not-yet-published proj-epsg-format, so cargo's
# isolated package verification cannot resolve it; stage proj-core's package
# file set instead and let the staged proj-wkt test build it against the
# packaged proj-epsg-format, which verifies the same thing end to end.
copy_crate_package_files proj-core "$stage_dir"
copy_crate_package_files proj-wkt "$stage_dir"
cp "$workspace_root/Cargo.lock" "$stage_dir/Cargo.lock"
cp "$workspace_root/README.md" "$stage_dir/README.md"

cat > "$stage_dir/Cargo.toml" <<EOF
[workspace]
members = ["proj-core", "proj-wkt"]
resolver = "2"

[workspace.package]
version = "$workspace_version"
edition = "$workspace_edition"
rust-version = "$workspace_rust_version"
license = "$workspace_license"
repository = "$workspace_repository"
homepage = "$workspace_homepage"

[workspace.dependencies]
proj-core = { version = "$workspace_version", path = "proj-core" }
proj-epsg-format = { version = "$workspace_version" }

[patch.crates-io]
proj-epsg-format = { path = "$packaged_proj_epsg_format_dir" }
EOF

cargo test -p proj-wkt --manifest-path "$stage_dir/Cargo.toml" "${cargo_flags[@]}"

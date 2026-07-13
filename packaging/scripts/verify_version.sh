#!/bin/sh
set -eu

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
cd "$repo_root"

fail() {
  printf 'version check failed: %s\n' "$1" >&2
  exit 1
}

workspace_version="$({
  awk '
    /^\[workspace\.package\]$/ { in_workspace_package = 1; next }
    in_workspace_package && /^\[/ { exit }
    in_workspace_package && /^version[[:space:]]*=/ {
      value = $0
      sub(/^[^=]*=[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*$/, "", value)
      print value
      found = 1
      exit
    }
    END { if (!found) exit 1 }
  ' Cargo.toml
} 2>/dev/null)" || fail "could not read [workspace.package].version from Cargo.toml"

[ -n "$workspace_version" ] || fail "Cargo.toml workspace version is empty"

check_version() {
  label="$1"
  actual="$2"

  [ -n "$actual" ] || fail "could not read version from $label"
  if [ "$actual" != "$workspace_version" ]; then
    fail "$label has version $actual; expected $workspace_version"
  fi
}

for manifest in \
  packaging/Packager.linux.toml \
  packaging/Packager.windows.toml \
  packaging/Packager.macos.toml \
  packaging/Packager.macos.dev.toml
do
  manifest_version="$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' "$manifest")"
  check_version "$manifest" "$manifest_version"
done

lock_package_version() {
  package_name="$1"
  awk -v package_name="$package_name" '
    $0 == "name = \"" package_name "\"" { in_package = 1; next }
    in_package && /^version = "/ {
      value = $0
      sub(/^version = "/, "", value)
      sub(/"$/, "", value)
      print value
      found = 1
      exit
    }
    END { if (!found) exit 1 }
  ' Cargo.lock
}

for package_name in gmpublished gmpublished-backend
do
  package_version="$(lock_package_version "$package_name")" || \
    fail "could not read $package_name version from Cargo.lock"
  check_version "Cargo.lock package $package_name" "$package_version"
done

cargo metadata --locked --no-deps --format-version 1 >/dev/null

if [ "$#" -gt 0 ]; then
  release_tag="$1"
  expected_tag="v$workspace_version"
  if [ "$release_tag" != "$expected_tag" ]; then
    fail "release tag is $release_tag; expected $expected_tag"
  fi
  printf 'release tag matches project version: %s\n' "$release_tag"
else
  printf 'version metadata is consistent: %s\n' "$workspace_version"
fi

#!/bin/sh
set -eu

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
build_profile="${GMPUBLISHED_PACKAGER_PROFILE:-release}"

case "$build_profile" in
  release)
    cargo_build_args="--release"
    profile_dir="release"
    ;;
  dev|debug)
    cargo_build_args=""
    profile_dir="debug"
    ;;
  *)
    cargo_build_args="--profile $build_profile"
    profile_dir="$build_profile"
    ;;
esac

target_binary="$repo_root/target/$profile_dir/gmpublished"

if [ "${GMPUBLISHED_SKIP_PREPARE_BUILD:-}" = "1" ]; then
  if [ ! -f "$target_binary" ]; then
    printf 'GMPUBLISHED_SKIP_PREPARE_BUILD=1 but missing staged %s binary: %s\n' "$build_profile" "$target_binary" >&2
    exit 1
  fi
else
  if [ -n "${GMPUBLISHED_PACKAGER_CARGO_FEATURES:-}" ]; then
    (cd "$repo_root" && cargo build $cargo_build_args --locked --package gmpublished --features "$GMPUBLISHED_PACKAGER_CARGO_FEATURES")
  else
    (cd "$repo_root" && cargo build $cargo_build_args --locked --package gmpublished)
  fi
fi

target_triple="${CARGO_BUILD_TARGET:-$(rustc -vV | awk '/^host:/ { print $2 }')}"
platform="$(uname -s)"

case "$platform" in
  Darwin)
    runtime_platform="macos"
    runtime_file="libsteam_api.dylib"
    generated_path="packaging/steam/generated/libsteam_api.dylib-${target_triple}"
    ;;
  Linux)
    runtime_platform="linux"
    runtime_file="libsteam_api.so"
    generated_path="packaging/steam/generated/libsteam_api.so-${target_triple}"
    ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    runtime_platform="windows"
    runtime_file="steam_api64.dll"
    generated_path="packaging/steam/generated/steam_api64.dll"
    ;;
  *)
    printf 'unsupported packaging platform: %s\n' "$platform" >&2
    exit 1
    ;;
esac

if [ -n "${GMPUBLISHED_STEAM_RUNTIME_DIR:-}" ]; then
  runtime_src="$GMPUBLISHED_STEAM_RUNTIME_DIR/$runtime_file"
else
  runtime_src="$repo_root/packaging/steam/redistributable/$runtime_platform/$runtime_file"
fi

if [ ! -f "$runtime_src" ]; then
  if [ -n "${GMPUBLISHED_STEAM_RUNTIME_DIR:-}" ]; then
    printf 'missing Steam runtime library: %s\n' "$runtime_src" >&2
    printf 'check GMPUBLISHED_STEAM_RUNTIME_DIR or see packaging/README.md\n' >&2
    exit 1
  fi

  steamworks_sys_runtime=""
  for build_dir in "$repo_root/target/$target_triple/$profile_dir/build" "$repo_root/target/$profile_dir/build"; do
    if [ ! -d "$build_dir" ]; then
      continue
    fi
    steamworks_sys_runtime="$(find "$build_dir" -path "*/steamworks-sys-*/out/$runtime_file" -type f | sort | tail -n 1)"
    if [ -n "$steamworks_sys_runtime" ]; then
      break
    fi
  done
  if [ -n "$steamworks_sys_runtime" ]; then
    runtime_src="$steamworks_sys_runtime"
    printf 'using steamworks-sys runtime for local bundle: %s\n' "$runtime_src" >&2
  else
    printf 'missing Steam runtime library: %s\n' "$runtime_src" >&2
    printf 'provide GMPUBLISHED_STEAM_RUNTIME_DIR or see packaging/README.md\n' >&2
    exit 1
  fi
fi

mkdir -p "$repo_root/packaging/steam/generated"
mkdir -p "$repo_root/target/$profile_dir"
cp "$runtime_src" "$repo_root/$generated_path"
cp "$runtime_src" "$repo_root/target/$profile_dir/$runtime_file"
printf 'prepared %s\n' "$repo_root/$generated_path"

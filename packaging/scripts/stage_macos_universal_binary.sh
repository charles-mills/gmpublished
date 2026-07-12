#!/bin/sh
set -eu

platform="$(uname -s)"
if [ "$platform" != "Darwin" ]; then
    printf 'macOS universal staging requires Darwin, got %s\n' "$platform" >&2
    exit 1
fi

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
cd "$repo_root"

arm_target="aarch64-apple-darwin"
intel_target="x86_64-apple-darwin"
arm_binary="target/$arm_target/release/gmpublished"
intel_binary="target/$intel_target/release/gmpublished"
universal_binary="target/release/gmpublished"

cargo build --release --locked --package gmpublished --target "$arm_target"
cargo build --release --locked --package gmpublished --target "$intel_target"

if [ ! -f "$arm_binary" ]; then
    printf 'missing arm64 macOS build output: %s\n' "$arm_binary" >&2
    exit 1
fi

if [ ! -f "$intel_binary" ]; then
    printf 'missing x86_64 macOS build output: %s\n' "$intel_binary" >&2
    exit 1
fi

mkdir -p target/release
lipo -create -output "$universal_binary" "$arm_binary" "$intel_binary"
chmod 755 "$universal_binary"

lipo -info "$universal_binary"
lipo "$universal_binary" -verify_arch arm64 >/dev/null
lipo "$universal_binary" -verify_arch x86_64 >/dev/null

printf 'staged universal macOS executable: %s\n' "$universal_binary"

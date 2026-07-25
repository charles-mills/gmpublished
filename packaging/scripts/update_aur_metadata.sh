#!/bin/sh
set -eu

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
cd "$repo_root/packaging/arch"

fail() {
  printf 'aur metadata update failed: %s\n' "$1" >&2
  exit 1
}

expected_version="${1-}"

pkgver="$(awk -F= '/^pkgver=/ { print $2; exit }' PKGBUILD)"
[ -n "$pkgver" ] || fail 'could not read pkgver from PKGBUILD'

if [ -n "$expected_version" ] && [ "$pkgver" != "$expected_version" ]; then
  printf 'pkgver is %s, not %s; leaving metadata untouched\n' "$pkgver" "$expected_version"
  exit 0
fi

attempt=1
until updpkgsums; do
  [ "$attempt" -lt 3 ] || fail "updpkgsums failed after $attempt attempts"
  printf 'attempt %s failed, retrying in 20s\n' "$attempt" >&2
  attempt=$((attempt + 1))
  sleep 20
done

makepkg --printsrcinfo > .SRCINFO

printf 'refreshed checksums and .SRCINFO for %s\n' "$pkgver"

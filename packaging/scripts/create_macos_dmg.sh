#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    echo "macOS DMG creation requires Darwin" >&2
    exit 1
fi

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
artifact_version="$(awk -F '"' '/^version = / { print $2; exit }' "$repo_root/Cargo.toml")"

app="${1:-target/packager/gmpublished.app}"
output="${2:-artifacts/macos/gmpublished-${artifact_version}-universal-apple-darwin.dmg}"
volume_name="${3:-gmpublished}"
app_name="$(basename "$app")"

if [ ! -d "$app" ]; then
    echo "missing app bundle: $app" >&2
    exit 1
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/gmpublished-dmg.XXXXXX")"
tmp_dir="$(cd "$tmp_dir" && pwd -P)"
mount_dir="$tmp_dir/mount"
rw_dmg="$tmp_dir/gmpublished.rw.dmg"
attached_device=""

cleanup() {
    if [ -n "$attached_device" ]; then
        hdiutil detach "$attached_device" -quiet >/dev/null 2>&1 || true
    fi
    chmod -R u+w "$tmp_dir" >/dev/null 2>&1 || true
    rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

mkdir -p "$mount_dir"
mkdir -p "$(dirname "$output")"

size_kb="$(du -sk "$app" | awk '{ print $1 + 32768 }')"
hdiutil create \
    -quiet \
    -size "${size_kb}k" \
    -fs HFS+ \
    -volname "$volume_name" \
    -type UDIF \
    "$rw_dmg"

attached_device="$(
    hdiutil attach \
        -readwrite \
        -noverify \
        -noautoopen \
        -mountpoint "$mount_dir" \
        "$rw_dmg" |
        awk -v mount="$mount_dir" '$NF == mount { print $1; found = 1 } END { if (!found) exit 1 }'
)"

ditto "$app" "$mount_dir/$app_name"
ln -s /Applications "$mount_dir/Applications"

osascript <<OSA
tell application "Finder"
    set mountedFolder to POSIX file "$mount_dir" as alias
    open mountedFolder
    set containerWindow to container window of mountedFolder
    try
        tell containerWindow
            set current view to icon view
            set toolbar visible to false
            set statusbar visible to false
            set bounds to {100, 100, 760, 500}
        end tell
    end try

    try
        set iconOptions to the icon view options of containerWindow
        tell iconOptions
            set arrangement to not arranged
            set icon size to 128
            set text size to 14
        end tell
    end try

    try
        set position of item "$app_name" of mountedFolder to {180, 170}
        set position of item "Applications" of mountedFolder to {480, 170}
        update mountedFolder without registering applications
    end try
    delay 2
    try
        close containerWindow
    end try
end tell
OSA

sync
hdiutil detach "$attached_device" -quiet
attached_device=""

rm -f "$output"
hdiutil convert \
    "$rw_dmg" \
    -quiet \
    -format UDZO \
    -imagekey zlib-level=9 \
    -o "$output"
hdiutil verify "$output" -quiet

printf 'created %s\n' "$output"

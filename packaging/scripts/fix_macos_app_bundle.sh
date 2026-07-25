#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    exit 0
fi

app="${1:-target/packager/gmpublished.app}"
info_plist="$app/Contents/Info.plist"
resources_dir="$app/Contents/Resources"
repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
icon_source="$repo_root/packaging/macos/AppIcon.icon"
asset_catalog="$repo_root/packaging/macos/Assets.xcassets"
icon_tmp_dir=""

cleanup() {
    if [ -n "$icon_tmp_dir" ]; then
        rm -rf "$icon_tmp_dir"
    fi
}
trap cleanup EXIT INT TERM

if [ ! -d "$app" ]; then
    echo "missing app bundle: $app" >&2
    exit 1
fi

if [ ! -f "$info_plist" ]; then
    echo "missing Info.plist: $info_plist" >&2
    exit 1
fi

if [ ! -d "$icon_source" ]; then
    echo "missing Icon Composer source: $icon_source" >&2
    exit 1
fi

if [ ! -d "$asset_catalog" ]; then
    echo "missing asset catalog: $asset_catalog" >&2
    exit 1
fi

if ! xcrun --find actool >/dev/null 2>&1; then
    echo "compiling AppIcon.icon requires Xcode's actool" >&2
    exit 1
fi

icon_tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/gmpublished-icon.XXXXXX")"
icon_output="$icon_tmp_dir/output"
icon_partial_plist="$icon_tmp_dir/partial.plist"
mkdir -p "$icon_output" "$resources_dir"

# Compile the layered Icon Composer source for current macOS appearances and
# produce the flattened .icns fallback used by older supported releases.
xcrun actool \
    "$icon_source" \
    "$asset_catalog" \
    --compile "$icon_output" \
    --output-format human-readable-text \
    --notices \
    --warnings \
    --app-icon AppIcon \
    --include-all-app-icons \
    --compress-pngs \
    --development-region en \
    --target-device mac \
    --minimum-deployment-target 10.13 \
    --platform macosx \
    --output-partial-info-plist "$icon_partial_plist"

test -f "$icon_output/Assets.car"
test -f "$icon_output/AppIcon.icns"
plutil -lint "$icon_partial_plist" >/dev/null

cp "$icon_output/Assets.car" "$resources_dir/Assets.car"
cp "$icon_output/AppIcon.icns" "$resources_dir/AppIcon.icns"

# cargo-packager 0.11.8 injects this legacy Carbon marker into macOS .app
# bundles. Modern Cocoa/winit apps must not advertise themselves as
# Carbon apps, or LaunchServices can reject them on current macOS releases.
/usr/libexec/PlistBuddy -c "Delete :LSRequiresCarbon" "$info_plist" 2>/dev/null || true

if /usr/libexec/PlistBuddy -c "Print :LSMinimumSystemVersion" "$info_plist" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Set :LSMinimumSystemVersion 10.13" "$info_plist"
else
    /usr/libexec/PlistBuddy -c "Add :LSMinimumSystemVersion string 10.13" "$info_plist"
fi

if /usr/libexec/PlistBuddy -c "Print :CFBundleIconFile" "$info_plist" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleIconFile AppIcon" "$info_plist"
else
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$info_plist"
fi

if /usr/libexec/PlistBuddy -c "Print :CFBundleIconName" "$info_plist" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleIconName AppIcon" "$info_plist"
else
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconName string AppIcon" "$info_plist"
fi

plutil -lint "$info_plist" >/dev/null

# Re-seal the bundle after plist patching. Ad-hoc signing only; CI re-signs
# with a Developer ID and notarizes when signing secrets are configured.
codesign --force --deep --sign - "$app" >/dev/null
codesign --verify --deep --strict --verbose=2 "$app" >/dev/null

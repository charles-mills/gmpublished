#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    exit 0
fi

app="${1:-target/packager/gmpublished.app}"
info_plist="$app/Contents/Info.plist"

if [ ! -d "$app" ]; then
    echo "missing app bundle: $app" >&2
    exit 1
fi

if [ ! -f "$info_plist" ]; then
    echo "missing Info.plist: $info_plist" >&2
    exit 1
fi

# cargo-packager 0.11.8 injects this legacy Carbon marker into macOS .app
# bundles. Modern Cocoa/winit apps must not advertise themselves as
# Carbon apps, or LaunchServices can reject them on current macOS releases.
/usr/libexec/PlistBuddy -c "Delete :LSRequiresCarbon" "$info_plist" 2>/dev/null || true

if /usr/libexec/PlistBuddy -c "Print :LSMinimumSystemVersion" "$info_plist" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c "Set :LSMinimumSystemVersion 10.13" "$info_plist"
else
    /usr/libexec/PlistBuddy -c "Add :LSMinimumSystemVersion string 10.13" "$info_plist"
fi

plutil -lint "$info_plist" >/dev/null

# Re-seal the bundle after plist patching. Ad-hoc signing only; CI re-signs
# with a Developer ID and notarizes when signing secrets are configured.
codesign --force --deep --sign - "$app" >/dev/null
codesign --verify --deep --strict --verbose=2 "$app" >/dev/null

#!/bin/sh
# Build target/Clip.app: release binary, icon, Info.plist for an accessory app.
# Signed with the Developer ID when it's in the keychain (hardened runtime, so it can be
# notarized, and a stable identity, so macOS keeps Clip's permissions across builds),
# else ad hoc. CLIP_NOTARIZE=1 also notarizes and staples it (notarytool profile
# CLIP_NOTARY_PROFILE, default isocast-notary).
#
# The version is Cargo.toml's, or CLIP_VERSION (a dev build's 0.0.0-dev-<run>); the
# build number follows from it. CLIP_BUILD_CHANNEL (stable, dev; default local) is
# baked in for the updater. CLIP_RELEASE_UNSIGNED=1 signs ad hoc whatever the keychain
# holds: the Release workflow signs in its own job (.github/release/sign-macos.sh).
set -eu
cd "$(dirname "$0")/.."
version=${CLIP_VERSION:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)}
case "$version" in
  0.0.0-dev-[0-9]*) build=${version#0.0.0-dev-} ;;
  *) build=$(printf '%s\n' "${version%%-*}" | awk -F. '{ print ($1 * 10000) + ($2 * 100) + $3 }') ;;
esac
export CLIP_VERSION="$version" CLIP_BUILD_CHANNEL="${CLIP_BUILD_CHANNEL:-local}"
export MACOSX_DEPLOYMENT_TARGET=15.0
cargo build --locked --release
app=target/Clip.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/clip "$app/Contents/MacOS/Clip"
# The icon, every size iconutil wants, from the 1024 master (scripts/icon.swift makes it).
iconset=target/AppIcon.iconset
rm -rf "$iconset" && mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/clip-icon-1024.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/clip-icon-1024.png --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Clip</string>
  <key>CFBundleDisplayName</key><string>Clip</string>
  <key>CFBundleIdentifier</key><string>dev.vogel.clip</string>
  <key>CFBundleExecutable</key><string>Clip</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$build</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSScreenCaptureUsageDescription</key><string>Clip records your screen when you double-tap Control.</string>
  <key>NSMicrophoneUsageDescription</key><string>Clip records your voice over a recording when you turn on Microphone.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Clip records what your Mac plays when you turn on System audio.</string>
</dict>
</plist>
PLIST
identity="${CLIP_SIGN_IDENTITY:-Developer ID Application: Ryan Vogel (9G68SMNHEU)}"
if [ "${CLIP_RELEASE_UNSIGNED:-0}" != 1 ] && security find-identity -v -p codesigning | grep -qF "$identity"; then
  codesign --force --options runtime --timestamp --entitlements .github/release/clip.entitlements --sign "$identity" "$app"
else
  echo "no $identity: signing ad hoc (permissions reset with every build)" >&2
  codesign --force --sign - --identifier dev.vogel.clip "$app"
fi
codesign --verify --strict "$app"
if [ "${CLIP_NOTARIZE:-0}" = 1 ]; then
  zip=target/Clip-notarize.zip
  rm -f "$zip"
  ditto -c -k --keepParent "$app" "$zip"
  xcrun notarytool submit "$zip" --keychain-profile "${CLIP_NOTARY_PROFILE:-isocast-notary}" --wait
  xcrun stapler staple "$app"
  rm -f "$zip"
fi
echo "$app"

#!/usr/bin/env bash
# sign-macos.sh <unsigned Clip.app zip> <out dir>: signs Clip.app with the
# Developer ID identity (hardened runtime, secure timestamp, Clip's
# entitlements), notarizes and staples it, then writes the release service's
# two macOS targets from the stapled app: clip-<version>-macos-arm64.zip (root
# Clip.app, for the updater) and clip-<version>-macos-arm64.dmg (signed,
# notarized, stapled, with an Applications link). The app is data here: it is
# never launched.
#
# Environment: CLIP_VERSION, CLIP_SIGNING_IDENTITY, CLIP_KEYCHAIN,
# CLIP_NOTARY_KEY (macos-credentials.sh), CLIP_NOTARY_KEY_ID, CLIP_NOTARY_ISSUER.
. "$(dirname "$0")/lib.sh"

input=$1
out=$2
release=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
zip="$out/clip-$CLIP_VERSION-macos-arm64.zip"
dmg="$out/clip-$CLIP_VERSION-macos-arm64.dmg"

/usr/bin/ditto -x -k "$input" "$work/unpacked"
app="$work/unpacked/Clip.app"
if [ ! -d "$app" ] || [ "$(find "$work/unpacked" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" != 1 ]; then
  die "The macOS build must hold exactly Clip.app."
fi
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")" = dev.vogel.clip ] ||
  die "Clip.app has the wrong bundle identifier."
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")" = "$CLIP_VERSION" ] ||
  die "Clip.app is not version $CLIP_VERSION."

# The one Mach-O in the bundle is its executable.
found=$(cd "$app" && find . -type f -exec sh -c 'file -b "$1" | grep -q Mach-O && printf "%s\n" "${1#./}"' _ {} \;)
[ "$found" = Contents/MacOS/Clip ] || die "Unexpected code in Clip.app: $(echo "$found" | tr '\n' ' ')"

sign() {
  codesign --force --timestamp --options runtime --keychain "$CLIP_KEYCHAIN" --sign "$CLIP_SIGNING_IDENTITY" "$@"
}
notarize() {
  xcrun notarytool submit "$1" --key "$CLIP_NOTARY_KEY" --key-id "$CLIP_NOTARY_KEY_ID" --issuer "$CLIP_NOTARY_ISSUER" --wait --timeout 45m
}

sign --entitlements "$release/clip.entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

# Notarize the app first so its ticket is stapled inside both targets.
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$app" "$work/notarize.zip"
notarize "$work/notarize.zip"
xcrun stapler staple "$app"
xcrun stapler validate "$app"

rm -f "$zip" "$dmg"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$app" "$zip"

mkdir "$work/staging"
/usr/bin/ditto "$app" "$work/staging/Clip.app"
ln -s /Applications "$work/staging/Applications"
hdiutil create -quiet -fs APFS -format ULFO -volname Clip -srcfolder "$work/staging" "$dmg"
codesign --force --timestamp --keychain "$CLIP_KEYCHAIN" --sign "$CLIP_SIGNING_IDENTITY" "$dmg"
notarize "$dmg"
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
ls -l "$out"

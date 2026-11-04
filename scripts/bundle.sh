#!/bin/sh
# Build target/Clip.app: release binary, an Info.plist for a menu-bar-less accessory app, ad-hoc signed.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
app=target/Clip.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/clip "$app/Contents/MacOS/Clip"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Clip</string>
  <key>CFBundleDisplayName</key><string>Clip</string>
  <key>CFBundleIdentifier</key><string>dev.vogel.clip</string>
  <key>CFBundleExecutable</key><string>Clip</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSScreenCaptureUsageDescription</key><string>Clip records your screen when you double-tap Control.</string>
</dict>
</plist>
PLIST
codesign --force --sign - --identifier dev.vogel.clip "$app"
echo "$app"

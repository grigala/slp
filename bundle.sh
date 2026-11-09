#!/bin/sh
# Build slp and wrap it in a macOS .app bundle (menu-bar only, no Dock icon).
# Usage: ./bundle.sh            -> target/release/slp.app
#        ./bundle.sh --install  -> also copies it to /Applications
set -eu
cd "$(dirname "$0")"

cargo build --release

APP=target/release/slp.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/slp "$APP/Contents/MacOS/slp"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>slp</string>
  <key>CFBundleDisplayName</key><string>slp</string>
  <key>CFBundleIdentifier</key><string>dev.george.slp</string>
  <key>CFBundleExecutable</key><string>slp</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppleEventsUsageDescription</key>
  <string>slp asks System Events to shut down or restart your Mac when the timer ends.</string>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP"
echo "Built $APP"

if [ "${1:-}" = "--install" ]; then
  rm -rf /Applications/slp.app
  cp -R "$APP" /Applications/
  echo "Installed to /Applications/slp.app"
fi

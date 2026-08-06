#!/bin/sh
# Electron's postinstall is blocked by the npm allow-scripts guard, and running
# install.js manually dies silently mid-extract under Node 26 (extract-zip's
# promise never settles, so node exits 0 with a half-written dist/).
# This downloads the zip via install.js, then extracts it properly with ditto.
set -e
cd "$(dirname "$0")/.."
node node_modules/electron/install.js || true
ZIP=$(ls "$HOME"/Library/Caches/electron/*/electron-v*.zip | head -1)
rm -rf node_modules/electron/dist
mkdir -p node_modules/electron/dist
ditto -xk "$ZIP" node_modules/electron/dist
printf 'Electron.app/Contents/MacOS/Electron' > node_modules/electron/path.txt

# Dev-mode menu-bar name: macOS takes the app menu title from the bundle's
# Info.plist, not from app.setName(), so patch the dev Electron bundle and
# re-sign it ad-hoc. Packaged builds get the name from productName instead.
PLIST=node_modules/electron/dist/Electron.app/Contents/Info.plist
/usr/libexec/PlistBuddy -c "Set :CFBundleName Tome" "$PLIST" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Add :CFBundleName string Tome" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName Tome" "$PLIST" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Add :CFBundleDisplayName string Tome" "$PLIST"
codesign --force --deep --sign - node_modules/electron/dist/Electron.app 2>/dev/null || true

echo "electron binary restored + renamed: $(cat node_modules/electron/path.txt)"

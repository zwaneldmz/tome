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
echo "electron binary restored: $(cat node_modules/electron/path.txt)"

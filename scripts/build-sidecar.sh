#!/usr/bin/env bash
# Stages the tome-shim Linux-sandbox sidecar where Tauri's `externalBin`
# bundler expects to find it: src-tauri/binaries/tome-shim-<target-triple>
# (see src-tauri/tauri.conf.json's `bundle.externalBin` entry, and
# src-tauri/crates/tome-shim/ for the binary itself — Phase 4 of the
# Electron -> Tauri rewrite's "Linux sandbox" work).
#
# Tauri's sidecar convention requires the binary on disk to already carry
# its build-time target-triple suffix before `tauri build`/`npm run tauri
# build` ever runs (`beforeBuildCommand` in tauri.conf.json calls this
# script for exactly that reason). Cargo's own `cargo build -p tome-shim`
# output has no such suffix, so this script's only real job is: build, then
# copy-with-a-triple-suffixed-name.
#
# NATIVE HOST TARGET ONLY — no cross-compilation. Every real caller today
# (a CI runner building natively for its own OS/arch, or a developer
# running `tauri build`/`tauri dev` locally) IS the target the sidecar
# needs to run on; staging a sidecar for a DIFFERENT target than the one
# this script runs on is Phase 7 (packaging/release matrix) scope, not
# this one — see the rewrite plan's phase list.
#
# Safe to run on every OS `cargo build -p tome-shim` compiles for,
# including macOS, where tome-shim is a real (tiny, `main()`-only) binary
# rather than a build failure — see crates/tome-shim/src/main.rs's
# `#[cfg(not(target_os = "linux"))]` branch. macOS packaging never lists
# tome-shim as a dependency of anything it actually ships (the seatbelt
# path needs no sidecar at all), so staging a macOS build of it here is
# harmless, not meaningfully wasted work, and keeps this script itself
# simple (one unconditional build, no per-OS branching).
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
if [ -z "$TARGET_TRIPLE" ]; then
  echo "build-sidecar: could not determine the host target triple from 'rustc -vV'" >&2
  exit 1
fi

EXT=""
case "$TARGET_TRIPLE" in
  *windows*) EXT=".exe" ;;
esac

echo "build-sidecar: cargo build -p tome-shim --release (host target: $TARGET_TRIPLE)"
(cd src-tauri && cargo build -p tome-shim --release)

SRC="src-tauri/target/release/tome-shim${EXT}"
OUT_DIR="src-tauri/binaries"
DEST="${OUT_DIR}/tome-shim-${TARGET_TRIPLE}${EXT}"

if [ ! -f "$SRC" ]; then
  echo "build-sidecar: expected build output missing at $SRC" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
cp "$SRC" "$DEST"
chmod +x "$DEST"
echo "build-sidecar: staged $DEST"

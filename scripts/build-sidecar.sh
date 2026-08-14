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
# Which triples to stage: `TAURI_ENV_TARGET_TRIPLE` when the caller set it
# (tauri-action does, and for `--target universal-apple-darwin` it carries
# BOTH real triples comma-separated — tauri-build's externalBin check runs
# per-arch, so a universal macOS build needs the sidecar staged for both
# aarch64-apple-darwin AND x86_64-apple-darwin), otherwise the host triple
# (native dev builds, the linux-sandbox CI job). Non-host triples are
# cross-built with `cargo build --target` — the same toolchain component
# the main build itself needs, so no extra setup is required beyond what a
# universal build already installs.
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

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
if [ -z "$HOST_TRIPLE" ]; then
  echo "build-sidecar: could not determine the host target triple from 'rustc -vV'" >&2
  exit 1
fi

TRIPLES="${TAURI_ENV_TARGET_TRIPLE:-$HOST_TRIPLE}"
OUT_DIR="src-tauri/binaries"
mkdir -p "$OUT_DIR"

# Comma-separated: tauri-action passes 'aarch64-apple-darwin,x86_64-apple-darwin'
# for a universal build.
IFS=',' read -ra WANTED <<< "$TRIPLES"
for TARGET_TRIPLE in "${WANTED[@]}"; do
  EXT=""
  case "$TARGET_TRIPLE" in
    *windows*) EXT=".exe" ;;
  esac

  if [ "$TARGET_TRIPLE" = "$HOST_TRIPLE" ]; then
    echo "build-sidecar: cargo build -p tome-shim --release (host target: $TARGET_TRIPLE)"
    (cd src-tauri && cargo build -p tome-shim --release)
    SRC="src-tauri/target/release/tome-shim${EXT}"
  else
    echo "build-sidecar: cargo build -p tome-shim --release --target $TARGET_TRIPLE (cross)"
    (cd src-tauri && cargo build -p tome-shim --release --target "$TARGET_TRIPLE")
    SRC="src-tauri/target/${TARGET_TRIPLE}/release/tome-shim${EXT}"
  fi

  DEST="${OUT_DIR}/tome-shim-${TARGET_TRIPLE}${EXT}"
  if [ ! -f "$SRC" ]; then
    echo "build-sidecar: expected build output missing at $SRC" >&2
    exit 1
  fi

  cp "$SRC" "$DEST"
  chmod +x "$DEST"
  echo "build-sidecar: staged $DEST"
done

#!/usr/bin/env bash
#
# Build Dimmy (Debug) from Xcode and install into /Applications so that
# TCC (microphone / accessibility / input monitoring) records a stable
# bundle path across rebuilds. Recommended dev loop for macOS.
#
# Usage: scripts/macos/install-to-applications.sh [--release]

set -euo pipefail

CONFIG="Debug"
if [[ "${1:-}" == "--release" ]]; then
    CONFIG="Release"
fi

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT/platforms/macos"

echo "[install] building Dimmy ($CONFIG)…"
xcodebuild \
    -project Dimmy.xcodeproj \
    -scheme Dimmy \
    -configuration "$CONFIG" \
    -destination 'platform=macOS,arch=arm64' \
    build \
    | tail -20

BUILT_DIR=$(xcodebuild \
    -project Dimmy.xcodeproj \
    -scheme Dimmy \
    -configuration "$CONFIG" \
    -showBuildSettings 2>/dev/null \
  | awk -F' = ' '/^ *BUILT_PRODUCTS_DIR/ {print $2}' | head -1)

SRC="$BUILT_DIR/Dimmy.app"
DST="/Applications/Dimmy.app"

if [[ ! -d "$SRC" ]]; then
    echo "[install] ERROR: $SRC not found" >&2
    exit 1
fi

echo "[install] bundling llama/ggml dylibs into $SRC/Contents/Frameworks"
FRAMEWORKS_DIR="$SRC/Contents/Frameworks"
RELEASE_DIR="$ROOT/core/target/aarch64-apple-darwin/release"
if compgen -G "$RELEASE_DIR/libllama*.dylib" >/dev/null; then
    mkdir -p "$FRAMEWORKS_DIR"
    for name in libllama libggml libggml-base libggml-cpu libggml-metal; do
        for dylib in "$RELEASE_DIR"/${name}*.dylib; do
            [ -e "$dylib" ] || continue
            DEST="$FRAMEWORKS_DIR/$(basename "$dylib")"
            cp -RL "$dylib" "$DEST"
            install_name_tool -id "@rpath/$(basename "$dylib")" "$DEST" 2>/dev/null || true
        done
    done
    codesign --force --sign - "$FRAMEWORKS_DIR"/*.dylib
else
    echo "[install] no llama/ggml dylibs in $RELEASE_DIR — skipping bundle step"
fi

echo "[install] replacing $DST"
rm -rf "$DST"
cp -R "$SRC" "$DST"

echo "[install] re-signing app (dylibs in Frameworks/ changed the bundle hash)"
codesign --force --deep --sign - "$DST"

echo "[install] verifying code signature"
codesign --verify --deep --strict "$DST"

echo "[install] launching"
open "$DST"

echo "[install] done."

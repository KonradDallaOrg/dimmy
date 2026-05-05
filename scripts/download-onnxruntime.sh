#!/usr/bin/env bash
# Download + extract a pinned onnxruntime native dylib into core/target.
#
# Mirrors scripts/download-onnxruntime.ps1 (Windows). Pin version MUST
# match the ort crate's bundled-version constant. ort 2.0.0-rc.10
# targets onnxruntime 1.22.0 — bumping ort = bumping this script.
#
# Idempotent: skips download + extract when the destination dylib
# already exists. Safe to call from local dev and from CI.
#
# Usage:
#   scripts/download-onnxruntime.sh           # arm64, default version
#   scripts/download-onnxruntime.sh -f        # force re-download
#   ORT_VERSION=1.22.0 scripts/download-onnxruntime.sh
set -euo pipefail

VERSION="${ORT_VERSION:-1.22.0}"
FORCE=0
ARCH="${ORT_ARCH:-arm64}"   # arm64 or universal2

while getopts "fa:v:" opt; do
    case "$opt" in
        f) FORCE=1 ;;
        a) ARCH="$OPTARG" ;;
        v) VERSION="$OPTARG" ;;
        *) echo "usage: $0 [-f] [-a arm64|universal2] [-v VERSION]" >&2; exit 2 ;;
    esac
done

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
target_dir="$repo_root/core/target/onnxruntime-osx-$ARCH-$VERSION"
lib_dir="$target_dir/lib"
dylib_path="$lib_dir/libonnxruntime.${VERSION}.dylib"

if [[ -f "$dylib_path" && $FORCE -eq 0 ]]; then
    echo "[ort] cached: $dylib_path"
    exit 0
fi

cache_dir="${HOME}/Library/Caches/dimmy-onnxruntime"
mkdir -p "$cache_dir"

tgz_name="onnxruntime-osx-$ARCH-$VERSION.tgz"
tgz_path="$cache_dir/$tgz_name"

if [[ ! -f "$tgz_path" || $FORCE -eq 1 ]]; then
    url="https://github.com/microsoft/onnxruntime/releases/download/v$VERSION/$tgz_name"
    echo "[ort] downloading $url"
    curl -fL --retry 3 -o "$tgz_path" "$url"
fi

extract_root="$repo_root/core/target"
mkdir -p "$extract_root"
if [[ -d "$target_dir" ]]; then
    rm -rf "$target_dir"
fi
echo "[ort] extracting to $target_dir"
tar -xzf "$tgz_path" -C "$extract_root"

if [[ ! -f "$dylib_path" ]]; then
    echo "[ort] expected dylib not found after extract: $dylib_path" >&2
    echo "[ort] contents of $lib_dir:" >&2
    ls -1 "$lib_dir" >&2 || true
    exit 1
fi

size_mb=$(( $(stat -f%z "$dylib_path") / 1024 / 1024 ))
echo "[ort] ready: $dylib_path (${size_mb} MB)"

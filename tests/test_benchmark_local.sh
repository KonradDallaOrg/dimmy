#!/usr/bin/env bash
# test_benchmark_local.sh — Local STT benchmark using whisper.cpp models
#
# Usage:
#   ./tests/test_benchmark_local.sh                   # Run benchmark (quick tier audio)
#   ./tests/test_benchmark_local.sh download           # Download audio samples first
#   ./tests/test_benchmark_local.sh info               # Show downloaded models & audio
#
# Prerequisites:
#   1. Build DLL/lib: cargo build --release --lib --features local-stt-vulkan
#   2. Download audio: ./tests/test_benchmark.sh download
#   3. Download at least one whisper model via the Dimmy app or FFI
#
# The benchmark binary loads WAV files directly, runs whisper inference per model,
# and outputs a markdown report identical in format to the cloud benchmark.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
CORE_DIR="$PROJECT_DIR/core"
AUDIO_DIR="$SCRIPT_DIR/audio"

MODE="${1:-run}"

# ── Detect platform feature flag ─────────────────────────────────────────────
detect_feature() {
  case "$(uname -s)" in
    Darwin) echo "local-stt-metal" ;;
    Linux)  echo "local-stt" ;;
    MINGW*|MSYS*|CYGWIN*|Windows*) echo "local-stt-vulkan" ;;
    *)      echo "local-stt" ;;
  esac
}

FEATURE=$(detect_feature)

case "$MODE" in
  download)
    echo ""
    echo "  Downloading audio samples (delegates to test_benchmark.sh)..."
    bash "$SCRIPT_DIR/test_benchmark.sh" download
    ;;
  info)
    echo ""
    echo "  ┌────────────────────────────────────────────────────────────────┐"
    echo "  │  LOCAL STT BENCHMARK — Model & Audio Inventory                │"
    echo "  └────────────────────────────────────────────────────────────────┘"
    echo ""
    echo "  Feature: $FEATURE"
    echo ""
    echo "  Building bench_local (release)..."
    cd "$CORE_DIR"
    cargo build --release --bin bench_local --features "$FEATURE" 2>&1 | tail -1
    echo ""
    echo "  Running with --help is not supported; just run without args."
    echo "  Audio dir: $AUDIO_DIR"
    echo ""
    if [[ -d "$AUDIO_DIR" ]]; then
      echo "  Available WAV files:"
      for f in "$AUDIO_DIR"/*.wav; do
        [[ -f "$f" ]] && echo "    $(basename "$f") — $(du -h "$f" | cut -f1)"
      done
    else
      echo "  No audio directory found. Run: ./tests/test_benchmark_local.sh download"
    fi
    echo ""
    ;;
  run|"")
    if [[ ! -d "$AUDIO_DIR" ]]; then
      echo "  No audio samples found. Downloading first..."
      bash "$SCRIPT_DIR/test_benchmark.sh" download
    fi

    echo ""
    echo "  Building bench_local (release, $FEATURE)..."
    cd "$CORE_DIR"
    cargo build --release --bin bench_local --features "$FEATURE" 2>&1 | tail -3
    echo ""

    echo "  Running local STT benchmark..."
    echo ""

    # Find the binary
    BENCH_BIN="$CORE_DIR/target/release/bench_local"
    if [[ -f "${BENCH_BIN}.exe" ]]; then
      BENCH_BIN="${BENCH_BIN}.exe"
    fi

    "$BENCH_BIN" "$AUDIO_DIR"
    ;;
  *)
    echo "Usage: $0 [run|download|info]"
    exit 1
    ;;
esac

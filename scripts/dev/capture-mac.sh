#!/usr/bin/env bash
#
# capture-mac.sh — one-shot macOS screenshot harness (pair of the Windows
# MarketingCapture / capture-ui.ps1). Builds the Rust static lib + the .app,
# launches it with DIMMY_SCREENSHOT_ALL=1 so the in-process
# SettingsScreenshotter renders every Settings tab (light+dark) PLUS the
# onboarding steps and pill states, then collects the PNGs.
#
# Designed to be run end-to-end by a Claude CLI session on a Mac (or
# MacInCloud) with ZERO manual clicking — `bash scripts/dev/capture-mac.sh`.
#
# Output:
#   out/mac-settings-screenshots/*.png            (Settings tabs, light+dark)
#   out/mac-settings-screenshots/help/first-dictation/mac-*.png   (onboarding)
#   out/mac-settings-screenshots/help/pill/state-*.png            (pill states)
#
# Prereqs on the Mac: Xcode (xcodebuild), Rust w/ aarch64-apple-darwin target,
# git. No network needed beyond the initial cargo fetch.
#
# This is in-process rendering (NSView.cacheDisplay), so it does NOT need a
# foreground GUI / screencapture for the Settings + onboarding shots. The pill
# states capture the live panel and may need the panel actually on screen — if
# they come out empty on a headless session, run from a real desktop session.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

MAC_FEATURES="local-stt-metal,local-llm-metal,local-stt-parakeet-coreml,local-stt-parakeet-fluid,local-dfn"
APP="platforms/macos/build/DerivedData/Build/Products/Debug/Dimmy.app"
OUT="out/mac-settings-screenshots"

step() { echo; echo "── $* ──"; }

step "1/4 cargo build static lib (aarch64-apple-darwin, $MAC_FEATURES)"
( cd core && cargo build --release --lib --target aarch64-apple-darwin --features "$MAC_FEATURES" )

step "2/4 xcodebuild Dimmy (Debug)"
xcodebuild \
    -project platforms/macos/Dimmy.xcodeproj \
    -scheme Dimmy \
    -configuration Debug \
    -derivedDataPath platforms/macos/build/DerivedData \
    -destination 'platform=macOS' \
    build > /tmp/dimmy-capture-xcodebuild.log 2>&1 || {
        echo "❌ xcodebuild failed — last 60 lines:" >&2
        tail -60 /tmp/dimmy-capture-xcodebuild.log >&2
        exit 1
    }
[ -d "$APP" ] || { echo "❌ Dimmy.app not found at $APP" >&2; exit 1; }

step "3/4 launch with DIMMY_SCREENSHOT_ALL=1 (renders + self-terminates)"
rm -rf "$OUT"
LOG="$(mktemp)"
# The screenshotter completes its loop then calls NSApp.terminate. Give it a
# generous window; kill as a backstop if it hangs.
DIMMY_SCREENSHOT_ALL=1 "$APP/Contents/MacOS/Dimmy" > "$LOG" 2>&1 &
PID=$!
for _ in $(seq 1 40); do
    kill -0 "$PID" 2>/dev/null || break
    sleep 1
done
kill "$PID" 2>/dev/null || true
echo "  (screenshotter log: $LOG)"
grep -E '\[Screenshotter\]' "$LOG" | tail -40 || true

step "4/4 collect"
if [ -d "$OUT" ]; then
    COUNT="$(find "$OUT" -name '*.png' | wc -l | tr -d ' ')"
    echo "✅ $COUNT PNG under $ROOT/$OUT"
    find "$OUT" -name '*.png' | sort
else
    echo "❌ no output dir $OUT — check the screenshotter log above."
    exit 1
fi

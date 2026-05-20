#!/usr/bin/env bash
# Pre-flight check for the Mac build path.
#
# Run this BEFORE pushing any commit that touches:
#   - core/src/ (FFI surface, especially new dimmy_* exports)
#   - platforms/macos/Dimmy/ (Swift sources, SelfTests, Info.plist)
#   - platforms/macos/Dimmy.xcodeproj/ (file registration)
#   - .github/workflows/release.yml (Mac job)
#
# What it does, in order:
#
#   1. cargo fmt --check
#   2. cargo clippy with the Mac frozen feature set (-D warnings)
#   3. cargo test --lib serially (the test suite has known shared-state
#      tests that break in parallel — release.yml uses the parallel
#      default, but for a pre-flight we want a clean signal)
#   4. cargo build --release --target aarch64-apple-darwin with the
#      Mac frozen feature set so `libdimmy_lib.a` has every FFI symbol
#      Swift references (claude_code_* etc.)
#   5. xcodebuild -scheme Dimmy build (Debug) so we catch missing
#      file-references in the pbxproj and Swift compile errors
#   6. Launch the built .app for 5 s in background — this exercises
#      SelfTests.runAtLaunch(), which is the only place a stale
#      assertion (e.g. "Onboarding has 4 steps" after a 5-step refactor)
#      shows itself before the DMG ships
#
# If any step fails, exit non-zero with a clear error.
#
# Burned 2026-05-13: v0.6.39-rc1 shipped a DMG that compiled cleanly
# but crashed on first launch because two SelfTests assertions
# (HTTPS-only LLM URLs + 4-step onboarding) were stale after merging
# `feat/anthropic-subscription-login` and `feat/v2-unified`. release.yml
# doesn't run SelfTests — only `xcodebuild build`. This script is the
# local belt-and-braces that catches the same class of bug.

set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
echo "== Dimmy Mac pre-flight =="
echo "Repo root: $ROOT"
echo

# Feature flag set must match release.yml's Mac build job — see
# .github/workflows/release.yml line ~189. If you change it here, change
# it there (and update CLAUDE.md if the meaning of any feature shifts).
MAC_FEATURES="local-stt-metal,local-llm-metal,local-stt-parakeet-coreml,local-stt-parakeet-fluid,local-dfn"
CI_FEATURES="local-stt,local-llm"

step() { echo; echo "── $* ──"; }

step "1/6 cargo fmt --check"
( cd core && cargo fmt --check )

step "2/6 cargo clippy ($CI_FEATURES) -D warnings"
( cd core && cargo clippy --features "$CI_FEATURES" -- -D warnings )

step "3/6 cargo test --lib serially"
( cd core && cargo test --lib --features "$CI_FEATURES" -- --test-threads=1 )

step "4/6 cargo build static lib for aarch64-apple-darwin ($MAC_FEATURES)"
( cd core && cargo build --release --lib --target aarch64-apple-darwin \
    --features "$MAC_FEATURES" )

step "5/6 xcodebuild -scheme Dimmy build (Debug)"
xcodebuild \
    -project platforms/macos/Dimmy.xcodeproj \
    -scheme Dimmy \
    -configuration Debug \
    -derivedDataPath platforms/macos/build/DerivedData \
    -destination 'platform=macOS' \
    build > /tmp/dimmy-xcodebuild.log 2>&1 || {
        echo "❌ xcodebuild failed — last 40 lines:" >&2
        tail -40 /tmp/dimmy-xcodebuild.log >&2
        exit 1
    }

step "6/6 launch Dimmy.app to exercise SelfTests"
APP="platforms/macos/build/DerivedData/Build/Products/Debug/Dimmy.app"
[ -d "$APP" ] || { echo "❌ Dimmy.app not found at $APP" >&2; exit 1; }
# Bump bundle version so Sparkle on a foregrounded dev machine
# can't downgrade-replace this build mid-run.
/usr/libexec/PlistBuddy -c 'Set :CFBundleShortVersionString 99.0.0' \
    -c 'Set :CFBundleVersion 99.0.0' "$APP/Contents/Info.plist" >/dev/null
LOG="$(mktemp)"
"$APP/Contents/MacOS/Dimmy" > "$LOG" 2>&1 &
PID=$!
sleep 5
if kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
    echo "✓ Dimmy ran 5 s without tripping SelfTests"
    if grep -q "All [0-9]* tests passed" "$LOG"; then
        echo "  $(grep "All [0-9]* tests passed" "$LOG" | tail -1)"
    fi
else
    echo "❌ Dimmy exited within 5 s — likely a SelfTest fatal:" >&2
    tail -20 "$LOG" >&2
    exit 1
fi
rm -f "$LOG"

echo
echo "✓ All 6 pre-flight steps passed. Safe to push."

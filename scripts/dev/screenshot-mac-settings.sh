#!/usr/bin/env bash
# screenshot-mac-settings.sh
#
# Capture one PNG per Settings page from a built Dimmy.app. Output goes
# under `out/mac-settings-screenshots/`. Run this from a Mac with a
# real display + an active console session — `screencapture` refuses
# to work from a headless SSH shell (no WindowServer).
#
# Usage:
#   ./scripts/dev/screenshot-mac-settings.sh           # uses the Debug build under platforms/macos/build/
#   APP_PATH=/path/Dimmy.app ./scripts/dev/screenshot-mac-settings.sh
#
# Prereqs:
#   - Accessibility permission granted to /usr/bin/osascript
#     (System Settings → Privacy → Accessibility → +osascript)
#   - The app must have been launched at least once so it shows up
#     in the Settings sidebar with all pages wired
#
# What it does (no AppleScript-on-Dimmy needed — pure System Events):
#   1. Launches the app
#   2. Opens Settings via Cmd+, applied to the frontmost Dimmy window
#   3. For each sidebar page, clicks via UI scripting + screencaps the
#      window only (not the whole screen)
#
# If a step fails, the script logs and continues to the next page so
# you still get partial output.

set -u

# Matches MacSettingsTab.label in MacSettingsContainerView.swift, in sidebar order.
# Advanced is gated by the "Show advanced settings" toggle — it'll show up here
# only if you've flipped that on in your local config before running.
PAGES=(
    "Home"
    "Voice input"
    "Output"
    "App rules"
    "History"
    "Integrations"
    "Pill overlay"
    "Shortcut"
    "Permissions"
    "Privacy & data"
    "License"
    "About"
    "Advanced"
)

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP_PATH="${APP_PATH:-$REPO_ROOT/platforms/macos/build/Debug/Dimmy.app}"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/out/mac-settings-screenshots}"

if [ ! -d "$APP_PATH" ]; then
    echo "❌ APP_PATH not found: $APP_PATH"
    echo "   Build first:"
    echo "   xcodebuild -project platforms/macos/Dimmy.xcodeproj -scheme Dimmy -configuration Debug -derivedDataPath platforms/macos/build"
    exit 1
fi

mkdir -p "$OUT_DIR"
echo "📸 Output → $OUT_DIR"
echo "📦 App    → $APP_PATH"

# Launch (or focus) Dimmy
open "$APP_PATH"
sleep 2

# Open Settings (Cmd+,)
osascript -e 'tell application "System Events" to keystroke "," using {command down}'
sleep 1.5

# Find the Settings window id
get_settings_window_id() {
    # Returns the CGWindowID of the topmost Dimmy window. Settings has
    # title "Settings" or starts with "Dimmy"; fall back to the first.
    /usr/sbin/system_profiler SPDisplaysDataType >/dev/null 2>&1 || true
    # Use a Swift one-liner via xcrun — robust against locale.
    /usr/bin/env -i \
        /usr/bin/swift - <<'SWIFT'
import AppKit
import CoreGraphics
let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else { exit(2) }
for info in list {
    let owner = (info[kCGWindowOwnerName as String] as? String) ?? ""
    let name  = (info[kCGWindowName       as String] as? String) ?? ""
    if owner == "Dimmy" && (name.lowercased().contains("settings") || name.isEmpty == false) {
        if let id = info[kCGWindowNumber as String] as? Int {
            print(id)
            exit(0)
        }
    }
}
exit(1)
SWIFT
}

WIN_ID=$(get_settings_window_id || true)
if [ -z "$WIN_ID" ]; then
    echo "⚠️  Could not resolve Settings window id — falling back to full-screen capture."
fi

for page in "${PAGES[@]}"; do
    safe=$(echo "$page" | tr ' ' '_' | tr '[:upper:]' '[:lower:]')
    out="$OUT_DIR/${safe}.png"

    # Click the sidebar item via UI scripting
    osascript <<APPLESCRIPT 2>/dev/null
tell application "System Events"
    tell process "Dimmy"
        try
            click (first button whose title is "$page") of window 1
        on error
            try
                click (first static text whose value is "$page") of window 1
            end try
        end try
    end tell
end tell
APPLESCRIPT
    sleep 0.6

    if [ -n "$WIN_ID" ]; then
        /usr/sbin/screencapture -x -o -l "$WIN_ID" "$out" 2>/dev/null
    else
        /usr/sbin/screencapture -x "$out" 2>/dev/null
    fi

    if [ -s "$out" ]; then
        echo "✓ $page → $out"
    else
        echo "✗ $page (capture failed)"
    fi
done

echo ""
echo "Done. Open the folder:"
echo "  open '$OUT_DIR'"

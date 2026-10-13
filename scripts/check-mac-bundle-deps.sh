#!/usr/bin/env bash
# Fail if any Mach-O inside a Dimmy.app links a library that only exists on
# the build machine.
#
# A library a user's Mac does not have kills the app in dyld, before main():
# no window, no message, nothing in Sentry or PostHog. v0.7.x shipped
# libllama-common linked to /opt/homebrew/opt/openssl@3/lib/libssl.3.dylib.
# It launched on every machine with Homebrew (the CI runner, the developer's
# Mac) and on no other. The @rpath check and the 8 s launch both passed,
# because the runner HAS Homebrew.
#
# Allowed: @rpath/@executable_path/@loader_path (resolved inside the bundle),
# /usr/lib and /System/Library (ship with macOS). Anything else is a path on
# the build machine.
#
# Usage: scripts/check-mac-bundle-deps.sh path/to/Dimmy.app
set -euo pipefail

APP="${1:?usage: $0 path/to/Dimmy.app}"
[ -d "$APP/Contents" ] || { echo "::error::$APP is not an app bundle"; exit 1; }

CHECKED=0
BAD=0
while IFS= read -r -d '' f; do
  [ -L "$f" ] && continue
  file -b "$f" | grep -q 'Mach-O' || continue
  CHECKED=$((CHECKED + 1))
  # Dependencies are the tab-indented lines; the unindented ones name the file
  # (once per architecture in a universal binary, e.g. Sparkle). A dylib also
  # lists its own install name (LC_ID_DYLIB), which is not a dependency.
  own_id=$(otool -D "$f" 2>/dev/null | sed -n '2p')
  while read -r dep; do
    [ -z "$dep" ] && continue
    [ "$dep" = "$own_id" ] && continue
    case "$dep" in
      @rpath/*|@executable_path/*|@loader_path/*|/usr/lib/*|/System/Library/*) ;;
      *)
        echo "::error::${f#"$APP"/} links $dep - that path exists only on the build machine; the app will not launch on a Mac without it"
        BAD=1
        ;;
    esac
  done < <(otool -L "$f" | awk '/^	/ {print $1}')
done < <(find "$APP/Contents" -type f -print0)

# A check that looked at nothing prints reassurance it has not earned.
if [ "$CHECKED" -eq 0 ]; then
  echo "::error::no Mach-O files found under $APP - this check is not looking at what it thinks it is"
  exit 1
fi
[ "$BAD" -eq 0 ] || exit 1
echo "all $CHECKED Mach-O files link only bundled or system libraries"

#!/usr/bin/env bash
# Fail if any Mach-O inside a Dimmy.app needs something a user's Mac may not
# have: a library that only exists on the build machine, or a newer macOS than
# the app declares.
#
# Either one kills the app in dyld, before main(): no window, no message,
# nothing in Sentry or PostHog. v0.7.x shipped both:
#   - libllama-common linked to /opt/homebrew/opt/openssl@3/lib/libssl.3.dylib.
#     It launched on every machine with Homebrew (the CI runner, the
#     developer's Mac) and on no other.
#   - every llama/ggml dylib was built for macOS 26.5 (CMake defaults to the
#     SDK of the build machine), and libllama-common imported
#     posix_spawn_file_actions_addchdir, which exists only from macOS 26.
#     The app declares 14.0, so Sonoma installed it and could not run it.
# The @rpath check and the 8 s launch both passed, because the runner HAS
# Homebrew and IS on macOS 26.
#
# Allowed dependencies: @rpath/@executable_path/@loader_path (resolved inside
# the bundle), /usr/lib and /System/Library (ship with macOS). Allowed minimum
# macOS: at most the app's LSMinimumSystemVersion.
#
# It also fails when one library ships under two file names (two Mach-O files
# with the same LC_UUID), or when the executable or a top-level Frameworks
# dylib names a bundled library that is not there. 0.7.13 shipped every
# llama/ggml dylib three times (libX.dylib, libX.0.dylib, libX.0.20.2.dylib);
# the executable loaded one name and the libraries the other, so dyld mapped
# two copies of ggml into the process and the app aborted on quit.
#
# Usage: scripts/check-mac-bundle-deps.sh path/to/Dimmy.app
set -euo pipefail

APP="${1:?usage: $0 path/to/Dimmy.app}"
[ -d "$APP/Contents" ] || { echo "::error::$APP is not an app bundle"; exit 1; }
APP_MIN=$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP/Contents/Info.plist")
[ -n "$APP_MIN" ] || { echo "::error::$APP has no LSMinimumSystemVersion"; exit 1; }

# True when version $1 is greater than version $2.
newer_than() { [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]; }

CHECKED=0
BAD=0
UUIDS="$(mktemp)"
trap 'rm -f "$UUIDS"' EXIT
EXE_DIR="$APP/Contents/MacOS"
# LC_RPATH entries of a Mach-O, one per line.
rpaths_of() { otool -l "$1" | awk '/ cmd LC_RPATH/ {r=1} r && $1 == "path" {print $2; r=0}'; }
EXE_RPATHS=""
for exe in "$EXE_DIR"/*; do
  if [ -f "$exe" ] && [ ! -L "$exe" ]; then
    EXE_RPATHS="$EXE_RPATHS $(rpaths_of "$exe" | tr -s '[:space:]' ' ')"
  fi
done

# Does dependency $2 of file $1 resolve to a file inside the bundle?
resolves() {
  local f="$1" dep="$2" rest rp dir
  dir="$(dirname "$f")"
  case "$dep" in
    @loader_path/*)     [ -e "$dir/${dep#@loader_path/}" ] ;;
    @executable_path/*) [ -e "$EXE_DIR/${dep#@executable_path/}" ] ;;
    @rpath/*)
      rest="${dep#@rpath/}"
      for rp in $EXE_RPATHS $(rpaths_of "$f"); do
        rp="${rp/#@executable_path/$EXE_DIR}"
        rp="${rp/#@loader_path/$dir}"
        [ -e "$rp/$rest" ] && return 0
      done
      return 1 ;;
    *) return 0 ;;
  esac
}
while IFS= read -r -d '' f; do
  [ -L "$f" ] && continue
  file -b "$f" | grep -q 'Mach-O' || continue
  CHECKED=$((CHECKED + 1))
  rel="${f#"$APP"/}"

  # Dependencies are the tab-indented lines; the unindented ones name the file
  # (once per architecture in a universal binary, e.g. Sparkle). A dylib also
  # lists its own install name (LC_ID_DYLIB), which is not a dependency.
  own_id=$(otool -D "$f" 2>/dev/null | sed -n '2p')
  otool -l "$f" | awk -v rel="$rel" '/ cmd LC_UUID/ {u=1} u && $1 == "uuid" {print $2, rel; u=0}' >> "$UUIDS"
  case "$rel" in
    Contents/MacOS/*|Contents/Frameworks/*.dylib) top=1 ;;
    *) top=0 ;;
  esac
  case "$rel" in Contents/Frameworks/*/*) top=0 ;; esac
  while read -r dep; do
    [ -z "$dep" ] && continue
    [ "$dep" = "$own_id" ] && continue
    case "$dep" in
      @rpath/*|@executable_path/*|@loader_path/*)
        if [ "$top" -eq 1 ] && ! resolves "$f" "$dep"; then
          echo "::error::$rel links $dep, which is not in the bundle - dyld will refuse to launch the app"
          BAD=1
        fi
        ;;
      /usr/lib/*|/System/Library/*) ;;
      *)
        echo "::error::$rel links $dep - that path exists only on the build machine; the app will not launch on a Mac without it"
        BAD=1
        ;;
    esac
  done < <(otool -L "$f" | awk '/^\t/ {print $1}')

  # LC_BUILD_VERSION carries `minos`, the older LC_VERSION_MIN_MACOSX `version`.
  while read -r minos; do
    if newer_than "$minos" "$APP_MIN"; then
      echo "::error::$rel is built for macOS $minos but the app declares $APP_MIN - it will not launch on anything older than $minos"
      BAD=1
    fi
  done < <(otool -l "$f" | awk '
    / cmd LC_BUILD_VERSION/      { want = "minos" }
    / cmd LC_VERSION_MIN_MACOSX/ { want = "version" }
    want != "" && $1 == want     { print $2; want = "" }')
done < <(find "$APP/Contents" -type f -print0)

# A check that looked at nothing prints reassurance it has not earned.
if [ "$CHECKED" -eq 0 ]; then
  echo "::error::no Mach-O files found under $APP - this check is not looking at what it thinks it is"
  exit 1
fi
# Same UUID = same library. Two of them means dyld can map it twice.
DUPS="$(awk '{print $1}' "$UUIDS" | sort | uniq -d)"
for u in $DUPS; do
  echo "::error::one library is bundled under several names (UUID $u):$(awk -v u="$u" '$1 == u {printf " %s", $2}' "$UUIDS") - ship one file and point every load command at it"
  BAD=1
done

[ "$BAD" -eq 0 ] || exit 1
echo "all $CHECKED Mach-O files are unique, link only bundled or system libraries and run on macOS $APP_MIN"

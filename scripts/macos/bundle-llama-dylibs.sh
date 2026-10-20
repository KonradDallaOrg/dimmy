#!/usr/bin/env bash
# Put the llama.cpp / ggml dylibs into Dimmy.app/Contents/Frameworks, ONE file
# per library, and point every binary at that one file.
#
# CMake leaves a versioned chain for each library:
#     libggml-base.dylib -> libggml-base.0.dylib -> libggml-base.0.20.2.dylib
# and llama-cpp-sys-4's build.rs then hard-links all three names into the
# target dir. It also stamps each REAL file's id with its full-version name
# (@loader_path/libggml-base.0.20.2.dylib), which is what the executable
# records, while the libraries keep referring to each other by the SONAME
# (@loader_path/libggml-base.0.dylib). Copying every name as its own file
# therefore made dyld load each library TWICE (same UUID, two paths): two
# ggml-base, two backend registries, two terminate handlers. Seen in the
# 0.7.13-rc.3 quit crash: ggml_uncaught_exception appears once per copy.
#
# The canonical name is the SONAME (libX.<major>.dylib): it is the name the
# libraries already use for one another, so only the executable needs
# rewriting.
#
# Usage: bundle-llama-dylibs.sh FRAMEWORKS_DIR RELEASE_DIR SIGN_IDENTITY [BINARY...]
#   BINARY: images outside Frameworks that link the libraries (the app
#   executable, the Debug build's Dimmy.debug.dylib). Their load commands are
#   rewritten to @rpath/<canonical>.
set -euo pipefail

FW="${1:?usage: $0 FRAMEWORKS_DIR RELEASE_DIR SIGN_IDENTITY [BINARY...]}"
RD="${2:?missing RELEASE_DIR}"
IDENTITY="${3:?missing SIGN_IDENTITY (use - for ad-hoc)}"
shift 3

FAMILIES="libllama libllama-common libmtmd libggml libggml-base libggml-cpu libggml-metal"

# libggml-base.0.20.2.dylib -> libggml-base ; libllama-common.dylib -> libllama-common
family_of() { basename "$1" | sed -E 's/(\.[0-9]+)*\.dylib$//'; }

is_family() {
  for n in $FAMILIES; do [ "$n" = "$1" ] && return 0; done
  return 1
}

# The SONAME-shaped file of a family in a directory: exactly one numeric
# component between the family and .dylib.
soname_in() {
  local dir="$1" fam="$2" f mid
  for f in "$dir/$fam".*.dylib; do
    [ -e "$f" ] || continue
    mid="${f##*/"$fam".}"
    mid="${mid%.dylib}"
    case "$mid" in *[!0-9]*|'') continue ;; esac
    basename "$f"
    return 0
  done
  return 1
}

mkdir -p "$FW"
# llama.cpp's tool libraries (bench, perplexity, quantize...). Nothing links
# them; the old libllama* glob shipped them by accident.
rm -f "$FW"/libllama-*-impl.dylib
COPIED=""
for fam in $FAMILIES; do
  [ -e "$RD/$fam.dylib" ] || continue
  canon="$(soname_in "$RD" "$fam")" || {
    echo "error: $RD has $fam.dylib but no $fam.<major>.dylib - cannot pick the canonical name" >&2
    exit 1
  }
  # Drop every other name, including copies left by an earlier build.
  rm -f "$FW/$fam.dylib" "$FW/$fam".*.dylib
  cp -L "$RD/$canon" "$FW/$canon"
  chmod 644 "$FW/$canon"
  install_name_tool -id "@rpath/$canon" "$FW/$canon"
  COPIED="$COPIED $canon"
done
[ -n "$COPIED" ] || { echo "error: no llama/ggml dylibs found in $RD" >&2; exit 1; }

# Rewrite load commands that name a non-canonical member of a family.
#   $1 file, $2 prefix to use for the new name (@loader_path or @rpath)
retarget() {
  local file="$1" prefix="$2" own dep base fam canon
  own="$(otool -D "$file" 2>/dev/null | sed -n '2p')"
  otool -L "$file" | awk '/^\t/ {print $1}' | while read -r dep; do
    [ "$dep" = "$own" ] && continue
    case "$dep" in @loader_path/*|@rpath/*|@executable_path/*) ;; *) continue ;; esac
    base="${dep##*/}"
    fam="$(family_of "$base")"
    is_family "$fam" || continue
    canon="$(soname_in "$FW" "$fam")" || continue
    [ "$dep" = "$prefix/$canon" ] && continue
    install_name_tool -change "$dep" "$prefix/$canon" "$file"
  done
}

for canon in $COPIED; do
  retarget "$FW/$canon" "@loader_path"
  codesign --force --sign "$IDENTITY" --timestamp=none "$FW/$canon"
done
for bin in "$@"; do
  [ -e "$bin" ] || continue
  retarget "$bin" "@rpath"
done

echo "[bundle-llama-dylibs] one file per library:$COPIED"

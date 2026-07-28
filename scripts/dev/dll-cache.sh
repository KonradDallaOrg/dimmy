#!/usr/bin/env bash
# DLL-reuse cache helper (Cloudflare R2 / S3-compatible). One audited
# implementation shared by the Windows + macOS release workflows so the
# "reuse the prebuilt core artifact when the Rust core didn't change" logic
# lives in exactly one place.
#
#   dll-cache.sh restore <platform> "<feat>" <dest-file>
#       Prints "HIT" (and places the artifact at <dest-file>) if a cached
#       artifact for the current core hash exists; otherwise prints "MISS".
#       ALWAYS exits 0 — any error degrades to MISS so the caller does a
#       normal full build. Never blocks a release.
#
#   dll-cache.sh store <platform> "<feat>" <src-file>
#       Best-effort upload of a freshly-built artifact under the core hash.
#       Failures are non-fatal (logged, exit 0).
#
# Key:  s3://dimmy-sccache/dll-cache/v2/<platform>/<core-hash>/<basename>
# Env:  AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, SCCACHE_R2_ENDPOINT
#
# SAFETY: the core hash (scripts/dev/core-build-hash.sh) is over-inclusive —
# any source/dep/feature/toolchain change misses the cache and rebuilds. The
# CI linker-version gate still runs on the restored DLL, so a bad cache entry
# cannot ship a sub-14.50 binary. The dangerous direction (reuse a stale DLL)
# is structurally prevented by the content-addressed hash.
set -uo pipefail

action="${1:?usage: dll-cache.sh <restore|store> <platform> <feat> <file>}"
platform="${2:?platform required}"
feat="${3:?feature string required}"
file="${4:?file path required}"

bucket="dimmy-sccache"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

hash="$("$here/core-build-hash.sh" "$feat" 2>/dev/null)" || hash=""
if [ -z "$hash" ]; then
  echo "dll-cache: could not compute core hash -> MISS (will build)" >&2
  [ "$action" = "restore" ] && echo "MISS"
  exit 0
fi

base="$(basename "$file")"
# Namespace bumped v1 -> v2 on 2026-07-28: the v1 cache was POISONED by
# v0.6.69-rc1, which stored a miscompiled dimmy_lib.dll (stale Swatinem
# whisper.cpp objects, no cargo clean) under a valid content hash. Since a
# version-only bump is normalized out of the hash by design, rc70 would
# otherwise REUSE that broken DLL. Bumping the prefix makes every lookup
# MISS v1 -> one clean rebuild -> repopulates a fresh v2. No R2 deletes,
# reuse mechanism unchanged.
url="s3://$bucket/dll-cache/v2/$platform/$hash/$base"
common=(--endpoint-url "${SCCACHE_R2_ENDPOINT:-}" --region auto)
echo "dll-cache: $action  hash=$hash  key=$url" >&2

case "$action" in
  restore)
    mkdir -p "$(dirname "$file")" 2>/dev/null || true
    if aws s3 cp "$url" "$file" "${common[@]}" >&2 2>&1 && [ -s "$file" ]; then
      echo "HIT"
    else
      rm -f "$file" 2>/dev/null || true
      echo "MISS"
    fi
    ;;
  store)
    if [ ! -s "$file" ]; then
      echo "dll-cache: store source missing/empty, skip" >&2
      exit 0
    fi
    aws s3 cp "$file" "$url" "${common[@]}" >&2 2>&1 \
      || echo "dll-cache: upload failed (non-fatal)" >&2
    ;;
  *)
    echo "dll-cache: unknown action '$action'" >&2
    exit 1
    ;;
esac

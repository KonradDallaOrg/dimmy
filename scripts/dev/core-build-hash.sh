#!/usr/bin/env bash
# Deterministic hash of EVERYTHING that determines the compiled Rust core
# artifact (Windows `dimmy_lib.dll` / macOS `libdimmy_lib.a`). Used as the
# DLL-reuse cache key in CI: if the hash matches a previously-built artifact
# stored in R2, the release skips the ~15-min `cargo build` and reuses it.
#
#   scripts/dev/core-build-hash.sh "<feature-string>"
#   e.g. scripts/dev/core-build-hash.sh "local-stt-vulkan,local-llm-vulkan,local-stt-parakeet,license-client"
#
# DESIGN — fail-safe by construction:
#   * OVER-inclusive: hashes the content of every tracked file under core/
#     (via git blob SHAs — content-addressed, deterministic). Hashing an
#     extra file can only cause a SAFE, unnecessary rebuild. It can NEVER
#     cause a wrong/stale DLL to be reused (the dangerous direction).
#   * The ONLY thing normalized out is the crate's own `version =` line in
#     Cargo.toml + Cargo.lock — a version-only bump (e.g. 0.6.64 -> 0.6.65,
#     which changes zero Rust code) must NOT force a 15-min rebuild. Every
#     dependency version in Cargo.lock stays significant.
#   * The feature string + the exact rustc version are folded in, so a
#     feature-set change ("le voglio tutte, sempre") or a toolchain change
#     busts the key and forces a fresh build.
#
# The CI linker-version gate (dumpbin >= 14.50) still runs on the reused DLL,
# so even a hypothetical bad cache entry cannot ship a sub-14.50 binary.
set -euo pipefail

feat="${1:?usage: core-build-hash.sh \"<feature-string>\"}"

{
  # 1. Content addresses of every tracked file under core/ EXCEPT the two
  #    manifests (handled below with version normalization). git ls-tree
  #    emits "<mode> blob <sha>\t<path>" — the blob SHA is the file content.
  git ls-tree -r HEAD -- core/ | grep -vE $'\tcore/(Cargo\\.toml|Cargo\\.lock)$'

  # 2. Cargo.toml with the package version line neutralized (bump-insensitive).
  git show HEAD:core/Cargo.toml | sed -E 's/^version[[:space:]]*=.*/version = "_norm_"/'

  # 3. Cargo.lock with ONLY the dimmy package's own version neutralized (the
  #    `version = ...` line immediately following `name = "dimmy"`). All
  #    dependency versions remain significant.
  git show HEAD:core/Cargo.lock | awk '
    prev == "name = \"dimmy\"" { sub(/version = .*/, "version = \"_norm_\"") }
    { print; prev = $0 }'

  # 4. Build-determining environment.
  echo "features=${feat}"
  rustc -V

  # 5. Build-time values baked INTO the binary via core/build.rs / option_env!
  #    (licensing pubkey + endpoint, build flavor, config namespace, telemetry
  #    keys). CRITICAL: without these in the key, a prod build and a staging
  #    build with identical source+features would share a cache entry — a prod
  #    release could then restore a STAGING-flavored DLL (staging license
  #    pubkey -> free/trial demotion) or vice-versa. Secrets are hashed, never
  #    emitted in the clear; only the final 16-hex digest leaves this script.
  echo "license_url=${DIMMY_LICENSE_SERVER_URL:-}"
  echo "flavor=${DIMMY_BUILD_FLAVOR:-}"
  echo "namespace=${DIMMY_CONFIG_NAMESPACE:-}"
  printf 'pubkey=%s\n'  "$(printf '%s' "${DIMMY_LICENSE_PUBKEY:-}" | sha256sum)"
  printf 'posthog=%s\n' "$(printf '%s' "${POSTHOG_API_KEY:-}"      | sha256sum)"
  printf 'sentry=%s\n'  "$(printf '%s' "${SENTRY_DSN:-}"           | sha256sum)"
} | sha256sum | cut -c1-16

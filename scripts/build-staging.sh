#!/usr/bin/env bash
# build-staging.sh — builds a release-quality dimmy_lib that points at
# the staging licensing Worker (license-staging.dimmy.app), with the
# staging pubkey embedded so signature verify succeeds against
# staging-issued tokens.
#
# Usage:
#   ./scripts/build-staging.sh                      # default cargo build
#   ./scripts/build-staging.sh --features local-stt-vulkan,local-llm-vulkan
#   ./scripts/build-staging.sh --release            # also implied by --features
#
# Why a wrapper instead of a Cargo alias: cargo aliases can't conditionally
# `source` a file. Without sourcing .env.staging we'd hit the build.rs
# guard ("DIMMY_LICENSE_PUBKEY empty in release") or, worse, a build that
# silently opts out of licensing.

set -euo pipefail

ENV_FILE="$(dirname "$0")/../.env.staging"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: $ENV_FILE missing — run from repo root or fix the path" >&2
  exit 1
fi

# `set -a` exports every var sourced from .env.staging into the cargo
# subprocess environment. `set +a` reverts so we don't pollute the
# caller's shell.
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

if [[ "$DIMMY_LICENSE_PUBKEY" == "REPLACE_WITH_STAGING_PUBKEY" ]]; then
  echo "error: .env.staging still has the placeholder pubkey" >&2
  echo "       run the keypair generator in WSL and paste the value in." >&2
  exit 1
fi

cd "$(dirname "$0")/../core"
exec cargo build --release --features license-client "$@"

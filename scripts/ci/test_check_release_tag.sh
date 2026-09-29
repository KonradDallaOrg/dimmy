#!/usr/bin/env bash
# Tests for scripts/check-release-tag.sh. Run: bash scripts/ci/test_check_release_tag.sh
#
# The published releases are injected through DIMMY_PUBLISHED_TAGS, so no
# gh call is made. Each case is: expected exit, tag, published tags.
set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/check-release-tag.sh"
fail=0

check() {
    local want="$1" tag="$2" published="$3" why="$4" got
    DIMMY_PUBLISHED_TAGS="$published" bash "$SCRIPT" "$tag" >/dev/null 2>&1
    got=$?
    if [ "$got" -eq "$want" ]; then
        echo "ok    $tag  ($why)"
    else
        echo "FAIL  $tag  want exit $want, got $got  ($why)"
        fail=1
    fi
}

# --- Shape ---------------------------------------------------------------
check 1 v0.7.0-rc10        "v0.7.0-rc.9"  "rcN without the dot sorts rc10 below rc8"
check 1 v0.7.12-staging3   "v0.7.12-staging.2" "stagingN without the dot"
check 1 v0.7.12-beta.1     "v0.7.11"      "unknown pre-release label would ship to prod"
check 1 v0.7               "v0.7.11"      "not X.Y.Z"
check 1 0.7.12-rc.1        "v0.7.11"      "missing the v prefix"

# --- Each track is ordered against itself ------------------------------------
# The day this was written: staging.1 published first, then the rc was refused
# because "rc" < "staging" in ASCII. Staging and prod are different packIds;
# no client ever compares one with the other.
check 0 v0.7.12-rc.1       $'v0.7.12-staging.1\nv0.7.11-rc.1\nv0.7.11-staging.2' "rc is not ranked against staging"
check 0 v0.7.12-staging.2  $'v0.7.12-staging.1\nv0.7.12-rc.1' "staging is not ranked against rc"
check 1 v0.7.12-staging.1  $'v0.7.12-staging.1' "duplicate staging"
check 1 v0.7.12-staging.1  $'v0.7.12-staging.2' "staging going backwards"
check 1 v0.7.12-rc.1       $'v0.7.12-rc.1\nv0.7.12-staging.3' "duplicate rc"
check 0 v0.7.12-rc.10      $'v0.7.12-rc.9\nv0.7.12-staging.1' "rc.10 outranks rc.9 numerically"
# rc and stable share the prod packId: the prerelease channel sees both.
check 1 v0.7.11-rc.3       $'v0.7.11\nv0.7.11-rc.2\nv0.7.11-staging.1' "rc below an already-published stable"
check 1 v0.7.10            $'v0.7.11\nv0.7.10-rc.1\nv0.7.11-rc.1\nv0.7.11-staging.1' "stable going backwards"

# --- Promotion ladder: staging leads, rc follows, stable follows rc --------
check 0 v0.7.13-staging.1  $'v0.7.12-staging.1\nv0.7.12-rc.1' "staging may run ahead"
check 1 v0.7.13-rc.1       $'v0.7.12-staging.1\nv0.7.12-rc.1' "rc ahead of every staging"
check 0 v0.7.12-rc.2       $'v0.7.13-staging.1\nv0.7.12-rc.1' "rc behind the staging line is fine"
check 0 v0.7.12            $'v0.7.12-rc.2\nv0.7.12-staging.1' "stable promotes a published rc"
check 1 v0.7.12            $'v0.7.11-rc.1\nv0.7.12-staging.1' "stable with no rc of its own"

# --- Drafts and non-version tags are not "published" -------------------------
check 0 v0.7.12-rc.1       $'staging-latest\nv0.7.12-staging.1' "non-version tags are ignored"

if [ "$fail" -ne 0 ]; then echo "check-release-tag: FAILED"; exit 1; fi
echo "check-release-tag: all passed"

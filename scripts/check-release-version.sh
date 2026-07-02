#!/usr/bin/env bash
# Dimmy release-version preflight — fail if Cargo.toml version is
# already taken on GitHub releases or git tags.
#
# Burned 2026-05-13: bumped Cargo.toml 0.6.37-rc1 → 0.6.37 while
# v0.6.38-rc1 had already been tagged the night before. Staging
# Release pipeline produced stale 0.6.37 artifacts; had to cancel
# and force-bump to 0.6.38 mid-flight.
#
# Hook this into pre-commit when the staged changes touch
# `core/Cargo.toml`. Also runnable standalone:
#   ./scripts/check-release-version.sh
#
# Exit codes:
#   0  Cargo.toml version is > all known releases (safe to bump)
#   1  duplicate or downgrade detected (refuse)
#   2  prerequisites missing (gh CLI / git not available) — warn only

set -e

CARGO_TOML="${CARGO_TOML:-core/Cargo.toml}"
if [ ! -f "$CARGO_TOML" ]; then
    echo "[check-version] $CARGO_TOML not found — script must be run from repo root."
    exit 2
fi

# Extract just the numeric version, dropping any -rc1/-beta/-staging.N
# suffix for comparison purposes. The suffix doesn't disambiguate
# ordering (rc1 < final), but we want the underlying number to be
# unique. NOTE the dot in the class: `-staging.13` has a dot, and the
# old `[a-zA-Z0-9]+` left the base as `0.6.66-staging.13`, making every
# comparison against staging tags fail (found 2026-07-02).
CARGO_VER=$(grep -m1 '^version = ' "$CARGO_TOML" | sed -E 's/version = "([^"]+)"/\1/' | sed 's/[[:space:]]//g')
CARGO_BASE=$(echo "$CARGO_VER" | sed -E 's/-[a-zA-Z0-9.]+$//')

echo "[check-version] Cargo.toml version: $CARGO_VER (base: $CARGO_BASE)"

# --- Git tags (always available locally) ---
TAGS=$(git tag --sort=-version:refname 2>/dev/null | head -20)
if [ -n "$TAGS" ]; then
    LATEST_TAG=$(echo "$TAGS" | head -1 | sed 's/^v//')
    LATEST_TAG_BASE=$(echo "$LATEST_TAG" | sed -E 's/-[a-zA-Z0-9.]+$//')
    echo "[check-version] highest local git tag: $LATEST_TAG (base: $LATEST_TAG_BASE)"
fi

# --- GitHub releases (gh CLI optional) ---
LATEST_GH=""
if command -v gh >/dev/null 2>&1; then
    LATEST_GH=$(gh release list --limit 20 2>/dev/null | awk -F'\t' '$3 ~ /^v[0-9]/ { print $3 }' | sort -V -r | head -1 | sed 's/^v//')
    if [ -n "$LATEST_GH" ]; then
        LATEST_GH_BASE=$(echo "$LATEST_GH" | sed -E 's/-[a-zA-Z0-9.]+$//')
        echo "[check-version] highest GitHub release: $LATEST_GH (base: $LATEST_GH_BASE)"
    fi
fi

# --- Compare ---
# version_gt A B → true iff A > B (semver-ish, sort -V)
version_gt() {
    [ "$1" = "$2" ] && return 1
    HIGHEST=$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)
    [ "$HIGHEST" = "$1" ]
}

FAIL=0
if [ -n "$LATEST_TAG_BASE" ] && ! version_gt "$CARGO_BASE" "$LATEST_TAG_BASE"; then
    if [ "$CARGO_BASE" = "$LATEST_TAG_BASE" ]; then
        # Same base — only OK if Cargo is a pre-release and the tag is final.
        # E.g. Cargo = 0.6.38-rc2, tag = 0.6.38-rc1 → OK (rc2 > rc1, still
        # working toward 0.6.38). Cargo = 0.6.38 with tag v0.6.38 = duplicate.
        if [ "$CARGO_VER" = "$LATEST_TAG" ]; then
            echo "[check-version] ✗ Cargo.toml '$CARGO_VER' is EXACTLY a published tag — duplicate would be created."
            FAIL=1
        fi
    else
        echo "[check-version] ✗ Cargo.toml base '$CARGO_BASE' is ≤ highest git tag base '$LATEST_TAG_BASE' — downgrade."
        FAIL=1
    fi
fi

if [ -n "$LATEST_GH_BASE" ] && ! version_gt "$CARGO_BASE" "$LATEST_GH_BASE"; then
    if [ "$CARGO_BASE" = "$LATEST_GH_BASE" ]; then
        if [ "$CARGO_VER" = "$LATEST_GH" ]; then
            echo "[check-version] ✗ Cargo.toml '$CARGO_VER' is EXACTLY a published GH release — duplicate."
            FAIL=1
        fi
    else
        echo "[check-version] ✗ Cargo.toml base '$CARGO_BASE' is ≤ highest GH release base '$LATEST_GH_BASE' — downgrade."
        FAIL=1
    fi
fi

if [ $FAIL -ne 0 ]; then
    echo ""
    echo "Fix: bump core/Cargo.toml to the next patch above"
    [ -n "$LATEST_GH_BASE" ] && echo "       $LATEST_GH_BASE  (GitHub release)"
    [ -n "$LATEST_TAG_BASE" ] && echo "       $LATEST_TAG_BASE  (git tag)"
    echo "    e.g. 0.6.39 if both are at 0.6.38."
    exit 1
fi

echo "[check-version] OK ✓"
exit 0

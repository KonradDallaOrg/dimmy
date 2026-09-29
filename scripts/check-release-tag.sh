#!/usr/bin/env bash
# Dimmy release-TAG preflight — refuse a tag that no client would ever be
# offered, or that skips a step of the promotion ladder.
#
# Burned 2026-09-07. `v0.7.0-rc10` was published and no Windows client
# ever offered it, because SemVer 11.4 compares a pre-release identifier
# containing letters CHARACTER BY CHARACTER:
#
#     "rc10" vs "rc8"  →  'r'='r', 'c'='c', '1'(0x31) < '8'(0x38)
#     → 0.7.0-rc10 is OLDER than 0.7.0-rc8
#
# The dot is the fix: in `rc.10` the `10` is its own identifier, made of
# digits only, and SemVer compares those NUMERICALLY. `rc.10 > rc.9`.
#
# Burned 2026-09-29. This check compared every tag with EVERY published
# release, so once `0.7.12-staging.1` was out, `0.7.12-rc.1` was refused:
# "rc" < "staging" in ASCII. Staging (packId Dimmy-Staging) and prod
# (packId Dimmy) are separate update tracks; no client ever ranks one
# against the other. Each track is now ranked against itself.
#
# Tracks and the ladder between them:
#
#   staging  vX.Y.Z-staging.N   ranked against staging releases; may run ahead
#   rc       vX.Y.Z-rc.N        ranked against prod (rc + stable) releases;
#                               X.Y.Z may not exceed the highest staging X.Y.Z
#   stable   vX.Y.Z             ranked against prod releases;
#                               needs a published X.Y.Z-rc.N to promote
#
# rc and stable share one track because they share the prod packId: the
# prerelease channel is offered both, so an rc below a published stable
# would never reach anyone.
#
# Usage:  ./scripts/check-release-tag.sh v0.7.1-rc.1
#         DIMMY_PUBLISHED_TAGS=$'v0.7.0\nv0.7.1-staging.1' ./scripts/check-release-tag.sh v0.7.1-rc.1
#         (tests inject the published list; otherwise it comes from gh)
# Tests:  bash scripts/ci/test_check_release_tag.sh
# Exit:   0 tag is fine   1 it is not   2 cannot tell
set -euo pipefail

TAG="${1:-}"
[ -n "$TAG" ] || { echo "usage: $0 <tag>"; exit 2; }

# --- SemVer 2.0.0 precedence -------------------------------------------
# Returns 0 when A > B, 1 otherwise. Build metadata is ignored, as the
# spec requires.
semver_gt() {
    local a="${1%%+*}" b="${2%%+*}"
    local a_core="${a%%-*}" b_core="${b%%-*}"
    local a_pre="" b_pre=""
    case "$a" in *-*) a_pre="${a#*-}" ;; esac
    case "$b" in *-*) b_pre="${b#*-}" ;; esac

    local i av bv
    for i in 1 2 3; do
        av=$(echo "$a_core" | cut -d. -f$i); bv=$(echo "$b_core" | cut -d. -f$i)
        av=${av:-0}; bv=${bv:-0}
        [ "$av" -gt "$bv" ] 2>/dev/null && return 0
        [ "$av" -lt "$bv" ] 2>/dev/null && return 1
    done

    # Equal cores. A version WITHOUT a pre-release outranks one with.
    [ -z "$a_pre" ] && [ -z "$b_pre" ] && return 1
    [ -z "$a_pre" ] && return 0
    [ -z "$b_pre" ] && return 1

    # Identifier by identifier: numeric ones compare numerically and rank
    # below alphanumeric ones; alphanumeric compare in ASCII order.
    local IFS=.
    read -r -a A <<< "$a_pre"
    read -r -a B <<< "$b_pre"
    local n=${#A[@]}; [ ${#B[@]} -gt "$n" ] && n=${#B[@]}
    local k x y
    for ((k = 0; k < n; k++)); do
        x="${A[k]-}"; y="${B[k]-}"
        # A larger set of identifiers wins when all preceding are equal.
        [ -z "$x" ] && return 1
        [ -z "$y" ] && return 0
        if [[ "$x" =~ ^[0-9]+$ && "$y" =~ ^[0-9]+$ ]]; then
            [ "$x" -gt "$y" ] && return 0
            [ "$x" -lt "$y" ] && return 1
        elif [[ "$x" =~ ^[0-9]+$ ]]; then
            return 1   # numeric ranks below alphanumeric
        elif [[ "$y" =~ ^[0-9]+$ ]]; then
            return 0
        else
            [[ "$x" > "$y" ]] && return 0
            [[ "$x" < "$y" ]] && return 1
        fi
    done
    return 1
}

# Prints the track of a tag (staging | rc | stable), or nothing if the tag
# is not one of the three shapes the pipelines understand.
track_of() {
    local n='(0|[1-9][0-9]*)'
    if   [[ "$1" =~ ^v$n\.$n\.$n-staging\.$n$ ]]; then echo staging
    elif [[ "$1" =~ ^v$n\.$n\.$n-rc\.$n$ ]];      then echo rc
    elif [[ "$1" =~ ^v$n\.$n\.$n$ ]];             then echo stable
    fi
}

base_of() { local v="${1#v}"; echo "${v%%-*}"; }

# --- Shape ---------------------------------------------------------------
TRACK=$(track_of "$TAG")
if [ -z "$TRACK" ]; then
    echo "[check-tag] ✗ '$TAG' is not vX.Y.Z, vX.Y.Z-rc.N or vX.Y.Z-staging.N."
    case "$TAG" in
        *-rc[0-9]*|*-staging[0-9]*)
            echo "[check-tag]   Put a DOT before the number: without it 'rc10' sorts BELOW 'rc8'." ;;
    esac
    exit 1
fi
VER="${TAG#v}"
BASE=$(base_of "$TAG")

# --- Published releases, split by track --------------------------------
if [ -n "${DIMMY_PUBLISHED_TAGS+x}" ]; then
    PUBLISHED="$DIMMY_PUBLISHED_TAGS"
else
    if ! command -v gh >/dev/null 2>&1; then
        echo "[check-tag] gh CLI unavailable — cannot compare against published releases."
        exit 2
    fi
    PUBLISHED=$(gh release list --limit 100 --json tagName,isDraft \
                  --jq '.[] | select(.isDraft | not) | .tagName')
fi

HIGH_STAGING="" HIGH_PROD="" HIGH_STAGING_BASE="" RC_FOR_BASE=""
while read -r t; do
    [ -n "$t" ] || continue
    tr=$(track_of "$t"); v="${t#v}"
    case "$tr" in
        staging)
            if [ -z "$HIGH_STAGING" ] || semver_gt "$v" "$HIGH_STAGING"; then HIGH_STAGING="$v"; fi ;;
        rc|stable)
            if [ -z "$HIGH_PROD" ] || semver_gt "$v" "$HIGH_PROD"; then HIGH_PROD="$v"; fi
            [ "$tr" = rc ] && [ "$(base_of "$t")" = "$BASE" ] && RC_FOR_BASE="$v" ;;
    esac
done <<< "$PUBLISHED"
[ -n "$HIGH_STAGING" ] && HIGH_STAGING_BASE="${HIGH_STAGING%%-*}"

# --- Rule 1: the tag outranks everything on its own track ----------------
if [ "$TRACK" = staging ]; then HIGH="$HIGH_STAGING"; else HIGH="$HIGH_PROD"; fi
echo "[check-tag] $TRACK tag $VER   highest on its track: ${HIGH:-none}"
if [ -n "$HIGH" ] && ! semver_gt "$VER" "$HIGH"; then
    echo "[check-tag] ✗ '$VER' does NOT outrank '$HIGH' under SemVer."
    echo "[check-tag]   Clients on this track would keep $HIGH and never see this build."
    exit 1
fi

# --- Rule 2: the promotion ladder ---------------------------------------
case "$TRACK" in
    rc)
        if [ -z "$HIGH_STAGING_BASE" ] || semver_gt "$BASE" "$HIGH_STAGING_BASE"; then
            echo "[check-tag] ✗ rc $BASE is ahead of staging (highest staging: ${HIGH_STAGING:-none})."
            echo "[check-tag]   Cut v$BASE-staging.N first; an rc promotes what staging has already shipped."
            exit 1
        fi ;;
    stable)
        if [ -z "$RC_FOR_BASE" ]; then
            echo "[check-tag] ✗ no published $BASE-rc.N to promote to stable."
            exit 1
        fi ;;
esac

echo "[check-tag] OK ✓"

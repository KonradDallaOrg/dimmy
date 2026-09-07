#!/usr/bin/env bash
# Dimmy release-TAG preflight — refuse a tag that SemVer ranks below one
# already published.
#
# Burned 2026-09-07. `v0.7.0-rc10` was published and no Windows client
# ever offered it, because SemVer 11.4 compares a pre-release identifier
# containing letters CHARACTER BY CHARACTER:
#
#     "rc10" vs "rc8"  →  'r'='r', 'c'='c', '1'(0x31) < '8'(0x38)
#     → 0.7.0-rc10 is OLDER than 0.7.0-rc8
#
# Every updater agreed and kept people on rc8. It had already happened
# once — v0.6.73 reached `rc11` — and the fix (dots, see below) was
# applied to the v0.6.74 line and never written down, so it lapsed.
#
# The dot is the fix: in `rc.10` the `10` is its own identifier, made of
# digits only, and SemVer compares those NUMERICALLY. `rc.10 > rc.9`.
# Without it there is no second identifier to compare numerically.
#
# The existing check-release-version.sh cannot catch this: it compares
# only the BASE (`0.7.0`), deliberately, and its `sort -V` would rank
# rc10 above rc8 anyway — GNU sort is more permissive than SemVer.
#
# Usage:  ./scripts/check-release-tag.sh v0.7.1-rc.1
# Exit:   0 tag outranks everything published   1 it does not   2 cannot tell
set -euo pipefail

TAG="${1:-}"
[ -n "$TAG" ] || { echo "usage: $0 <tag>"; exit 2; }

VER="${TAG#v}"

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

# --- Shape ---------------------------------------------------------------
# Only warn: a stable tag (v0.7.1) has no suffix, and staging tags use
# `-staging.N`, which is already dotted.
case "$VER" in
    *-rc[0-9]*)
        echo "[check-tag] ✗ '$TAG' uses -rcN. Use -rc.N (a DOT before the number)."
        echo "[check-tag]   Without the dot 'rc10' sorts BELOW 'rc8' and no client is offered the build."
        exit 1
        ;;
esac

# --- Compare against what is actually published --------------------------
if ! command -v gh >/dev/null 2>&1; then
    echo "[check-tag] gh CLI unavailable — cannot compare against published releases."
    exit 2
fi

HIGHEST=""
while read -r r; do
    [ -n "$r" ] || continue
    cand="${r#v}"
    if [ -z "$HIGHEST" ] || semver_gt "$cand" "$HIGHEST"; then HIGHEST="$cand"; fi
done < <(gh release list --limit 40 --json tagName,isDraft \
           --jq '.[] | select(.isDraft | not) | .tagName' 2>/dev/null || true)

if [ -z "$HIGHEST" ]; then
    echo "[check-tag] no published release to compare against — allowing '$TAG'."
    exit 0
fi

echo "[check-tag] tag $VER   highest published $HIGHEST"
if semver_gt "$VER" "$HIGHEST"; then
    echo "[check-tag] OK ✓ SemVer ranks it above every published release."
    exit 0
fi

echo "[check-tag] ✗ '$VER' does NOT outrank '$HIGHEST' under SemVer."
echo "[check-tag]   Clients would keep offering $HIGHEST and never see this build."
echo "[check-tag]   Fix: bump the patch (e.g. the next $(echo "$HIGHEST" | cut -d. -f1,2).x) rather than adding another rc."
exit 1

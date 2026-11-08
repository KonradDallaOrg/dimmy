#!/usr/bin/env bash
# Sentry steps shared by release.yml, staging-tester.yml and
# staging-auto-update.yml.
#
#   sentry.sh name               print the release this build reports
#   sentry.sh release            create that release, attach commits, finalize
#   sentry.sh upload <path>...   upload debug files (PDB, DLL, dSYM) + sources
#
# Always exits 0: a Sentry outage or a missing token must never block a
# release. Needs SENTRY_AUTH_TOKEN; without it every action but `name` is
# a no-op.
set -uo pipefail

CLI_VERSION="3.8.2"
export SENTRY_ORG="dimmy" SENTRY_PROJECT="rust" SENTRY_URL="https://de.sentry.io/"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

cli() { npx --yes "@sentry/cli@$CLI_VERSION" "$@"; }

# MUST match core/build.rs::resolve_build_id + sentry_pipeline.rs::sentry_release,
# or symbols and commits attach to a release no binary reports.
# Args: version ref_type ref_name run_number explicit_build_id
release_name() {
    local version="$1" ref_type="$2" ref_name="$3" run="$4" explicit="$5" id
    if [ -n "$explicit" ]; then
        id="$explicit"
    elif [ -z "$ref_name" ]; then
        id="local"
    elif [ "$ref_type" = "tag" ] || [ -z "$run" ]; then
        id="$ref_name"
    else
        id="$ref_name.$run"
    fi
    if [ "$id" = "local" ]; then
        echo "dimmy@$version"
        return
    fi
    case "$id" in
        v[0-9]*) echo "dimmy@${id#v}" ;;
        *) echo "dimmy@$version+$id" ;;
    esac
}

current_name() {
    local version
    version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/core/Cargo.toml" | head -1)"
    release_name "$version" "${GITHUB_REF_TYPE:-}" "${GITHUB_REF_NAME:-}" \
        "${GITHUB_RUN_NUMBER:-}" "${DIMMY_BUILD_ID:-}"
}

need_token() {
    if [ -z "${SENTRY_AUTH_TOKEN:-}" ]; then
        echo "sentry: SENTRY_AUTH_TOKEN not set, skipping" >&2
        exit 0
    fi
}

case "${1:-}" in
    name)
        current_name
        ;;
    release)
        need_token
        name="$(current_name)"
        echo "sentry: release $name"
        cli releases new "$name" || { echo "::warning::sentry: releases new failed"; exit 0; }
        # --auto needs the GitHub integration in Sentry; --local reads this
        # checkout's history (the job checks out with fetch-depth: 0).
        cli releases set-commits "$name" --auto \
            || cli releases set-commits "$name" --local \
            || echo "::warning::sentry: set-commits failed"
        cli releases finalize "$name" || echo "::warning::sentry: finalize failed"
        ;;
    upload)
        need_token
        shift
        files=()
        for f in "$@"; do
            if [ -e "$f" ]; then files+=("$f"); else echo "sentry: $f missing, skipped" >&2; fi
        done
        if [ "${#files[@]}" -eq 0 ]; then
            echo "::warning::sentry: no debug files to upload"
            exit 0
        fi
        cli debug-files upload --include-sources "${files[@]}" \
            || echo "::warning::sentry: debug-files upload failed"
        ;;
    *)
        echo "usage: sentry.sh <name|release|upload <path>...>" >&2
        ;;
esac
exit 0

#!/usr/bin/env bash
# Tests for scripts/ci/sentry.sh `name`. Run: bash scripts/ci/test_sentry.sh
#
# The cases mirror core/src/telemetry/sentry_pipeline.rs::sentry_release
# tests: the CI name and the name the binary reports must be the same.
set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/sentry.sh"
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$(dirname "$SCRIPT")/../../core/Cargo.toml" | head -1)"
fail=0

check() {
    local want="$1" ref_type="$2" ref_name="$3" run="$4" explicit="$5" got
    got="$(GITHUB_REF_TYPE="$ref_type" GITHUB_REF_NAME="$ref_name" \
        GITHUB_RUN_NUMBER="$run" DIMMY_BUILD_ID="$explicit" bash "$SCRIPT" name)"
    if [ "$got" = "$want" ]; then
        echo "ok    $want"
    else
        echo "FAIL  want $want, got $got"
        fail=1
    fi
}

check "dimmy@0.7.14-rc.1"           tag    v0.7.14-rc.1      57  ""
check "dimmy@0.7.14-staging.3"      tag    v0.7.14-staging.3 12  ""
check "dimmy@0.7.14"                tag    v0.7.14           9   ""
check "dimmy@$VERSION+staging.1234" branch staging           1234 ""
check "dimmy@$VERSION+staging"      branch staging           ""  ""
check "dimmy@$VERSION"              ""     ""                ""  ""
check "dimmy@$VERSION+verify-thing" branch verify-thing      ""  ""
check "dimmy@0.6.73-rc6"            branch staging           5   v0.6.73-rc6
check "dimmy@$VERSION"              branch staging           5   local

exit "$fail"

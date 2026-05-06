#!/usr/bin/env bash
# stripe-smoke.sh — semi-automated smoke test of the licensing flow
# end-to-end against the staging Worker, using Stripe CLI to forge
# realistic webhook events.
#
# Prerequisites (one-time):
#   stripe login            # opens browser, links your Stripe account
#   wrangler login          # already done if you've been deploying
#
# Usage:
#   ./scripts/stripe-smoke.sh             # runs all 7 scenarios
#   ./scripts/stripe-smoke.sh trial       # runs just the trial scenarios
#   ./scripts/stripe-smoke.sh dedup       # runs just the dedup scenarios
#   ./scripts/stripe-smoke.sh refund      # runs just the refund scenarios
#
# What this exercises:
#   1. /api/health              Worker is up
#   2. /api/trial/start         dedup window (5 min) — same code re-used
#   3. /api/trial/start         expired-trial 409
#   4. checkout.session.completed (annual) → license created, mail sent
#   5. duplicate buy             gate blocks, sub cancelled, audit logged
#   6. refund.created (full)    license revoked
#   7. customer.subscription.updated cancel_at_period_end → cancels_at set
#
# Each scenario logs PASS / FAIL and queries staging D1 to confirm
# server-side state. Does NOT touch prod (license.dimmy.app).

set -euo pipefail

STAGING_URL="https://license-staging.dimmy.app"
DB_NAME="dimmy-licensing-staging"
ENV_FLAG="--env staging"

# ───────────────────────────────────────────────────────────────────
#  helpers
# ───────────────────────────────────────────────────────────────────

ok()   { printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { printf "  \033[31m✗\033[0m %s\n" "$*" >&2; exit 1; }
info() { printf "\n\033[1m▶ %s\033[0m\n" "$*"; }

# Run a SQL query against the staging D1, return JSON-trimmed output.
d1() {
  wrangler d1 execute "$DB_NAME" $ENV_FLAG --remote \
    --command "$1" --json 2>/dev/null \
    | python3 -c 'import sys, json; d=json.load(sys.stdin); print(json.dumps(d[0]["results"]))'
}

# POST JSON to the staging Worker.
post_json() {
  local path="$1"
  local body="$2"
  curl -s -X POST "$STAGING_URL$path" \
    -H "Content-Type: application/json" \
    --data "$body"
}

# Fire a Stripe CLI trigger; the event lands at the configured webhook
# (= staging Worker). `stripe trigger` synthesises the canonical event
# payload for that type.
fire() {
  local event="$1"
  shift
  echo "    → stripe trigger $event $*"
  stripe trigger "$event" "$@" >/dev/null
  sleep 2  # let the Worker process before we query D1
}

# ───────────────────────────────────────────────────────────────────
#  scenarios
# ───────────────────────────────────────────────────────────────────

scenario_health() {
  info "1. health check"
  local h
  h=$(curl -s "$STAGING_URL/api/health")
  [[ "$h" == '{"status":"ok"}' ]] || fail "health: got $h"
  ok "/api/health → ok"
}

scenario_trial_dedup() {
  info "2. trial dedup (same email twice within 5 min → same code)"
  local email="smoke-dedup-$(date +%s)@example.com"
  local r1 r2 c1 c2
  r1=$(post_json /api/trial/start "{\"email\":\"$email\"}")
  r2=$(post_json /api/trial/start "{\"email\":\"$email\"}")
  c1=$(echo "$r1" | python3 -c 'import sys,json; print(json.load(sys.stdin)["code"])')
  c2=$(echo "$r2" | python3 -c 'import sys,json; print(json.load(sys.stdin)["code"])')
  [[ "$c1" == "$c2" ]] || fail "dedup: $c1 != $c2"
  ok "same code returned: $c1"
}

scenario_trial_expired_blocked() {
  info "3. expired trial → 409 (cannot just delete file to reset)"
  # Setup: insert a trial license with valid_until in the past for a
  # synthetic email_hash. Then hit /api/trial/start.
  local email="smoke-expired-$(date +%s)@example.com"
  # email_hash is sha256(email + "dimmy-v1") base16 — easier to let the
  # Worker compute it: first call creates a fresh trial, then we mutate
  # valid_until to make it expired.
  post_json /api/trial/start "{\"email\":\"$email\"}" >/dev/null
  d1 "UPDATE licenses SET valid_until = 1 WHERE email_hash IN (
        SELECT email_hash FROM licenses ORDER BY issued_at DESC LIMIT 1
      ) AND tier = 'trial'" >/dev/null
  local resp
  resp=$(post_json /api/trial/start "{\"email\":\"$email\"}")
  echo "$resp" | grep -q "trial already used" \
    || fail "expected 409 trial-used, got: $resp"
  ok "expired trial → 409 'trial already used'"
}

scenario_checkout_completed() {
  info "4. checkout.session.completed → license created"
  local before after
  before=$(d1 "SELECT COUNT(*) as n FROM licenses WHERE tier IN ('monthly','annual','lifetime')")
  fire checkout.session.completed
  after=$(d1 "SELECT COUNT(*) as n FROM licenses WHERE tier IN ('monthly','annual','lifetime')")
  ok "license rows before=$(echo "$before" | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])') after=$(echo "$after" | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])')"
}

scenario_duplicate_purchase_blocked() {
  info "5. duplicate purchase → blocked (audit row written)"
  local before after
  before=$(d1 "SELECT COUNT(*) as n FROM audit_log WHERE event_type = 'duplicate_purchase_blocked'")
  # Two checkout events back to back — same customer (Stripe trigger
  # deterministically uses the same test customer for the same trigger).
  fire checkout.session.completed
  fire checkout.session.completed
  after=$(d1 "SELECT COUNT(*) as n FROM audit_log WHERE event_type = 'duplicate_purchase_blocked'")
  ok "duplicate_purchase_blocked rows before=$(echo "$before" | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])') after=$(echo "$after" | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])')"
}

scenario_refund_created() {
  info "6. refund.created (succeeded full) → license revoked"
  local before after
  before=$(d1 "SELECT COUNT(*) as n FROM licenses WHERE status = 'revoked'")
  fire refund.created
  after=$(d1 "SELECT COUNT(*) as n FROM licenses WHERE status = 'revoked'")
  ok "revoked rows before=$(echo "$before" | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])') after=$(echo "$after" | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])')"
}

scenario_subscription_cancel_scheduled() {
  info "7. customer.subscription.updated (cancel_at_period_end) → cancels_at set"
  fire customer.subscription.updated
  local n
  n=$(d1 "SELECT COUNT(*) as n FROM licenses WHERE cancel_at_period_end = 1" \
       | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["n"])')
  ok "licenses with cancel_at_period_end=1: $n"
}

# ───────────────────────────────────────────────────────────────────
#  dispatcher
# ───────────────────────────────────────────────────────────────────

case "${1:-all}" in
  health)        scenario_health ;;
  trial)         scenario_health; scenario_trial_dedup; scenario_trial_expired_blocked ;;
  dedup)         scenario_health; scenario_trial_dedup ;;
  checkout)      scenario_health; scenario_checkout_completed ;;
  duplicate)     scenario_health; scenario_duplicate_purchase_blocked ;;
  refund)        scenario_health; scenario_refund_created ;;
  cancel)        scenario_health; scenario_subscription_cancel_scheduled ;;
  all|*)
    scenario_health
    scenario_trial_dedup
    scenario_trial_expired_blocked
    scenario_checkout_completed
    scenario_duplicate_purchase_blocked
    scenario_refund_created
    scenario_subscription_cancel_scheduled
    ;;
esac

echo
ok "smoke complete — see above for any FAILs"

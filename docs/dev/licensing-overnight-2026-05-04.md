# Licensing — overnight session 2026-05-04 (1:30 CET)

Status drop for the morning. Branch `feat/licensing-poc`, all commits
pushed and staging deployed.

## ⚠ CRITICAL FIX (commit `228d587`, 07:20 CET)

**The "Switch to Annual while on Monthly charges you again but stays
on Monthly" bug** is fixed. Path:

- New endpoint `POST /api/plan-change` calls Stripe
  `subscriptions.update` with `proration_behavior=create_prorations`
  on the existing subscription (same sub id, new price). Stripe issues
  ONE prorated invoice line for the difference — no second sub, no
  second monthly charge.
- `customer.subscription.updated` webhook handler now detects price
  flips on `items[0].price.id` and mirrors the new tier into
  `licenses.tier` so the next `/api/refresh` returns a token signed
  with the new tier.
- Win + Mac UI: `BuyTierAsync` / `buy(tier:)` branch on
  `Active{monthly|annual}` + new tier in {monthly, annual} → call the
  new FFI `dimmy_license_plan_change` instead of opening Checkout.
  All other paths (lifetime upgrade, first purchase, trial→paid)
  still go through `licenseCheckoutUrl` as before.

War-test against staging: 17/17 green including price-flip mirror
(scenario #10) and `/api/plan-change` input validation (#15).

User-visible impact: clicking **Switch to Annual** while on Monthly
now flips the existing sub in place and shows a "Plan switched to
annual" status; no second charge, no zombie sub. **Test in the
morning before merging the PR** — the Win + Mac binaries you'll find
running already have this in.

## What landed tonight (since `ad2bde7`)

| Commit  | What                                                                 |
|---------|----------------------------------------------------------------------|
| (UI)    | Win magic-link activation surfaces in UI reliably (foreground + auto-refresh) |
| (CTAs)  | Tier-aware Buy buttons (Win + Mac) — only higher tiers visible      |
| (dedup) | Magic-link 5-min idempotency window — no double-mint                |
| (plan)  | `/api/plan-change` for monthly⇄annual via Stripe sub-update (commit `228d587`) |

Worker tests: **144/144 green** (was 142). Staging deploy: live, version
`f00d0af8-f559-429c-9c00-407726fac722` at `license-staging.dimmy.app`.
Health probe `{"status":"ok"}` ✓. Dedup probe (two trial-start calls
back-to-back returning the same code) ✓.

## State of the local app (Win)

- **Running**: `Dimmy.Windows.exe` PID was 15832 at 01:20 CET (may have
  been killed by the Stop-Process between then and morning — relaunch
  via `C:\code\pai-voice\platforms\windows\Dimmy.Windows\bin\Debug\net8.0-windows10.0.19041.0\win-x64\Dimmy.Windows.exe`
  if missing).
- **dimmy_lib.dll built with**: `DIMMY_LICENSE_PUBKEY=avlM65...` +
  `DIMMY_LICENSE_SERVER_URL=https://license-staging.dimmy.app` (verified
  by string scan; embedded URL is ONLY staging, never prod).
- **license.json**: ABSENT (clean state, NotFound). When you click
  Buy or Start Trial it'll be populated.

## What you can test in the morning (5 min smoke)

Step-by-step against the running Win app:

### 1. Trial flow (validates PRIO 1 + dedup)

1. Settings → License → enter your email → Start Trial
2. Mail arrives from `staging@dimmy.app` (= staging FROM, NOT prod)
3. **PRIO 1 fix**: clicking the magic link now properly:
   - Brings the Settings window to foreground
   - Switches to the License tab
   - Refreshes status badge → **"Trial — 14 day(s) left"** appears
4. **Dedup verify**: click "Start Trial" a SECOND time within 5 min
   with the same email. Only ONE email arrives in your inbox (or two
   identical ones, depending on Resend's own dedup; both clickable).
5. License.json now exists and reads `tier:"trial"`.

### 2. Buy → Active (validates duplicate-purchase gate + UI)

1. Click **"Upgrade to Pro"** card → click **Annual** (TEST card 4242
   4242 4242 4242, any future expiry / any CVC)
2. Stripe redirects to checkout/success page → click "Open Dimmy"
3. **PRIO 1 fix**: Settings opens to License tab, badge updates →
   **"Active — annual (366 day(s) left)"**
4. **PRIO 2 fix**: scroll to Buy card. Now you see ONLY:
   - "Upgrade to Lifetime" button (Monthly + Annual hidden)
   - Portal hint text "Need to downgrade, cancel, or update payment? …"
5. **Duplicate gate verify**: click Annual AGAIN (e.g. via a
   bookmarked Stripe Payment Link if you have one). Backend blocks
   the duplicate sub, sends magic link for the EXISTING annual
   license, audit row `duplicate_purchase_blocked` written to D1.

### 3. Plan upgrade (validates lifetime in-place upgrade)

1. While on Active{Annual}, click **"Upgrade to Lifetime"** → pay TEST
2. Worker handler:
   - Cancels old annual sub via Stripe API
   - Mutates the SAME license row to `tier=lifetime`,
     `valid_until = now + 1095 days`, `stripe_subscription_id = NULL`
   - Sends magic link
3. Click magic link → token refreshed with `tier:"lifetime"`
4. UI now shows **"Active — lifetime"** + Buy card HIDDEN entirely
   (lifetime is the ceiling)
5. D1: still ONE license row for your email, just with new tier.

### 4. Plan change via Customer Portal (Stripe-side flow)

1. While on Active{Annual or Monthly}, click **"Manage subscription"**
2. Browser opens Stripe Customer Portal
3. Cancel the subscription (Stripe schedules cancellation at period end)
4. Stripe webhook fires → license `cancels_at` populated
5. Back in Dimmy License tab → headline shows "Subscription scheduled
   to cancel on …" subtitle, status remains Active until period end
6. To undo: in portal, "Renew now" → webhook fires → cancels_at
   cleared → subtitle disappears

### 5. Plan change in-app (validates the new /api/plan-change endpoint)

1. While on Active{Monthly}, scroll to Buy card → click **"Switch to
   Annual"**. NOTE: do NOT expect a Stripe Checkout window to open —
   this path now hits `/api/plan-change` directly.
2. Status row updates to **"Plan switched to annual. Stripe will
   prorate the difference on the next invoice."**
3. After ~1.5s the License page auto-refreshes; badge shows
   **"Active — annual"**.
4. Stripe Dashboard → Customers → look at the subscription:
   - Same `sub_xxx` id, new `price_…annual` line item
   - Upcoming invoice has TWO line items: a credit for unused monthly
     time + a debit for the new annual prorated to today
5. Try the reverse (Annual → Monthly): same flow, opposite direction.
   Stripe will issue a credit on the next invoice for the unused
   annual time. (Won't refund cash; it sits as account credit.)
6. NEGATIVE: while on Active{Lifetime}, no Monthly/Annual buttons are
   shown at all (lifetime is the ceiling). The Buy card is hidden.
7. NEGATIVE: while on Trial, clicking Annual goes through the regular
   Checkout flow (not /api/plan-change) — there's no sub to update.

## What you can test programmatically

### Option A: war-test (no Stripe CLI auth needed, recommended)

`scripts/war-test-staging.mjs` forges signed Stripe webhook events
itself (HMAC-SHA256 with the staging webhook secret) and POSTs them
straight at the staging Worker. Queries D1 via WSL+wrangler to assert
the resulting state. **15 scenarios, 17 assertions, ~26s runtime, all
green as of `228d587`.**

```powershell
$env:STRIPE_WHSEC = 'whsec_test_warroom_45e02445c2814ad1bc3a9eee7ba716e3'
node scripts\war-test-staging.mjs                # all 15
node scripts\war-test-staging.mjs plan-change    # just /api/plan-change validation
node scripts\war-test-staging.mjs tier-flip      # just price-flip → tier mirror
```

**N.B.** the `STRIPE_WHSEC` above is the value I set on staging via
`wrangler secret put STRIPE_WEBHOOK_SECRET --env staging` — it
overwrites the real Stripe-dashboard whsec, so real Stripe events
sent to staging right now won't validate. To restore: copy the
"Signing secret" from Stripe Dashboard → Developers → Webhooks →
your staging endpoint → Reveal, then re-`wrangler secret put` it.

### Option B: stripe-smoke.sh (Stripe CLI, real events)

`scripts/stripe-smoke.sh` runs 7 scenarios using the actual Stripe
CLI's `stripe trigger`. Requires `stripe login` (Stripe CLI
authenticated). Slower than war-test but exercises real Stripe
event payload shapes:

```bash
cd /mnt/c/code/pai-voice
./scripts/stripe-smoke.sh           # all 7 scenarios
./scripts/stripe-smoke.sh dedup     # just the dedup
./scripts/stripe-smoke.sh refund    # just the refund flow
```

## Edge cases NOT yet covered (to consider for Mon)

These exist as design but lack automated test coverage:

1. **Resume after refund-revoke**: user re-purchases after a refund
   should create a NEW license (different lid). Current code does this
   (insertLicense always uses `ulid()`); no test asserts the lid is
   different from the revoked one.
2. **Tampered token**: user edits `license.json` payload manually →
   verify_token rejects, status = Invalid. Covered by client tests
   in `core/src/license.rs::tests`, NOT by an end-to-end test.
3. **Offline grace expiry**: license still valid in Stripe but client
   hasn't refreshed in `max_offline + 7` days → status = Suspended.
   Covered by `core/src/license.rs::tests::suspended_when_offline_too_long`.
4. **GDPR delete**: 2-step OTP. Covered by `delete.test.ts` × 9 cases.
5. **Stripe LIVE webhook secret with TEST priv mismatch**: signature
   verify fails → 400. Covered indirectly by `stripe-signature.test.ts`.

The above are reasonably covered by unit tests; if you want a
dedicated e2e for any of them, add to `stripe-smoke.sh` as a new
`scenario_*` function.

## Files touched (delta from `ad2bde7`)

```
backend/src/db.ts                      +53  (find unconsumed code, updateLicenseTierBySubscription)
backend/src/handlers/trial.ts          +18  (dedup wired in)
backend/src/handlers/stripe.ts         +55  (dedup + price-flip → tier mirror)
backend/src/handlers/plan-change.ts    NEW  (POST /api/plan-change)
backend/src/index.ts                   +4   (route)
backend/tests/_d1-mock.ts              +14  (handler for new query shape)
backend/tests/trial.test.ts            +60  (3 new + 1 updated tests)
core/src/license.rs                    +32  (change_plan async fn)
core/src/ffi.rs                        +48  (dimmy_license_plan_change)
platforms/windows/Dimmy.Windows/App.xaml.cs                  +75  (foreground+handoff)
platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml.cs +138 (refresh+tier-aware+plan-change)
platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml    +21  (button names+hint)
platforms/windows/Dimmy.Windows/Interop/DimmyNative.cs       +5   (P/Invoke)
platforms/windows/Dimmy.Windows/Services/LicenseService.cs   +15  (PlanChangeAsync)
platforms/macos/Dimmy/DimmyFFI.h                             +8   (decl)
platforms/macos/Dimmy/Managers/DimmyCore+License.swift       +15  (Swift wrapper)
platforms/macos/Dimmy/Views/Settings/MacLicensePage.swift    +108 (tier-aware mirror+plan-change)
scripts/stripe-smoke.sh                NEW  (Stripe CLI orchestrator)
scripts/war-test-staging.mjs           NEW  (HMAC-signed event battery, 15 scenarios)
docs/dev/licensing-overnight-2026-05-04.md  UPDATED  (this file)
```

## Known limitations / FYIs

- **Mac code untested locally**: I have no Xcode here. Win pattern
  applied verbatim; verify on Mac when next opening Xcode.
- **Customer Portal availability**: assumes you've enabled the Stripe
  Billing Customer Portal in TEST dashboard with downgrade/cancel
  actions allowed. If you haven't, "Manage subscription" → 502.
- **`refund.created` requires `STRIPE_SECRET_KEY` set on staging** —
  it IS set (verified). Cancel-sub in the duplicate-purchase gate
  also needs it; same secret.
- **`charge.refunded` STILL handled** as defense alongside
  `refund.created`. Drop in a follow-up after `refund.created` has
  30 days in prod. Both events are idempotent at the DB level
  (`stripe_events` per-event_id dedup + status filter).

## What I did NOT touch

- Prod Worker (`license.dimmy.app`) — completely untouched. Its
  Stripe TEST webhook secret is now stale (Stripe events go only to
  staging) but the prod Worker's licenses + customers stay frozen
  exactly as they were.
- Prod keypair — unchanged, your existing prod-keyed binaries
  continue to work.
- Linux UI — no License page there to update.

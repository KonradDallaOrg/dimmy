# Dimmy Staging — tester guide

> **Devs / maintainers**: this page is for testers installing the
> `Dimmy-Staging` build (the side-by-side installer produced by
> `staging-tester.yml`). For the broader question of "which workflow
> ships to which endpoint", see the **Release pipelines** table in
> [`../RELEASING.md`](../RELEASING.md#release-pipelines--what-triggers-what)
> — there are three workflows that look similar and only one of them
> produces this side-by-side install.

This is the **staging** flavor of Dimmy. It looks and works like the
real app but ships with these differences:

- **Licensing server**: `license-staging.dimmy.app` (separate D1, separate
  Ed25519 keypair, separate Stripe TEST mode).
- **Payments**: Stripe TEST mode — your card is never charged. Use:
  - Card: `4242 4242 4242 4242`
  - Expiry: any future date (e.g. `12 / 30`)
  - CVC: any 3 digits (e.g. `123`)
  - ZIP: any
- **Watermark**: yellow "STAGING BUILD" stripe in the Settings sidebar
  + " · STAGING" suffix on the version number, so you can never confuse
  it with prod.
- **Side-by-side**: separate install path, separate config dir, separate
  single-instance mutex. Both flavors can run at the same time.

## Install

### Windows

Download `Dimmy-Staging-win-Setup.exe` from the latest staging release
on GitHub: <https://github.com/KonradDallaOrg/dimmy/releases?q=staging>.

Double-click. Velopack installs to `%LOCALAPPDATA%\Dimmy-Staging\`. A
"Dimmy Staging" entry appears in the Start menu next to (not replacing)
your existing "Dimmy" install. Auto-update will pull future staging
builds via the `staging` channel; prod won't see them and vice versa.

### macOS (Apple Silicon only)

Download `Dimmy-Staging-macos-arm64.dmg`. Open the DMG, drag
"Dimmy Staging.app" to Applications. Bundle id is `com.dimmy.staging`,
fully distinct from prod's `com.dimmy.app`.

First launch: right-click → Open (Gatekeeper warning), or run:

```bash
xattr -d com.apple.quarantine "/Applications/Dimmy Staging.app"
```

## Where things live

| What | Win | Mac |
|---|---|---|
| App install | `%LOCALAPPDATA%\Dimmy-Staging\` | `/Applications/Dimmy Staging.app` |
| Config + license | `%APPDATA%\dimmy-staging\` | `~/Library/Application Support/dimmy-staging/` |
| Logs | `%APPDATA%\dimmy-staging\dimmy.log` | `~/Library/Application Support/dimmy-staging/dimmy.log` |

## Test scenarios

The license/payment flows you can exercise without spending a real cent:

### 1. Trial → magic-link activation

1. Settings → License → enter your email → **Activate**
2. Email lands in your inbox (subject ends with last 6 chars of the
   activation code, e.g. `Activate your Dimmy trial · Mn8kqQ`)
3. Click magic link → Dimmy comes to foreground, License tab shows
   "Trial — 14 day(s) left"

### 2. Buy any tier

1. Settings → License → click `Monthly` / `Annual` / `Lifetime`
2. Stripe Checkout opens in browser
3. Pay with `4242 4242 4242 4242` + any future expiry + any CVC
4. Browser lands on success page → "Payment confirmed. Check your inbox."
5. Click the email magic link → tier badge updates

### 3. Plan change (monthly ⇄ annual)

While on `Active{monthly}`, click **Switch to Annual** in Settings →
License. The app calls `/api/plan-change` (not Checkout), Stripe
mutates the subscription in place with proration, the next email
subject says "annual". Same in reverse.

### 4. Cancel via Stripe Portal

While on `Active{any paid}`, click **Manage subscription**. Stripe
Customer Portal opens. Cancel → webhook fires → license shows
"Cancels on YYYY-MM-DD" subtitle, status remains Active until period
end. Click "Renew" in the portal to undo.

### 5. Upgrade sub → Lifetime

While on `Active{monthly|annual}`, click **Upgrade to Lifetime**.
Stripe Checkout (one-time payment), pay, the webhook handler
detects the existing sub, cancels it, mutates the same license row
to `tier=lifetime`. Single license row, no duplicate.

### 6. Duplicate-purchase gate (negative test)

While on `Active{annual}`, try to buy `Annual` again from a
bookmarked Stripe Payment Link. The webhook handler blocks the
duplicate, refunds the new sub, and emails a magic link for the
existing license. New row count: still one. (The pre-checkout gate
in the in-app Buy button blocks BEFORE Stripe charges — this scenario
is the defense-in-depth path for direct Stripe links.)

### 7. Refund → revoke

In the Stripe Customer Portal (or via Stripe Dashboard for the test
account), refund the latest charge. The `charge.refunded` webhook
revokes the license; client status becomes `Revoked` after the next
`/api/refresh`.

## Reporting bugs

File issues at <https://github.com/KonradDallaOrg/dimmy/issues> and
include:

- Tag of the staging build (visible in Settings → About: e.g.
  `v0.6.27-staging.3 · STAGING`)
- OS + version
- Steps that reproduce
- `dimmy.log` from the config dir above (`%APPDATA%\dimmy-staging\` on
  Win, `~/Library/Application Support/dimmy-staging/` on Mac)

## Rollback

Uninstalling staging does NOT touch your prod install or its config.
On Windows: Settings → Apps → "Dimmy Staging" → Uninstall.
On macOS: drag `Dimmy Staging.app` to the bin and remove the config
dir if you want a clean slate.

## Rate limit (devs / QA running intensive smoke tests)

The staging Worker rate-limits the public endpoints to stop trivial
abuse:

| Endpoint | Limit | Per |
|---|---|---|
| `/api/trial/start` | 5 / day | IP |
| `/api/checkout/create` | 10 / hour | IP |
| `/api/plan-change` | 5 / hour | license token |
| `/api/billing-portal` | 10 / hour | license token |

If you hammer the war-test or the UI buy flow more than that you'll
hit `429 Too Many Requests`. Clear the staging counters with:

```bash
wsl bash -lc "cd /mnt/c/code/pai-voice/backend && \
  wrangler d1 execute dimmy-licensing-staging --env staging --remote \
  --command 'DELETE FROM rate_limits'"
```

(Staging only — never run this on prod.)

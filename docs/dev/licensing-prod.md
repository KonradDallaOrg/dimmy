# Licensing — production migration runbook

> **Status (2026-05-01):** PoC validated end-to-end (`feat/licensing-poc` → PR #43). Cloudflare Worker + Stripe webhook + Resend integration built (this PR). Migration to production needs the manual ops steps below.

> **Status, checked against the code on 2026-11-11:** The server half (Worker, D1, Stripe webhook, Resend) is not in this repo (it is in the private `dimmy-backend` repo) and was not checked. The app half was: `core/src/license.rs` and `core/src/ffi.rs` know four tiers (`trial`, `monthly`, `annual`, `lifetime`; there is no `3year`) and call `/api/trial/start`, `/api/activate`, `/api/refresh`, `/api/checkout/create`, `/api/billing-portal`, `/api/plan-change`, `/api/devices/list`, `/api/devices/deactivate`, `/api/config` and `/api/lead`. Nothing in this repo calls `/api/account/delete`. `release.yml` already injects the production public key and server URL. The Rust licensing server (`licensing_server`, feature `licensing-server`) no longer exists here. The sections below are corrected where they said otherwise.

## What's in scope of this runbook

Going from "PoC running on localhost" to "production licensing live at `license.dimmy.app`, Stripe processing real payments, Resend sending real emails".

## Architecture summary

```
                            ┌─────────────────────┐
   Marketing site           │  buy.stripe.com/…   │ ← user pays
   ($19/$39 buttons) ──────►│  (Stripe Checkout)  │
                            └──────────┬──────────┘
                                       │ webhook
                                       ▼
                            ┌─────────────────────┐    ┌──────────┐
                            │  Cloudflare Worker  │───►│  Resend  │ ← magic link
                            │  license.dimmy.app  │    └──────────┘
                            └──────────┬──────────┘
                                       │ D1
                                       ▼
                            ┌─────────────────────┐
                            │  dimmy-licensing    │
                            │  (D1 / SQLite)      │
                            └─────────────────────┘

Dimmy app  ──── /api/trial/start, /api/activate, /api/refresh, /api/checkout/create,
                /api/billing-portal, /api/plan-change, /api/devices/list,
                /api/devices/deactivate, /api/config, /api/lead ─────► Worker
```

The app starts a purchase through `/api/checkout/create` (the server returns the Stripe Checkout URL; `license::create_checkout`), not only through Payment Links on the marketing site. While `/api/config` reports `sales_open: false` (or fails) the app shows "I'd buy this" (`/api/lead`) instead of Buy. No code in this repo calls `/api/account/delete`.

Code lives in the **private** repo [`KonradDallaOrg/dimmy-backend`](https://github.com/KonradDallaOrg/dimmy-backend) (moved out of this public repo on 2026-05-17 to keep handler logic + Stripe wire shapes private). Clone it as a sibling of this repo (`~/code/dimmy-backend/`) and see its `README.md` for the per-file map. Production secrets stay in Cloudflare Worker Secrets + 1Password, never in either repo.

## Manual ops checklist (for Konrad)

The numbered items must be done by you — they require login credentials, payment cards, DNS, or domain ownership that I can't access.

### 1. Generate the production Ed25519 keypair (5 min, security-critical)

This is the **most important** step. Anyone with the private key can mint valid Dimmy licenses, so it never leaves the Cloudflare Worker secrets store.

The Rust `licensing_server` binary that this step used to boot is gone: there is no `core/src/bin/licensing_server.rs`, no `core/src/license_server.rs` and no `licensing-server` cargo feature, and `core/Cargo.toml` says server-side signing is done in the Cloudflare Worker (TypeScript). Generate the keypair the way `LICENSING_TODO.md` step 1 does, with Node's `crypto.generateKeyPairSync("ed25519")`: the JWK `x` is the public key and `d` the private key, both base64url. The public key must be 43 characters (`core/build.rs` warns otherwise). Print the pair once, copy it to the secret manager, and keep nothing on disk.

Now you have:
- **Public key** → goes to GitHub Secret `DIMMY_LICENSE_PUBKEY` for release.yml AND to Worker secret `DIMMY_LICENSE_PUBKEY` for refresh-time verify. (Staging has its own pair: GitHub Secret `DIMMY_LICENSE_PUBKEY_STAGING` for `staging-tester.yml` and `agent-preview.yml`; `staging-auto-update.yml` hardcodes the staging public key inline.)
- **Private key** (extracted via xxd) → ONLY to Worker secret `DIMMY_LICENSE_PRIVKEY`. NEVER commit, NEVER paste in chat, NEVER store outside the secret manager.

**Backup**: 1Password / Bitwarden vault entry "Dimmy License Privkey (rotate-only)". Encrypted with master password. The point is so that if Cloudflare ever loses the secret, you can restore it without invalidating every existing license. Without backup → all users must reactivate.

### 2. Cloudflare setup (10 min)

```bash
cd ~/code/dimmy-backend    # private repo, sibling of this one; there is no backend/ here
npm install
npx wrangler login
npx wrangler d1 create dimmy-licensing
# → copy the printed `database_id` into wrangler.toml line `database_id = ...`

npx wrangler secret put DIMMY_LICENSE_PRIVKEY    # paste the priv from step 1
npx wrangler secret put DIMMY_LICENSE_PUBKEY     # paste the pub from step 1
npx wrangler secret put STRIPE_WEBHOOK_SECRET    # placeholder until step 3
npx wrangler secret put RESEND_API_KEY           # placeholder until step 4

npx wrangler d1 migrations apply dimmy-licensing --remote
npx wrangler deploy
# → outputs the worker URL like https://dimmy-licensing.<account>.workers.dev
```

**DNS** (Cloudflare Pages → custom domains, or DNS dashboard):
- Add CNAME: `license.dimmy.app` → `dimmy-licensing.<account>.workers.dev`
- Update `wrangler.toml` `PUBLIC_URL = "https://license.dimmy.app"` and re-deploy.

### 3. Stripe setup (15 min)

In Stripe Dashboard:

1. **Products → New product**, one per paid tier the app can request (`core/src/license.rs::Tier`): `monthly` (recurring), `annual` (recurring), `lifetime` (one-time; its token is valid 1095 days). The 2026-05-01 text of this step listed "Annual License" at €19 one-time and "3-Year License" at €39 one-time. The code has no `3year` tier and documents Monthly and Annual as recurring subscriptions; the amounts cannot be checked from this repo.
   - → copy each `price_…` ID

2. (Merged into step 1: the app sends `tier` = `monthly`, `annual` or `lifetime` to `/api/checkout/create`.)

3. **Tax settings**:
   - Enable Stripe Tax (€0.50/transaction or 0.5% minimum, whichever higher).
   - Register your business location: Italy (IT).
   - Stripe will automatically apply correct EU VAT rates per customer location.

4. **Payment Links**:
   - Create a Payment Link for each price.
   - Settings: collect email (required), collect address (required for VAT).
   - Copy the `https://buy.stripe.com/…` URLs → these go on the marketing site.

5. **Webhooks → Add endpoint**:
   - URL: `https://license.dimmy.app/api/stripe/webhook`
   - Events to send: `checkout.session.completed`, `charge.refunded` (older list; `license.rs` says `valid_until` is bumped on each `invoice.paid` webhook, and the live-mode section below says 6 events. The handler list lives in the private repo and was not checked)
   - → copy the signing secret (`whsec_…`)

6. Update Cloudflare:
   ```bash
   echo "whsec_…" | npx wrangler secret put STRIPE_WEBHOOK_SECRET
   ```

7. Update `wrangler.toml`:
   ```toml
   STRIPE_PRICE_ANNUAL = "price_…"
   STRIPE_PRICE_3YEAR  = "price_…"
   ```
   and `npx wrangler deploy` again.

### 4. Resend setup (10 min)

In Resend Dashboard:

1. **Domains → Add domain** `dimmy.app`.
2. Resend prints a list of DNS records (TXT for SPF, DKIM, DMARC).
3. Add those records in Cloudflare DNS.
4. Wait ~5 min for verification (Resend auto-checks).
5. **API Keys → Create**:
   - Name: "Dimmy licensing prod"
   - Permission: `email.send`
   - → copy `re_…`
6. Update Cloudflare:
   ```bash
   echo "re_…" | npx wrangler secret put RESEND_API_KEY
   ```

### 5. GitHub Actions secret (2 min)

For release.yml to embed the public key in shipped binaries:

```bash
gh secret set DIMMY_LICENSE_PUBKEY --body "<base64url-pub-from-step-1>"
```

Or via web: Settings → Secrets and variables → Actions → New repository secret.

`release.yml` already injects it: `DIMMY_LICENSE_PUBKEY` from this secret plus `DIMMY_LICENSE_SERVER_URL=https://license.dimmy.app`, with the `license-client` feature on. `core/build.rs` aborts a release build when `license-client` is on and the key is empty, and when the key is set without a server URL.

### 6. VAT-OSS registration (one-time, ~30 min)

Required for legally collecting VAT on EU consumer software sales. Do this BEFORE you take the first payment.

1. Login to **Agenzia delle Entrate** (Italy) with SPID / CIE.
2. Search "OSS" or "Sportello Unico OSS" → "Iscrizione regime UE".
3. Fill the form (P.IVA, business activity codes, etc.).
4. After approval (~1-3 days), you're registered for cross-EU VAT collection.
5. Quarterly: download a CSV from Stripe Tax of EU sales by country, file in OSS portal. Stripe Tax can also auto-generate the report.

If you don't have P.IVA yet: open one as `regime forfettario` (5%-15% flat) — much simpler than ordinaria. Or as a SRL if you expect >€85k/y revenue.

### 7. Marketing site (separate; ~3-4 hours)

Out of scope for this PR (it's a static site, separate codebase). Required pages:
- Landing (download + buy buttons → Stripe Payment Links)
- About / Privacy / TOS / Refund policy
- Status (link to license.dimmy.app/api/health for transparency)

When ready, add CNAME `dimmy.app → cloudflare-pages-domain`.

## Code path summary (what's automatic)

These are done in code in this PR (no manual steps after the env above is set):

- ✅ Worker scaffold + 7 endpoints
- ✅ Token sign/verify via Web Crypto API
- ✅ D1 schema migrations
- ✅ Stripe webhook signature verification (HMAC-SHA256, 5min tolerance)
- ✅ Stripe checkout.session.completed → license + email
- ✅ Stripe charge.refunded → license revoke
- ✅ Idempotency (stripe_events table)
- ✅ Resend integration (with stdout dev fallback)
- ✅ GDPR data-deletion (two-step OTP)
- ✅ Audit log

## Rollout sequence (recommended order)

Do them in this order so each step is independently testable:

1. **Generate keys** (step 1 above). Backup. Verify both halves are reachable.
2. **Deploy Worker without Stripe / Resend** — set both secrets to placeholder strings. Validate `/api/health` works, `/api/trial/start` returns OK and prints magic link to Worker logs (since RESEND_API_KEY is empty → falls back to console).
3. **CLI E2E against the Worker** — same 7 scenarios from the PoC, but pointing the CLI at `--server https://license.dimmy.app`. Should pass identically.
4. **Stripe in test mode** — use Stripe test cards (`4242 4242 4242 4242`), test the full checkout → webhook → license issuance → magic link → activation flow. Switch to live mode only when the test mode flow is bulletproof.
5. **Resend with verified domain** — send a real activation email to your own address. Click the link. Should activate.
6. **GitHub Actions release** — push a no-op release-candidate tag (shape `vX.Y.Z-rc.N`, with the dot; `scripts/check-release-tag.sh` enforces it), verify the release build embeds the pubkey. The key is compiled into `dimmy_lib.dll`, not the EXE (`Get-Content` / `strings` the DLL for the pubkey prefix); `build.rs` also prints "DIMMY_LICENSE_PUBKEY embedded ok (43 chars, prefix=…)" in the build log.
7. **Soft launch** — enable purchase on the marketing site. Watch Stripe + Worker logs for the first 24h.

## From test mode to live mode (when you're ready to take real money)

Test mode setup is the rehearsal — same code path, fake card numbers,
zero risk. Live mode is the same Worker pointed at the live half of
the same Stripe account. Sequence (~30 min total):

### 1. Stripe — clone products into live mode

In Stripe Dashboard, top-right toggle "Test mode" → OFF (= live).
The 3 test products + Payment Links you already have don't carry
over automatically. Two options:

- **Manual** (3 min): repeat step 3 (Stripe) of `LICENSING_TODO.md` from
  the live dashboard — same 3 products, same prices, same
  `metadata.tier`. Copy the new `price_…` IDs and Payment Link URLs.
- **CLI** (5 min): use `stripe-cli` to clone:
  ```bash
  stripe --api-key <test_key> products list
  # then for each product:
  stripe --api-key <live_key> products create --name "..." --description "..."
  stripe --api-key <live_key> prices create --product prod_… --unit-amount … --currency eur
  stripe --api-key <live_key> payment_links create --line-items[0].price=price_… --metadata.tier=…
  ```

In live mode, also:
- Enable **Stripe Tax** (Settings → Tax → Italy). 0.5% / transaction
  but legally required for EU B2C.
- Enable **fraud rules** (Radar): default settings are sane; only
  tighten if you see fraudulent disputes.

### 2. Live webhook endpoint

Live mode has a separate webhook list. Add an endpoint with the
**same** URL (`https://license.dimmy.app/api/stripe/webhook`) but
the live signing secret will be a fresh `whsec_…`. Subscribe to
the same 6 events.

### 3. Swap Worker secrets

```bash
cd ~/code/dimmy-backend    # private repo; there is no backend/ here
echo "sk_live_…"  | npx wrangler secret put STRIPE_SECRET_KEY
echo "whsec_…"    | npx wrangler secret put STRIPE_WEBHOOK_SECRET
```

The Resend API key + license keypair (PRIV/PUB) stay the same — they
have no notion of test vs live.

### 4. Update wrangler.toml with live price IDs

The 3 `STRIPE_PRICE_{MONTHLY,ANNUAL,LIFETIME}` env vars are the
fallback when `metadata.tier` is missing. Update to the live IDs:

```toml
STRIPE_PRICE_MONTHLY  = "price_<live>"
STRIPE_PRICE_ANNUAL   = "price_<live>"
STRIPE_PRICE_LIFETIME = "price_<live>"
```

```bash
npx wrangler deploy
```

### 5. Smoke test live, low risk

Use a real card with a tiny amount (Stripe accepts €1 minimum). Do
the full checkout for `monthly` (lowest commitment). Verify in D1:

```bash
npx wrangler d1 execute dimmy-licensing --remote \
  --command "SELECT license_id, tier, status, stripe_customer_id FROM licenses ORDER BY issued_at DESC LIMIT 1"
```

Then refund yourself from the Stripe dashboard (charge → Refund) and
verify the license flips to `revoked` via the `charge.refunded`
webhook. ~€1 spent on a real card to validate the live pipeline end-
to-end. Worth it.

### 6. Re-build binaries (new pubkey is the same — no code change)

The pubkey embedded in current binaries works in both test and live
mode (it's the cryptographic verifier, agnostic to which Stripe is
talking). No rebuild required for the toggle. Only when you ROTATE
the keypair would you need to rebuild + redistribute.

### 7. Marketing site → live Payment Links

Replace any `https://buy.stripe.com/test_…` links on the marketing
site with the live equivalents from step 1.

### 8. (Optional) Disable test mode endpoint

Keep the test webhook endpoint disabled but not deleted in case you
need to repro a bug from a test transaction later. Stripe doesn't
charge for inactive endpoints.

## Rollback plan

If anything goes catastrophically wrong post-launch:

- **Hot-fix code bug**: redeploy Worker with `wrangler deploy` (~10 sec).
- **Compromised private key**: rotate (step 1) → re-deploy → push a release with new pubkey embedded → all users must reactivate. Painful but bounded.
- **Stripe issue**: pause Payment Links in Stripe Dashboard. App keeps working for existing users; new purchases blocked.
- **D1 corruption**: D1 has automatic snapshots — restore from console, ~5 min downtime.

## Operating cost estimate (monthly)

| Service | Plan | Estimated cost |
|---|---|---|
| Cloudflare Workers | Free tier (100k req/day) | €0 |
| Cloudflare D1 | Free tier (5M reads/month) | €0 |
| Cloudflare Pages (marketing) | Free tier | €0 |
| Resend | $20/mo (10k emails) | ~€19 |
| Domain `dimmy.app` | Annual ÷ 12 | ~€1 |
| Stripe Tax | 0.5% per transaction (no fixed) | variable |
| Stripe Checkout | 1.4% + €0.25 (EU) | variable |
| **Fixed monthly** | | **~€20** |

Profitable from the first sale.

## Feature flags / staged rollout

For the first week post-launch:
- Cap `max_devices` per license at 5 (already configured).
- Disable `/api/account/delete` automated flow — manually process the first few requests via support email so we can sanity-check the audit log.
- Monitor `audit_log` for anomalies (mass activations, repeated failed activations from same IP).

After 1 week without incidents, lift the manual-review on deletes.

## Open follow-ups (post-launch, not blockers)

- Replace the activation-email template with a deletion-OTP-specific one (currently reuses the activation template).
- ~~Add a "device manager" endpoint + UI~~ Done in the app: `dimmy_license_devices_list` / `dimmy_license_device_deactivate` (`/api/devices/list`, `/api/devices/deactivate`), used by Settings → License on Windows and macOS. The 60-day auto-prune is server-side and was not checked.
- ~~Replace one-shot purchases with subscriptions for the annual tier?~~ Done as far as the app is concerned: `license.rs` treats `monthly` and `annual` as recurring subscriptions (plan switch through `/api/plan-change`, `cancels_at` claim) and `lifetime` as the one-time purchase.
- Migrate audit-log retention to 7 years (legal default for accounting).

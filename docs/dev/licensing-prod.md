# Licensing — production migration runbook

> **Status (2026-05-01):** PoC validated end-to-end (`feat/licensing-poc` → PR #43). Cloudflare Worker + Stripe webhook + Resend integration built (this PR). Migration to production needs the manual ops steps below.

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

Dimmy app  ──── /api/activate, /api/refresh, /api/account/delete ─────► Worker
```

Code lives in `backend/`, see [`backend/README.md`](../../backend/README.md) for the per-file map.

## Manual ops checklist (for Konrad)

The numbered items must be done by you — they require login credentials, payment cards, DNS, or domain ownership that I can't access.

### 1. Generate the production Ed25519 keypair (5 min, security-critical)

This is the **most important** step. Anyone with the private key can mint valid Dimmy licenses, so it never leaves the Cloudflare Worker secrets store.

```bash
# From core/ — boot the licensing server with a one-time data dir,
# copy the keys, then delete the dir.
cd core
DIMMY_LICENSING_DATA=/tmp/dimmy-prod-keygen \
  cargo run --bin licensing_server --features licensing-server \
  &
sleep 3
# Server prints:
#   DIMMY_LICENSE_PUBKEY=<base64url-pub-32-bytes>
# Find the priv:
xxd -p -c 32 /tmp/dimmy-prod-keygen/keys.bin | python3 -c \
  'import sys,base64; print(base64.urlsafe_b64encode(bytes.fromhex(sys.stdin.read().strip())).rstrip(b"=").decode())'
# That printed the priv as base64url-no-pad. Copy it.

# Stop server.
kill %1

# Remove keys from disk — never to be seen again.
shred -u /tmp/dimmy-prod-keygen/keys.bin
rm -rf /tmp/dimmy-prod-keygen
```

Now you have:
- **Public key** (printed by server) → goes to GitHub Secret `DIMMY_LICENSE_PUBKEY` for release.yml AND to Worker secret `DIMMY_LICENSE_PUBKEY` for refresh-time verify.
- **Private key** (extracted via xxd) → ONLY to Worker secret `DIMMY_LICENSE_PRIVKEY`. NEVER commit, NEVER paste in chat, NEVER store outside the secret manager.

**Backup**: 1Password / Bitwarden vault entry "Dimmy License Privkey (rotate-only)". Encrypted with master password. The point is so that if Cloudflare ever loses the secret, you can restore it without invalidating every existing license. Without backup → all users must reactivate.

### 2. Cloudflare setup (10 min)

```bash
cd backend
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

1. **Products → New product**:
   - Name: "Dimmy — Annual License"
   - Price: €19 EUR, one-time
   - → copy the `price_…` ID

2. Repeat for "Dimmy — 3-Year License", €39, one-time → copy the price ID.

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
   - Events to send: `checkout.session.completed`, `charge.refunded`
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

The release.yml change to inject this is in [`pr-licensing-prod-migration`](TODO link) — review + merge separately.

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
6. **GitHub Actions release** — push a no-op tag (e.g. v0.6.27-rc1), verify the release-build embeds the pubkey (`Get-Content` the EXE strings | grep for the pubkey prefix).
7. **Soft launch** — enable purchase on the marketing site. Watch Stripe + Worker logs for the first 24h.

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
- Add a "device manager" endpoint + UI: list active devices, click to revoke. (Today auto-prune at 60 days handles 95% of cases.)
- Replace one-shot purchases with subscriptions for the annual tier? Only if revenue ≥ ~€500/mo to justify the extra Stripe complexity.
- Migrate audit-log retention to 7 years (legal default for accounting).

# Dimmy licensing backend (Cloudflare Worker)

Production deployment of the licensing v2 architecture validated in [`core/src/license_server.rs`](../core/src/license_server.rs). Same wire shapes as the Rust PoC, different runtime: Cloudflare Worker + D1 instead of axum + SQLite.

## Layout

```
backend/
├── wrangler.toml           Cloudflare Worker config (D1 binding, env vars, secrets)
├── package.json            wrangler + workers-types dev deps
├── tsconfig.json
├── migrations/
│   └── 0001_initial.sql    D1 schema (licenses, devices, activation_codes,
│                           stripe_events idempotency, audit_log)
└── src/
    ├── index.ts            router + Env type
    ├── crypto.ts           Ed25519 sign/verify via Web Crypto API,
    │                       base64url, ULID, activation-code generation
    ├── db.ts               type-safe D1 query helpers
    ├── email.ts            Resend integration (with stdout fallback)
    └── handlers/
        ├── trial.ts        POST /api/trial/start — provision trial
        ├── activate.ts     GET  /api/activate    — exchange code → token
        ├── refresh.ts      POST /api/refresh     — bump last_seen, re-issue
        ├── stripe.ts       POST /api/stripe/webhook — Stripe events
        ├── status.ts       GET  /api/license/status — debug introspection
        └── delete.ts       POST /api/account/delete — GDPR erasure
```

## Deploy

### One-time setup

```bash
cd backend
npm install
wrangler login

# Create the D1 database (returns an `id` — paste into wrangler.toml).
wrangler d1 create dimmy-licensing

# Generate the Ed25519 keypair LOCALLY, push private to Worker secret.
# The PoC server prints both halves on first boot; reuse from there
# OR run a fresh `cargo run --bin licensing_server --features licensing-server`
# and grab them from stdout. Public key embeds in client builds.
echo "<base64url-priv>" | wrangler secret put DIMMY_LICENSE_PRIVKEY
echo "<base64url-pub>"  | wrangler secret put DIMMY_LICENSE_PUBKEY

# Stripe (after you've created the Webhook endpoint in Stripe dashboard).
wrangler secret put STRIPE_WEBHOOK_SECRET   # whsec_…

# Resend (after you've verified the dimmy.app sender domain).
wrangler secret put RESEND_API_KEY          # re_…

# Apply migrations + deploy.
wrangler d1 migrations apply dimmy-licensing --remote
wrangler deploy
```

### Iterate

```bash
# Local dev with file-backed D1 (no remote calls, fast iteration):
wrangler dev

# Tail production logs:
wrangler tail
```

## Wire-shape compatibility with the Rust client

The token format and API request/response shapes are **identical** to what the Rust client (`core/src/license.rs`) expects, so the same `cargo build --bin license_cli --features license-cli,license-client` works against either:
- the local PoC server (`http://localhost:8787`),
- this Worker (`https://license.dimmy.app` or whatever `PUBLIC_URL` resolves to).

The only client-side change for production is pointing `--server` at the Worker URL instead of localhost.

## Endpoints

All return `application/json`. Errors follow `{"error": "human-readable string"}`.

| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/health` | Liveness probe |
| POST | `/api/trial/start` | `{ email }` → magic link via Resend, license created if not present |
| GET  | `/api/activate` | `?code=…&device_label=…` → consumes code, returns signed token |
| POST | `/api/refresh` | `{ token }` → bumps `last_seen`, re-issues token |
| POST | `/api/stripe/webhook` | Stripe events — checkout.session.completed creates licenses, charge.refunded revokes |
| GET  | `/api/license/status` | `?email=…` (debug) — list licenses + active devices |
| POST | `/api/account/delete` | GDPR erasure — two-step OTP flow |

## Stripe configuration

In Stripe Dashboard → Products:

1. Create **"Dimmy — Annual License"** at €19, recurring=no (one-shot).
2. Create **"Dimmy — 3-Year License"** at €39, recurring=no.
3. Note both `price_…` IDs and paste into `wrangler.toml` as `STRIPE_PRICE_ANNUAL` / `STRIPE_PRICE_3YEAR`.

In Stripe Dashboard → Developers → Webhooks:

4. Add endpoint `https://license.dimmy.app/api/stripe/webhook` (or your worker URL).
5. Subscribe to events: `checkout.session.completed`, `charge.refunded`.
6. Copy the signing secret (`whsec_…`) → `wrangler secret put STRIPE_WEBHOOK_SECRET`.

Marketing site links to:
- `https://buy.stripe.com/<your-link-id>` for annual
- `https://buy.stripe.com/<other-id>` for 3-year prepay

When checkout completes, Stripe POSTs to `/api/stripe/webhook` and we email the activation magic link.

## Resend configuration

1. Verify the `dimmy.app` sender domain in Resend dashboard (DNS TXT + DKIM records).
2. Create an API key with `email.send` scope.
3. `wrangler secret put RESEND_API_KEY`.

## Threat model recap

(Full details in [`docs/dev/licensing-poc.md`](../docs/dev/licensing-poc.md) § "Security walkthrough".)

| Threat | Mitigation in this Worker |
|---|---|
| Stripe webhook spoofing | HMAC-SHA256 sig verify w/ tolerance window |
| Webhook retry double-process | `stripe_events` idempotency table |
| OTP replay | Single-use codes (`UPDATE…WHERE consumed_at IS NULL`) |
| Brute-force activation codes | 32-char alphanumeric (≈190 bits) + per-license codes |
| GDPR — proof of erasure | `audit_log` retains the action, email_hash anonymised |
| Public key leak | Public key only verifies; private key in Worker secret |
| Private key leak | Rotate via `wrangler secret put` + new release embedding new pubkey |

## Migration story (PoC → this Worker)

The PoC's `core/src/license_server.rs` and this Worker share:
- token format,
- DB schema (only differences: `stripe_*` columns + `stripe_events` + `audit_log` are net new — additive),
- HTTP wire shapes.

Migration steps were:
1. **TypeScript port of Rust**: `axum::Router` → Worker fetch handler, `sqlx::SqlitePool` → `env.DB.prepare(...)`, `ed25519-dalek::Signer` → Web Crypto API `crypto.subtle.sign("Ed25519", …)`.
2. **Email**: stdout `println!` → Resend HTTP POST (with stdout fallback when no API key).
3. **Stripe**: brand-new (PoC had a fake `/api/license/issue` mock; production has real webhook).
4. **GDPR**: brand-new endpoint (out-of-scope for PoC).

The PoC `license_server.rs` stays in the Rust core for local dev / colleague onboarding — running `cargo run --bin licensing_server --features licensing-server` is much faster than `wrangler dev` for iteration on token logic.

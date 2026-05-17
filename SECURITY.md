# Security policy

## Reporting a vulnerability

If you find a security issue in Dimmy, please email **konrad.dalla@gmail.com** with a description and reproduction steps. Do not open a public GitHub issue for security reports — we want to fix the bug before it's broadcast.

We aim to acknowledge reports within 72 hours and ship a fix in the next release (or sooner, if the issue is critical).

## What's in this repo (and what isn't)

This repository is the **public** Dimmy desktop client (Rust core + Windows/macOS/Linux UI). The licensing backend lives in a **separate private repo** (`KonradDallaOrg/dimmy-backend`, Cloudflare Workers + D1 + Stripe). Production secrets are stored in Cloudflare Worker Secrets and 1Password — they are **never** committed to either repo.

### Public material (intentional)

- The Ed25519 **public key** that verifies license tokens (`avlM65...`). The matching private key never leaves Cloudflare/1Password.
- `license.dimmy.app` / `license-staging.dimmy.app` URLs — public HTTPS endpoints.
- The Cloudflare D1 `database_id` in `wrangler.toml` (an internal Cloudflare identifier — accessing the DB requires a Cloudflare API token, which is not in the repo).
- All Rust + C# + Swift + GTK source code.

### Not in this repo

- `*.env`, `*.env.*` files (gitignored, exception only for `.env.example` placeholder)
- Wrangler dev runtime files (`backend/.dev.vars`, `backend/.wrangler/`, `backend/.dev-data/`) — gitignored
- Any Stripe API key (`sk_live_*`, `sk_test_*`), webhook secret (`whsec_*`), Resend API key (`re_*`), Cloudflare API token
- The Ed25519 **private key** used by the Worker to sign license tokens
- The OAuth client secrets for any third-party integration

If you spot anything in the above list inside the repo or in git history, **please report it via the email above immediately**.

## Threat model — summary

| Threat | Mitigation |
|---|---|
| Stolen API key | All third-party keys live in Cloudflare Worker Secrets + 1Password; rotation playbook in `docs/dev/licensing-prod.md` |
| Forged license token | Ed25519 signature verification on the client; only the Worker has the private key |
| Replay of activation magic link | Single-use code stored in D1 with `redeemed_at` timestamp |
| Backend code disclosure | Backend is in a private repo; even the public PoC handlers reveal no cryptographic material |
| Compromised client binary | Velopack release artifacts are not code-signed by an EV cert today (planned); SHA256 of installers published in release notes |
| MITM on `license.dimmy.app` | HTTPS-only enforced in `core/src/provider.rs::validate_url`; Cloudflare TLS |
| Local secret exfil | API keys encrypted with AES-256-GCM via machine-specific derivation in `keys.enc` (see `core/src/keystore.rs`) |

## In-scope vs out-of-scope

**In scope** for security reports:
- Dimmy desktop client (this repo)
- Cloudflare Worker endpoints (`*.dimmy.app`)
- Stripe Checkout / webhook flow

**Out of scope**:
- Third-party services Dimmy connects to with the user's own API key (Groq, OpenAI, Anthropic, Gemini, Notion, etc.) — report to those vendors directly
- Reports requiring physical access to a logged-in user's device
- Social engineering attacks against the developer

## Bug bounty

There is no formal bounty program at this time. If you report a serious issue, expect a public acknowledgement in the next release notes (or anonymous, if you prefer).

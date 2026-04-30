# Licensing — local-server PoC

> **Status (2026-05-01):** PoC validated end-to-end on Windows + macOS.
> Architecture green-lit; production migration to Cloudflare Workers
> tracked separately.
>
> This document is the source of truth for the licensing v2 design.
> When (if) the architecture diverges, update here first.

## TL;DR

- Source build (no `DIMMY_LICENSE_PUBKEY` at compile time) → no licensing enforcement, ever.
- Pre-built binary → 14-day trial → paid tier (annual / 3-year prepay).
- Tokens are JWT-like, **Ed25519-signed**, **verified offline** in the client.
- Licensing server is the **source of truth** for trial state and device counts; the on-disk file is just a signed cache. Deleting the file does *not* reset the trial.
- Local features (BYOK STT/LLM, history, paste) **are never gated**. Only "Dimmy-managed cloud" + "auto-update" are.

## File map

```
core/src/license.rs                 client-side: types + offline verify + HTTP client
core/src/license_server.rs          server-side: axum + sqlx + sign + DB (gated)
core/src/bin/licensing_server.rs    server entry point
core/src/bin/license_cli.rs         CLI client to drive the whole flow
core/Cargo.toml                     features: licensing-server / license-cli / license-client
```

## Cargo features

| Feature | Pulls in | Used by |
|---|---|---|
| `licensing-server` | axum, sqlx-sqlite, ed25519-dalek, sha2, rand, anyhow, tracing-subscriber | server bin only |
| `license-cli` | clap (+ ed25519-dalek + sha2 from `license-client`) | CLI bin only |
| `license-client` | ed25519-dalek, sha2 | production cdylib (planned default) |

The server / CLI deps are heavy and **never** end up in the cdylib that ships with the Dimmy app.

## Token format

Three URL-safe-base64 segments separated by `.` (JWT-like):

```
HEADER.PAYLOAD.SIGNATURE
```

- **HEADER** — fixed: `{"alg":"EdDSA","typ":"DLT"}` (DLT = Dimmy License Token).
- **PAYLOAD** — `Claims` struct (see `core/src/license.rs`):

  | field | type | meaning |
  |---|---|---|
  | `v`   | u32     | schema version (currently 1) |
  | `lid` | string  | license_id (ULID) — stable across refresh |
  | `eh`  | string  | hex(SHA-256(email)) — plain email never on disk |
  | `tier`| string  | `trial` \| `annual` \| `3year` |
  | `iat` | i64     | issued-at unix seconds |
  | `exp` | i64     | expires-at unix seconds |
  | `max_offline` | u32 | days offline tolerated before suspend |
  | `did` | string  | device_id (ULID) — issued per activation |
  | `scope` | array | capabilities (`cloud`, `updates`) |

- **SIGNATURE** — Ed25519 over `header_b64.payload_b64` raw bytes.

Total size: ~440 bytes base64-encoded.

## Storage on disk

```
~/.config/dimmy/license.json           {"schema_version": 1, "token": "..."}
~/.config/dimmy/last_online_check.txt  unix epoch of last successful refresh
```

Same dir as `config.json`. Plain files — **no Keychain / Credential Manager** (the macOS Keychain prompts admin password for unsigned apps; horrible UX). Security comes from the Ed25519 signature, not from where the file lives.

## DB schema (server)

```sql
licenses (
  license_id    TEXT PRIMARY KEY,
  email_hash    TEXT NOT NULL,
  tier          TEXT NOT NULL,
  issued_at     INTEGER NOT NULL,
  valid_until   INTEGER NOT NULL,
  max_devices   INTEGER NOT NULL DEFAULT 5,
  status        TEXT NOT NULL DEFAULT 'active'
)

devices (
  device_id     TEXT PRIMARY KEY,
  license_id    TEXT NOT NULL,
  device_label  TEXT NOT NULL,
  issued_at     INTEGER NOT NULL,
  last_seen     INTEGER NOT NULL,
  status        TEXT NOT NULL DEFAULT 'active'
)

activation_codes (
  code          TEXT PRIMARY KEY,
  license_id    TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  consumed_at   INTEGER
)
```

Activation codes are short-lived (10 min TTL), single-use. Each successful `/api/activate` consumes the code + issues a token + records a device.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/health` | Liveness probe — returns `{"status":"ok"}` |
| POST | `/api/trial/start` | `{ email }` → mints activation code, prints magic link |
| GET  | `/api/activate` | `?code=…&device_label=…` → consumes code, returns signed token |
| POST | `/api/refresh` | `{ token }` → bumps `last_seen`, re-issues token |
| POST | `/api/license/issue` | `{ email, tier }` → simulates Lemon Squeezy webhook (PoC only) |
| GET  | `/api/license/status` | `?email=…` or `?license_id=…` → debug introspection |

In production the magic link is delivered via Resend; in the PoC it's printed to the server's stdout.

## Running locally

### Boot the server

```bash
cd core
cargo run --bin licensing_server --features licensing-server
```

First boot:
- generates Ed25519 keypair → `data/licensing/keys.bin` (mode 0600 on Unix),
- prints public key:
  ```
  DIMMY_LICENSE_PUBKEY=HZWIbVv9aduuwj2vx0IOLiB2LAWbLaKJII_lgc0B0pw
  ```
- migrates SQLite schema → `data/licensing/licensing.db`,
- listens on `0.0.0.0:8787`.

Override via env: `DIMMY_LICENSING_BIND`, `DIMMY_LICENSING_DATA`, `DIMMY_LICENSING_PUBLIC_URL`.

### Build the CLI with the embedded pubkey

The pubkey from server stdout has to be baked into the client at compile time. From a separate shell:

```powershell
$env:DIMMY_LICENSE_PUBKEY="<pubkey-from-server-stdout>"
cargo build --bin license_cli --features license-cli,license-client
```

Without `DIMMY_LICENSE_PUBKEY`, the CLI runs but `status` returns `Unrestricted` (the source-build escape hatch is permanent — don't trip on it during testing).

## The 7 PoC test scenarios

All passing on Windows as of 2026-05-01.

### 1. Trial issuance + magic link

```bash
license_cli request-trial alice@test.com
# → magic_link: http://0.0.0.0:8787/api/activate?code=<32-char>
# Server prints the same line.
```

### 2. Activation

```bash
license_cli activate "http://0.0.0.0:8787/api/activate?code=<...>" \
            --device-label "Konrad's Laptop"
# → activated. token saved to ~/.config/dimmy/license.json
# → token (~440 bytes) printed to stdout for inspection.
```

### 3. Offline status check

```bash
# (kill the server first — proves verification is fully offline)
license_cli status
# → status: TrialActive { days_remaining: 13 }
# → claims: { lid, eh, tier: "trial", iat, exp, max_offline, did, scope }
```

### 4. Refresh

```bash
license_cli refresh
# → refreshed. token rotated.
# Server bumps device.last_seen + re-issues a fresh token with updated iat.
```

### 5. Tampering rejected

Modify `license.json` payload (e.g. extend `exp`) without re-signing:

```python
import json, base64
path = r'~/.config/dimmy/license.json'
with open(path) as f: env = json.load(f)
header, payload, sig = env['token'].split('.')
def dec(s): s += '=' * (4 - len(s) % 4); return base64.urlsafe_b64decode(s).decode()
def enc(b): return base64.urlsafe_b64encode(b.encode()).decode().rstrip('=')
claims = json.loads(dec(payload)); claims['exp'] += 99999999
env['token'] = f"{header}.{enc(json.dumps(claims))}.{sig}"
with open(path,'w') as f: json.dump(env, f)
```

```bash
license_cli status
# → status: Invalid("signature verify: signature error: Verification equation was not satisfied")
```

### 6. Multi-device limit

```bash
# loop 6 activations for the same email
for i in {1..6}; do
  CODE=$(curl -s -X POST -H "Content-Type: application/json" \
         -d '{"email":"alice@test.com"}' \
         http://localhost:8787/api/trial/start \
         | jq -r '.magic_link' | sed 's/.*code=//')
  curl -s -w "%{http_code}\n" \
       "http://localhost:8787/api/activate?code=$CODE&device_label=Device$i"
done
# → first 5: HTTP 200 + token
# → 6th: HTTP 429, {"error":"device limit 5 reached — deactivate one to continue"}
```

### 7. Trial reset prevention (the critical one)

```bash
# Initial activation
license_cli request-trial bob@test.com
license_cli activate "<magic-link>" --device-label "Bob #1"
license_cli status   # remember the lid + exp from claims

# User wipes the file
rm ~/.config/dimmy/license.json

# Re-activate
license_cli request-trial bob@test.com    # server re-issues code
                                          # for the SAME license, NOT a fresh trial
license_cli activate "<new-magic-link>" --device-label "Bob #2"
license_cli status   # lid + exp MUST match the previous values
```

The server tracks trial start time in the `licenses` table; deleting the on-disk token only removes a cached signed copy. The user gets back the same license_id with the same valid_until — no fresh 14 days.

## Security walkthrough (recap)

| Threat | Mitigation |
|---|---|
| Pirate shares license with 50 friends | `max_devices=5` enforced at activation; refresh fails for 5+ |
| Cracker patches `check_status()` to always Active | Acceptable loss — same as Sublime / Cleanshot |
| OTP / activation-code spam | Rate limit per IP + per email at server (TODO in production) |
| Brute-force activation codes | 32-char alphanumeric (~190 bits) + per-license codes — infeasible |
| Public-key extraction | Public key only verifies; signing requires private key (server-only) |
| MITM on activation | HTTPS + magic-link delivered via separate channel (email) |
| Replay attack with stolen token | Server validates `did` on refresh; mismatched device → revoke |
| Compromised private key | Rotate keypair → embed new pubkey in next release → all old tokens invalid |
| Local file deletion → reset trial | Server-side trial tracking — file is cache only |
| User changes hardware | No fingerprint coupling — license follows email, not hardware |

## Migration path to Cloudflare

The PoC is structured so the migration is mechanical:

| PoC layer | Cloudflare equivalent |
|---|---|
| `axum::Router` + handlers | `wrangler dev` Worker handlers (similar `Request` / `Response` shapes) |
| `sqlx::SqlitePool` | `env.D1.prepare(...)` (signature is similar) |
| stdout email mock | `resend.send(...)` SDK call |
| `keys.bin` on disk | `wrangler secret put DIMMY_LICENSE_PRIV` |
| `0.0.0.0:8787` | `*.pages.dev` / custom domain |

Estimated migration: **1 day of work** once the PoC architecture is signed off.

## Out of scope for this PoC

- Resend / email delivery (mocked via stdout)
- Lemon Squeezy webhook signature validation (the `/api/license/issue` endpoint is open in PoC)
- Custom URL handler (`dimmy://activate?token=...`) — CLI takes the URL as an arg
- C# / Swift UI integration — CLI is the proxy
- Web dashboard for device management
- GDPR data-deletion endpoint (planned, easy)
- Certificate pinning / TLS hardening (handled at Cloudflare edge in production)

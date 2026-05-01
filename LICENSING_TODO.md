# Licensing — manual rollout checklist

> Cose che **devi fare tu** per portare il licensing v2 da PR #43 in produzione live. ~70 min totali (più ~30 min VAT-OSS, più qualche ora per il sito marketing). Tutto il resto è già in codice (vedi PR #43).

Per il dettaglio architetturale + perché fare le cose in questo ordine, vedi [`docs/dev/licensing-prod.md`](docs/dev/licensing-prod.md).
Per il setup di test **in test mode contro il Worker prod**, vedi [`docs/dev/prod-test-setup.md`](docs/dev/prod-test-setup.md).

⚠️ **Ordine importante**: ogni step si testa prima del successivo. Fare in disordine = rifare lavoro.

---

## Status @ 2026-05-02 (overnight session)

**Coverage tests**: 133 Worker + 362 Rust core, **tutti verdi**. Dettaglio:

| Suite | Tests | Cosa copre |
|---|---|---|
| `crypto.test.ts` | 12 | base64url, ULID, activation code entropy, sign/verify round-trip, tampered-payload reject, schema-version reject |
| `scopes.test.ts` | 9 | tier→scope mapping, max_offline values, parità con Rust |
| `stripe-signature.test.ts` | 7 | HMAC verify, tolerance window, rotation, header parse |
| `stripe-webhook.test.ts` | 19 | checkout.session.completed (lifetime/subscription, fallback), invoice paid/failed, sub deleted/updated/uncancel/recovery, charge.refunded full/partial/orphan, idempotency replay, signature reject |
| `billing-portal.test.ts` | 8 | auth, no-customer-id 409, return_url sanitize, Stripe 5xx → 502 |
| `checkout.test.ts` | 19 | tier validation, real Stripe form-body shape, mode mapping, token email_hash carry, return_url sanitize |
| `activate.test.ts` | 13 | code validation, device limit, suspended licence, cancels_at population, scope-from-tier, exp from license |
| `refresh.test.ts` | 9 | token verify, license/device gates, last_seen bump, scope refresh, cancels_at on update |
| `devices.test.ts` | 9 | list (active only), self-deactivate, deactivate other, audit, replay |
| `trial.test.ts` | 8 | email validation, idempotent re-issuance (scenario #7), expired-trial 409 |
| `delete.test.ts` | 9 | GDPR 2-step OTP, anonymisation (not delete), cross-account defence, replay reject |
| `html-pages.test.ts` | 8 | /checkout/success/cancel, /activate?code= bridge HTML, security headers |
| **Rust core** | 362 | tutti i moduli + 15 license-specific (cancels_at serde, has_scope per state, claims integrity) |

**Feature shipped tonight**:
- ✅ Buy / Upgrade / Renew CTAs (state-aware, Win + Mac)
- ✅ Manage subscription button via FFI (Win + Mac, dev URL respected)
- ✅ /api/checkout/create endpoint with `metadata.tier` (deterministic webhook routing)
- ✅ /api/billing-portal endpoint via Customer Portal API
- ✅ /checkout/success + /checkout/cancel landing pages
- ✅ charge.refunded full vs partial distinction
- ✅ subscription.updated UNCANCEL handling
- ✅ cancels_at end-to-end: token claim → LicenseStatus → FFI JSON → Win + Mac UI subtitle "Subscription scheduled to cancel on …"
- ✅ Build prod-test DLL Windows con prod pubkey embedded (`bin/Release-prod/.../dimmy_lib.dll`)
- ✅ Stripe CLI installed + listen forwarding to localhost:8787
- ✅ wrangler dev runtime up with .dev.vars (gitignored — secrets, prices, prod-test config)
- ✅ Documentazione setup test prod su Mac → `docs/dev/prod-test-setup.md`

**Per domani mattina (Mac side)**:
1. `git pull` sul Mac
2. `cargo build --target aarch64-apple-darwin --release --lib --features local-stt-metal,local-llm-metal,license-client` con `DIMMY_LICENSE_PUBKEY=uut9...`
3. Build app via Xcode
4. Esegui i 12 step del playbook in `docs/dev/prod-test-setup.md`
5. Se qualcosa rompe, leggi i log del Worker `wrangler tail` (deve essere già deployato in prod) o lancia tu `wrangler dev` localmente

**Bloccante per il go-live live**:
- VAT-OSS Italia (step 6 di questo file) — obbligatorio per legge prima della prima vendita B2C
- Resend domain verification + production API key
- Stripe LIVE prodotti + webhook signing secret separati dai test
- GitHub Action secret `DIMMY_LICENSE_PUBKEY` con la prod pubkey — **solo dopo** test e2e verde

---

## Canonical keypairs (reference)

> **Public keys are not secret** — published here so any future Claude
> session knows the exact bytes the client builds embed. Private keys
> live ONLY in Cloudflare secrets + 1Password ("Dimmy License Privkey
> — rotate-only"). Never document or log a privkey.

| Keypair | Pubkey (`DIMMY_LICENSE_PUBKEY`) | Used by |
|---|---|---|
| **dev** | `FvIwxXaU49zV0Czz87rHs1uQe90KRYefrFN17zhOMhY` | Local dev: `wrangler dev` + `dev-server.js` + Win/Mac dev binaries. Throwaway. |
| **prod** | `uut9CwgkhU-Q76gguvGJID4D48xAQc4h1LAG829hacE` | Cloudflare Worker at `license.dimmy.app` + every shipped binary built from CI with this pubkey injected. Rotate-only via Step 1 below. |

When deploying client builds for **internal testing against prod**, build with:
```
DIMMY_LICENSE_PUBKEY=uut9CwgkhU-Q76gguvGJID4D48xAQc4h1LAG829hacE \
  cargo build --release --lib --features license-client
```

Output goes in `core/target/release-prod/` (parallel path, doesn't clobber dev).

---

## ☐ 1. Generate Ed25519 keypair (2 min, security-critical)

Una sola volta. La chiave privata non deve mai uscire dal Cloudflare secret store + un backup cifrato in 1Password.

```bash
node -e '
const c = require("crypto");
const { publicKey, privateKey } = c.generateKeyPairSync("ed25519");
const priv = privateKey.export({ format: "jwk" }).d;
const pub  = publicKey.export({ format: "jwk" }).x;
console.log("DIMMY_LICENSE_PUBKEY=" + pub);
console.log("DIMMY_LICENSE_PRIVKEY=" + priv);
'
```

L'output è qualcosa tipo:

```
DIMMY_LICENSE_PUBKEY=zxC3U7wfleoTiADAaqbhTnbBepysOiohApSShfcPJXY
DIMMY_LICENSE_PRIVKEY=04669UEz1QcUoGqe2909y_e6jOBx4LFUtB60gjABAws
```

- **Copia PUB** in un blocco note temporaneo — serve negli step 2, 5.
- **Copia PRIV** subito in 1Password (entry "Dimmy License Privkey — rotate-only"). Poi `clear` o Ctrl+L per pulire il terminale.

⚠️ Il PRIV viene generato e mostrato per pochi secondi. Non tocca disco — perfetto. Se chiudi il terminale prima di copiarlo, rifai il comando (genera una nuova coppia, riparti dallo step 1).

---

## ☐ 2. Cloudflare deploy (5 min — D1 già creato)

D1 `dimmy-licensing` (ID `06a210a1-2e3b-4142-9d21-0eef9ee517de`) è stato creato in WEUR via MCP — già in `wrangler.toml`.

```bash
cd backend
npm install   # installa wrangler + vitest

# 2a. Setta i secret. PRIV e PUB dallo step 1.
echo "<priv-from-step-1>" | npx wrangler secret put DIMMY_LICENSE_PRIVKEY
echo "<pub-from-step-1>"  | npx wrangler secret put DIMMY_LICENSE_PUBKEY
echo "PLACEHOLDER" | npx wrangler secret put STRIPE_WEBHOOK_SECRET   # finiremo nello step 3
echo "PLACEHOLDER" | npx wrangler secret put RESEND_API_KEY          # finiremo nello step 4

# 2b. Migra schema + deploy.
npx wrangler d1 migrations apply dimmy-licensing --remote
npx wrangler deploy
# → output: "https://dimmy-licensing.<your-account>.workers.dev"
```

```bash
# 2d. Test rapido che il Worker risponda.
curl https://dimmy-licensing.<your-account>.workers.dev/api/health
# → {"status":"ok"}
```

**DNS** (Cloudflare dashboard → DNS):
- Aggiungi CNAME: `license.dimmy.app` → `dimmy-licensing.<account>.workers.dev` (proxy on)

Aggiorna `backend/wrangler.toml`:
```toml
PUBLIC_URL = "https://license.dimmy.app"
```

```bash
npx wrangler deploy   # ridistribuisci con il PUBLIC_URL aggiornato
```

---

## ☐ 3. Stripe (15 min — i prodotti li crea Claude via MCP)

**Tier model finale (deciso 2026-05-01)**:

| Tier | Prezzo | Stripe mode | Validity | Test Payment Link |
|---|---|---|---|---|
| Monthly | €4.99/mese | recurring sub | rolls forward su `invoice.paid` | https://buy.stripe.com/test_fZu7sLbxn6Ea5K32CF4Rq00 |
| Annual  | €39/anno   | recurring sub | rolls forward su `invoice.paid` | https://buy.stripe.com/test_6oUcN5gRH1jQegz4KN4Rq01 |
| Lifetime | €99 one-time | one-time payment | 3 anni date-based | https://buy.stripe.com/test_9B68wP6d35A62xR5OR4Rq02 |

Stripe price IDs (test mode, già in wrangler.toml):
- Monthly:  `price_1TSKE8HxRNDPFvsZegNx8slR`
- Annual:   `price_1TSKE9HxRNDPFvsZv4T1Ampf`
- Lifetime: `price_1TSKEAHxRNDPFvsZvcQOWqbr`

In **Stripe Dashboard → Settings → Tax**:

- ☐ Enable Stripe Tax
- ☐ Registra business location: Italy (IT)

In **Stripe Dashboard → Developers → Webhooks → Add endpoint**:

- ☐ URL: `https://license.dimmy.app/api/stripe/webhook`
- ☐ Events to send: `checkout.session.completed`, `customer.subscription.updated`, `customer.subscription.deleted`, `invoice.paid`, `invoice.payment_failed`, `charge.refunded`
- ☐ Copia il signing secret (`whsec_…`)

Aggiorna Cloudflare secret:
```bash
echo "whsec_..." | npx wrangler secret put STRIPE_WEBHOOK_SECRET
```

I 3 prodotti + Payment Links li crea Claude via Stripe MCP (con metadata `tier` settato sui PL così il webhook li riconosce senza chiamate API extra).

Output che ti tornerà:
```toml
STRIPE_PRICE_MONTHLY  = "price_..."
STRIPE_PRICE_ANNUAL   = "price_..."
STRIPE_PRICE_LIFETIME = "price_..."
```
Lo metto io in `backend/wrangler.toml` e fai `npx wrangler deploy`.

**Test in modalità Stripe Test**: usa carta `4242 4242 4242 4242` su un Payment Link → verifica che la mail di attivazione arrivi (per ora va su stdout del Worker se step 4 non ancora fatto — `npx wrangler tail` per vedere).

---

## ☐ 4. Resend (10 min)

In **Resend Dashboard → Domains → Add domain**: `dimmy.app`

- ☐ Aggiungi i record DNS che Resend stampa (TXT/SPF, DKIM, DMARC) nel pannello DNS Cloudflare
- ☐ Aspetta verifica auto (~5 min)

In **Resend Dashboard → API Keys → Create**:

- ☐ Nome: "Dimmy licensing prod"
- ☐ Permission: `email.send`
- ☐ Copia `re_…`

```bash
echo "re_..." | npx wrangler secret put RESEND_API_KEY
npx wrangler deploy
```

**Test**: trigger un trial dal CLI (modificando `--server` per puntare al Worker):
```bash
./core/target/debug/license_cli --server https://license.dimmy.app \
  request-trial tu@email.com
# → Inbox di tu@email.com riceve la mail con magic link.
```

---

## ☐ 5. GitHub Actions secret (2 min)

Per far sì che le release embeddino la pubkey nei binari prodotti:

```bash
gh secret set DIMMY_LICENSE_PUBKEY --body "<pub-from-step-1>"
```

Oppure via UI: GitHub → Settings → Secrets and variables → Actions → New repository secret.

⚠️ **Da quel momento, ogni release builderà con licensing enforcement attivo.** Aggiungi il secret SOLO dopo che gli step 2-4 sono verdi e hai testato l'attivazione end-to-end via Worker. Se aggiungi prematuramente, gli utenti delle nuove release non potranno usare le feature cloud finché il Worker non risponde.

---

## ☐ 6. VAT-OSS Italia (~30 min, una sola volta)

Obbligatorio per legge prima della prima vendita B2C cross-EU.

1. Login **Agenzia delle Entrate** (con SPID o CIE).
2. Cerca "**Sportello Unico OSS**" → "**Iscrizione regime UE**".
3. Compila form con la tua P.IVA.
4. Approval ~1-3 giorni lavorativi.
5. Ogni trimestre: scarica il report Stripe Tax (CSV) e caricalo nel portale OSS.

**Se non hai P.IVA**: aprila come "**regime forfettario**" (semplice, 5%-15% flat fino a €85k/y).

---

## ☐ 7. Sito marketing (~3-4 ore, separato)

Out of scope di questo PR — è un sito statico in repo separato. Pagine necessarie:

- ☐ Landing con bottoni "Download" (link a GitHub Releases) + "Buy Annual €19" + "Buy 3y €39" (link Stripe Payment Links)
- ☐ About / Privacy Policy / Terms of Service / Refund Policy ("14-day money-back guarantee, no questions")
- ☐ Status (link a `https://license.dimmy.app/api/health`)
- ☐ DNS: CNAME `dimmy.app` → la sua piattaforma host (Cloudflare Pages, Vercel, Netlify, GitHub Pages)

---

## ✅ Definizione di "fatto"

Hai completato il rollout quando:

1. ☐ `curl https://license.dimmy.app/api/health` → `{"status":"ok"}`
2. ☐ Trial email arriva inbox (non spam) e magic link attiva il Dimmy installato
3. ☐ Pagamento Stripe test mode `lifetime` (carta `4242…`) → mail → activation OK → token saved
4. ☐ Pagamento Stripe test mode `monthly` → primo `invoice.paid` estende validity → license `current_period_end` aggiornato
5. ☐ Annulla subscription in Stripe → `customer.subscription.deleted` → status='revoked' nella D1
6. ☐ Carta declinata in Stripe test → `invoice.payment_failed` → status='past_due' (validity intatta)
7. ☐ La prossima `git tag v0.6.27 && git push --tags` builda con licensing enforcement attivo
8. ☐ Sito marketing live su `https://dimmy.app` con i pulsanti Buy funzionanti

Quando tutti ✅, sei in produzione.

---

## Quanto costa

| Servizio | Mensile fisso | Variabile |
|---|---|---|
| Cloudflare Workers + D1 + Pages | **€0** (free tier copre 100k req/giorno) | — |
| Resend | **~€19** ($20/mese, 10k email) | — |
| Domain `dimmy.app` | **~€1** (annual ÷ 12) | — |
| Stripe Tax | — | 0.5%/transazione |
| Stripe Checkout | — | 1.4% + €0.25 (EU) |
| **Totale fisso** | **~€20/mese** | + ~3.5% revenue |

Profittevole dalla prima vendita.

---

## In caso di problemi

Vedi [`docs/dev/licensing-prod.md`](docs/dev/licensing-prod.md) § "Rollback plan" per ogni scenario:

- Bug nel Worker → `wrangler deploy` con fix (10 sec)
- Privkey compromessa → rotate + nuova release con nuova pubkey + reactivation di tutti gli utenti
- D1 corruption → restore da snapshot (5 min downtime)
- Stripe issue → pausa Payment Links, app esistenti continuano

Tieni questa checklist a portata di mano la prossima volta che chiedi a Claude di aiutarti — basta un "vai avanti dal LICENSING_TODO.md step N".

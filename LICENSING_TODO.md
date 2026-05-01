# Licensing — manual rollout checklist

> Cose che **devi fare tu** per portare il licensing v2 da PR #43 in produzione live. ~70 min totali (più ~30 min VAT-OSS, più qualche ora per il sito marketing). Tutto il resto è già in codice (vedi PR #43).

Per il dettaglio architetturale + perché fare le cose in questo ordine, vedi [`docs/dev/licensing-prod.md`](docs/dev/licensing-prod.md).

⚠️ **Ordine importante**: ogni step si testa prima del successivo. Fare in disordine = rifare lavoro.

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

## ☐ 2. Cloudflare deploy (10 min)

```bash
cd backend
npm install
npx wrangler login

# 2a. Crea D1 database — copia "database_id" dall'output.
npx wrangler d1 create dimmy-licensing
```

Apri `backend/wrangler.toml` e sostituisci `TODO_REPLACE_WITH_REAL_D1_ID` con l'ID stampato.

```bash
# 2b. Setta i secret. PRIV e PUB dallo step 1.
echo "<priv-from-step-1>" | npx wrangler secret put DIMMY_LICENSE_PRIVKEY
echo "<pub-from-step-1>"  | npx wrangler secret put DIMMY_LICENSE_PUBKEY
echo "PLACEHOLDER" | npx wrangler secret put STRIPE_WEBHOOK_SECRET   # finiremo nello step 3
echo "PLACEHOLDER" | npx wrangler secret put RESEND_API_KEY          # finiremo nello step 4

# 2c. Migra schema + deploy.
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

## ☐ 3. Stripe (15 min)

In **Stripe Dashboard → Products**:

- ☐ Crea prodotto "**Dimmy — Annual License**"
  - Prezzo: €19 EUR, **one-time** (non recurring)
  - Copia il `price_…` ID
- ☐ Crea prodotto "**Dimmy — 3-Year License**"
  - Prezzo: €39 EUR, **one-time**
  - Copia il `price_…` ID

In **Stripe Dashboard → Settings → Tax**:

- ☐ Enable Stripe Tax
- ☐ Registra business location: Italy (IT)

In **Stripe Dashboard → Payment Links** (uno per prodotto):

- ☐ Crea Payment Link annual → settings: collect email (required), collect address (required for VAT)
- ☐ Crea Payment Link 3-year → idem
- ☐ Copia entrambi gli URL `https://buy.stripe.com/…` (vanno sul sito marketing)

In **Stripe Dashboard → Developers → Webhooks → Add endpoint**:

- ☐ URL: `https://license.dimmy.app/api/stripe/webhook`
- ☐ Events to send: `checkout.session.completed`, `charge.refunded`
- ☐ Copia il signing secret (`whsec_…`)

Aggiorna Cloudflare secret:
```bash
echo "whsec_..." | npx wrangler secret put STRIPE_WEBHOOK_SECRET
```

Aggiorna `backend/wrangler.toml`:
```toml
STRIPE_PRICE_ANNUAL = "price_..."
STRIPE_PRICE_3YEAR  = "price_..."
```

```bash
npx wrangler deploy
```

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
3. ☐ Pagamento Stripe test mode (carta `4242…`) → mail con magic link → activation OK → token salvato
4. ☐ La prossima `git tag v0.6.27 && git push --tags` builda con licensing enforcement attivo
5. ☐ Sito marketing live su `https://dimmy.app` con i pulsanti Buy funzionanti

Quando tutti e 5 ✅, sei in produzione.

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

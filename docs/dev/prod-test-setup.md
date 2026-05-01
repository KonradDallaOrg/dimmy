# Prod-test setup — internal testing

> Per testare il flusso licensing **end-to-end** contro il Worker
> production a `https://license.dimmy.app` (in **Stripe Test mode**),
> senza invitare utenti reali. Una volta verde, si rimuove la barra
> "test mode" passando a `sk_live_…` + GitHub Action secret PUB.

## TL;DR per il test domattina

Hai due binari paralleli:

| Build | DLL pubkey | Server target | Per cosa |
|---|---|---|---|
| **Dev** (esistente) | `FvIw…` | `wrangler dev` su localhost:8787 | Sviluppo iterativo + unit tests |
| **Prod-test** | `uut9…` | `https://license.dimmy.app` | Test e2e con Stripe + Resend reali (test mode) |

Il **prod-test DLL Win** è già buildato in:
```
platforms/windows/Dimmy.Windows/bin/Release-prod/net8.0-windows10.0.19041.0/win-x64/dimmy_lib.dll
```

Lo testi domani con un launch script che lancia Dimmy.exe puntandolo lì.

## Mac — cosa fare al risveglio

```bash
git pull
cd core

# Build dylib release con prod pubkey
DIMMY_LICENSE_PUBKEY=uut9CwgkhU-Q76gguvGJID4D48xAQc4h1LAG829hacE \
  cargo build --release --target aarch64-apple-darwin --lib \
  --features local-stt-metal,local-llm-metal,license-client

# Output: core/target/aarch64-apple-darwin/release/libdimmy_lib.dylib

# Embed in Mac app — Xcode steps al solito (vedi platforms/macos/README.md
# "Build" sezione). Quando l'app gira, è già configurata per parlare a
# license.dimmy.app perché LicenseService default punta lì in release;
# in dev override via Settings → License → Advanced → Server URL.
```

## Cosa fare in app per validare

1. **Vergine state**: cancella `~/.config/dimmy/license.json` se esiste
2. **Settings → License → "Buy Annual"** (o monthly/lifetime, scegli)
3. Si apre Safari su `https://checkout.stripe.com/c/pay/cs_test_…` reale
4. Email tua, carta `4242 4242 4242 4242`, qualsiasi CVC + scadenza futura
5. Click **Pay** → Stripe redirige a `https://license.dimmy.app/checkout/success?session_id=…`
   → vedi pagina "Thanks! Check your inbox"
6. **Gmail/Apple Mail**: arriva email reale via Resend con magic link
7. Click magic link → Safari apre la HTTPS bridge page → JS dispatch
   `dimmy://activate?code=…` → conferma "Apri Dimmy"
8. Dimmy attiva → Settings → License → status **Active — annual**, badge **PRO • ANNUAL**
9. Click **Manage subscription** → si apre Stripe Customer Portal (vero)
10. **Click "Cancel subscription"** dal portal → torna su Dimmy
11. Refresh license → ora vedi subtitle **"Subscription scheduled to cancel on …"**
12. **Refund test (admin)**: vai su `dashboard.stripe.com/test/payments`,
    trova il charge, click **Refund**. Webhook fires → `charge.refunded` →
    license revoked → next refresh in Dimmy ti dà status `Expired`

Tutto questo deve funzionare senza intervento manuale lato Worker o D1.
Se step 7 non apre Dimmy direttamente, è il discrepancy `dimmy://` registry
del Mac — verifica `Info.plist` CFBundleURLTypes.

## Checklist domani per andare LIVE veri (post test)

In ordine di precedenza, **non saltare**:

- [ ] Stripe **live products + prices**: stessa struttura del test mode
      ma con `price_…` live. Aggiorna `wrangler.toml` `[vars]`.
- [ ] Stripe **live webhook**: `https://license.dimmy.app/api/stripe/webhook`,
      eventi `checkout.session.completed`, `customer.subscription.{updated,deleted}`,
      `invoice.{paid,payment_failed}`, `charge.refunded`. Copia signing secret.
- [ ] `wrangler secret put STRIPE_WEBHOOK_SECRET` con il live `whsec_…`
- [ ] `wrangler secret put STRIPE_SECRET_KEY` con `sk_live_…`
- [ ] Resend: verifica DNS DKIM/SPF su `dimmy.app` se non già fatto
- [ ] `wrangler secret put RESEND_API_KEY` con la live key Resend
- [ ] Disabilita il webhook **test mode** in dashboard (per evitare doppi)
- [ ] **VAT-OSS Italia** registrato (legge prima vendita B2C cross-EU)
- [ ] `wrangler deploy` finale
- [ ] GitHub Action secret `DIMMY_LICENSE_PUBKEY=uut9…` set → release.yml
      builda binari coi token-verify enforcement attivo
- [ ] Tag `v0.6.27` o successivo → CI builda + Velopack pubblica installer
- [ ] Test live con carta TUA reale (1 transazione 5€), poi rimborso totale
      per confermare che il flusso `charge.refunded` revoca

## Backstop / rollback

Se qualcosa non funziona in prod live:
1. **Riabilita il webhook test mode** in Stripe dashboard (= traffico paganti
   parcheggiato)
2. `wrangler rollback <deployment_id>` per tornare alla versione precedente
3. I token già emessi continuano a funzionare offline finché `last_online_check`
   non eccede `max_offline` (30gg per monthly/annual, 1095gg per lifetime)

## File coinvolti — riferimento

| Layer | File |
|---|---|
| Worker handlers | `backend/src/handlers/{trial,activate,refresh,stripe,checkout,billing-portal,devices,delete}.ts` |
| Worker entry + routes | `backend/src/index.ts` |
| Worker DB helpers | `backend/src/db.ts` |
| Worker tests (121) | `backend/tests/*.test.ts` |
| Rust client core | `core/src/license.rs` |
| Rust FFI surface | `core/src/ffi.rs` |
| Win UI | `platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml{,.cs}` |
| Win FFI bindings | `platforms/windows/Dimmy.Windows/{Interop/DimmyNative.cs, Services/LicenseService.cs}` |
| Mac UI | `platforms/macos/Dimmy/Views/Settings/MacLicensePage.swift` |
| Mac FFI bindings | `platforms/macos/Dimmy/Managers/DimmyCore+License.swift` + `DimmyFFI.h` |

Ogni endpoint ha test integration coperti — vedi `backend/tests/`.

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

Il **prod-test DLL Win** era un build locale in:
```
platforms/windows/Dimmy.Windows/bin/Release-prod/net8.0-windows10.0.19041.0/win-x64/dimmy_lib.dll
```

Nel repo non esiste nessuna configurazione, script o workflow che produce
la cartella `Release-prod`: non è ricostruibile da qui. Per un DLL Win con
prod pubkey servono `DIMMY_LICENSE_PUBKEY` **e** `DIMMY_LICENSE_SERVER_URL`
(`core/build.rs` rifiuta il solo pubkey), come in `release.yml`.

## Mac — cosa fare al risveglio

```bash
git pull
cd core

# Build static lib release con prod pubkey + URL (build.rs rifiuta una
# pubkey senza DIMMY_LICENSE_SERVER_URL). Feature set Mac = quello di
# release.yml / MAC_FEATURES in scripts/dev/preflight-mac.sh:
DIMMY_LICENSE_PUBKEY=uut9CwgkhU-Q76gguvGJID4D48xAQc4h1LAG829hacE \
DIMMY_LICENSE_SERVER_URL=https://license.dimmy.app \
  cargo build --release --target aarch64-apple-darwin --lib \
  --features local-stt-metal,local-stt-coreml,local-llm-metal,local-stt-parakeet-coreml,local-stt-parakeet-fluid,local-dfn,license-client,telegram,local-stt-qwen

# Output: core/target/aarch64-apple-darwin/release/libdimmy_lib.a
# (release.yml cancella il .dylib per forzare il link statico)

# Embed in Mac app — Xcode steps al solito (vedi platforms/macos/README.md,
# sezione "Quick dev loop"). L'URL del server NON si cambia a runtime:
# è quello incorporato in build (DIMMY_LICENSE_SERVER_URL). L'override
# `dimmy_license_set_server_url` esiste solo nelle build debug / test-ffi
# e la UI "Settings → Advanced → server URL" è stata rimossa.
```

## Cosa fare in app per validare

1. **Vergine state**: cancella `license.json` nella config dir (Mac: `~/Library/Application Support/dimmy/`, via `dirs::config_dir()`) se esiste
2. **Settings → License** → bottone `Annual` (o `Monthly` / `Supporter`, il piano una-tantum da 3 anni = tier `lifetime`), poi email nel dialog "Continue to … checkout". Se il server dice che le vendite sono chiuse, i bottoni registrano solo interesse (nessun checkout)
3. Si apre Safari su `https://checkout.stripe.com/c/pay/cs_test_…` reale
4. Email tua, carta `4242 4242 4242 4242`, qualsiasi CVC + scadenza futura
5. Click **Pay** → Stripe redirige a `https://license.dimmy.app/checkout/success?session_id=…`
   → vedi pagina "Thanks! Check your inbox"
6. **Gmail/Apple Mail**: arriva email reale via Resend con magic link
7. Click magic link → Safari apre la HTTPS bridge page → JS dispatch
   `dimmy://activate?code=…` → conferma "Apri Dimmy"
8. Dimmy attiva → Settings → License → titolo "Pro license, Annual", badge **PRO • ANNUAL**
9. Click **Manage subscription** → si apre Stripe Customer Portal (vero)
10. **Click "Cancel subscription"** dal portal → torna su Dimmy
11. Refresh license → ora vedi subtitle **"Subscription scheduled to cancel on …"**
12. **Refund test (admin)**: vai su `dashboard.stripe.com/test/payments`,
    trova il charge, click **Refund**. Webhook fires → `charge.refunded` →
    license revoked → al refresh successivo Dimmy cancella il token e la pagina torna a "No license on this device" (non esiste uno stato `Revoked`/`Expired` per questo caso)

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
- [ ] Tag `vX.Y.Z-rc.N` (forma con il punto, vedi `scripts/check-release-tag.sh`) → `release.yml` builda + Velopack pubblica installer
- [ ] Test live con carta TUA reale (1 transazione 5€), poi rimborso totale
      per confermare che il flusso `charge.refunded` revoca

## Backstop / rollback

Se qualcosa non funziona in prod live:
1. **Riabilita il webhook test mode** in Stripe dashboard (= traffico paganti
   parcheggiato)
2. `wrangler rollback <deployment_id>` per tornare alla versione precedente
3. I token già emessi continuano a funzionare offline finché `last_online_check`
   non eccede `max_offline` (claim del token; default del client in `Tier::default_max_offline_days`: 14gg monthly, 30gg annual, 1095gg lifetime; Suspended scatta dopo `max_offline` + 7gg di grace)

## File coinvolti — riferimento

Backend code lives in the private repo [`KonradDallaOrg/dimmy-backend`](https://github.com/KonradDallaOrg/dimmy-backend) (moved out of this repo 2026-05-17). Clone it as a sibling: `~/code/dimmy-backend/`. Paths in the Worker column below are relative to that repo's root.

| Layer | File |
|---|---|
| Worker handlers | *(private repo)* `src/handlers/{trial,activate,refresh,stripe,checkout,billing-portal,devices,delete}.ts` |
| Worker entry + routes | *(private repo)* `src/index.ts` |
| Worker DB helpers | *(private repo)* `src/db.ts` |
| Worker tests (121) | *(private repo)* `tests/*.test.ts` |
| Rust client core | `core/src/license.rs` |
| Rust FFI surface | `core/src/ffi.rs` |
| Win UI | `platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml{,.cs}` |
| Win FFI bindings | `platforms/windows/Dimmy.Windows/{Interop/DimmyNative.cs, Services/LicenseService.cs}` |
| Mac UI | `platforms/macos/Dimmy/Views/Settings/MacLicensePage.swift` |
| Mac FFI bindings | `platforms/macos/Dimmy/Managers/DimmyCore+License.swift` + `DimmyFFI.h` |

Ogni endpoint ha test integration coperti — vedi `tests/` nel repo backend privato.

# Publishing Dimmy (outside the stores)

> Direct-download distribution from the Dimmy website. No Mac App Store, no Microsoft Store. Users must see Dimmy as a **signed, notarised, auto-updating** app — not a "unknown developer" warning.
>
> For the release runbook itself see [`RELEASING.md`](RELEASING.md). For Windows CI invariants see [`dev/windows-ci.md`](dev/windows-ci.md).

## TL;DR — minimum-cost track (individual, closed-source)

Dimmy ships as closed-source for now (may open-source selected components later). That disqualifies the free OSS-only signing programs (SignPath Foundation, Certum Open Source). The realistic floor is therefore **~$170–220/yr total**: $99 Apple + €69–120 Windows cloud OV cert.

| Item | Cost | Lead time | Notes |
|---|---|---|---|
| Apple Developer Program (**Individual**) | $99 / yr | hours (no D-U-N-S needed) | Cert reads "Developer ID Application: Firstname Lastname (TEAMID)". Upgrade to Organization later without losing the Team ID. |
| Apple "Developer ID Application" cert + app-specific password | included | minutes | `codesign` + `notarytool submit` for direct distribution |
| **Certum Standard Code Signing** (individual OV, cloud HSM via SimplySign) | ~€69–99 / yr | 3–7 days ID validation | Cheapest reliable closed-source path. Shows publisher as "Firstname Lastname". No USB token required (SimplySign is their cloud-HSM service). |
| PostHog Cloud EU | free tier (1M events/mo) | minutes | product analytics, feature flags, session replay |
| Sentry (optional, later) | free tier (5k errors/mo) | minutes | crash reporting |

**No free option for macOS or Windows signing** once the project is closed-source. Self-signed bundles are blocked by Gatekeeper; Authenticode "unknown publisher" triggers SmartScreen's red full-screen warning. These are mandatory spends.

### Windows signing — alternatives, ranked by total cost of ownership

| Option | Cost | Pros | Cons |
|---|---|---|---|
| **Certum Standard (individual OV)** | ~€69–99/yr | Cheapest closed-source. SimplySign cloud HSM works in CI. EU-based. | Slower SmartScreen reputation ramp than EV. ID validation is strict. |
| **Azure Trusted Signing** | ~$10/mo (~$120/yr) | Native Microsoft service; cleanest GitHub Actions integration (OIDC). Reputation inherited from Microsoft's pool. | Eligibility: individuals need ID verification; businesses need ≥3 years history. May reject new individuals depending on region. Try only if Certum is unavailable. |
| **SSL.com OV with eSigner** | ~$170–230/yr | Battle-tested for CI. eSigner is a documented cloud-HSM API. | ~3× the cost of Certum for the same trust level. |
| Commercial EV (DigiCert, Sectigo) | $300–600/yr | Instant SmartScreen trust, no reputation ramp. | Overkill pre-revenue. Individuals often rejected. Skip. |
| Self-signed / unsigned | $0 | — | SmartScreen red wall. Conversion killer. Internal builds only. |

### About the current licence

The repo still has an `AGPL-3.0-only` LICENSE and a public GitHub URL from the earlier open-source plan. Two loose ends to close before (or alongside) the first closed-source release:
- Decide whether the repo goes **private**, stays public with **source-available** licence (e.g. BUSL-1.1, Elastic 2.0, PolyForm Noncommercial), or stays AGPL with future selective open-sourcing.
- If the repo goes private, confirm Velopack's auto-update still works: GitHub Releases in a private repo are private too. Either make the release artifacts public (Releases can be made public on a private repo) or move the update feed to a public CDN (S3/R2 + CloudFront).

This is a licensing/distribution policy decision, not a technical one — flagging so it isn't forgotten.

## macOS checklist (direct-download DMG)

1. **Enroll** in the Apple Developer Program as **Individual** (no D-U-N-S required, no company docs — just a personal Apple ID, credit card, and ID verification; usually live within a few hours). The legal name on the Apple ID is what will appear in the Gatekeeper dialog and in `codesign`'s "Signed by" line, so use the name you're OK showing users. We can migrate to *Organization* later by contacting Apple Developer Support — the Team ID and certs carry over.
2. In App Store Connect / Developer portal, create a **Developer ID Application** certificate. Download the `.p12`. Note the Team ID.
3. Generate an **app-specific password** for the Apple ID that owns the team (used by `notarytool`).
4. Store secrets in GitHub Actions: `MACOS_CERTIFICATE_P12_BASE64`, `MACOS_CERTIFICATE_PASSWORD`, `MACOS_NOTARY_APPLE_ID`, `MACOS_NOTARY_TEAM_ID`, `MACOS_NOTARY_PASSWORD`.
5. Extend `release.yml` macOS job to:
   - `security import` the p12 into a temp keychain
   - `codesign --force --options runtime --timestamp --sign "Developer ID Application: <Your Name> (<TeamID>)"` the `.app` (deep, including embedded dylibs and `dimmy_core`)
   - wrap into DMG (`create-dmg` or `hdiutil`) and `codesign` the DMG too
   - `xcrun notarytool submit Dimmy.dmg --wait` → on success, `xcrun stapler staple Dimmy.dmg`
6. Enable **Hardened Runtime** and declare entitlements in `platforms/macos/Dimmy/Dimmy.entitlements`:
   - `com.apple.security.device.audio-input` (mic)
   - `com.apple.security.automation.apple-events` (paste into focused app, if used)
   - `com.apple.security.cs.disable-library-validation` **only if** we load unsigned dylibs (avoid if possible)
7. Verify locally: `spctl --assess -vv Dimmy.app` should say *accepted, source=Notarized Developer ID*.

**Auto-updater (macOS).** Two options:
- **Sparkle** (industry standard). Generate an EdDSA key (`generate_keys`), ship the public key in `Info.plist` as `SUPublicEDKey`, host `appcast.xml` at `https://dimmy.app/appcast.xml`, sign each DMG with `sign_update`. Pros: battle-tested, supports delta updates. Cons: Swift-side integration work.
- **Velopack macOS** (we already use it on Windows). Pros: one codebase for both OSes. Cons: younger on macOS; confirm it supports notarised DMG flow end-to-end before committing.
- **Recommendation:** try Velopack first for parity with Windows. Fall back to Sparkle if its macOS flow is rough.

## Windows checklist (direct-download Setup.exe) — Certum Standard path

Velopack + auto-updater are already wired (see `release.yml`). Remaining work is **signing**, and the cheapest closed-source path is **Certum Standard Code Signing** (~€69–99/yr) with their **SimplySign** cloud HSM.

1. **Buy** the individual OV cert on `shop.certum.eu` → "Standard Code Signing Certificate" → 1 year, individual. Choose **SimplySign** (cloud) — not the USB token option. USB tokens break headless CI.
2. **Validate identity.** Certum requires a video call or document upload (passport / ID card + utility bill). Allow 3–7 days. The cert's CN will be your legal name as it appears on the ID.
3. Certum emits the cert directly into their HSM. Download the **SimplySign Desktop** client once to activate, then **never again** — CI uses the SimplySign API.
4. Add GitHub Actions secrets:
   - `CERTUM_API_USER`, `CERTUM_API_PASSWORD` (SimplySign API credentials)
   - `CERTUM_CERT_THUMBPRINT` (the cert's SHA-1, used by `signtool /sha1`)
5. In `release.yml` and `staging-native.yml`, install Certum's `signtool`-compatible wrapper (they ship a `.dll` that fronts the cloud HSM) and sign **in this order**:
   - `dimmy_lib.dll` and `Dimmy.Windows.exe` **before** `vpk pack`
   - the final `Dimmy-win-Setup.exe` **after** `vpk pack`
   - Always include `/tr http://time.certum.pl /td sha256 /fd sha256` so signatures survive cert rotation.
6. Velopack's `--signTemplate` accepts a full `signtool sign` command — use it to sign the inner binaries in one pass instead of separate steps. Shape:
   ```
   vpk pack ... --signTemplate "signtool sign /sha1 $env:CERTUM_CERT_THUMBPRINT /tr http://time.certum.pl /td sha256 /fd sha256 {{file}}"
   ```
7. Verify: `signtool verify /pa /v Dimmy-win-Setup.exe` → chain rooted at Certum's CA, publisher = your legal name.
8. Update `dev/windows-ci.md` with a new invariant: **every `.exe`/`.dll` shipped to users must have a valid Authenticode signature**; add a CI gate (`signtool verify /pa`) in `test-install.yml`.

**SmartScreen reality check.** A brand-new OV cert has **zero reputation**. First users on Windows 11 will see "More info" → "Run anyway" prompts for roughly the first ~1000–3000 downloads before SmartScreen warms up. Nothing to do except ship, not re-issue the cert, and wait. Symptoms that look like "the signature doesn't work" are almost always just reputation ramp-up.

### Alternative: Azure Trusted Signing (~$120/yr)

If Certum's individual validation fails or is too slow, try **Azure Trusted Signing** (https://learn.microsoft.com/azure/trusted-signing/). Pros: $10/mo flat, OIDC auth from GitHub Actions (no long-lived secret), reputation inherits from Microsoft's pool (faster SmartScreen ramp). Cons: eligibility is selective for individuals — expect an identity-verification step that Microsoft may reject. Only worth pursuing after Certum, not in parallel (you don't need two certs).

## Auto-updater (already in place on Windows)

- Velopack polls `releases.win.json` on GitHub Releases. Keep using GitHub Releases as the CDN for now; if bandwidth or privacy becomes an issue, move to S3/R2 + CloudFront and point Velopack at it.
- macOS: Sparkle or Velopack-macOS as above. Host `appcast.xml` and artifacts on the same CDN used for the DMG download.
- **Kill-switch:** if a release is broken, mark the GitHub Release as draft (already in `RELEASING.md` §Rolling back). Document the same flow for macOS once Sparkle is in.

## Analytics & observability — PostHog

PostHog is the right tool: product analytics + feature flags + session replay + (beta) error tracking, in one backend. EU Cloud covers GDPR. Self-host is an option later.

**What to instrument (minimum viable, opt-in):**
- App launch: version, OS, locale, CPU arch
- Recording events: `recording_started`, `recording_completed` (duration bucket, NOT audio), `transcription_failed` (error family, NOT message body)
- Provider usage: which STT / LLM backend (no keys, no transcripts)
- Auto-update: `update_available`, `update_installed`, `update_failed`

**What NOT to send:** audio, transcripts, API keys, file paths with usernames, anything from `config.json` beyond the fields above. Truncate every error string to 200 chars (same rule as HTTP errors in [`CLAUDE.md`](../CLAUDE.md#production-stability)).

**Implementation:**
- Rust core: use `posthog-rs` crate OR a thin `reqwest` client against `POST https://eu.i.posthog.com/capture/`. Put it behind a `telemetry` feature flag.
- Distinct ID: random UUID generated on first run, stored in `~/.config/dimmy/telemetry_id`. Never tied to user identity.
- **Opt-in, not opt-out.** First-run screen asks: "Help improve Dimmy? Sends anonymous usage, never audio." Default OFF in the EU.
- Kill switch: a single config flag `telemetry_enabled: bool` short-circuits every call.

**Crash reporting.** PostHog's error tracking is still beta; for real crash symbolication add **Sentry** later (separate SDK, separate DSN). Not urgent for v1.

## Order of work (suggested)

1. **Apple Developer enrollment as Individual** — a few hours; start first.
2. **Certum Standard Code Signing purchase + ID validation** in parallel — 3–7 days is the long pole on Windows.
3. **Decide licence + repo visibility** (see "About the current licence" above). Blocks nothing technical but needs a call before the first public release.
4. **macOS signing + notarisation** in `release.yml` as soon as the Apple cert is issued.
5. **Windows signing** via Certum SimplySign in `release.yml` + `staging-native.yml` — biggest UX win, kills the SmartScreen red wall.
6. **macOS auto-updater** (Velopack-macOS preferred for parity, Sparkle as fallback).
7. **PostHog integration** (opt-in), ship with the next signed release.
8. **Sentry** (optional, when crash reports from the field become the bottleneck).

**Total upfront cost: ~$170–220/yr** ($99 Apple + €69–99 Certum). No recurring infra costs on top while PostHog/Sentry stay on free tiers.

## Things NOT to do

- Do not ship an unsigned binary "just for now" — SmartScreen / Gatekeeper reputation starts on the first signed artifact; every unsigned release is a reset.
- Do not buy a **USB-token** code-signing cert. They cannot sign from GitHub Actions without manual intervention — cloud HSM only (SimplySign, Azure TS, eSigner).
- Do not re-issue the Certum cert trying to "fix" SmartScreen warnings during the reputation ramp — you lose what little reputation you'd accrued. Ship, wait.
- Do not pay for a commercial EV cert pre-revenue. The instant-trust UX doesn't justify $300+/yr at this stage.
- Do not send any user content (audio, transcripts) to PostHog, ever. This is a hard line, enforced in code review.
- Do not skip notarisation for "internal builds" we share with users — Gatekeeper on recent macOS will block them and cost us trust.

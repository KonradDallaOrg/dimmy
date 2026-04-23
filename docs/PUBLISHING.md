# Publishing Dimmy (outside the stores)

> Direct-download distribution from the Dimmy website. No Mac App Store, no Microsoft Store. Users must see Dimmy as a **signed, notarised, auto-updating** app — not a "unknown developer" warning.
>
> For the release runbook itself see [`RELEASING.md`](RELEASING.md). For Windows CI invariants see [`dev/windows-ci.md`](dev/windows-ci.md).

## TL;DR — what we need to buy / register

| Item | Cost | Lead time | Blocks |
|---|---|---|---|
| Apple Developer Program (Organization) | $99 / yr | 1–5 days (D-U-N-S lookup) | macOS signing & notarisation |
| Apple "Developer ID Application" certificate | included | minutes | `codesign` for direct distribution |
| Apple app-specific password + Team ID | included | minutes | `notarytool submit` |
| Windows OV Code Signing cert (e.g. SSL.com, DigiCert, Sectigo) | ~$200–400 / yr | 1–3 days validation | `signtool sign` — shows publisher name, SmartScreen warms up over time |
| Windows **EV** Code Signing cert (optional, recommended) | ~$300–600 / yr | 3–10 days (HW token / cloud HSM) | instant SmartScreen trust, kernel-mode if ever needed |
| PostHog account (Cloud EU) | free tier → paid | minutes | product analytics, feature flags, session replay |
| Sentry account (optional) | free tier | minutes | crash/error reporting |

Note: since Jun 2023 **Windows code-signing keys must live in an HSM or cloud KMS**. Buy the certificate with a cloud-signing option (SSL.com eSigner, DigiCert KeyLocker, Azure Trusted Signing) so CI can sign without shipping a YubiKey to a GitHub runner. Azure Trusted Signing is the cheapest path (~$10/mo) but requires an Azure tenant ≥ 3 years old or an identity validation.

## macOS checklist (direct-download DMG)

1. **Enroll** the company in the Apple Developer Program as *Organization* (needs D-U-N-S number — free from Dun & Bradstreet, ~2 days).
2. In App Store Connect / Developer portal, create a **Developer ID Application** certificate. Download the `.p12`. Note the Team ID.
3. Generate an **app-specific password** for the Apple ID that owns the team (used by `notarytool`).
4. Store three secrets in GitHub Actions:
   - `MACOS_CERTIFICATE_P12_BASE64` (the .p12)
   - `MACOS_CERTIFICATE_PASSWORD`
   - `MACOS_NOTARY_APPLE_ID`, `MACOS_NOTARY_TEAM_ID`, `MACOS_NOTARY_PASSWORD`
5. Extend `release.yml` macOS job to:
   - `security import` the p12 into a temp keychain
   - `codesign --force --options runtime --timestamp --sign "Developer ID Application: <Org> (<TeamID>)"` the `.app` (deep, including embedded dylibs and `dimmy_core`)
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

## Windows checklist (direct-download Setup.exe)

Velopack + auto-updater are already wired (see `release.yml`). Remaining work is **signing**.

1. Buy an **OV or EV Code Signing certificate** with **cloud-signing** (Azure Trusted Signing / SSL.com eSigner / DigiCert KeyLocker). Avoid physical USB tokens — they break CI.
2. Go through identity validation (company docs, phone verification).
3. Add secrets to GitHub Actions (exact names depend on provider; for Azure Trusted Signing):
   - `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`
   - signing account + certificate profile name
4. Extend `release.yml` and `staging-native.yml` to sign **in this order**:
   - `dimmy_lib.dll` and `Dimmy.Windows.exe` **before** `vpk pack`
   - the final `Dimmy-win-Setup.exe` **after** `vpk pack` (Velopack supports `--signTemplate` to sign embedded binaries in one pass — use it)
   - Include `/tr http://timestamp.digicert.com /td sha256 /fd sha256` so signatures remain valid after the cert expires.
5. Verify: `signtool verify /pa /v Dimmy-win-Setup.exe` → all chains valid; on a clean Windows 11 VM, SmartScreen should either (OV) show the publisher name with a "Run" button after a reputation period, or (EV) launch silently.
6. Update `windows-ci.md` with a new invariant: **every `.exe`/`.dll` shipped to users must have a valid Authenticode signature**; add a CI gate (`signtool verify /pa`) in `test-install.yml`.

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

1. **Windows signing** (biggest UX win: removes SmartScreen wall). Buy Azure Trusted Signing, wire into `release.yml`. ~1 week end-to-end including cert validation.
2. **Apple Developer enrollment** in parallel (D-U-N-S delay is the long pole).
3. **macOS signing + notarisation** in `release.yml`.
4. **macOS auto-updater** (Velopack or Sparkle).
5. **PostHog integration** (opt-in), ship with the next signed release.
6. **Sentry** (optional, when crash reports from the field become the bottleneck).

## Things NOT to do

- Do not ship an unsigned binary "just for now" — SmartScreen / Gatekeeper reputation starts on the first signed artifact; every unsigned release is a reset.
- Do not store the signing key in a GitHub secret as a raw `.pfx`. Use a cloud KMS / HSM path.
- Do not send any user content (audio, transcripts) to PostHog, ever. This is a hard line, enforced in code review.
- Do not skip notarisation for "internal builds" we share with users — Gatekeeper on recent macOS will block them and cost us trust.

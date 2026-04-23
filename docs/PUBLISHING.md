# Publishing Dimmy (outside the stores)

> Direct-download distribution from the Dimmy website. No Mac App Store, no Microsoft Store. Users must see Dimmy as a **signed, notarised, auto-updating** app — not a "unknown developer" warning.
>
> For the release runbook itself see [`RELEASING.md`](RELEASING.md). For Windows CI invariants see [`dev/windows-ci.md`](dev/windows-ci.md).

## TL;DR — minimum-cost track (individual, closed-source, private repo)

Dimmy ships as closed-source for now (may open-source selected components later), and the repo is private. That disqualifies every free OSS-only signing program. The cheapest path that still gives us a usable product on both OSes is a **two-phase plan**:

- **Phase 1 — ship now: $99/yr (Apple only).** macOS signed + notarised, Windows **unsigned** with a clear "How to install" explainer page on the site. SmartScreen shows a "More info → Run anyway" warning — ugly, but the app runs. Gatekeeper blocks unsigned macOS apps much harder, so macOS is the mandatory spend, not Windows.
- **Phase 2 — when justified (revenue or user complaints): +~€69–120/yr for Windows signing.** Add a Certum Standard individual OV cert (or Azure Trusted Signing) to kill the SmartScreen warning.

| Item | Phase | Cost | Lead time | Notes |
|---|---|---|---|---|
| Apple Developer Program (**Individual**) | **1** | $99 / yr | hours | No D-U-N-S. Cert CN = your legal name. Upgrade to Organization later, Team ID + certs carry over. |
| Apple "Developer ID Application" cert + app-specific password | **1** | included | minutes | `codesign` + `notarytool` for direct DMG distribution |
| **Windows: ship unsigned + install instructions page** | **1** | $0 | — | SmartScreen "More info → Run anyway" UX. Acceptable for early access; not for scale. |
| PostHog Cloud EU | **1** | free tier (1M events/mo) | minutes | product analytics, feature flags, session replay |
| Certum Standard Code Signing (individual OV, SimplySign cloud HSM) | **2** | ~€69–99 / yr | 3–7 days ID validation | Kills SmartScreen warning after reputation ramp. Do this when Phase 1 friction starts costing us installs. |
| Sentry | optional | free tier (5k errors/mo) | minutes | crash reporting |

**Why no free Windows signing?** Self-signed Authenticode certs don't chain to a trusted root, so Windows treats them as untrusted (identical UX to unsigned). The only "free" Windows cert programs (SignPath Foundation, Certum Open Source) require a qualified OSS project on a **public** repo. Both disqualified for us.

### Windows signing — options ranked for Phase 2

| Option | Cost | Pros | Cons |
|---|---|---|---|
| **Certum Standard (individual OV)** | ~€69–99/yr | Cheapest reliable path. SimplySign cloud HSM works in CI. EU-based (GDPR-friendly). | Slow SmartScreen ramp (1000–3000 downloads before warning clears). |
| **Azure Trusted Signing** | ~$120/yr | OIDC auth from GitHub Actions (no long-lived secret). Reputation inherits from Microsoft pool → faster ramp. | Individual eligibility is selective; Microsoft may reject. |
| **SSL.com OV with eSigner** | ~$170–230/yr | Mature cloud-HSM API. | ~3× Certum for same trust level. |
| Commercial EV (DigiCert, Sectigo) | $300–600/yr | Instant SmartScreen trust. | Individuals often rejected. Skip until there's revenue. |

### Private-repo caveat for auto-update

GitHub Releases in a **private** repo are private — Velopack can't download updates anonymously. Two ways to fix this:
- **Mark the Release itself public.** In the GitHub UI, a release on a private repo can be flipped to "public" — artifact URLs then work without auth. Check that this option is still exposed (behaviour has changed before).
- **Mirror artifacts to a public CDN** (S3 / Cloudflare R2 + public bucket) and point Velopack's update feed at that URL. Slightly more work, fully independent of GitHub's private-repo policy.

Verify this **before** shipping the first auto-updating release, or every user will be stuck on whatever version they first installed.

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

## Windows checklist (direct-download Setup.exe)

Velopack + auto-updater are already wired (see `release.yml`). Two-phase plan.

### Phase 1 — ship unsigned (free)

Velopack already builds a working `Dimmy-win-Setup.exe`. Ship that as-is, with a dedicated install page on the website that walks users through the SmartScreen bypass so the red warning doesn't scare them off.

1. **Website install page** (e.g. `dimmy.app/download/windows`) — one screenshot per step:
   - Download `Dimmy-Setup.exe`
   - Double-click → if SmartScreen appears, click **"More info"** → **"Run anyway"**
   - Installer runs, app launches
   - One line explaining why: "We're working on code-signing. The installer is safe — see our GitHub Release checksums: [link]"
2. **Publish SHA-256 checksums** alongside each release (GitHub's UI does this automatically for assets, but add them to the install page too) so paranoid users can verify.
3. **Leave `release.yml` and `staging-native.yml` as-is** — no signing secrets, no signtool step. One less thing to break.
4. **Do not self-sign**: a self-signed cert adds zero trust (Windows still shows SmartScreen), makes the chain look more suspicious, and resets the SmartScreen reputation once you *do* buy a real cert. Ship plain unsigned.

**Expected friction.** Modern Windows Defender SmartScreen is aggressive with unsigned installers — expect ~30–50% of first-time users to bounce on the red warning if the install page isn't prominent. That's the cost of Phase 1. Track the install→first-launch conversion in PostHog (`installer_downloaded` vs `app_first_launch`); when the drop-off gets painful, flip to Phase 2.

### Phase 2 — add signing (when justified)

Go with **Certum Standard Code Signing** (~€69–99/yr) via **SimplySign** cloud HSM.

1. **Buy** on `shop.certum.eu` → "Standard Code Signing Certificate" → 1 year, individual. Choose **SimplySign** (cloud), NOT the USB token option — USB tokens can't sign from CI.
2. **Validate identity.** Document upload or video call (passport / ID card + proof of address). 3–7 days. Cert CN = your legal name.
3. Certum emits the cert directly into SimplySign's HSM. Install **SimplySign Desktop** once to activate, then CI uses the API.
4. Add GitHub Actions secrets: `CERTUM_API_USER`, `CERTUM_API_PASSWORD`, `CERTUM_CERT_THUMBPRINT` (SHA-1, for `signtool /sha1`).
5. In `release.yml` and `staging-native.yml`:
   - Install Certum's `signtool` CSP/wrapper on the runner
   - Sign `dimmy_lib.dll` and `Dimmy.Windows.exe` **before** `vpk pack`
   - Sign the final `Dimmy-win-Setup.exe` **after** `vpk pack`
   - Always include `/tr http://time.certum.pl /td sha256 /fd sha256`
6. Velopack's `--signTemplate` accepts a full `signtool sign` command — use it to sign embedded binaries in one pass:
   ```
   vpk pack ... --signTemplate "signtool sign /sha1 $env:CERTUM_CERT_THUMBPRINT /tr http://time.certum.pl /td sha256 /fd sha256 {{file}}"
   ```
7. Verify: `signtool verify /pa /v Dimmy-win-Setup.exe` → chain rooted at Certum, publisher = your legal name.
8. Add to `dev/windows-ci.md`: new invariant that every shipped `.exe`/`.dll` must have a valid Authenticode signature; add a CI gate (`signtool verify /pa`) in `test-install.yml`.
9. Update the install page — remove the SmartScreen bypass instructions.

**SmartScreen reputation ramp.** A brand-new OV cert starts at **zero reputation**. Expect "More info → Run anyway" prompts for the first 1000–3000 downloads. Don't re-issue the cert trying to "fix" it; you'd reset the reputation.

### Phase 2 alternative: Azure Trusted Signing

If Certum's individual validation fails, try **Azure Trusted Signing** (~$120/yr). OIDC auth from GitHub Actions (no long-lived secret), and SmartScreen reputation inherits faster from Microsoft's pool. Downside: individual eligibility is selective and Microsoft may reject. Don't pursue in parallel with Certum — one cert is enough.

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

**Phase 1 — get to "shippable" ($99/yr):**
1. **Apple Developer enrollment as Individual** — a few hours; start first, it's the only long-lead-free blocker.
2. **Verify private-repo auto-update path** — confirm Velopack can fetch releases (public-release-on-private-repo, or mirror to CDN).
3. **macOS signing + notarisation** in `release.yml` once the Apple cert is in hand.
4. **macOS auto-updater** (try Velopack-macOS first for parity with Windows, Sparkle as fallback).
5. **Windows install page** on the website with the SmartScreen bypass walkthrough + SHA-256 checksums.
6. **PostHog integration** (opt-in), ship with the next release so we have install-funnel telemetry before Phase 2.
7. **Ship v1.0.0** — signed/notarised macOS, unsigned-but-documented Windows.

**Phase 2 — polish when the data says so:**
8. **Windows signing** via Certum SimplySign (or Azure Trusted Signing) when PostHog funnel data or user reports show SmartScreen is costing us too many installs.
9. **Sentry** (optional, when crashes in the field become the bottleneck we can't debug from PostHog alone).

**Upfront cost: $99/yr** (Apple only). Phase 2 adds ~€69–120/yr when triggered by real data, not upfront guessing.

## Things NOT to do

- Do not ship an **unsigned macOS** build — Gatekeeper blocks it with no easy bypass and the user blames us. macOS signing is non-negotiable from day one.
- Do not **self-sign** the Windows installer — a self-signed Authenticode cert adds zero trust (Windows still flags it) and resets SmartScreen reputation when you later switch to Certum.
- Do not buy a **USB-token** code-signing cert when you reach Phase 2 — they can't sign from GitHub Actions headlessly. Cloud HSM only (SimplySign, Azure TS, eSigner).
- Do not re-issue the Certum cert trying to "fix" SmartScreen warnings during the reputation ramp — you lose what little reputation accrued. Ship, wait.
- Do not pay for a commercial EV cert pre-revenue. $300+/yr isn't justified by the instant-trust UX at this stage.
- Do not send any user content (audio, transcripts) to PostHog, ever. Hard line, enforced in code review.
- Do not skip notarisation for "internal builds" shared with users — Gatekeeper blocks them and costs us trust.

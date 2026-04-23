# Publishing Dimmy (outside the stores)

> Direct-download distribution from the Dimmy website. No Mac App Store, no Microsoft Store. Users must see Dimmy as a **signed, notarised, auto-updating** app — not a "unknown developer" warning.
>
> For the release runbook itself see [`RELEASING.md`](RELEASING.md). For Windows CI invariants see [`dev/windows-ci.md`](dev/windows-ci.md).

## TL;DR — minimum-cost track (individual, OSS project)

Dimmy is AGPL-3.0 on a public GitHub repo → we qualify for OSS-only signing programs. The cheapest realistic bill is **~$99/yr total** (Apple), with Windows signing **free** via SignPath Foundation.

| Item | Cost | Lead time | Notes |
|---|---|---|---|
| Apple Developer Program (**Individual**) | $99 / yr | hours (no D-U-N-S needed) | Cert reads "Developer ID Application: Firstname Lastname (TEAMID)". Upgrade to Organization later without losing the Team ID. |
| Apple "Developer ID Application" cert + app-specific password | included | minutes | `codesign` + `notarytool submit` for direct distribution |
| **SignPath Foundation** (Windows, free for OSS) | $0 | ~1–2 weeks approval | They own the OV cert in an HSM and sign our binaries via GitHub Action. Requires: public repo, OSI-approved licence (AGPL qualifies), maintainer identity check, signed Submitter Agreement. |
| PostHog Cloud EU | free tier (1M events/mo) | minutes | product analytics, feature flags, session replay |
| Sentry (optional, later) | free tier (5k errors/mo) | minutes | crash reporting |

**No free option exists for macOS signing.** Self-signed `.app` bundles are blocked by Gatekeeper on any modern macOS; notarisation requires an Apple Developer membership. The $99/yr is the minimum viable cost.

### Fallbacks if SignPath approval is slow or denied

| Option | Cost | When to use |
|---|---|---|
| **Certum Open Source Code Signing** | ~€28/yr (promotional) | If SignPath rejects us or takes >2 weeks. Still needs HSM — Certum provides SimplySign cloud signing. Shows "Open Source Developer, <name>" as publisher. |
| **Azure Trusted Signing** | ~$10/mo + Azure tenant | If we later need closed-source signing. Eligibility tightened in 2024 — individuals need identity verification, businesses need ≥3 years history. |
| **Unsigned + manual user bypass** | $0 | Never for public users — SmartScreen full-screen block is a conversion killer. Internal testing only. |
| Commercial OV/EV (DigiCert, Sectigo) | $200–600/yr | Only once we're a company with revenue. Skip for now. |

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

## Windows checklist (direct-download Setup.exe) — SignPath Foundation path

Velopack + auto-updater are already wired (see `release.yml`). Remaining work is **signing**, and the cheapest path for an AGPL OSS project is **free via SignPath Foundation** (https://signpath.org/). They hold an OV cert in an HSM and sign on our behalf via a GitHub Action — we never touch a `.pfx`.

1. **Apply** at https://about.signpath.io/foundation. Submit: repo URL (`github.com/KonradDallaOrg/dimmy`), project description, maintainer name + government ID (KYC), AGPL licence link. Approval typically 1–2 weeks.
2. Once approved, SignPath creates a project + signing policy. We configure an **Artifact Configuration** that matches our Velopack output (`Dimmy-win-Setup.exe` + embedded `Dimmy.Windows.exe` + `dimmy_lib.dll`).
3. In `release.yml`, after `vpk pack`, call the **SignPath GitHub Action** (`signpath/github-action-submit-signing-request`):
   - Uploads the unsigned Setup.exe as an artifact
   - Blocks until SignPath signs it (cert lives in their HSM)
   - Downloads the signed `.exe` back into the workflow
   - Only the Setup.exe needs signing externally — Velopack's `--signTemplate` handles signing the embedded `.exe` and `.dll` in the same pass *if* we pass a `signtool`-compatible command. With SignPath Foundation the cleanest flow is: sign `dimmy_lib.dll` and `Dimmy.Windows.exe` via SignPath **before** `vpk pack`, then sign the final `Setup.exe` via SignPath **after**.
4. GitHub Actions secrets needed: `SIGNPATH_API_TOKEN`, `SIGNPATH_ORG_ID`, `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG`.
5. Timestamping is handled by SignPath's policy (set to SHA-256 + RFC 3161). Signatures remain valid after cert rotation.
6. Verify: `signtool verify /pa /v Dimmy-win-Setup.exe` → valid chain rooted at SignPath's CA. Publisher shows as "Open Source Developer, <our name>".
7. Update `windows-ci.md` with a new invariant: **every `.exe`/`.dll` shipped to users must have a valid Authenticode signature**; add a CI gate (`signtool verify /pa`) in `test-install.yml`.

**SmartScreen reality check.** SignPath Foundation uses a shared "Open Source Developer" identity — reputation is accrued per-publisher, not per-project, so we start with *some* reputation inherited from other OSS projects signed through them. First-time users on Windows 11 may still see a "More info" → "Run anyway" prompt for the first few hundred downloads, then it clears. An EV cert ($300+/yr) is the only way to get instant trust; not worth it at our stage.

### Fallback: Certum Open Source

If SignPath is not an option, buy a **Certum Open Source Code Signing** cert (~€28/yr). It's a standard OV cert restricted to OSS projects. Use their **SimplySign** cloud HSM (no USB token needed) and wire it into CI the same way as any cloud-signing provider. Shows publisher as "Open Source Developer, <name>".

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

1. **Apple Developer enrollment as Individual** — fastest thing to kick off (a few hours), and it's the one blocker nothing else can work around.
2. **SignPath Foundation application** in parallel — 1–2 week approval window, so start early. If it stalls, buy Certum Open Source (€28/yr) and keep moving.
3. **macOS signing + notarisation** in `release.yml` as soon as the Apple cert is issued.
4. **Windows signing** via SignPath (or Certum) in `release.yml` + `staging-native.yml` — biggest UX win, removes SmartScreen wall.
5. **macOS auto-updater** (Velopack-macOS preferred for parity, Sparkle as fallback).
6. **PostHog integration** (opt-in), ship with the next signed release.
7. **Sentry** (optional, when crash reports from the field become the bottleneck).

**Total upfront cost to ship a signed + notarised + auto-updating Dimmy on both OSes: $99/yr.** Everything else on the checklist is free as long as SignPath approves us.

## Things NOT to do

- Do not ship an unsigned binary "just for now" — SmartScreen / Gatekeeper reputation starts on the first signed artifact; every unsigned release is a reset.
- Do not store the signing key in a GitHub secret as a raw `.pfx`. With SignPath / Certum the key never leaves the HSM, which is the correct model.
- Do not pay for a commercial OV/EV cert while the OSS free tier works. The UX difference (publisher name vs instant trust) doesn't justify $300+/yr for a pre-revenue project.
- Do not send any user content (audio, transcripts) to PostHog, ever. This is a hard line, enforced in code review.
- Do not skip notarisation for "internal builds" we share with users — Gatekeeper on recent macOS will block them and cost us trust.

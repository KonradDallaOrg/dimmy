# RFC: publishing Dimmy outside the stores — signing, updates, analytics

**Status:** Draft — needs Konrad's input on the marked decisions.
**Date:** 2026-04-23
**Companion doc:** [`docs/PUBLISHING.md`](../../PUBLISHING.md) — the actionable checklist. Read this RFC for the **why**; read PUBLISHING.md for the **how**.

## Problem

We want Dimmy to look like a serious product on macOS and Windows: signed, notarised where it applies, auto-updating, with product analytics and observability. No store distribution — direct download from the site. And we want to spend as little as possible.

Constraints as of today:
- **No company yet.** Any developer-account enrollment is individual.
- **Repo is private and closed-source.** We may open-source *selected components* later but not the whole app.
- **Tight budget.** Free is preferred; paid only where there's no alternative.

## Summary of recommendation

Two-phase plan. **Phase 1 costs $99/yr total (Apple only) and is enough to launch.** Phase 2 adds ~€69–120/yr for Windows signing, triggered by real install-funnel data rather than upfront spend.

| | Phase 1 (launch) | Phase 2 (scale) |
|---|---|---|
| macOS | Signed + notarised via **Apple Developer Individual** ($99/yr) | unchanged |
| Windows | **Unsigned**, with an install page explaining the SmartScreen bypass + SHA-256 checksums | Sign via **Certum Standard OV** (~€69–99/yr) once PostHog data shows SmartScreen is killing installs |
| Auto-updater | Velopack on Windows (already wired). macOS: try Velopack-macOS first, fall back to Sparkle. | unchanged |
| Analytics | **PostHog Cloud EU**, opt-in, anonymous UUID | unchanged |
| Crash reporting | defer | Sentry when needed |

## Decisions that need Konrad's input

### 1. Apple enrollment as Individual, not Organization

**Recommendation:** Enrol Individual now.

**Why:** No D-U-N-S number needed (Organization requires one; it's a free 2-day wait at Dun & Bradstreet, but only bookable by a registered legal entity — which we don't have). Individual enrollment is instant (hours). We can migrate to Organization later without losing the Team ID or certs.

**Trade-off:** The Developer ID cert's Common Name will be *our legal personal name* (`Developer ID Application: Firstname Lastname (TEAMID)`). That's what users see in the Gatekeeper "Verified developer" dialog, and what `codesign -dv` shows. If we're uncomfortable with a personal name on the distribution, the only fix is to constitute a company and re-enrol as Organization — which is a much bigger project.

**Ask:** Konrad, are you OK with **your legal name** appearing as the publisher of Dimmy on every macOS install until we form a company? If not, flag now — we'll need to rethink the timeline.

### 2. Ship Windows unsigned in Phase 1

**Recommendation:** Don't spend on Windows signing yet.

**Why:** Windows and macOS have asymmetric tolerance for unsigned apps. Unsigned macOS is **effectively blocked** by Gatekeeper — users must dig through System Settings → Privacy & Security, which is a dead-end for most. Unsigned Windows triggers SmartScreen's red warning but users **can click through** in two clicks ("More info" → "Run anyway"). The delta: macOS signing is mandatory; Windows signing is quality-of-life.

Paying €69–99/yr for a Certum OV cert *before we have install data* is premature optimisation. A fresh OV cert has zero SmartScreen reputation anyway — users still see warnings for the first 1000–3000 downloads. We'd be paying to go from "red warning with bypass" to "yellow warning with bypass", which isn't obviously worth it until the Phase 1 funnel data says otherwise.

**Trade-off:** ~30–50% of first-time Windows users may bounce on the red warning. This is the visible cost. Mitigations in Phase 1:
- A prominent install page on the website with the exact click path and a SHA-256 checksum table.
- PostHog funnel tracking (`installer_downloaded` → `app_first_launch`) so we *know* when the drop-off justifies Phase 2 spend.

**Ask:** Konrad, OK with shipping Windows unsigned in Phase 1 and moving to Certum reactively? Or do you prefer paying the €69 up front to avoid the aesthetic of a red warning on launch day?

### 3. Private repo + Velopack auto-updater

**Known risk, not yet verified.** Velopack fetches updates from GitHub Releases. On a private repo, Releases are private by default — anonymous downloads 404. Two options:

- **Public release on private repo.** GitHub lets you mark individual Releases as "public" on a private repo (artefact URLs then auth-free). Need to double-check this UI option is still there; GitHub has flipped it in the past.
- **Mirror artefacts to a public CDN.** S3 / Cloudflare R2 + public bucket + a custom `releases.win.json` hosted alongside. More work, fully independent.

**Ask:** Konrad — any preference between these two, or should we just verify option 1 works and pick it if so? Option 2 is also useful if we ever want to serve downloads from our own domain for analytics/conversion reasons.

### 4. Auto-updater on macOS: Velopack vs Sparkle

**Recommendation:** Try Velopack-macOS first.

**Why:** We already run Velopack on Windows. Single codebase, single release pipeline, single update-feed format = fewer places for drift. Sparkle is the macOS industry standard and more mature, but Velopack-macOS has gotten serious in the last year and supports notarised DMG flows.

**Fallback:** If Velopack-macOS breaks on notarisation or staple-after-sign edge cases, drop to Sparkle (EdDSA keys, `appcast.xml`, `sign_update`). Budget a day to validate Velopack end-to-end before committing.

**Ask:** Konrad — any strong preference? If you've used Sparkle in the past and prefer it, call it.

### 5. PostHog opt-in vs opt-out

**Recommendation:** Opt-in, default OFF, first-run prompt.

**Why:** We're in the EU. Opt-out analytics without consent violates GDPR for anything that could be considered personal data, and our distinct-ID UUID *probably* qualifies. Opt-in is safer and simpler to defend. It also costs us some data — fewer events, skewed sample toward engaged users — but that's an acceptable trade-off for not getting into legal trouble.

**Hard rules (already in PUBLISHING.md, worth re-stating here):**
- Never send audio.
- Never send transcripts.
- Never send API keys.
- Never send file paths (they contain usernames).
- Truncate all error strings to 200 chars at the call site (same rule we already apply to HTTP error bodies — see `CLAUDE.md`).

**Ask:** Konrad — agreed on opt-in? If you want opt-out, we need a legal review before shipping.

### 6. Open-sourcing selected components later

**Implication worth flagging.** If at some future point parts of Dimmy do go open source (e.g. `core/` but not the native UIs), we could apply to **SignPath Foundation** — they offer free OV code-signing for qualified OSS projects. That's a ~€70/yr saving in Phase 2 if we ever want to switch. Similarly, Certum Open Source (~€28/yr) becomes an option.

**No decision needed now.** Just: if the open-source conversation happens later, one consequence is a cheaper signing bill. Not a reason to open-source on its own, but a nice-to-have.

## Out of scope for this RFC

- **Store distribution** (MAS, MS Store): explicit non-goal for now.
- **Linux signing/distribution**: AppImage flow is already in place and doesn't benefit from the same cert model. Separate doc.
- **Windows MSIX**: would need Store registration ($19 one-time individual). Store distribution again — not pursuing.
- **Enterprise customers** (MDM, SSO, SOC 2): not a target yet.

## Total cost comparison

| | Up front (Phase 1) | If/when Phase 2 triggers | Total ongoing |
|---|---|---|---|
| This plan | **$99/yr** (Apple) | +~€69–99/yr (Certum) | ~$170–220/yr |
| Aggressive "sign everything now" | ~$170–220/yr | — | ~$170–220/yr |
| Enterprise-grade (EV cert etc.) | ~$400–700/yr | — | ~$400–700/yr |

Phase 1 of our plan is **half the cost** of "sign everything now" with marginally worse Windows first-launch UX. When data tells us the UX is hurting us, we upgrade.

## Open questions

- Do we want a dedicated Italian-legal-entity tax advisor to weigh in before any of this? (Downloadable paid-for app from an EU individual is a tax-filing concern — software VAT rules, distance-selling thresholds, MOSS. Probably a conversation for later when we have revenue to worry about, but flagging.)
- Is `dimmy.app` (or similar) registered? The install page needs a stable URL and the macOS auto-updater `appcast.xml` needs a permanent home.

# Mac screenshot harness — handoff (2026-06-16)

**You are a Claude CLI session on a Mac (MacInCloud).** Your job: produce the
website/docs screenshots for the **macOS** Dimmy app, to parity with the
already-working Windows harness. Run end-to-end, no human clicking.

## TL;DR — run this
```bash
bash scripts/dev/capture-mac.sh
```
It builds the Rust static lib + the .app, launches it with
`DIMMY_SCREENSHOT_ALL=1` (the in-process `SettingsScreenshotter` renders
everything), and collects PNGs under `out/mac-settings-screenshots/` (+
`.../help/...`). If `xcodebuild` or the Swift harness fails to compile, **fix
the Swift** (you have a Mac; the author wrote it blind on Windows) and re-run.

## The target: website screenshot manifest
The source of truth for WHAT images are needed lives in the website repo:
`~/code/dimmy-website/help-sync/screenshot-manifest.json`. Each item has a
`src` like `/assets/help/<slug>/<name>.png` and a `platform`. The Mac-specific
slots are the onboarding ones: `first-dictation/mac-{welcome,model,shortcut,tryit}`.
Most `app-settings` slots are platform-agnostic (one screenshot serves the
article) — Windows already produced them; Mac versions are a nice-to-have
("se ne hai in più non fa male"). Read that manifest first.

## What the Windows harness does (your parity reference)
- `platforms/windows/Dimmy.Windows.UiTests/MarketingCapture.cs` — env-gated
  (`DIMMY_CAPTURE=1`) FlaUI harness. Coverage, all light+dark:
  - Onboarding: every step (welcome/model/shortcut/tryit).
  - Settings: every tab, Advanced ON, window resized tall (full page).
  - Meeting: open window, select first past meeting → done-view (recap +
    transcript + waveform).
  - Pill: force each `AppState` via the `--command demo-pill:<state>` deep link
    (added to `App.xaml.cs`), screen-capture each, compose a states-strip.
  - Wizards: open the Notion + Claude Desktop modal wizards (via "Change
    destination" / "Re-run wizard" when already connected), capture.
  - Models: expand the On-device provider card.
  - Emits `docs/ui-shots/win/help/manifest-map.json` mapping produced files to
    manifest `src` + a status.
- Output (gitignored): `docs/ui-shots/win/help/<slug>/<name>-<theme>.png`.

## What's already written for Mac (this branch)
`platforms/macos/Dimmy/Utilities/SettingsScreenshotter.swift` (the proven
in-process shooter) was EXTENDED with:
- `captureOnboarding(modes:)` — presents `OnboardingContainerView(appState:, startStep:)`
  per step (0..4) offscreen via `renderHosted` and writes
  `help/first-dictation/mac-{welcome,permissions,shortcut,model,tryit}-{light,dark}.png`.
- `capturePillStates()` — sets `AppState.shared.recordingState` through
  idle/recording/transcribing/processing/completing, captures the live pill
  panel → `help/pill/state-<label>.png`.
- Settings tabs already worked (all tabs, advanced ON, light+dark) →
  `out/mac-settings-screenshots/<tab>-<mode>.png`.

`scripts/dev/capture-mac.sh` builds + runs + collects.

## Your TODO (complete to Windows parity)
1. **Make it compile + run.** The Swift above was written without a Mac. Fix
   any API mismatches (e.g. `OnboardingContainerView` env requirements,
   `RecordingState` cases, pill panel discovery). Re-run `capture-mac.sh`.
2. **Pill states** — verify the panel is found and each state renders
   distinctly (Windows shows: idle dot, recording rainbow+timer, transcribing,
   processing, done check, error ring). If the offscreen panel is empty,
   capture the live pill window on the desktop instead. Add the `error` state.
   Compose a `pill/states-strip.png` (see Windows `ComposeStrip`).
3. **Meeting done-view** — open the meeting window, select the first past
   meeting, capture the recap+transcript+waveform. Look at
   `platforms/macos/Dimmy/Views/Meeting/MeetingViewModel.swift` (`selectedDir`,
   `phase`) and how the meeting window is presented. In-process you can set the
   VM directly.
4. **Wizards** — present the Notion / Claude Desktop / Codex connect sheets and
   capture them. (`MacRulesPage` / `MacOutputPage` / Integrations views.)
5. **Manifest naming + map** — rename/copy outputs to the manifest `src` names
   and emit a `manifest-map.json` like Windows, so the website agent can drop
   them into `~/code/dimmy-website/assets/help/<slug>/`.
6. Real data is fine for now (the user confirmed). Light + dark for every shot.

## Deliver
Leave PNGs under `out/mac-settings-screenshots/` (gitignored) OR copy them into
`docs/ui-shots/mac/help/<slug>/...`. Do NOT commit images. The user pulls them.
If you change the Swift, that's fine to leave on this branch (it's a throwaway
harness branch, likely never merged).

## Constraints (from CLAUDE.md — still apply)
- Don't break the frozen build feature sets. `capture-mac.sh` uses the Mac
  release feature set already.
- No em-dashes / tildes in any user-facing copy you touch.
- This is a Debug build; flavor=prod, config dir `dimmy`.

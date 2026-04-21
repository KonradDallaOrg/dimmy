# macOS Onboarding & Hotkey Redesign — Design Doc

**Date:** 2026-04-21
**Version target:** Dimmy 0.7.0 (post v0.6.2 baseline)
**Scope:** macOS platform only — no Rust core changes, no Windows/Linux UI changes.

## Context

v0.6.2 on `main` ships a working DMG, but a local ~600 LoC work-in-progress rewrites onboarding and hotkey handling and is **not stable enough to ship**. Two concrete failure modes are blocking:

1. **Permission UI stays in the "not granted" state** after the user actually grants the permission in System Settings. Root causes: polling-based `@Published` propagation under `LSUIElement=true` (accessory app) is flaky, and repeated terminal builds of a DevID-signed bundle from changing paths occasionally confuse TCC.
2. **Fn-only shortcut never triggers the hotkey.** This is **not a Dimmy bug** — macOS pre-consumes Fn for Dictation/Globe/Siri/Emoji unless the user explicitly disables it, and Fn additionally requires Input Monitoring (HID pipeline) on modern MacBooks. Fn is therefore unsuitable as a default preset.

The secondary failure mode is developer-facing: we have no in-app diagnostic surface, so every debugging round requires `tail /tmp/dimmy-hotkey.log` and educated guesses.

## Goals

- **Day-1 experience works every time** on a clean /Applications install for a user who grants Mic + Accessibility. No silent failures.
- **Reduce moving parts** in onboarding/hotkey: one mechanism for hotkey interception, one source of truth for permissions, one polling loop with a clear purpose.
- **Expose health state** to the user (pill / menu bar) and to the developer (Diagnostics pane in Settings) so problems are visible, not silent.
- **Keep the architecture we already have** (Rust core + FFI, SwiftUI UI, `PermissionsManager` single-source pattern, deferred Rust init). Do not rewrite what works.
- **Zero regressions** on core record→transcribe→paste loop already shipping in v0.6.2.

## Non-goals

- No Rust core changes. No FFI signature changes. No Windows/Linux UI changes.
- No new STT/LLM providers. No model-download rearchitecture (whisper-rs + HuggingFace stays).
- No new UI framework; this is a SwiftUI-only refactor.
- No code-signing or notarization pipeline changes.

---

## Design

### 1. Onboarding: 4 steps instead of 5

New order: **Welcome → Permissions → Shortcut → Try It**.

**Model Download step is removed.** It was the only step that could fail for network reasons during onboarding, and it gated "Try It" even for users who never intend to use local STT. New flow:

- Default `sttMode` stays `"cloud"`.
- Try It step, on appear, inspects `sttMode` + API-key presence + local-model presence:
  - If cloud + has_key → ready. Show live-shortcut prompt.
  - If local + model present → ready. Show live-shortcut prompt.
  - Otherwise → show a **non-blocking setup card** inside the step: two CTAs, "Add API key (Settings)" and "Download local model (78 MB)". Progress rendered inline. "Finish" button always enabled — user can always reach the menu bar.

Progress dots in `OnboardingContainerView` update from 5 to 4.

### 2. Shortcut step: safe presets only, Fn behind disclosure

Visible presets: **⌃⌥ (default)**, ⌃⇧, ⌥⇧. No `fn` in the primary preset row.

A collapsible "Advanced: use Fn key" disclosure, when expanded, shows the Fn preset plus the existing `fnConflictBanner` plus a link to Keyboard Settings. If the user picks Fn, a confirmation modal appears: *"Fn requires disabling macOS Dictation/Globe shortcuts and granting Input Monitoring. Continue?"*. Only on confirm does `appState.shortcut` become `fnOnly`.

Click-to-record and live modifier capture remain available.

### 3. Permissions: single source, trimmed polling

`PermissionsManager` is kept. Changes:

- **Polling cadence** changes from 1.5s to **5s** (safety net, not the driver). Rationale: `didBecomeActiveNotification` is the primary trigger when user returns from System Settings; polling is a fallback for the accessory-app case where that notification is sometimes suppressed.
- Add a **manual `refreshNow()`** call on every permission-row button click, both for the "Grant" flow and after the native dialog returns. Guarantees UI reflects latest TCC state without waiting for the 5 s tick.
- Input Monitoring row **visible only when relevant**: always visible if the user's current shortcut is `isFnOnly`, hidden behind "Show advanced" otherwise. Prevents scaring new users with three permissions when two are enough.

No other logic changes. `@Published` `change-only` assignments already correct.

### 4. Hotkey: single mechanism, visible status

**Remove** the passive NSEvent fallback entirely. It hides Accessibility failures rather than surfacing them. If Accessibility is not granted, the hotkey simply does not work — and the UI must say so.

Introduce `HotkeyStatus` enum on `AppState`:

```swift
enum HotkeyStatus: Equatable {
    case uninstalled      // app just launched, not yet attempted
    case installed        // CGEventTap active
    case accessibilityMissing   // perm not granted
    case tapFailed(reason: String)  // unexpected install failure
}
```

Published on `AppState.hotkeyStatus`. Consumers:

- `StatusBarController` shows a yellow dot + tooltip ("Accessibility missing") when not `.installed`.
- `PillWindowController` shows a small warning overlay on the pill when not `.installed`.
- `DiagnosticsView` shows the full enum + reason.

`HotkeyManager.start()` simplifies to:

```
tryInstallEventTap()
if installed: status = .installed; stopAccessibilityPolling
else if not AXIsProcessTrusted: status = .accessibilityMissing; startAccessibilityPolling
else: status = .tapFailed(reason)
```

Wake observer stays — macOS disables taps during sleep, reinstall is correct. Accessibility polling stays, but strictly bounded: it exists only to flip the status from `.accessibilityMissing` to `.installed` and then tears itself down.

### 5. Diagnostics pane in Settings

New `DiagnosticsSettingsView` shown as a tab in `SettingsContainerView`. Contents (all live-bound, no manual refresh required):

- **Bundle info:** `Bundle.main.bundlePath`, bundle ID, short version + build number
- **TCC state:** microphone / accessibility / input-monitoring (uses `PermissionsManager`)
- **Hotkey status:** `AppState.hotkeyStatus` formatted
- **Recording state:** `AppState.recordingState` + last error
- **Core state:** Rust-core initialized yes/no, sttMode, has-key flags
- **Actions:**
  - "Simulate shortcut press" — calls `HotkeyManager.shared.stopToggleRecording()` or a new `simulatePress()` for test
  - "Reset onboarding" — sets `isOnboardingComplete = false`, closes Settings, reopens onboarding
  - "Open log (`/tmp/dimmy-hotkey.log`)" — `NSWorkspace.open(URL(fileURLWithPath:))`

This pane is the single biggest DX improvement. Every subsequent bug report can start with "screenshot of the Diagnostics pane".

### 6. AppDelegate init chain

Mostly kept. One change: when onboarding was previously completed but permissions are missing at launch (`permissionsGranted() == false`), instead of resetting `isOnboardingComplete = false` and running the full 4-step onboarding, **reopen the onboarding window directly at the Permissions step**. Internal API: `OnboardingContainerView(appState:, startStep: Int)`.

Rationale: the user has already seen Welcome and picked a shortcut — forcing them to click through those again is pure friction when the only thing to fix is a system preference. The existing `currentStep` @State becomes seeded from an `@State private var currentStep: Int` initializer parameter.

### 7. Info.plist

No changes beyond the WIP: `LSUIElement=true`, `NSAppleEventsUsageDescription`, `NSMicrophoneUsageDescription`. Keep as-is.

### 8. Build-and-test convention for developers

Add `scripts/macos/install-to-applications.sh` (new): builds with `xcodebuild`, `rm -rf /Applications/Dimmy.app`, `cp -R build/...Dimmy.app /Applications/`, `codesign --verify --deep --strict`, and launches via `open /Applications/Dimmy.app`. Developers should use this script instead of running from DerivedData to avoid TCC path-mismatch confusion. Add to `docs/dev/macos-development.md` as the canonical dev loop.

---

## Data & state changes

`AppState` additions:
- `@Published var hotkeyStatus: HotkeyStatus = .uninstalled`
- No other new properties.

`PermissionsManager`:
- Polling cadence: 1.5s → 5.0s.
- New public method: `refreshNow()` (just calls `refresh()`; name signals intent at call sites).

`OnboardingContainerView`:
- Step count: 5 → 4.
- Initializer gains optional `startStep: Int = 0`.

`HotkeyManager`:
- Passive NSEvent fallback paths removed. `globalFlagsMonitor` / `localFlagsMonitor` properties and `installPassiveFallback`/`teardownPassiveFallback` methods deleted.
- `hotkeyStatus` writes to `AppState.hotkeyStatus` on every state transition.

New files:
- `platforms/macos/Dimmy/Views/Settings/DiagnosticsSettingsView.swift`
- `scripts/macos/install-to-applications.sh`

Removed files:
- None. `ModelDownloadStepView.swift` is kept (re-used inside Try It step's setup card), so the file stays.

---

## Testing

**Self-tests (DEBUG, crash-on-fail — consistent with Negative Space Programming):**
- `HotkeyStatus` `.installed` is equatable and has stable hash — covered in existing `testRecordingStateAnimationIds`-style test, new method `testHotkeyStatusCases`.
- `OnboardingContainerView` with `startStep: 1` renders Permissions step first — covered in a new SwiftUI preview-based test or by asserting on the render tree via ViewInspector if already in project (else skip the assertion, document manually verified).

**Manual QA plan (documented in spec and in PR description):**

1. Clean install from freshly built DMG into /Applications. Reset TCC: `tccutil reset All com.konrad.dimmy` (or the real bundle id). Launch.
2. Go through Welcome → Permissions. Grant Mic (native dialog). Grant Accessibility (via System Settings). Verify both rows flip to green within ~5 s.
3. Pick ⌃⌥ preset (default). Continue. Try It: test dictation. Verify pill animates, text pastes.
4. Open Settings → Diagnostics. Verify all fields populated, hotkey status = `.installed`.
5. Revoke Accessibility in System Settings. Verify hotkey status flips to `.accessibilityMissing` within 5 s and pill overlay shows warning.
6. Re-grant. Verify it flips back to `.installed` without app relaunch.
7. Quit app, relaunch. Verify onboarding does not reappear.
8. Revoke Accessibility again, quit, relaunch. Verify onboarding reopens at **Permissions step**, not Welcome.
9. Pick Fn in the advanced disclosure. Confirm modal. Verify Input Monitoring row appears. Test that Fn works only after user disables Keyboard → Dictation shortcut + grants Input Monitoring.

---

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Removing passive NSEvent fallback breaks users who had it as de-facto workaround | They will see `.accessibilityMissing` in pill/menu bar instead of silent no-op — net win. |
| Polling 5 s feels laggy for granting permissions | `refreshNow()` on button clicks + `didBecomeActive` observer keeps perceived latency <1 s in happy path. |
| TryIt's inline setup card is too much UI for one step | Hide CTAs behind a single "Setup needed" label that expands on tap; keeps happy path (user already has model/key) clean. |
| Diagnostics pane leaks internal state users misuse | Gate Diagnostics tab behind `showAdvanced` flag from Settings (already in `AppState`). |
| `OnboardingContainerView` `startStep` parameter breaks SwiftUI `@State` seeding expectations | Use `@State private var currentStep: Int` with explicit `init(appState:, startStep:)` that assigns `_currentStep = State(initialValue: startStep)`. Standard pattern. |

---

## Out of scope (future work)

- Windows / Linux onboarding redesign — different UX, different permission model.
- Programmatic override of macOS "Press fn key to" setting — no public API; cannot be fixed from user-space.
- Replace polling entirely with KVO on TCC — no public API.
- Model-download UI polish — separate design.

# Handoff — Mac parity for call-detect, stop-suggestion, denoise, audio resilience

> Source: Windows-side implementation lives on two branches that merged
> into `staging` in May 2026: `feat/call-auto-detect` (call detection +
> nudge popup + new stop-suggestion mode) and `feat/denoise-dfn`
> (nnnoiseless + DeepFilterNet3). Plus a non-trivial audio resilience
> pass on the Rust core (canonical 48 kHz, LinearResampler in cpal
> callbacks, recovery preserving buffers) that is platform-agnostic and
> already works on Mac at the core level — but the host UX wiring for
> the call surfaces is Win-only today.
>
> Predecessor: `2026-05-09-mac-system-audio-capture.md` (Phases 1-4 of
> the v2 work). This handoff covers everything landed ON TOP of that.
>
> Author of the Win side: Claude (Opus 4.7) + user pairing sessions.
> Tested on Win11 with Teams, Jabra USB headset, internal mic, BT/HFP.

## TL;DR for the Mac engineer

You will need to port **two user-visible features** and **one
invisible-but-mandatory safety hook** to Mac:

1. **Call detection nudge** (`call_detected` event → bottom-right
   non-activating popup → "Record now" / "Not now" / "Don't ask
   again"). Rust state machine is already cross-platform; you need a
   macOS audio-session enumerator and a Swift overlay window.
2. **Stop-suggestion nudge** (`meeting.stop_suggested` event → same
   popup reskinned with "Stop & recap" / "Keep recording" buttons).
   Rust state machine + FFI already done; you mirror the C# `App.xaml.cs`
   handlers in `AppState`/`AppDelegate`.
3. **Skip emitting `dimmy_call_signal` while WE are recording.** This
   is a hot-fix that prevents Dimmy's own cpal mic stream from
   triggering its own detector. Win does it in
   `CallDetectionService.cs`. Mac must do the equivalent in
   `CallDetectionManager.swift`.

Plus, denoise (`local-dfn` cargo feature) is already wired on both
platforms in the Rust core — Mac just needs a Settings toggle and
to confirm the feature flag is in the Mac build's cargo invocation.

---

## 1. Rust surfaces — already cross-platform

Both features are fully implemented in the Rust core and exercised
by 15 unit tests in `core/src/call_detector.rs::tests`. Nothing for
Mac to add Rust-side.

### 1.1 FFI events (`emit_event` callback)

| Event name              | Payload (JSON)                                                | Emitted when                                       |
|-------------------------|---------------------------------------------------------------|----------------------------------------------------|
| `call_detected`         | `{app: "teams"\|null, since_seconds: 17}`                     | mic active past debounce, not cooldown/excluded    |
| `call_ended`            | `{app: "teams"\|null}`                                        | mic active → inactive transition                   |
| `meeting.stop_suggested`| `{app, inactive_for_secs: 15, reason: "call_ended"}`          | mic silent ≥ 15 s WHILE we're meeting-recording    |

`call_detected` is the same one Win has been consuming since
`feat/call-auto-detect` landed. `meeting.stop_suggested` is the new
one (this branch).

### 1.2 FFI calls

`core/src/ffi.rs`:

| Function                                      | Signature                                                       |
|-----------------------------------------------|------------------------------------------------------------------|
| `dimmy_call_signal`                           | `(mic_active: c_int, app_id_or_null: *const c_char) -> c_int`    |
| `dimmy_call_signal_response`                  | `(app_id_or_null: *const c_char, resp: *const c_char) -> c_int`  |

`dimmy_call_signal` returns `0`/`1`/`2`/`3`:
- `0` — no transition (suppressed, debouncing, or no change)
- `1` — `call_detected` was emitted (host should show nudge)
- `2` — `call_ended` was emitted (host should hide nudge if showing)
- `3` — `meeting.stop_suggested` was emitted (host shows nudge in stop mode)

`dimmy_call_signal_response` accepts these strings:
- Detection-mode: `"record_now"`, `"not_now"`, `"never"`, `"timeout"`
- **Stop-mode (NEW)**: `"stop_and_recap"`, `"keep_recording"`, `"stop_timeout"`

The Rust state machine internally maps these to cooldown semantics
(see `core/src/call_detector.rs::record_response`).

### 1.3 What enforces the "stop-suggested" preconditions

Already done in `call_detector::handle_inactive`. Conditions ALL must hold:

- `recording_active_from_us == true` (set by accepting a previous
  `record_now`, cleared by `meeting_stopped()` in `dimmy_meeting_stop`).
- Meeting is currently active (passed by host via `is_meeting_active`).
- Mic has been silent ≥ `mic_inactive_for_stop_secs` (default 15).
- Not already inside `stop_suggestion_until` cooldown
  (`stop_keep_cooldown_secs`, default 300 s, started on KeepRecording).
- `stop_suggestion_emitted == false` for this recording session.

Mac does **not** need to re-implement any of this — just pass
`is_meeting_active` correctly when calling `dimmy_call_signal`.

---

## 2. CallDetectionManager — what to build (Mac equivalent of `CallDetectionService.cs`)

Windows uses WASAPI `IAudioSessionManager2` to enumerate audio
sessions and check which ones have active input streams, matching
process names against a whitelist (teams / zoom / slack / discord /
webex). Polls at 1 Hz. **Pure observer — never opens the mic.**

On Mac the equivalent stack is CoreAudio:
`AudioObjectGetPropertyData(kAudioDevicePropertyDeviceIsRunningSomewhere)`
per input device, then `runningInputProcesses` (macOS 14+) for the
process-name match. macOS 13 fallback: enumerate via
`AudioComponentInstanceGetVersion` + `kAudioHardwarePropertyTranslatePIDToProcessObject`.

### 2.1 Skeleton (Swift)

```swift
// platforms/macos/dimmy/Services/CallDetectionManager.swift
final class CallDetectionManager {
    private let pollInterval: TimeInterval = 1.0
    private var pollTimer: Timer?
    private let whitelist = ["teams", "zoom", "slack", "discord", "webex"]

    func start() {
        pollTimer = Timer.scheduledTimer(withTimeInterval: pollInterval, repeats: true) { [weak self] _ in
            self?.tick()
        }
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        // Tell Rust we're not signalling anymore so cooldown timers
        // can wind down cleanly. App-id null is fine on stop.
        _ = dimmy_call_signal(0, nil)
    }

    private func tick() {
        // 🚨 SAFETY HOOK: skip while Dimmy is itself recording —
        // otherwise our own cpal mic stream triggers the detector.
        // Mirror of CallDetectionService.cs:90-95.
        if AppState.shared.isRecording || AppState.shared.meetingActive {
            _ = dimmy_call_signal(0, nil)
            return
        }

        let (micActive, appId) = scanRunningInputProcesses()
        let cAppId = appId?.cString(using: .utf8)
        _ = dimmy_call_signal(micActive ? 1 : 0,
                              cAppId.flatMap { UnsafePointer($0) })
    }

    private func scanRunningInputProcesses() -> (Bool, String?) {
        // ... enumerate CoreAudio input devices, check
        // kAudioDevicePropertyDeviceIsRunningSomewhere,
        // map PIDs to bundle ids, match against `whitelist`.
        // Return the first whitelist match found, else
        // (any-mic-active, nil) for the "Microphone in use" headline.
    }
}
```

### 2.2 SAFETY HOOK — non-negotiable

The "skip while we're recording" guard is **load-bearing**. Without
it, the popup spuriously fires the moment the user starts a
dictation shortcut, because cpal opens the mic and CoreAudio reports
"input device running". Win had this bug land in production briefly
(2026-05-13 user feedback: "se premo shortcut per dictation NON deve
partire NOTIFICA del meeting!"). Don't repeat it.

The check uses two flags from the central app state:
`isRecording` (pill / dictation) and `meetingActive` (meeting
worker). Both already exist on Mac per the v2 handoff.

---

## 3. CallNudgeWindowController — Mac mirror of `CallNudgeWindow.xaml(.cs)`

Win uses a WinUI 3 Window with the **same transparency recipe as the
pill** (`TransparentBackdrop` + `EnableTransparency` from
`Helpers/WindowHelper.cs`). The user explicitly approved that path
after multiple attempts with DWM tweaks failed ("FANNO MERDA!"
quote, 2026-05-19).

The Mac equivalent is an `NSPanel` with these flags:
- `.styleMask = [.nonactivatingPanel, .borderless, .fullSizeContentView]`
- `.level = .floating` (or `.statusBar` if needed above ScreenCapture overlays)
- `.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]`
- `.hasShadow = false`
- `.isOpaque = false`
- `.backgroundColor = .clear`

Content view: a `NSVisualEffectView` (`.material = .hudWindow`,
`.state = .active`) with a single rounded-corner subview holding the
labels + buttons.

### 3.1 Geometry — Teams-style toast

Win settled on these dimensions after user feedback ("io se possibile
farei una notifica come quelle di teams, a livello di posizione e
dimensione"):

| Property        | Value     |
|-----------------|-----------|
| Width (DIP)     | 360       |
| Height (DIP)    | 112       |
| Right margin    | 16 px     |
| Bottom margin   | 60 px     |
| Corner radius   | 8         |
| Inner padding   | 14,10,14,10 |
| Auto-dismiss    | 30 s       |

Position: bottom-right of the screen's *work area* (so it never
overlaps the dock or menu bar). `NSScreen.main!.visibleFrame`.

### 3.2 Two modes share the same window

| Mode             | Trigger                       | Title                                  | Body                                            | Primary           | Secondary        |
|------------------|-------------------------------|----------------------------------------|--------------------------------------------------|-------------------|------------------|
| Detected         | `call_detected`               | `Meeting detected in {app}`            | `Dimmy can record + recap this call.`            | `Record now`      | `Not now`        |
| StopSuggested    | `meeting.stop_suggested`      | `{app} call ended?`                    | `No activity for a while. Stop & recap?`         | `Stop & recap`    | `Keep recording` |

If `app == nil`: title becomes `Microphone in use` / `Call ended?`.

The "X" close button on the header behaves as the secondary button.
The auto-dismiss timer behaves as `timeout` / `stop_timeout`
respectively.

**Don't ask again** menu item is only visible in Detected mode (its
`never` response is meaningless in stop mode).

### 3.3 Click handlers

Each click sends `dimmy_call_signal_response(appId, response_string)`
where `response_string` is one of the 7 listed in §1.2. After
`stop_and_recap`, the host ALSO calls `dimmy_meeting_stop` + the
recap pipeline — mirror what `App.xaml.cs::OnNudgeStopAndRecap` does
(it delegates to `PillWindow.StopMeetingFromPillAsync()`).

On Mac the equivalent is `MeetingPostProcessService.runRecap(dir:transcript:)`
in `platforms/macos/dimmy/Services/MeetingPostProcessService.swift`
which already exists.

---

## 4. AppState wiring — events to handlers

In `platforms/macos/dimmy/Services/DimmyCore.swift::handleEvent`, add
three new cases mirroring `AppViewModel.HandleEvent` on Win:

```swift
case "call_detected":
    let app = payload["app"] as? String
    let since = (payload["since_seconds"] as? Int) ?? 0
    Task { @MainActor in AppState.shared.onCallDetected(app: app, sinceSecs: since) }

case "call_ended":
    let app = payload["app"] as? String
    Task { @MainActor in AppState.shared.onCallEnded(app: app) }

case "meeting.stop_suggested":
    let app = payload["app"] as? String
    let inactive = (payload["inactive_for_secs"] as? Int) ?? 0
    Task { @MainActor in AppState.shared.onCallStopSuggested(app: app, inactiveSecs: inactive) }
```

Then in `AppState`:

```swift
func onCallDetected(app: String?, sinceSecs: Int) {
    guard callDetectEnabled else { return }
    callNudgeController.showFor(app: app)
}

func onCallEnded(app: String?) {
    callNudgeController.hide()
}

func onCallStopSuggested(app: String?, inactiveSecs: Int) {
    guard callDetectEnabled else { return }
    // Preconditions already enforced Rust-side; just paint.
    callNudgeController.showStopSuggestion(app: app)
}
```

---

## 5. Denoise — `local-dfn` feature

`feat/denoise-dfn` landed nnnoiseless (cheap, ~85 KB, RNNoise port,
pure Rust) AND DeepFilterNet 3 (SOTA, via patched fork at
`KonradDallaOrg/DeepFilterNet` branch `fix/tract-ndarray-port`).
Cargo feature `local-dfn` toggles between them at compile time —
DFN3 when enabled, nnnoiseless as the silent fallback.

### 5.1 Mac cargo invocation

Verify `local-dfn` is in the Mac build feature list. The Win frozen
set is documented in `CLAUDE.md` § "Windows local DLL build — feature
flag set is FROZEN":

```
local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,local-dfn
```

Mac equivalent should be `local-stt-metal,local-llm-metal,local-stt-parakeet-fluid,local-dfn`.
Check `scripts/dev/preflight-mac.sh` + `release.yml` Mac job. The DFN3
fork pulls in ndarray ^0.16 and a tract runtime; build time goes up
by ~90 s on first build, ~15 s on incremental.

### 5.2 Settings UI

No UI surface in Win yet (the feature is unconditionally on when the
cargo feature is). If you want one on Mac, mirror what
`MacOutputPage.swift` does for other Advanced toggles.

---

## 6. Audio resilience — confirmed cross-platform; verify on Mac

This isn't really a "port" job — the code lives in `core/src/audio.rs`
and works on Mac through cpal. But please **smoke-test** on Mac:

1. **Canonical 48 kHz**: every device resamples to 48 k in the cpal
   callback via `LinearResampler` (stateful, with phase + last_sample
   preserved across callbacks). No more 16 k BT/HFP feeding 16 k
   buffers when the meeting worker expects 48 k.
2. **Device-switch recovery**: cpal `StreamError::DeviceNotAvailable`
   triggers a recovery start that **preserves the existing buffers**
   (don't clear them on recovery — only on user-initiated stop).
   Win was burned when switching mic ↔ speakers mid-meeting wiped
   the meeting audio.
3. **Align secondary zero-pad**: `core/src/meeting.rs::align_secondary`
   ensures `secondary.len() >= primary.len()` each tick. On Mac
   `secondary` is always empty (no system-audio loopback without
   ScreenCaptureKit), but the same invariant must hold for the AEC
   to not stall.

Mac test scenarios to run:
- Start dictation on built-in mic, switch to AirPods mid-recording. Pill amp must keep moving; final transcript must include both halves.
- Start meeting on AirPods, take them off (default falls back to built-in). Meeting must continue, transcript must cover the whole span.
- BT/HFP switch (AirPods → Studio headphones over BT). Same.

---

## 7. Test plan

### Unit tests
The Rust call_detector tests (15) pass on Mac unchanged — they're
cross-platform pure-logic tests. Run them with:

```bash
cd core
cargo test --release --lib --features local-stt-metal,local-llm-metal,local-dfn call_detector
```

### Manual smoke

1. Open Zoom + start a test meeting. Within ~15 s a popup should
   appear bottom-right offering "Record now".
2. Click "Record now" — meeting starts (pill flips to recording
   state, MeetingWindow opens via the existing meeting flow).
3. Stop talking + close Zoom. After ~15 s the same popup re-appears
   offering "Stop & recap" / "Keep recording".
4. Click "Stop & recap" — recap pipeline runs (~10-30 s with the
   configured recap model), MeetingWindow flips to Done view with
   the recap card populated.

### Negative case (skip hook)

5. While the popup is NOT showing, hit the Dimmy dictation shortcut.
   **The call-detect popup must not appear during dictation.** If it
   does, the safety hook (§2.2) is missing or wrong.

---

## 8. Cross-references

- Win implementation: commits `0d11b46` on `feat/call-auto-detect`,
  plus the Teams-style polish + stop-suggestion follow-up on
  `staging` (2026-05-20).
- Rust state machine: `core/src/call_detector.rs` (full file is
  ~600 lines, 15 unit tests at the bottom).
- Rust FFI: `core/src/ffi.rs::dimmy_call_signal` +
  `dimmy_call_signal_response` + `emit_event("meeting.stop_suggested", ...)`.
- Win C# host: `platforms/windows/Dimmy.Windows/App.xaml.cs`
  (handlers + EnsureCallNudgeWindow), `ViewModels/AppViewModel.cs`
  (event surfaces), `Views/CallNudgeWindow.xaml(.cs)` (popup),
  `Services/CallDetectionService.cs` (audio session poll).
- DFN3 fork: `KonradDallaOrg/DeepFilterNet` branch
  `fix/tract-ndarray-port`, referenced in `core/Cargo.toml` under
  the `local-dfn` feature.

When this is wrapped up, drop the `2026-05-20` handoff from
`CLAUDE.md`'s see-also list per the "don't link handoffs from
CLAUDE.md — they decay" rule.

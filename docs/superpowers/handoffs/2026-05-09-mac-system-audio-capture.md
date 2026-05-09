# Handoff — Mac side for `feat/system-audio-capture` branch

> Source: Win-side implementation lives on `feat/system-audio-capture`,
> 50+ commits over 2026-04-25 → 2026-05-09 (today). Builds + runs on
> Windows; tests green. The Rust core is fully cross-platform. Mac
> needs Swift wrappers + native UI glue for the new FFI surface and
> the new UX shapes.
>
> Predecessor handoff: `2026-05-06-mac-v2-features.md` covers
> `feat/v2-unified` (Phase 7+8). This handoff covers everything ON
> TOP of that.

## What's in `feat/system-audio-capture`

The branch landed in roughly four phases:

```
1. System-audio capture        — WASAPI loopback (Win-only feature)
   + Mic|System|Mix source       (later collapsed; see "Always-mix" below)
2. Meeting Phase 3 → 6         — per-track WAV, AEC3, native rate, dual-band
                                  waveform, configurable chunk window
3. Pause/resume + regen        — meeting pause/resume FFI, regenerate
                                  transcript/recap buttons in Done view
4. Always-mix refactor         — drop the AudioSource enum at call sites,
                                  AEC tolerant of missing ref, pill always
                                  visible during meeting, recap pipeline
                                  reachable from pill Stop, taskbar amp
                                  reflects loopback too, settings UI
                                  cleanup (recap-model dropdown,
                                  AudioSource radios removed)
```

Plus a hardening pass: **40 net new tests** (23 Rust unit + 4 Rust
integration + 13 C# xUnit) covering the riskiest surfaces. See
`docs/dev/system-audio-capture-tests.md` for the full inventory.

## TL;DR for Mac engineer

You will mostly want to **NOT port** the Windows-specific WASAPI
loopback (Win-only API). What you DO need:

1. New FFI: meeting pause/resume + status (3 functions).
2. New FFI return code -7 to handle on `dimmy_start_recording`.
3. New shared service shape: meeting recap pipeline reachable from
   non-meeting-window surfaces.
4. New UX shape: pill Stop ends meeting if active.
5. Settings UI: recap-model dropdown, AudioSource radios gone.
6. File-load preprocess uses a different code path now.
7. Sidebar Delete button for past meetings.

Macros + the audio pipeline core stays the same (mic-only on Mac;
no system-audio capture without screen-recording entitlements +
ScreenCaptureKit which is its own beast).

---

## 1. Meeting pause/resume FFI (NEW)

Three new exports in `core/src/ffi.rs`:

```rust
pub extern "C" fn dimmy_meeting_pause()      -> c_int;
pub extern "C" fn dimmy_meeting_resume()     -> c_int;
pub extern "C" fn dimmy_meeting_is_paused()  -> c_int;
```

Return-code contract:
- `1`  → state actually flipped (was running, now paused; or vice versa)
- `0`  → no-op (already in target state, or no meeting active)
- `-1` → internal lock failure (rare)

Behavior while paused:
- cpal callbacks keep filling the audio buffers (we DON'T bounce the
  streams — that races with device acquisition on resume).
- Worker thread skips drain, skips WAV writes, skips STT chunks.
- On resume (or stop while paused), worker advances `samples_written`
  + `last_processed` to current `buf_len_now`, so the paused window
  is excluded from `audio.wav` AND from the chunked transcript timeline.
- A `[paused] (resumed after N ms)` line lands in `transcripts.txt`
  at the seam.

### Mac UI work
- Add a Pause button on the Meeting window (next to Stop).
- Bind to `dimmy_meeting_pause`/`_resume`. Toggle icon between play (▶)
  and pause (⏸); label "Pause" ↔ "Resume". Read state via
  `dimmy_meeting_is_paused` so re-attaching after the window was closed
  shows the correct icon.

### Reference
- C# implementation: `MeetingWindow.xaml.cs` `Pause_Click` +
  `UpdatePauseButtonUi`. Glyphs E769 (pause) / E768 (play).
- Rust impl: `MeetingSession::pause()/.resume()/.is_paused()` in
  `core/src/meeting.rs`.

---

## 2. `dimmy_start_recording` rc=-7 (NEW)

When a meeting is active, `dimmy_start_recording` now returns `-7`
instead of starting a parallel dictation stream. This prevents the
audio thread from getting hijacked mid-meeting.

### Mac UI work
On the hotkey-pressed handler (Swift equivalent of
`OnHotkeyPressed`):

```swift
let rc = dimmy_start_recording()
if rc == -7 {
    // Silent no-op — meeting recording in flight. Log only,
    // no error toast (don't pull user out of their meeting context).
    log("PTT hotkey suppressed: meeting recording active (rc=-7)")
    return
}
// existing -1 (no key) / -2 (already recording) / <0 (failed) branches
```

Reference: `App.xaml.cs:861`.

---

## 3. Always-mix capture (Win-relevant only, keep Mac as-is)

**Mac is unaffected.** The Win-side change forces every recording
session to open BOTH mic + WASAPI loopback streams via cpal, with
AEC3 always cleaning the mic with the loopback as far-end reference.
On Mac there is no system-audio loopback in core (would require
ScreenCaptureKit + screen-recording entitlement), so:

- `AudioSource::Mix` on Mac silently degrades to `AudioSource::Mic`
  in the existing audio.rs paths. No changes needed.
- The pill amplitude probe on Win reads `MAX(mic, loopback)`. On Mac
  `dimmy_get_loopback_amplitude()` always returns 0, so the existing
  pill can either keep using mic-only OR switch to MAX (no-op when
  loopback is 0). Cosmetic; pick the simpler one.

### Robustness fix that DOES matter cross-platform

`core/src/aec.rs::run` no longer blocks on lockstep mic+ref drain.
When `ref_ring` is empty (no system audio), the worker pads the
render frame with zeros and processes the mic immediately. Without
this, every Mac recording (loopback always empty) would have hung
the audio buffer forever once Mix mode is forced. The fix is in the
Rust core so Mac inherits it for free.

---

## 4. File-load preprocess uses new helper (CROSS-PLATFORM)

`core/src/preprocess.rs::process_buffer_for_file_load` is a new
function that runs **highpass-only** (no VAD, no AGC). The
`dimmy_transcribe_file` FFI uses it instead of the live-recording
`process_buffer`.

Why: dagc emits NaN on long silence stretches. Running it across a
full pre-recorded file (e.g. a 90-min meeting WAV) corrupted 97 % of
the audio. Live-recording preprocess (`process_buffer`) keeps using
dagc because each capture instantiates a fresh AudioPreprocessor and
short captures don't cross the AGC trigger.

### Mac impact
Mac's `dimmy_transcribe_file` already routes through the same
`ffi.rs::dimmy_transcribe_file`. Mac inherits the fix automatically
once you pull the Rust core. **No Swift work needed**, but make sure
the Mac file-drop UI hits the same FFI; don't add a separate path
that goes through `RawAudio::preprocess(true)` — that'll hit the
NaN bug.

---

## 5. Meeting recap pipeline as a shared service

The C# side has a new service: `Services/MeetingPostProcessService.cs`.

```csharp
public static class MeetingPostProcessService {
    public static async Task<RecapResult> RunRecapAsync(string dir, string transcript);
}
```

Builds the Notion-style structured-recap prompt, calls
`dimmy_llm_call_raw` with the user-picked recap model, persists
`recap.md` + actions plain text via
`dimmy_meeting_save_post_process`. Used by:

- `MeetingWindow.OnStop` (when user clicks Stop in the meeting
  window) — existing flow.
- `PillWindow.StopMeetingFromPillAsync` (when user clicks Stop on
  the pill while a meeting is active and the meeting window is
  closed/hidden) — new flow.

### Mac UI work
On Mac, the meeting recap was previously inside the meeting window's
Stop handler. Extract the same shared service shape so when (in the
future) you want to stop a meeting from a tray menu or another
surface, you don't duplicate the prompt + parser code.

The Rust prompt + parser are NOT shared between platforms. The C#
`MeetingWindow.BuildStructuredRecapPromptInternal` /
`ParseStructuredRecapInternal` / `BuildMarkdownFromSectionsInternal`
need a Swift mirror. The prompt body lives in
`MeetingWindow.xaml.cs::BuildStructuredRecapPrompt` (lines 1153+) —
~80 lines of carefully-tuned text. **Copy verbatim**, don't rewrite,
the parser keys off exact `===KEY===` markers.

Reference port: `core/src/bin/recap_one_shot.rs` — Rust port of the
prompt for a one-off CLI tool. If you'd rather move the prompt into
Rust + share via FFI, that bin is the seed of that refactor.

---

## 6. Pill Stop routes to meeting/dictation

`PillWindow.xaml.cs::Stop_Click` now branches:

```csharp
bool meetingActive = DimmyNative.dimmy_meeting_is_active() == 1;
if (meetingActive) {
    await StopMeetingFromPillAsync();   // includes recap pipeline
    return;
}
// existing dictation stop + paste flow
```

`StopMeetingFromPillAsync`:
1. Sets pill to `AppState.Transcribing` (spinner visible during
   recap, ~10-30 s).
2. Calls `dimmy_meeting_stop` (returns transcript + dir as JSON).
3. `MeetingPostProcessService.RunRecapAsync(dir, transcript)`.
4. On success: `App.NotifyMeetingRecapSaved(dir)` → if
   MeetingWindow open, refreshes sidebar + auto-selects the row.
5. Resets pill to Idle.

Plus a 500 ms `_meetingStatePollTimer` in PillWindow that mirrors
the core's meeting state into the pill UI: when a meeting starts
from any surface (MeetingWindow, future tray menu) and the pill is
Idle, the pill transitions to Recording so dual-source amp bars +
the Stop button are visible.

### Mac UI work
- Mirror the branching in pill Stop handler.
- Add the meeting-state poll on the pill (NSTimer 500 ms).
- Add the analogue of `App.NotifyMeetingRecapSaved` on the
  MeetingWindow to refresh + jump.

---

## 7. MeetingWindow lifecycle decoupled

`MeetingWindow.Closing` no longer cancels on `_recordingActive`,
and `Closed` no longer force-stops the FFI. Recording state lives
in the Rust core. Reopening MeetingWindow re-attaches via the
`dimmy_meeting_is_active()` probe in the constructor.

### Mac UI work
- Remove any "stop meeting on window close" in the SwiftUI Mac
  MeetingWindow. Closing should hide; recording continues.
- On open, probe `dimmy_meeting_is_active()` and, if active,
  re-attach to the in-flight session (jump straight to Recording
  state, hook polling).

---

## 8. Sidebar Delete button (CROSS-PLATFORM UX)

Each row in the meeting history sidebar has a trash glyph (E74D)
button. Click → `ContentDialog` confirm → `Directory.Delete(dir,
recursive=true)` → row removed from observable collection.

C# impl: `MeetingWindow.xaml.cs::HistoryRowDelete_Click`.

### Mac UI work
- Add the same affordance to the Mac sidebar row. NSAlert for
  confirm.
- `FileManager.removeItem(at:)` for the dir.
- Stop in-flight playback if the deleted dir is the current
  MediaPlayer source (avoid file-disappeared race).

---

## 9. Settings UI cleanups

Two changes Mac should mirror (or document as "Win-only" if you
don't have the same Settings layout):

1. **AudioSource radio buttons removed** — the old "Microphone /
   System / Mic + System" trio is gone. The runtime always-mix
   ignores the config field at runtime; the field is still in
   config.json for backward compat. Mac probably never exposed
   this UI; nothing to do.

2. **Meeting recap model picker** — replaced the free-text TextBox
   with a curated ComboBox: "Auto", Anthropic Opus 4.7 / Sonnet
   4.6 / Haiku 4.5, Gemini 3.1 Pro / 2.5 Pro / 2.5 Flash, GPT-5 /
   GPT-4o, plus a Custom escape hatch. Each entry has the provider
   SVG icon. Stored as `recap_model_override` in config.json (model
   id string).

   **Footgun**: Today the recap shares the LLM API URL + key with
   dictation. If user picks a Gemini model with Anthropic
   configured → 400 invalid_request_error. Tracked in followup
   task #137 (multi-provider keystore). Mac UI should at minimum
   show the same dropdown but you can defer the multi-provider
   keystore.

Reference impl: `SettingsWindow.xaml` (recap model card),
`SettingsWindow.xaml.cs::SyncRecapModelPicker`.

---

## 10. Anthropic adaptive thinking dispatch

`llm.rs::process_raw_prompt` automatically detects Opus 4.7 / Sonnet
5+ models and uses `thinking.type=adaptive` + `output_config.effort=high`
instead of the legacy `thinking.type=enabled` + `budget_tokens` form.
This is in core, Mac inherits it for free. Just bump
`max_tokens` to 32 K when calling raw — the dispatch sets the right
shape based on model id.

Recap timeout bumped from 60 s to 600 s (CLAUDE.md `30s + 1s/MB
capped at 600s` rule). Anthropic's own doc recommends 600 s for
extended thinking. Same default applies cross-platform.

---

## 11. AppRules drag-reorder regression (NOT addressed)

WinUI 3 v3.1.7 + SentinelOne EDR + COM apartment state interaction
can crash the renderer mid-drag (`combase.dll +0x37fc4 E_UNEXPECTED`).
Documented in `docs/dev/system-audio-capture-tests.md` under the
"Manual / FlaUI" section. Reproduction is environmental — reboot
sometimes resets COM state.

**Not Mac's problem.** Mac AppKit drag-reorder uses NSTableView's
native API, no COM apartment dance.

---

## 12. Test coverage worth porting

The Rust unit tests live in core and run identically on Mac. Just
ensure CI runs the full suite:

```bash
cargo test --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan -- --test-threads=1
cargo test --release --test meeting_pause_resume --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan -- --test-threads=1
```

(On Mac the feature flags become `local-stt-metal,local-stt-parakeet,local-llm-metal`.)

C# xUnit tests live in `Dimmy.Windows.Tests/` — Mac would need
analogous Swift tests for `MeetingRecapPrompt`, `RecapModelPicker`
view-model logic, `SttSnapshot.local_backend` round-trip.

See `docs/dev/system-audio-capture-tests.md` for the full inventory
and the manual sweep checklist.

---

## Order of work for Mac engineer

If you have one focused day:

1. **Pull the Rust core** (already merged via this branch's commits).
   `cargo build --release --target aarch64-apple-darwin --features
   local-stt-metal,local-llm-metal,local-stt-parakeet`. Verify
   `dimmy_meeting_pause/_resume/_is_paused` symbols are exported.
2. **Wire the Pause button** in the SwiftUI Meeting window (item 1).
3. **Handle rc=-7** in the PTT hotkey handler (item 2).
4. **Decouple MeetingWindow close** from FFI stop (item 7).
5. **Add Pill Stop → meeting routing** (item 6) plus the meeting-state
   poll. Defer the recap pipeline port if the Mac MeetingWindow already
   has a stop+recap path you can leave alone for now.
6. **Sidebar Delete button** (item 8).
7. **Recap model dropdown** in Settings (item 9.2).

Items 4 (file-load preprocess), 10 (Anthropic adaptive), 12 (test
coverage) are cross-platform Rust changes already merged.

The Win-only items (always-mix loopback, taskbar amp, drag-reorder
crash) skip on Mac.

---

## Useful commits to read

- `3eddac3` — refactor(audio): always-mix capture (Win-only impact;
  the AEC ref-tolerance fix is the cross-platform piece)
- `a663c45` — feat(meeting): pause/resume
- `10db8bb` — feat(meeting): pill Stop fires the recap pipeline
- `f42890f` — feat(meeting): decouple recording lifecycle + per-row
  delete in sidebar
- `0ed682b` — fix(stt/file-load): skip AGC on file-load
- `4e8e611` / `5f4b918` — recap model dropdown
- `9729ca4` — fix(llm): Anthropic Opus 4.7 adaptive thinking
- `a3f4131` — test: exhaustive coverage

The full PR description on GitHub will have the cumulative list. No
single commit is load-bearing in isolation — the architectural
shifts are spread across the always-mix refactor (3 commits in
sequence) and the recap pipeline plumbing (4 commits across PillWindow
+ MeetingWindow + the new service).

# macOS parity handover — `feat/meeting-live-notes`

**Date:** 2026-05-29
**Branch:** `feat/meeting-live-notes` (off `origin/staging`)
**Author of Windows side:** Konrad + Claude
**Audience:** whoever ports this surface to macOS (SwiftUI)

This branch shipped a batch of **Windows-only** meeting changes. The Rust
core pieces are cross-platform (already in the dylib); the UI work is all
WinUI/C# and needs a SwiftUI mirror. This doc is the punch list.

> ⚠️ Cross-platform parity is mandatory (CLAUDE.md). Nothing here is
> "Windows forever" — it's "Windows first, Mac to follow". Three items have
> **load-bearing gotchas** (ogg gate, recap flag, call-detect ordering) —
> read those carefully before mirroring.

---

## 0. What's already cross-platform (Rust core — Mac just calls it)

These landed in `core/` and are in the dylib for every target. The Mac UI
only needs to **call** them, not reimplement:

- **`meeting.rs` records Ogg/Vorbis** via `TrackSink`, but **gated to
  Windows** with `cfg!(target_os = "windows")` in `TrackSink::create`.
  → **On macOS the core still writes WAV.** This is deliberate (see §2).
- **`dimmy_call_meeting_started_external()`** (new FFI in `ffi.rs`) — arms
  the call-detector's `recording_active_from_us` for a meeting started
  outside the "Record now" nudge. Mac needs the Swift `@_silgen_name` /
  interop decl + to call it (see §4).
- Playback/peaks/regenerate on Windows resolve `.ogg` then fall back to
  `.wav`. The core's `dimmy_compute_audio_peaks` + `dimmy_transcribe_file`
  already decode ogg via Symphonia (verified: a 50-file WAV→ogg migration
  re-decoded every track cleanly).

---

## 1. Live Notes tab (Recording view)

**Windows:** the meeting Recording view gained a **Notes tab** beside the
Live transcript — a multi-line markdown editor. "Add note" / Ctrl+Enter
stamps the meeting elapsed time and **appends to `notes.md`** in the
meeting dir.

**Mac today:** the Done view already has a Notes tab (`doneNotes` →
`notes.md`). What's **missing**: a **live** Notes tab *during* recording.

**Mac tasks:**
- Add a Notes tab to the recording-state meeting view (`MeetingViewModel`
  + the recording sub-view). Same `notes.md` file as the Done notes.
- Timestamp helper: stamp `[mm:ss]` from meeting elapsed time on add.
- Verify the file is the single store (Done notes + live notes edit the
  same `notes.md`).

---

## 2. Ogg/Vorbis audio  🚧 GOTCHA

**Windows:** meetings record `audio*.ogg` (~10× smaller than WAV). The C#
UI resolves `.ogg` with `.wav` fallback for playback, waveform peaks, and
regenerate (`ResolveAudioTrack` helper).

**Why Mac is still on WAV:** the `cfg!(target_os = "windows")` gate in
`meeting.rs::TrackSink::create`. If you flip that gate to include macOS
**before** the Mac UI can read ogg, you ship a **broken Done view** (Mac UI
reads `audio.wav`, finds only `audio.ogg`, → no playback / no waveform /
regenerate fails). That's why it's gated.

**Mac tasks (in order — do NOT reorder):**
1. **First**, make the Mac meeting UI resolve `.ogg` with `.wav` fallback
   everywhere it currently hardcodes `audio*.wav`: playback (AVAudioPlayer
   / AVPlayer), waveform peaks (it already calls the peaks FFI — just point
   it at the resolved path), regenerate-transcript.
2. **Then** widen the gate: `cfg!(target_os = "windows")` →
   `cfg!(any(target_os = "windows", target_os = "macos"))` (or just drop
   the gate once both platforms read ogg). **Update `SelfTests` if it pins
   anything about audio format**, and run `scripts/dev/preflight-mac.sh`.
3. AVFoundation should decode ogg/vorbis natively on recent macOS; verify
   playback actually plays. If not, the waveform (our Symphonia peaks via
   FFI) still works independent of the player — same split as Windows.

**Note:** existing Mac users' meetings stay WAV; new ones become ogg only
after step 2. No migration is required (WAV still plays via the fallback).

---

## 3. Waveform robustness (lessons, not literal code)

The Windows waveform had three bugs worth checking on Mac (Mac has its own
drawing code, so these are *lessons* to verify, not files to copy):

- **Decode peaks SEQUENTIALLY, not 3× in parallel.** Three concurrent
  full-file Vorbis decodes of a long meeting starved memory/CPU and one
  track intermittently returned empty → single-band waveform. One at a
  time is reliable.
- **Cache peaks on disk** (`<audio>.peaks.json`, keyed by file size +
  bucket count) so only the first open of a meeting decodes; later opens
  are instant.
- **Skip the mix decode when mic+system both exist** (the mix peaks are
  thrown away when drawing the dual band) — but keep the mix as the
  fallback when a per-track decode is empty.
- **Debounce resize redraws** — redrawing hundreds of bars on every
  SizeChanged froze the UI; redraw once after the resize settles.

If the Mac waveform already opens fast and is responsive, it may not have
these issues — verify before changing anything.

---

## 4. Call-detect: stop-suggestion for manually-started meetings  🚧 GOTCHA

**Windows:** the call-detect stop-suggestion popup only fired when the
meeting was started via the "Record now" nudge (it bound the call's audio
session as the meeting origin). Two fixes made it work for **manual**
starts:

1. **Bind at meeting start** if a call is already active
   (`MarkMeetingOriginFromCurrentSession` on the start edge).
2. **Bind mid-meeting** if the call appears *after* the meeting started —
   the common flow (start REC, *then* join the call). On every
   meeting-active tick with no origin bound yet, sample capture sessions;
   the first real call app (not Dimmy's own process, not a system process)
   becomes the origin (`TryAdoptCallOriginDuringMeeting` in
   `CallDetectionService.cs`).

Both paths call **`dimmy_call_meeting_started_external()`** to arm the Rust
state machine's `recording_active_from_us` — without it,
`dimmy_call_signal_session_ended` returns NoChange (rc=0) and **no popup**.

**Mac tasks:**
- Mac call detection uses `kAudioDevicePropertyDeviceIsRunningSomewhere`
  (CoreAudio), not WASAPI session enumeration. Mirror the **logic**, not
  the API:
  - When a meeting becomes active and a call is detected (now or later),
    treat that call as the meeting origin and call
    `dimmy_call_meeting_started_external()`.
  - Exclude Dimmy's own capture from the "is a call running" signal.
- On the bound call ending → `dimmy_call_signal_session_ended()` →
  rc=3 → show the Mac stop-suggestion (`CallNudgeWindowController`).

---

## 5. Recap choice honored by ALL stop paths  🚧 GOTCHA

**Windows bug fixed:** the "Generate recap on stop" checkbox was only
honored by the meeting-window stop. The **pill** and **call-detect popup**
stop paths ran the recap unconditionally — so a meeting started with recap
*unchecked* got a recap anyway when stopped from the popup/pill.

**Fix:** a single shared flag `AppViewModel.MeetingGenerateRecap` captured
at meeting **start** (from the checkbox; defaulted `true` for
call-detect-started meetings) and read by **every** stop path. The popup
CTA also relabels: "Stop & recap" vs "Stop".

**Mac tasks:**
- Add the equivalent shared flag (e.g. on `AppState` /
  `MeetingViewModel`), captured at meeting start from the Mac "generate
  recap" control.
- Honor it in **every** Mac stop path: meeting window, menu-bar / pill
  equivalent, and the call-detect stop-suggestion. `MeetingPostProcessService.swift`
  must skip `RunRecap` when the flag is false.
- Relabel the Mac stop-suggestion button "Stop" vs "Stop & recap" to match.

---

## 6. Also verify on Mac (carried by this branch)

- **Recap reads `notes.md`** as high-priority emphasis: Windows'
  `MeetingPostProcessService` + `MeetingRecapHelpers` feed `notes.md` into
  the recap prompt. Confirm the Mac `MeetingPostProcessService.swift` does
  the same (so notes actually influence the Mac recap).

---

## Build & test on Mac

```bash
# Rust static lib (Mac frozen feature set) — see docs/BUILD.md
cargo build --lib --release --target aarch64-apple-darwin \
  --features local-stt-metal,local-llm-metal,local-stt-parakeet-fluid
# then xcodebuild + LAUNCH (SelfTests fire at launch):
scripts/dev/preflight-mac.sh
```

`preflight-mac.sh` is mandatory when touching `platforms/macos/**` — it
launches the .app so runtime `SelfTests` assertions fire (a stale SelfTest
ships a DMG that crashes on first launch). If you widen the ogg gate (§2),
double-check no SelfTest pins the audio format.

---

## TL;DR ordering

1. Notes tab (live) — easy, same `notes.md`.
2. Recap-honoring flag — small, correctness.
3. Call-detect manual/mid-meeting origin + `dimmy_call_meeting_started_external`.
4. Ogg: **UI reads ogg FIRST**, then flip the `cfg!` gate. Not before.
5. Waveform lessons — verify, fix only if Mac shows the same symptoms.

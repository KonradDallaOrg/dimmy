# Handover — Mac: system-audio tap must follow default-output change (replug)

**Date:** 2026-06-05
**Branch this lands on:** `fix/audio-device-change-loopback-follow` (Windows half already merged to `staging`, shipped in `v0.6.53-staging.28`)
**Win reference commit:** `80540e3` — `fix(audio/win): loopback follows the default output on mid-meeting device change`
**Tracks:** task #224 (audio device-change resilience during meeting recording)

## The bug (reproduced from a real meeting on Windows, same risk on Mac)

A user recorded a 33-minute meeting. ~3 minutes in they unplugged their
Bluetooth headphones and plugged them back. From that point the **system
audio was silence** for the rest of the meeting (`audio_system.ogg` = 1.5 MB
of near-silence vs `audio_mic.ogg` = 14 MB). The recap was built from
mic-only audio = useless for the other party's side.

Root cause on Windows: the WASAPI loopback was bound to the output device
that was default **at meeting start**. On unplug, Windows flipped the default
to a different endpoint; the old loopback stream stayed alive capturing
silence. cpal raises an error only on device REMOVAL, not on a default
CHANGE, so the existing auto-recovery (which fired on `AUDIO_STREAM_DEAD`)
never re-bound on the replug.

## What shipped on Windows (mirror this design)

`core/src/audio.rs`:
- The audio thread's 1 s heartbeat already had a device-change auto-recovery
  that fires on `AUDIO_STREAM_DEAD`. Added a second trigger in the same tick:
  track the output device the loopback is bound to (`bound_loopback_name`),
  and when the live default output diverges from it (and Mix/System is
  active), rebind via the same recovery (buffers preserved).
- Pure decision fn `loopback_should_follow_default(source, bound, current)`
  + `current_default_output_name(host)` (Windows reads cpal default output;
  returns `None` and is inert on non-Windows). 3 unit tests.
- Diagnostic: `audio.device_change_recovery` event now carries
  `trigger="default_output_changed"` vs `"stream_dead"`.

**Note:** `current_default_output_name` is already `None` on macOS by design,
so the Rust side does nothing on Mac. The Mac system-audio path is entirely
Swift (Core Audio process tap), so the Mac fix lives in Swift, not Rust.

## Mac analysis — same class of bug, and the rebuild machinery already exists

Mac system audio is NOT cpal loopback. It is a Core Audio **per-process tap**
pushed to Rust via `dimmy_push_loopback_audio`. Files:
- `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift` — the tap.
- `platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift` — driver
  (chooses tap vs ScreenCaptureKit fallback; `start()`/`stop()`).

Key facts (line anchors as of this branch):
- The tap content is per-process (`CATapDescription(monoMixdownOfProcesses:)`,
  ~line 100), but it is **carried by an aggregate device anchored to a
  specific output UID**: `kAudioAggregateDeviceMainSubDeviceKey = outputUID`
  and `SubDeviceList = [outputUID]` (~lines 135–161), where `outputUID =
  Self.defaultOutputDeviceUID()` is read **once at tap build time** (~line 135).
- There is already a rebuild path: `rescanAndRebuildIfNeeded()` (~line 381)
  tears down + `start()`s the tap. It is wired to property listeners on the
  audio-active **process list** (`startRescan()` ~line 266, listener
  registration ~line 283; per-process listeners ~line 344) + a backstop timer.
- **The gap:** `rescanAndRebuildIfNeeded()` only rebuilds when the active
  **PID set** changes (~lines 387–391). There is **no listener on
  `kAudioHardwarePropertyDefaultOutputDevice`**, and the function does not
  compare the current default output UID to the one the aggregate was built
  with. So a mid-meeting output switch (unplug/replug headphones, AirPods
  connect, Sound prefs change) is invisible → the aggregate stays anchored to
  the stale output → same silent-system-audio failure as Windows had.

## Implementation plan (minimal, reuses the existing rebuild)

The rebuild machinery exists; we only need to (a) notice the default output
changed and (b) let it trigger a rebuild. Suggested steps in
`SystemAudioProcessTap.swift`:

1. **Remember the output UID the aggregate was built with.** Add a field
   `private var builtOutputUID: String?`. Set it where the aggregate is
   created (right where `outputUID` is resolved, ~line 135 / used ~line 160).

2. **Register a default-output listener** in `startRescan()` (alongside the
   process-list listener ~line 283), on the SYSTEM object:
   ```swift
   var addr = AudioObjectPropertyAddress(
       mSelector: kAudioHardwarePropertyDefaultOutputDevice,
       mScope: kAudioObjectPropertyScopeGlobal,
       mElement: kAudioObjectPropertyElementMain)
   AudioObjectAddPropertyListenerBlock(
       AudioObjectID(kAudioObjectSystemObject), &addr, ioQueue) { [weak self] _, _ in
           self?.rescanAndRebuildIfNeeded()
       }
   ```
   Keep the returned block/registration so `stopRescan()` (~line 303) removes
   it. Listeners survive tap rebuilds (they are on the system object), same
   note as the existing process-list listener.

3. **Make `rescanAndRebuildIfNeeded()` rebuild on output change too** (~line
   387). Today it returns early unless the PID set changed. Add: also rebuild
   when `Self.defaultOutputDeviceUID() != builtOutputUID`. Log it:
   `NSLog("[SystemAudio/tap] default output changed (%@ -> %@) — rebuilding tap", ...)`.
   The rebuild path (`teardown()` ~line 459 then `start()`) already re-reads
   the default output and rebuilds the aggregate, so once this branch fires
   the new output is picked up and `builtOutputUID` updates on the next build.

4. **Debounce / no-thrash.** A plug event can fire the listener several times
   in a burst. The existing rescan already coalesces on `ioQueue`; confirm the
   output-UID compare makes it idempotent (after rebuild, `builtOutputUID ==
   current` so the next spurious callback is a no-op). If bursts still cause
   2–3 rebuilds, that is acceptable (matches Windows), but a ~250 ms debounce
   on the queue is fine if needed.

5. **ScreenCaptureKit fallback path** (`startWithScreenCapture`,
   `SystemAudioCaptureService.swift` ~line 171): SCKit captures system audio
   globally (not anchored to one output device), so it is likely immune. VERIFY
   on a Mac that uses the SCKit path (older OS / Tahoe ad-hoc) that an output
   switch keeps delivering. If it doesn't, the SCStream may need restart on the
   same listener — but do not add that speculatively; confirm first.

6. **Mic side is already covered.** The mic on Mac is the cpal primary stream
   in `audio.rs`; a mic device removal sets `AUDIO_STREAM_DEAD` and the
   existing recovery rebinds it. No change needed there.

## Tests / validation

- No pure-Swift unit test harness for Core Audio here. Mirror the Windows
  intent with a Swift unit test only if you extract a pure decision (e.g.
  `shouldRebuild(builtUID:currentUID:pidSetChanged:)`) — recommended, it keeps
  parity with `loopback_should_follow_default` and is trivially testable.
- **Manual repro on a Mac (mandatory):**
  1. Start a meeting, play system audio (a YouTube video / a call with the
     other side talking).
  2. Switch the output device mid-meeting: unplug/replug wired headphones, or
     toggle AirPods, or change output in System Settings > Sound.
  3. Expect `NSLog` `[SystemAudio/tap] default output changed ... rebuilding`
     and that system audio keeps being captured (the live meeting waveform's
     system band stays non-zero; `audio_system.*` ends up full-size, not a
     few KB of silence).
  4. Cross-check `audio_system` size vs `audio_mic` after stop — they should be
     comparable, as in healthy meetings.

## Pre-flight — MANDATORY

This touches `platforms/macos/**`. Run `scripts/dev/preflight-mac.sh` (builds
the Rust static lib with the Mac frozen feature set, `xcodebuild`, AND launches
the .app for 5 s so `SelfTests.runAtLaunch` fires). `xcodebuild` alone is not
enough — a stale SelfTests assertion ships a DMG that crashes on first launch.

## When done

Commit on this branch (`fix/audio-device-change-loopback-follow`) or a fresh
`fix/mac-audio-device-change-follow` branched off latest `origin/staging`.
Then merge to `staging` and roll into the next `v0.6.53-staging.N` so the
colleague's Mac can exercise the replug case. The Windows half is already in
`staging` (>= staging.28).

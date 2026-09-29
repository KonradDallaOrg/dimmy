# Meeting reliability between calls — work log (2026-09-29)

Branch: `fix/meeting-reliability` (off `origin/staging` @ db921b0). NOT pushed, NOT merged.
User tests locally on the Mac before anything goes up.

## Reports being worked
1. Colleague (Mac): Mac froze between two meetings (large whisper model still
   transcribing), the second meeting recorded "sheep bleating" audio and a bad transcript.
2. Auto-record starts as soon as the Teams window opens, before joining.
3. Few-second recordings right before / after real meetings.
4. Between meetings it must never freeze; the next recording must start clean.
5. "Check for updates": spinner forever, never says update available; relaunch updates.

## Staging check (done)
CI, E2E, Staging Auto-Update green on db921b0. Mac Rust lib + xcodebuild of staging
compile (commit 2fa7401 said "not compiled" — it compiles).

## Analysis — ranked causes (NEED the colleague's dimmy.log to confirm)
Ask for `~/Library/Application Support/dimmy/dimmy.log` (or `dimmy-staging`) and grep:
- `HAL destroy dispatched` without a following `HAL destroy completed` before the next
  `[SystemAudio/tap] started` → stale tap (A).
- `[Audio/loopback] 5s tick: ... in_samples=` ~480000 (2x) or
  `WARN claimed 48000 Hz but MEASURED ~96000` → two taps pushing (A).
- `[Meeting] WARN capture ratio`, `stop join TIMED OUT`, `transcription is behind`.
- `[CoreML] preparing the Neural Engine encoder` near the meeting boundary (C).

A. **Stale system-audio tap (most likely "bleating")** — `SystemAudioProcessTap.teardown()`
   destroys the HAL objects ASYNC on a serial queue; until `AudioDeviceStop` runs, the old
   IO proc keeps calling `dimmy_push_loopback_audio`. If the next meeting creates a new tap
   first (or one teardown wedges the serial queue), two taps push into one buffer: each
   ~10 ms grain duplicated/interleaved → warble on the system track, and the AEC reference
   gets 2x data → AEC3 suppresses the mic by the wrong envelope → mic warbles too.
   FIX TODO (Swift): per-instance `alive` OSAllocatedUnfairLock<Bool> captured by the IO
   proc block, flipped false SYNCHRONOUSLY in `teardown()` before the async destroy; the
   block returns early when false. Same idea for the SCStream fallback.
B. **Old meeting worker touching the new meeting's buffers** — FIXED in core (below).
C. **Load between meetings**: post-stop full re-transcription (whisper large over the whole
   meeting + diarization) when live labels had holes, recap, and `coreml_encoder::run_deferred()`
   starting the multi-minute ANE compile AT meeting stop. Re-transcription now steps aside
   (FIXED). CoreML: TODO — delay `run_deferred` (e.g. spawn, wait ~5 min, re-defer if a
   meeting is active) instead of compiling at the stop instant.
D. Capture loop called `dimmy_update_stats` (snapshot of every config lock + config.json
   write) every chunk — FIXED.
E. AEC ring design notes (not changed, audio untested on Mac — be careful): zero-filled render
   frames are never consumed later so ref lag only grows; Mac PushLoopback ref ring is
   uncapped (Windows uses `push_to_ring` 1 s cap); `drain(..480)` is O(backlog) under the lock
   the realtime mic callback also takes.

## Done in this branch (core, tests green: `cargo test --lib --features local-stt,local-llm` 1025/1025)
- `meeting::claim_capture_buffers()` + `owns_capture_buffers`: every meeting claims the shared
  buffers (session start AND `dimmy_meeting_start` before clearing); a worker that lost the claim
  breaks out before align / write / drain. Test: `a_meeting_that_lost_the_buffers_never_touches_them_again`
  in `tests/meeting_pause_resume.rs` (was red: drained 22 s of the next meeting; NOT re-run
  after the fix yet — run `cargo test --release --features local-stt,test-ffi --test meeting_pause_resume -- --test-threads=1`).
- Usage-stats seconds travel with the STT job (`SttJob::Chunk::stats_secs`), dropped windows carry
  over, leftover flushed after the sinks are finalized. Guard test
  `the_capture_loop_never_persists_the_config`.
- `dimmy_meeting_retranscribe` returns **-7** when a meeting is recording (checked at start, every
  chunk, before diarization; nothing written). Hosts already fall back to the live transcript.
  TODO: Mac `meetingRetranscribe` map -7 to a clear message ("A meeting is recording…"); Win same.
  Test `retranscribe_steps_aside_while_a_meeting_records`.
- `[CallDetect]` log line per decision (detected / preexisting / ended / stop suggested) — Mac had
  none in dimmy.log (only `print`).
- `emit_meeting_state_payload_is_valid_json_with_correct_keys` made `#[serial]` (global callback).

## TODO next session (Mac first, all testable locally)
1. (A) tap `alive` gate — see above. Small, RT-safe.
2. MeetingViewModel between meetings:
   - `stopAndProcess` keeps `isWorking = true` through relabel + recap → `start()` silently refuses
     a second meeting (manual or auto) during the previous recap. Set `isWorking = false` as soon as
     `meetingStop` returns; add a generation token so the previous stop's completion never sets
     `phase = .done` / done fields while a newer meeting records (only `loadHistory()` + toast).
   - `MeetingWindowController` `meetingRecapSaved` observer calls `loadDoneFromDisk` unconditionally
     → a recap of meeting 1 (pill / auto-stop path) switches the window away from meeting 2's
     recording. Guard: if `viewModel.phase == .recording` only `loadHistory()`.
   - `AppState.callNudgeRespond("record_now")` tells the core RecordNow BEFORE `start()`; if start
     refuses (consent cancelled, busy) the detector stays in RecordingAccepted forever. Make start
     report success and undo on failure.
3. Call detect (reports 2 + 3): Mac pre-meeting scan treats OUTPUT of a whitelisted app (Teams open,
   notification, "call ended" sound) as `mic_active` → auto-record before joining, and the origin
   watch then ends it seconds later (short recordings before/after). Windows uses capture sessions
   only. Fix: pre-meeting `scanRunningProcesses(includeOutput: false)` (keep output for mid-meeting
   adoption); mirror `[CallDetect] mic_active=...` prints to `dimmyHostLog`. Residual risk on both
   OSes: Teams pre-join screen may open the mic (preview meter) — verify with colleague logs
   (`[CallDetect] call detected` timestamp vs join time).
4. Update spinner (Mac, Sparkle 2.9.1): with `automaticallyDownloadsUpdates` a background session
   is in progress / update resumable; a manual `checkForUpdates` then fires none of
   didFindValidUpdate / didNotFind / didAbort → `isChecking` stuck. Implement
   `updater(_:didFinishUpdateCycleFor:error:)` → `isChecking = false`; `didDownloadUpdate` →
   "Update X downloaded — installs when you quit"; if `updater.sessionInProgress` or
   `isUpdateReady` don't spin; optional "Install and relaunch" via `willInstallUpdateOnQuit`
   returning YES + stored handler (guard `mayEndProcess`). Windows (`CheckUpdates_Click` →
   `CheckAndDownloadAsync`): no concurrency guard, re-downloads when `IsUpdateReady`, no progress →
   short-circuit when ready, share the in-flight task, report download %. Build on Windows.
5. Rebuild Mac lib (`source .env.staging` + frozen features) + xcodebuild + launch; run
   `scripts/dev/preflight-mac.sh` steps; then ask the user to test, then push branch.

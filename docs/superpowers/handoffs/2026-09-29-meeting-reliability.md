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

## Done in the second pass (same branch, local commits only)
Verified: `cargo fmt --check`, `cargo clippy --features local-stt,local-llm -D warnings`,
`cargo test --lib` 1025/1025, `--test ffi_e2e` 14/14, `--test meeting_pause_resume` 5/5 (handover
test green), Mac static lib (frozen features + `.env.staging`), `xcodebuild` Debug, Swift tests
250/250, app launched 7 s without a SelfTests crash. Windows C# NOT compiled (no WinUI on the Mac):
build on Windows before merging.

Mac
- (A) `SystemAudioProcessTap`: one `OSAllocatedUnfairLock<Bool>` gate per tap BUILD, captured by
  the IO proc, closed synchronously first thing in `teardown()`; the proc returns before touching
  even the heartbeat. `SystemAudioCaptureService`: same for SCStream (`deliveringStream` id checked
  in the sample handler, cleared on stop).
- `MeetingViewModel`: `isWorking` only covers the core start/stop call; `meetingGeneration` bumped
  per start; a stop's relabel/recap completion (and regenerate recap) never touches the window if a
  newer meeting started — it still runs the recap in the background and refreshes the sidebar. The
  recap flag is pinned at stop time. `start()` returns Bool and sets `activeMeetingDir` from
  `dimmy_meeting_active_dir` (was left on the previous meeting). `.meetingRecording` error for -7.
- `MeetingWindowController`: recap-saved notice does not open Done while `phase == .recording`.
- `AppState.callNudgeRespond("record_now")`: RecordNow sent to the core only after the window
  accepted the start; refused start is logged, no origin bound.
- `CallDetectionManager`: pre-meeting scan is microphone-only (`scanRunningProcesses(includeOutput:
  false)`); output kept for mid-meeting origin adoption. Transitions logged with `dimmyHostLog`.
- `UpdateService` (Sparkle): no spinner when `!canCheckForUpdates` (background download in progress
  — Sparkle ignores the user check, the root cause) or when an update is already downloaded;
  `didFinishUpdateCycleFor` catch-all; 60 s watchdog; `didDownloadUpdate` status;
  `willInstallUpdateOnQuit` keeps the handler → About page "Install and relaunch" button
  (`installNow`, refused while a meeting records).

Core
- `coreml_encoder::run_deferred`: waits 10 min after the meeting stop before compiling (re-defers if
  a meeting is recording then).

Windows (uncompiled)
- `UpdateService.CheckAndDownloadAsync`: single in-flight pass (click joins the background one);
  `DownloadProgress` event from Velopack's `progress:` callback.
- `SettingsWindow.CheckUpdates_Click`: instant answer when `IsUpdateReady`; "Downloading vX… N%".
- `MeetingWindow`: `_meetingGeneration` guard in the stop pipeline (background recap via
  `MeetingPostProcessService.RunRecapAsync` when superseded), recap flag pinned at stop,
  `RefreshAndSelectDir` ignored while Recording, "Regenerate transcript" says a meeting is recording.
- `PillWindow`: recap flag read before the speaker pass.

## Still open (deliberately not done)
- Confirm the causes with the colleague's dimmy.log (fingerprints above).
- Teams pre-join screen may open the MICROPHONE (level meter) — then auto-record still starts in the
  lobby on both OSes. Now visible in the log (`[CallDetect] call detected: teams`,
  `[CallDetect] microphone in use app=teams`) vs the join time. If confirmed, next step: require the
  call app to hold mic AND output together (duplex) before AUTO-record, ask otherwise.
- `dimmy_meeting_start` failing in the core (-3) after "record now" leaves the detector in
  RecordingAccepted until the next stop — rare (mkdir/spawn failure); needs a small FFI to reset.
- AEC notes (E) untouched on purpose: Mac audio path never fully tested, change only with a repro.
- `meeting_transcription_behind` `emit_event` still sits in the capture loop (FREEZE list says it
  should not); host callbacks are async on both OSes today, so left as is.

## Third pass (2026-09-29 evening) + push
- 6f54955 Mac: scrolling the transcript seeks the audio (parity with Win SeekToScrolledTranscript).
- 8b62482 Notes: Mac recording Notes tab = composer + "Add note" (Cmd+Return), appends
  `**[mm:ss]** text` (h:mm:ss past the hour) like Win SubmitNote; Done Notes says editable + used by
  the recap, "Update recap with notes" button. Win: placeholder no longer claims notes skip the recap.
- Branch pushed on user request so staging + rc can be cut from Windows.

## Windows session checklist (before merging into staging)
1. `dotnet build platforms/windows/Dimmy.Windows/Dimmy.Windows.csproj -c Debug -p:Platform=x64` —
   the C# in this branch was never compiled (UpdateService single-flight + `progress:` on
   `DownloadUpdatesAsync`, SettingsWindow CheckUpdates_Click, MeetingWindow generation guard,
   PillWindow recap flag, MeetingWindow.xaml placeholder).
2. Core checklist from CLAUDE.md (already green on the Mac: fmt, clippy, lib 1025, ffi_e2e 14,
   meeting_pause_resume 5).
3. Version check before any tag (CLAUDE.md "Versioning"): 0.7.11 already has staging.1/.2 and rc.1.
4. Then back on the Mac: install the rc DMG from the web and test Sparkle's update path (the
   spinner fix is in UpdateService.swift).

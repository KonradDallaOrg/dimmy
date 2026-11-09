# Handoff: explicit state machines (2026-11-09)

Branch `feat/explicit-state-machines` (local, NOT pushed), base `origin/staging`
`0d456207`. Plan and findings: the report "Dimmy implicit state machines",
https://claude.ai/artifact/35owJxDa2q9BuB8YCrenYS (sections "Plan" and
"Implementation"). This file is what a fresh session needs to finish it.

## Decisions already taken by the user

- Nemotron cannot start → only tell the user, no automatic fallback engine.
- Linux is frozen (only the CLAUDE.md drift was fixed).
- Verification at the end is done by a FRESH agent with a clean context, never
  by the session that wrote the code. Brief:
  `E:\dimmy-run\state-machines-verify\verification-brief.md` (update it with
  whatever the next session adds).
- Keep everything on this branch; push / PR only when the user says so.

## Session 3 (2026-11-09 night, PC-KONRAD): user answers applied, read this first

The branch moved to PC-KONRAD: worktree `C:\ctwt-sm` (the other PC's `E:\` is
this PC's `C:\`, so the scripts are in `C:\dimmy-run\state-machines-verify`).
16 GB RAM here: debug tests use `CARGO_TARGET_DIR=C:/code/dimmy/core/target`,
`-j 4`, from Git Bash with plain `cargo` (no `cargo-e.ps1`, its paths are the
other PC's). Never a release build into that target: it holds the dev DLL.

User answers to the session 2 questions:
- Parity gap: **Windows matches the Mac** — done in `4c6d625e`. The meeting
  stop and a Telegram audio count `TranscribesRunning` and never write the
  pill state; the pill shows the Transcribing capsule while idle
  (`AppViewModel.ShowsBackgroundTranscribe`); `TelegramService._drivingPill`
  is gone. During the core drain (a few seconds) the core still refuses a
  dictation on both OSes (`Busy::MeetingDraining`).
- `meeting_phase` event: user left it to the agent; **skipped** (no reader).
- Item 6 leftovers: **not done** (no reported bug behind them).
- Lost UI: **both brought back** — `46b1d8a6`. Win `Helpers/MeetingDoneNotes`
  (audio incomplete + "Open Recap settings" for rc -5/-6/-7), Mac
  `MeetingViewModel.audioIncomplete`. In memory per run, as before.
- Still to ask: imported audio with an unknown extension is saved as
  `audio.wav` on both OSes now (the Mac used to keep any extension).

Gates in session 3: `v2_ffi` 64/64, `meeting_pause_resume` 5/5 (log
`gates-c-v2ffi.log`), C# 628/628, Windows host build OK, Mac preflight (see
the brief). Not runnable here: Mac XCTest (no GUI session over SSH); FlaUI
not run locally (it kills the running Dimmy and deletes the real
`.onboarding_done`), it runs in CI on the PR.

## Session 2 (2026-11-09 evening): items 1-7 done, read this first

Moving to the other PC. 10 more commits on the branch (25 in all), from
`9b3d8e78` to the commit that adds this section. The list below "Remaining,
in this order" is session 1's; its status now:

1. DONE `9b3d8e78`: license polling gone on both hosts (Win waits for
   `LicenseChanged`, Mac for `dimmyLicenseChanged`, 8 s budget); Mac
   `LicenseKind` enum.
2. DONE `84296c9e`: Mac pill `.error(String)`; all pill errors go through
   `AppState.showError` (beside a recording: "Still recording" toast; while
   transcribing: shown at `endDictation`; else 5 s on the pill, token-guarded).
   The Mac pill meeting stop is background work (`transcribesRunning`), never
   writes the dictation state. SelfTests `testPillErrorRoute`.
3. DONE `d3b689d1` (Win) + `d3dcb808` (Mac): ONE meeting stop pipeline,
   `Services/MeetingStopService.cs` on Windows (pill, window, shortcut,
   call-detect), `PillWindowController.stopMeetingFromPill` on Mac (the
   window's Stop calls it too). In-flight guard on both. Window waits via
   EnterWrapUp / RefreshAndSelectDir / LeaveWrapUp. Win pause label follows
   `MeetingPaused`; `RecordingStartedUtc` follows `dictation_phase`.
   **NOT built: the core `meeting_phase` event.** With one pipeline nobody
   would read it (`meeting_state` + the stop result cover every transition).
   Ask the user before adding it.
4. DONE `d440d9dd` (+ `edad5fff`, which undoes an accidental `cargo fmt --all`
   on `core/vendor/fluidaudio-rs`; use `cargo fmt`, never `--all`):
   `dimmy_meeting_import` (core makes imported/Telegram meetings, schema 2),
   `dimmy_meeting_set_recap_status` (pending/failed shown in both Done views),
   Notion page recorded + auto-send skips a meeting that has one, Regenerate
   transcript updates `chunk_count`/`transcript` (+ atomic write),
   `dimmy_consent_has`, `dimmy_onboarding_done` / `_set_done` (Mac UserDefaults
   key now only means "the wizard ran here"; `configuredByAssistant` = marker
   && !that key).
5. PARTLY `b1d1446d`: the core's stop/cancel now feed `dictation_session`.
   The three statics STAY (each has a comment saying why): not provably
   equivalent if moved into `capture.rs`.
6. PARTLY `16a91d83`: Mac download bars keyed per engine
   (`bundleDownloadProgress`). NOT done, and to be decided with the user
   before coding: legacy blocking download FFIs -> download center (a UX
   change on both hosts), onboarding step enums, update phase enum, Telegram
   login phase, command arming enum, engine slots / Nemotron failure latch.
   These are refactors with no reported bug behind them.
7. DONE `21349222`: no more hard-coded `%APPDATA%\dimmy` on Windows (ptt.log
   x4, crash.log, ModelPaths: the staging prefetch used the PROD models
   folder). Mac TerminalRunner same fix.

Open questions for the user (raise them, don't decide):
- Parity gap: on the Mac a dictation may start while a meeting saves (stop
  is background work); on Windows the pill still shows Transcribing and
  blocks it (MeetingStopService sets `AppState.Transcribing`). Make Windows
  match (needs a "background transcribe" pill panel like `RecapPanel`)?
- `meeting_phase` core event: skip (recommended) or build?
- Item 6 leftovers: which, if any.
- Lost on the window-stop path: the "⚠ audio incomplete" suffix on DoneMeta
  (the toast remains) and the actionable "Open recap settings" CTA on a
  failed recap (the Done view now says "The recap failed: <reason>").
- Imported audio with an unknown extension is now saved as `audio.wav`
  (Windows rule); the Mac used to keep any extension.

Gates run in session 2 (this PC, namespace `dimmy-agenttest`):
- clippy `-D warnings` OK; lib tests 1405/1405 OK; `ffi_e2e` release 14/14 OK.
- C# build OK, `dotnet test` 609/609 OK.
- Mac preflight (MacinCloud) after every Mac commit: static lib, xcodebuild,
  6 s launch without SelfTests trip OK. Mac lib tests 1389/1390: only the
  known environmental `process_raw_prompt_with_claude_code_url_...` failure.
- NOT run yet: `v2_ffi` (new round-trip tests for import / recap status /
  consent / onboarding are in it), `meeting_pause_resume`, FlaUI smoke tests,
  Mac XCTest (`DimmyTests`, one test removed there).
- Then: update `verification-brief.md` (add items 1-7 above, especially
  MeetingStopService, the import FFI, the onboarding/consent migration),
  fresh-agent review, and only then push/PR when the user says so.

## Done in session 1 (15 commits)

Phase 0 docs; 1a `core/src/transcriber_health.rs` (+ Win/Mac toasts, meta
`transcript`); 1b call-detect `record_aborted` (holds THAT call until the mic is
free; a first version used the 30-min cooldown and was wrong); 1c `StreamEnd`
guard + `llm_stream` `job_id`/`purpose`; 2 `core/src/capture.rs` (Owner enum,
meeting start rc -4 under a dictation, `dictation_phase` event); 3 Win
`AppViewModel.RestState` / cancellable overlay timers / errors beside a
recording, Mac Telegram as background work; 4 outside start attaches the
meeting window (also from Done), failed pill stop releases the window, Mac
`setPaused`; 5 `meeting::update_meta` (atomic, schema 2, stage, recap),
`dimmy_meeting_set_title`, `dimmy_meeting_recover_interrupted`; 6 `[paused]`
retry, sticky GPU fallback announced, failed realtime socket → batch STT.
The user tested the local build by hand: OK.

## Remaining, in this order

1. **License polling (rule violation)**: `SettingsWindow.xaml.cs` ~4024 loops
   on `dimmy_license_status` every 400 ms; a `LicenseChanged` event exists. Mac:
   `kind: String` compared 41 times → decode once into an enum.
2. **Mac pill error state**: no `.error` in `RecordingState`; split dictation
   state from background work (`PillWindowController` meeting stop still sets
   `.transcribing`, lines ~465-572). Update `SelfTests.testRecordingStateAnimationIds`
   and run the Mac preflight.
3. **Phase 4 proper**: one meeting stop pipeline on Windows (pill
   `StopMeetingFromPillAsync` and `MeetingWindow` stop duplicate ~250 lines),
   a core `meeting_phase` event (recording|paused|finishing|done, keep the
   `meeting_state` bools), Win stale pause label, stale `RecordingStartedUtc`.
   Mirror on Mac (`MeetingViewModel`).
4. **Phase 5 rest**: file-load import writes meta.json itself (Win
   `Services/FileLoadToMeetingService.cs` ~107, Mac `FileLoadToMeetingService.swift`
   ~124) → core FFI; record recap `failed`/`pending`; Regenerate transcript must
   update `chunk_count` + `transcript` status; record Notion export (re-send
   makes a 2nd page); onboarding completion + auto-record consent stored 3× →
   core (`dimmy_consent_has`, onboarding record).
5. **Phase 2 rest**: remove `STOP_DRAINING` / `STOP_IN_PROGRESS` /
   `MEETING_GENERATION` into `capture.rs` only if it stays provably equivalent;
   let the core's stop drive `dictation_session.rs`.
6. **Phase 6 rest**: Mac download bars fight (`DimmyCore.swift` ~1596, one job
   state per id), legacy blocking download FFI → download center, onboarding
   step enums (Win `OnboardingViewModel`, Mac `totalSteps` + SelfTests), update
   phase enum (Velopack/Sparkle), Telegram login phase, command arming enum,
   engine slots / Nemotron failure latch.
7. Pre-existing bug found: the staging build writes its C# log to the prod
   folder `Roaming\dimmy\ptt.log` (Windows host log path ignores the namespace).

Gate before handing to the fresh verifier: clippy, lib tests,
`meeting_pause_resume`, `ffi_e2e` (release, `--features local-stt,test-ffi`,
`--test-threads=1`, NOT run yet), C# tests, FlaUI smoke tests (NOT run yet),
Mac preflight.

## How to build and test on THIS PC (paid for today)

- C: is nearly full and commit memory is ~23-28 GB with little headroom.
  Everything builds on E:. Use `E:\dimmy-run\state-machines-verify\cargo-e.ps1`
  (VS 2026 dev shell, `CARGO_TARGET_DIR=E:\dimmy-build\agent`,
  `CARGO_HOME=E:\dimmy-build\cargo-home`, `LOCALAPPDATA=E:\lad`, TEMP on E:).
  It sets `DIMMY_CONFIG_NAMESPACE=dimmy-agenttest` for `test`/`clippy` (tests
  must never touch the real config) and the staging values for `build`.
  PowerShell eats `--`: write `__`.
- Always `-j 2` (and `$env:CMAKE_BUILD_PARALLEL_LEVEL="2"`) for release builds:
  `-j 8`/`-j 3` were killed by Claude Code for low memory. A background command
  can be reaped when memory is critical; a foreground one (<10 min) was not.
  OS error 1455 "paging file too small" = memory, then `cargo clean -p` the
  crates whose rlibs went bad.
- Release DLL (staging flavor, frozen features + license-client + telegram):
  `cargo-e.ps1 build -j 2 --release --lib --features "local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,denoise-gtcrn,local-stt-qwen,license-client,telegram"`.
  Only the `dimmy` crate rebuilds now (~2.5 min).
- Local staging the user runs: `E:\dimmy-run\state-machines\Dimmy.Windows.exe`
  (`assemble-staging.ps1` builds it from scratch; `sq.version` must be deleted
  from it). To swap a new build in: check the last `[Capture]` line of
  `%APPDATA%\dimmy-staging\dimmy.log` ends in `-> Idle` (no recording), send
  `quit` on the named pipe `DimmyCommand`, copy `dimmy_lib.dll` and/or
  `Dimmy.Windows.dll`+`.pdb` (C# via `dotnet publish -c Release -r win-x64
  --self-contained true -p:Platform=x64 -p:WindowsAppSDKSelfContained=true
  -p:SelfContained=true -o E:\dimmy-run\state-machines-next`), restart.
- Mac: MacinCloud over ssh (`~/.ssh/macincloud_dimmy`, Git Bash ssh). Send the
  branch as `git bundle create sm.bundle 0d456207..feat/explicit-state-machines`
  and run `E:\dimmy-run\state-machines-verify\mac-preflight.sh` there
  (`SKIP_TESTS=1` to skip lib tests). Gotchas it already handles: fetch into
  `refs/sm/head` (the worktree branch is checked out), per-worktree
  `CARGO_TARGET_DIR` with one path spelling (`pwd -P`), the staging license
  env for `license-client`, copying llama/ggml dylibs into
  `target/aarch64-apple-darwin/release`. Launch check: `mac-launch.sh` (copies
  the dylibs into `release/deps` too). One Mac lib test fails,
  `process_raw_prompt_with_claude_code_url_returns_no_api_key_when_not_installed`,
  believed environmental (claude CLI installed there): confirm on staging.
- Edits: `E:\dimmy-run\state-machines-verify\edit_multi.py <spec.py>` does
  exact all-or-nothing replacements keeping line endings (bash heredocs mangle
  quotes; Windows-Python text mode turns LF files CRLF, git normalizes).
- `cargo fmt` before every commit (pre-commit hook refuses otherwise).

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

## Done (15 commits)

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

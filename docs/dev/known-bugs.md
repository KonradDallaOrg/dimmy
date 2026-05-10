# Known Bugs & Root Causes

Check this file before touching audio preprocessing, macOS FFI, or Windows transparency code.

## AUDIO-001: dagc produces NaN on zero-amplitude input (CRITICAL)
- **Symptom (dictation)**: Speech after a 5s+ pause is killed — processed audio is -91dB (dead silent) from the pause onward
- **Symptom (file load, 2026-05-08)**: Long files with stretches of silence (typical meeting recordings) come back almost entirely empty from the STT engine. 97 % of a 95-min WAV became silent zeros; Parakeet emitted empty for 186 of 191 chunks while the first 5 transcribed normally.
- **Root cause**: `dagc::MonoAgc` produces ALL NaN when fed zero-amplitude (silence) samples. Once corrupted, ALL subsequent output is NaN forever. The NaN gets clamped to 0.0 by our safety net.
- **How it happened (dictation)**: VAD grace period (3s) was emitting silence frames to the output buffer. These zero-amplitude frames went through AGC → NaN corruption → all subsequent speech destroyed.
- **How it happened (file load)**: `dimmy_transcribe_file` was using the same `RawAudio::preprocess` pipeline as live dictation. Long silence stretches in the source file went through AGC, which corrupted internal gain state on the first stretch and emitted NaN-then-zero for everything after.
- **First fix attempt (v0.3.48, FAILED for dictation)**: Reset AGC when grace expires. Didn't work because `process_buffer()` calls `process()` ONCE with all samples. Grace silence and post-silence speech end up in the same output Vec. Fresh AGC processes grace silence first → NaN again.
- **Correct fix for dictation (v0.3.49)**: Grace period only delays `in_speech→false` — does NOT emit silence frames. Hysteresis branch checks RMS before emitting. AGC NEVER sees zero-energy audio.
- **Correct fix for file load (commit `0ed682b`, 2026-05-08)**: New `preprocess::process_buffer_for_file_load` — clamp + highpass only, NO VAD, NO AGC. `dimmy_transcribe_file` calls this instead of `RawAudio::preprocess`. File-load doesn't need AGC: the source has whatever gain it has and we'd rather pass it through than risk silence-stretch corruption.
- **Key rule**: NEVER feed silence/zero samples to dagc. If dagc needs to be replaced, verify the replacement handles zero input gracefully. **For file load: never call the live preprocess pipeline.** Use `process_buffer_for_file_load`.
- **Files**: `preprocess.rs` (`vad_filter`, `process`, `process_buffer_for_file_load`), `ffi.rs` (`dimmy_transcribe_file` callsite)
- **Tests (dictation)**: `dagc_produces_nan_after_silence`, `no_nan_in_output_after_silence_gap`, `output_no_nan_with_multiple_silence_gaps`
- **Tests (file load)**: `file_load_long_silence_does_not_corrupt_subsequent_audio` + 7 other `file_load_*` tests in `preprocess.rs`. Plus `core/tests/parakeet_long_file.rs` as a diagnostic-style early-warning test on real long WAVs.

## AUDIO-002: VAD onset not re-triggering after long silence
- **Symptom**: Same as AUDIO-001 — speech after pause not transcribed
- **Related to**: AUDIO-001. Even if AGC is fixed, the VAD onset mechanism must work correctly after grace period expires.
- **How onset works**: After grace expires (`in_speech=false`), new speech needs `MIN_SPEECH_FRAMES=3` consecutive frames where `voice_prob > effective_onset || energy_override`. If frames alternate above/below threshold, `speech_frames` resets to 0 and onset never confirms.
- **Mitigations**: `energy_override` (rms > ENERGY_FLOOR=0.015 && has_spoken) catches loud speech even when nnnoiseless gives low voice_prob. `effective_onset` uses lower threshold (0.3 vs 0.5) after first speech.
- **Files**: `preprocess.rs` (vad_filter)

## MACOS-001: objc_msgSend variadic declaration crashes on ARM64
- **Symptom**: SIGSEGV with PAC failure on Apple Silicon at runtime. CI builds pass (cross-compile doesn't run binary).
- **Root cause**: Declaring `objc_msgSend` as variadic (`fn objc_msgSend(...) -> Id`) makes Rust emit stack-based args on ARM64 where the actual ABI uses registers.
- **Fix**: Declare as `fn objc_msgSend()` (no args), then `std::mem::transmute` to typed function pointers per call signature.
- **Key rule**: CI builds pass ≠ runtime works on macOS ARM64. Always test on real hardware.
- **Files**: `hotkey.rs`, `lib.rs` (macOS window setup)

## MACOS-002: kCFTypeDictionaryKeyCallBacks symbol type
- **Symptom**: Wrong pointer on macOS
- **Fix**: Must be `static ... : [u8; 0]` not `u8`, use `.as_ptr()` for correct symbol address
- **Files**: `hotkey.rs`

## WIN-002: jump-list customisations dropped on Win11 unpackaged without matching Start-menu shortcut + per-window AUMI
- **Symptom**: `JumpListService.Register()` succeeds (logs `CommitList ok`), but right-clicking the taskbar icon still shows only system defaults (Pin / Close window). No custom Tasks / Style / Translate sections appear.
- **Root cause**: Windows 11 looks up the running process's AUMI, then matches it against AUMIs on Start-menu shortcuts. If no shortcut with the matching AUMI exists, the OS silently drops any `ICustomDestinationList` registration. Process-wide AUMI alone (`SetCurrentProcessExplicitAppUserModelID`) is necessary but not sufficient on Win11 — Velopack production installs DO have a matching shortcut, but dev builds running the EXE directly don't.
- **Fixes layered**:
  1. `SetCurrentProcessExplicitAppUserModelID("Dimmy")` in `App.OnLaunched` BEFORE any window is created.
  2. `SHGetPropertyStoreForWindow` + `PKEY_AppUserModel.ID` set on the anchor window's HWND right after creation (Win11 needs both process AND per-window AUMI for unpackaged apps).
  3. `JumpListService.EnsureStartMenuShortcut()` writes `Dimmy (Dev).lnk` pointing at the running EXE in `%APPDATA%\Microsoft\Windows\Start Menu\Programs\` — idempotent, recreates if EXE path changes. Velopack's installed `Dimmy.lnk` already carries the matching AUMI in production.
  4. `cdl.SetAppID("Dimmy")` before `BeginList` — binds the destination list to our AUMI in Explorer's view.
- **Diagnostic**: `%TEMP%\dimmy_jumplist.log` — every step logged.
- **Also pinned**: `Marshal.ReleaseComObject` on a QI'd interface (`(IPropertyStore)link`) tears down the shared RCW and invalidates the original `link` handle. Releasing happens once, in the AddCategory loop, on the original `IShellLinkW` reference — never on intermediate cast targets.
- **Files**: `JumpListService.cs`, `App.xaml.cs`

## WIN-001: DwmExtendFrameIntoClientArea makes transparency worse
- **Symptom**: Glass blur/shadow added to window borders on Windows 11
- **Root cause**: Using `DwmExtendFrameIntoClientArea` with margins -1 adds glass effect
- **Fix**: Use `WS_POPUP` style (removes DWM frame) + `DWMWCP_DONOTROUND` + `DWMWA_COLOR_NONE`
- **Known limitation**: Thin border persists in some Windows 11 builds (microsoft/WindowsAppSDK#4987)
- **Files**: `lib.rs` (Windows window setup)

## MACOS-003: tao crashes on macOS 26 Tahoe with transparent: true (tao#1171)
- **Symptom**: App crashes immediately on launch with SIGABRT. Crash in `tao::platform_impl::platform::app_delegate::did_finish_launching`. `panic_cannot_unwind` → panic inside FFI callback.
- **Root cause**: tao 0.34.5 has a compatibility issue with macOS 26 (Tahoe). The `transparent: true` config triggers code paths in `did_finish_launching` that panic on macOS 26's changed app lifecycle APIs.
- **Affects**: macOS 26.1+ on Apple Silicon (confirmed on MacBookPro18,3, macOS 26.2)
- **Fix (v0.3.53)**: Disabled `transparent: true` in tauri.conf.json. All transparency is now configured manually in `.setup()` callback: `set_background_color(Color(0,0,0,0))` + platform-specific FFI. Window starts with `visible: false` and is shown after transparency is configured to prevent white flash. On Windows, added explicit `DwmEnableBlurBehindWindow` call (previously done by tao).
- **Upstream**: tao#1171 (open, no fix as of 2026-03-16). Also related: tao#1193 (setStyleMask deadlock on macOS 26).
- **Key rule**: Do NOT re-enable `transparent: true` until tao upstream is fixed.
- **Files**: `tauri.conf.json`, `lib.rs` (.setup callback)

## STT-001: Gemini benchmark ARG_MAX
- **Symptom**: Gemini STT benchmark fails on large audio files
- **Root cause**: base64 data passed as shell argument to `jq -n --arg data "$WAV_DATA"` exceeds ARG_MAX
- **Fix**: Pipe base64 via stdin: `base64 -w0 file | jq -Rs ...` and `curl -d @"$body_file"`
- **Files**: `tests/test_benchmark.sh`

## AUDIO-003: AEC ref-ring starvation hangs Mix-mode capture
- **Symptom**: User starts a meeting (Mix mode) on a setup with no
  active loopback (no default output device, BT headset routed to
  HFP/SCO, output muted, headset unplugged). The audio buffer never
  grows; the meeting records silence. Pre-`3eddac3` this also blocked
  pill dictation in Mix mode.
- **Root cause**: pre-fix, the AEC worker drained mic + reference rings
  in lockstep — it waited until both rings had ≥ 480 samples before
  emitting a frame. If the loopback ring stayed empty, the worker
  blocked forever and never pushed mic samples to `audio_buffer`.
- **Fix (commit `3eddac3`)**: when the reference ring is empty after
  the poll interval, zero-pad the ref frame and run AEC anyway. The
  delay estimator inside `aec3` resyncs once real loopback samples
  start arriving (or stays in the no-echo regime if they never do).
  Mic capture is preserved either way.
- **Key rule**: in always-mix architectures, the AEC must be tolerant
  of one stream being absent. Lockstep drain is incompatible with the
  "Mix is the default, even on systems without loopback" UX choice.
- **Files**: `core/src/aec.rs::spawn_aec_thread`
- **Tests**: `worker_processes_mic_when_ref_ring_empty` +
  `worker_processes_mic_with_ref_present` (symmetric) +
  `worker_honours_shutdown_signal` (no hang on terminate).

## LLM-001: Anthropic Opus 4.7+ rejects `thinking.type=enabled` + `budget_tokens`
- **Symptom**: Recap pipeline fails on Opus 4.7 / Sonnet 5+ with HTTP 400
  `invalid_request_error: thinking.type.enabled is not supported for
  this model`.
- **Root cause**: Anthropic's API change — flagship models from Opus 4.7
  onwards require `thinking.type=adaptive` and reject the legacy
  `budget_tokens` form. Older Sonnets (4 / 4.5 / 4.6) still want the
  legacy form. The Dimmy LLM dispatch hard-coded `enabled` for any
  model with extended thinking turned on.
- **Fix**: extracted dispatch helpers in `llm.rs`:
  `anthropic_wants_thinking(model_lc)` (any thinking-capable model),
  `anthropic_uses_adaptive_thinking(model_lc)` (the new shape — Opus
  4.7+ / Sonnet 5+), `gemini_wants_thinking(model_lc)`,
  `is_gemini_native_url(api_url)`. `process_raw_prompt` picks the
  request shape from these. While writing the unit tests we caught a
  latent bug where `sonnet-6` fell through to plain budget mode —
  fixed in the same commit.
- **Files**: `core/src/llm.rs::process_raw_prompt` + the four helper
  fns. Recap-model override (`recap_model_override` config field) is
  honoured before URL heuristic.
- **Tests**: `anthropic_thinking_dispatch_flagship_models`,
  `anthropic_thinking_dispatch_skips_haiku_and_sonnet3`,
  `anthropic_adaptive_thinking_only_for_new_models`,
  `anthropic_dispatch_combinations_match_routing_rule`,
  `gemini_thinking_dispatch_pro_and_3x`, `case_insensitive_model_matching`.

## WIN-003: WinUI 3 v3.1.7 ListView drag-reorder hard-crashes the renderer
- **Symptom**: App rules drag-reorder in `SettingsWindow` crashes the
  process mid-drag with `combase.dll +0x37fc4 E_UNEXPECTED` /
  `0xc000027b STOWED_EXCEPTION_NOT_HANDLED`. Faulting module is
  `Microsoft.UI.Xaml.dll v3.1.7.0`, exception offset `0x000000000039ce55`.
- **Root cause**: real WinUI 3 v3.1.7 bug in the `ListView`/`ListBox`
  drag pipeline. Same crash signature reproduces on ANY XAML drag
  entry point (`CanReorderItems`, `CanDrag` + `DragStarting` on a
  child, ListView-level `Drop` handler) — the unifying factor is the
  `IDataObject` / `IDropTarget` code path inside
  `Microsoft.UI.Xaml.dll`. Documented in microsoft-ui-xaml issues
  [#5607](https://github.com/microsoft/microsoft-ui-xaml/issues/5607)
  (closed *won't-fix*) and
  [#7690](https://github.com/microsoft/microsoft-ui-xaml/issues/7690)
  (open, no PR, parked on the Markup-team backlog). **No fix in any
  shipping WindowsAppSDK through 2.0.1** — verified against release
  notes Feb–Apr 2026. Bumping the package won't help.
- **Earlier mis-diagnosis**: a previous version of this entry blamed
  EDR-induced COM apartment corruption. That was wrong. The drag
  reorder actually worked on this codebase 2026-05-06 → 2026-05-08
  (commit `ef15a86` added it; same XAML, same WindowsAppSDK
  1.7.260224002 throughout) before the bug surfaced. Reboot
  "fixing it" was coincidence — the latent WinUI bug is gated on
  some other interaction (most likely candidate: the 500 ms UI-thread
  DispatcherTimer added in `feat/system-audio-capture` for meeting-
  state polling, whose ticks during a drag are believed to corrupt
  the drag pipeline's internal state). Root-causing the interaction
  needs WinDbg on a `.mdmp` and is parked.
- **Fix**: replace the built-in pipeline with a manual implementation
  that never touches `IDataObject` / `IDropTarget`:
  - Drop `CanReorderItems` / `AllowDrop` / `ReorderMode` from the
    ListView entirely.
  - Add a dedicated drag handle (Segoe Fluent grip glyph `E76F`) as
    the leftmost column of each row, wrapped in a transparent-
    background `Border` so the entire bounding box is hit-testable.
  - Pointer-event-only mechanics: `PointerPressed` (capture) →
    `PointerMoved` (track + auto-scroll + drop-indicator update) →
    `PointerReleased` / `PointerCaptureLost` (commit + cleanup).
    Compute the target slot via `TransformToVisual` hit-test on the
    rendered `ListViewItem` containers; call
    `ObservableCollection<T>.Move` with the adjustment
    `if (src < dst) dst--` so `Move`'s "final-position" semantics
    match the user's intent.
  - Visual feedback the broken XAML pipeline would have provided:
    - Source row dimmed to opacity 0.45 in place.
    - 2-px accent-colour insertion line tracks the slot boundary.
    - Floating ghost popup with a `RenderTargetBitmap` snapshot of
      the source row, opaque background, follows the cursor.
  - Edge auto-scroll: 40-px top / bottom edge zone, `DispatcherTimer`
    at 40 ms ticks scrolling the inner `ScrollViewer` 14 px per
    tick (drop indicator refreshes after each scroll step).
  - `Win32DropTarget`: tighten `Register()` to skip ALL wiring on
    content-host HWNDs (`Microsoft.UI.Content.DesktopChildSiteBridge`
    + `InputSiteWindowClass`) — they belong to WinUI 3's own
    `IDropTarget` for ListView reorder + child drag. Wiring
    `DragAcceptFiles` + WM_DROPFILES subclass there destabilises
    the renderer even when our XAML doesn't use drag, observed
    during this debug pass. The earlier May 7 baseline gated only
    `RevokeDragDrop` to chrome HWNDs; the new gate restricts the
    entire wiring to `WinUIDesktopWin32WindowClass` +
    `InputNonClientPointerSource`. WAV file drops still bubble up
    via WinUI's own `AllowDrop` chain to the chrome layer.
- **Files**: `platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml`
  + `.xaml.cs` (manual drag handlers),
  `platforms/windows/Dimmy.Windows/Helpers/Win32DropTarget.cs`
  (chrome-only Win32 wiring).
- **Coverage status**: pure index-math helpers can be unit-tested;
  the pointer-driven UI flow is NOT addressable by FlaUI — UIA3
  doesn't drive captured-pointer drag operations on dynamically-
  generated `ListViewItem` containers. Tracked as manual sweep in
  [`docs/dev/system-audio-capture-tests.md`](system-audio-capture-tests.md)
  Tier 3.

## STT-002: SIGABRT at process exit when Parakeet feature is on (cosmetic)
- **Symptom**: parakeet smoke / e2e test runs print `test result: ok` with all asserts green, then the binary exits with SIGABRT (`libc++abi: terminating due to uncaught exception of type std::__1::system_error: mutex lock failed: Invalid argument`). Cargo surfaces it as `process didn't exit successfully (signal: 6)`.
- **Root cause**: ort 2.0.0-rc.10's `Session::drop` touches a global onnxruntime mutex during tear-down. By the time atexit fires that mutex has already been destroyed by libonnxruntime's own atexit chain, and the C++ runtime aborts.
- **Mitigation in place**: `core/src/parakeet.rs` `Box::leak`s the `Mutex<Option<Inner>>` so Rust's static-destructor pass never drops the cached Sessions. Shrinks the surface but doesn't fully suppress the abort because libonnxruntime still has its own globals.
- **Production impact**: none. The released app calls `dimmy_shutdown()` then NSApp / Velopack tears the process down — the user's paste-and-quit flow is finished by then.
- **Test-runner impact**: cargo reports the test binary exit as failure even though all asserts passed. Workaround: read the `test result: ok` line, ignore the SIGABRT trailer.
- **Followup**: revisit when ort ships 2.0.0 stable.

## Native UI Era
No platform-specific bugs filed yet. Report issues at https://github.com/KonradDallaOrg/dimmy/issues

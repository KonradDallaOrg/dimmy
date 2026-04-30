# Known Bugs & Root Causes

Check this file before touching audio preprocessing, macOS FFI, or Windows transparency code.

## AUDIO-001: dagc produces NaN on zero-amplitude input (CRITICAL)
- **Symptom**: Speech after a 5s+ pause is killed — processed audio is -91dB (dead silent) from the pause onward
- **Root cause**: `dagc::MonoAgc` produces ALL NaN when fed zero-amplitude (silence) samples. Once corrupted, ALL subsequent output is NaN forever. The NaN gets clamped to 0.0 by our safety net.
- **How it happened**: VAD grace period (3s) was emitting silence frames to the output buffer. These zero-amplitude frames went through AGC → NaN corruption → all subsequent speech destroyed.
- **First fix attempt (v0.3.48, FAILED)**: Reset AGC when grace expires. Didn't work because `process_buffer()` calls `process()` ONCE with all samples. Grace silence and post-silence speech end up in the same output Vec. Fresh AGC processes grace silence first → NaN again.
- **Correct fix (v0.3.49)**: Grace period only delays `in_speech→false` — does NOT emit silence frames. Hysteresis branch checks RMS before emitting. AGC NEVER sees zero-energy audio.
- **Key rule**: NEVER feed silence/zero samples to dagc. If dagc needs to be replaced, verify the replacement handles zero input gracefully.
- **Files**: `preprocess.rs` (vad_filter, process)
- **Tests**: `dagc_produces_nan_after_silence`, `no_nan_in_output_after_silence_gap`, `output_no_nan_with_multiple_silence_gaps`

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

## Native UI Era
No platform-specific bugs filed yet. Report issues at https://github.com/KonradDallaOrg/dimmy/issues

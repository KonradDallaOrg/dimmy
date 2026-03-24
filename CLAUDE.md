# Dimmy — Project Instructions

## What Dimmy Does

Cross-platform voice transcription overlay. Records audio via global hotkey, transcribes via STT providers (Groq, OpenAI, Deepgram, Gemini), optionally post-processes with LLM, pastes result into active app. Built with Tauri 2 (Rust backend, vanilla HTML/CSS/JS frontend).

Current version: check `src-tauri/Cargo.toml`.

## Development Philosophy (MANDATORY)

### Negative Space Programming
Every function must assert its preconditions and postconditions in production code. Assertions are NOT debug-only — they run in release builds. The absence of a crash IS the proof of correctness.

Rules:
- Assert inputs at function entry (non-zero, non-empty, valid range)
- Assert outputs before return (finite values, expected length, non-empty when expected)
- Assert invariants at state transitions (counters don't overflow, enums are exhaustive)
- Assert postconditions after complex operations (total samples preserved, no NaN)
- Use `assert!()` not `debug_assert!()` — we WANT crashes in prod over silent corruption

### Test-Driven Development (TDD)
Write failing tests BEFORE implementation. Every bug fix must include:
1. A test that reproduces the exact failure
2. The minimal fix
3. Verification that the test passes

Test hierarchy:
- Unit tests in each Rust module (`#[cfg(test)]`)
- Integration tests for provider APIs (`tests/`)
- Regression tests for every bug fixed (with comment linking to the bug)

### Production Stability
"Deve essere super stabile sta app." Every change must be defensive:
- Clamp all audio samples to [-1.0, 1.0]
- Check for NaN/Inf after every DSP operation
- Truncate error bodies to 200 chars (prevents key/PII leak)
- Timeout all HTTP requests (30s base + 1s/MB, cap 600s)
- Validate URLs before use (reject non-HTTPS except localhost)

## Cross-Platform Consistency (MANDATORY)

Every feature, fix, or change MUST work identically on Windows, macOS, Linux.

- Use Tauri abstractions, not OS-specific hacks
- If `#[cfg(target_os = ...)]` is unavoidable, ALL platforms must have equivalent impl
- Never ship a feature that works on one OS but silently fails on another

## Version Bumping (MANDATORY)

ALWAYS update BOTH files in the same commit:
- `src-tauri/Cargo.toml` → `version = "x.y.z"`
- `src-tauri/tauri.conf.json` → `"version": "x.y.z"`

CI enforces consistency — mismatch fails Lint.

After commit: `git tag v0.3.X && git push origin v0.3.X` to trigger Release.

## CI/CD Pipeline

### Workflows
- **ci.yml** — Runs on push/PR to main/staging: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --lib`, version match check
- **staging-release.yml** — Runs on push to staging: lint + test + build all platforms, creates pre-release `staging-latest`
- **release.yml** — Runs on tag push (`v*`): builds Windows (NSIS+MSI), macOS (DMG, universal), Linux (AppImage+deb). Publishes GitHub Release with auto-updater JSON.

### Release Process
1. Commit changes to `main` (or merge feature branch)
2. Bump version in BOTH files, commit
3. Push to origin
4. `git tag v0.3.X && git push origin v0.3.X`
5. Wait for release workflow to complete (~15 min)
6. Users get auto-update notification

### Pre-Push Checklist (MANDATORY — run ALL before every push)
- `cargo fmt --check` — clean
- `cargo clippy -- -D warnings` — zero warnings (CI treats warnings as errors!)
- `cargo test --lib` — all pass
- Version matches in Cargo.toml and tauri.conf.json
- Feature branch merged (if applicable)

## Native UI Migration (in progress)

Branch `feat/native-ui`: replacing WebView with native UIs per platform.

### Phase order
1. **Phase 0** — Rust C FFI layer (`ffi.rs`) — COMPILES but NOT TESTED (TDD violation!)
2. **Phase 1** — Windows native (WinUI3/C#) — settings avanzati behind "Advanced" checkbox
3. **Phase 2** — macOS native (SwiftUI from mockup in `mockup/dimmy-new/`)
4. **Phase 3** — Linux native (GTK4)

### FFI layer status
- `ffi.rs` exports 20 C functions, uses global `OnceLock<AppState>`
- **MUST add tests + assertions before proceeding to Phase 1**
- See memory file `phase0_ffi_status.md` for full checklist
- Gap analysis between WebView and SwiftUI mockup in memory `native_ui_plan.md`

## Architecture Quick Reference

```
Native UIs (future)       → SwiftUI (macOS) / WinUI3 (Win) / GTK4 (Linux)
                          ↕ C FFI (ffi.rs)
Frontend (src/)           → index.html + main.js + styles.css (vanilla JS, Tauri WebView — current)
                          ↕ Tauri IPC (invoke/listen)
Backend (src-tauri/src/)  → lib.rs (state + commands)
                            ffi.rs (C API for native UIs — 20 exported functions)
                            audio.rs (cpal capture, RawAudio → ProcessedAudio → WavPayload)
                            preprocess.rs (highpass → VAD → AGC → downsample)
                            transcribe.rs (multi-provider STT + chunked transcription)
                            llm.rs (multi-provider LLM post-processing)
                            provider.rs (Provider enum, URL detection, file limits, security)
                            error.rs (TranscribeError enum)
                            hotkey.rs (global keyboard hook per-platform)
```

## Audio Pipeline — CRITICAL

See memory file `audio_pipeline.md` for full details. Key rules:

- **NEVER feed zero-amplitude samples to dagc (AGC)**. It produces ALL NaN permanently.
- VAD grace period must NOT emit silence frames — only delay `in_speech→false` transition.
- All audio output must be checked for NaN/Inf and clamped.
- `process_buffer()` calls `process()` ONCE with all samples — this means the entire recording goes through a single VAD→AGC pass.

## Window Transparency — CRITICAL (tao#1171 workaround)

`transparent: true` is **disabled** in `tauri.conf.json` to work around a crash on macOS 26 (Tahoe). The tao library panics in `did_finish_launching` when transparent windows are requested on macOS 26+ (see [tao#1171](https://github.com/tauri-apps/tao/issues/1171)).

**How transparency works now:**
- `tauri.conf.json`: `transparent: false`, `visible: false`
- All transparency is configured manually in `.setup()` callback:
  - `window.set_background_color(Color(0,0,0,0))` — makes WebView transparent (all platforms)
  - macOS: Objective-C FFI sets `setOpaque:NO`, `setBackgroundColor:clearColor`, `setDrawsBackground:NO`, `setTitlebarAppearsTransparent:YES`
  - Windows: `DwmEnableBlurBehindWindow` (replaces what tao did), `WS_POPUP`, `WS_EX_LAYERED`, DWM no-round-corners, DWM no-border
- After transparency setup + positioning, `window.show()` reveals the window

**DO NOT re-enable `transparent: true`** until tao fixes the macOS 26 crash upstream.

**macOS build notes:**
- Builds require Xcode + Command Line Tools
- `Info.plist` provides `NSMicrophoneUsageDescription` for mic permission
- `Entitlements.plist` provides `com.apple.security.device.audio-input` + JIT entitlements
- Build: `cargo tauri build --target universal-apple-darwin` (universal binary for Intel+ARM)
- Dev: `cargo tauri dev`
- The GitHub Actions release workflow (`release.yml`) builds macOS DMG as universal binary

## Known Bugs & Lessons Learned

See memory file `known_bugs.md` for the full registry. Check it before touching:
- Audio preprocessing (preprocess.rs)
- macOS FFI (hotkey.rs, lib.rs window setup)
- Windows transparency (lib.rs window setup)

## Provider System

- Provider enum in `provider.rs`: Groq, OpenAI, OpenRouter, Gemini, Deepgram, Anthropic, Custom
- Auto-detected from URL (`from_url()`)
- Each provider has `max_file_bytes()` for chunking decisions
- STT routing in `transcribe.rs`: OpenAI-compatible (multipart), Deepgram (raw body), Gemini (base64 JSON)
- LLM routing in `llm.rs`: OpenAI-compatible (chat completions), Anthropic (Messages API)

## Conventions

- CLAUDE.md is in .gitignore (not committed)
- Feature branches: `feat/description` or `fix/description`
- Merge to main with `--no-ff` for clear history
- Commit messages: `feat:`, `fix:`, `chore:`, `docs:`, `test:`, `style:`
- One concern per commit — don't bundle unrelated changes

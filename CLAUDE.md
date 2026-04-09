# Dimmy — Project Instructions

## What Dimmy Does

Cross-platform voice transcription overlay. Records audio via global hotkey, transcribes locally via whisper.cpp (default) or cloud STT providers (Groq, OpenAI, Deepgram, Gemini), optionally post-processes with LLM, removes filler words, saves to history, and pastes result into active app. Built as a shared Rust core library with native UIs per platform (WinUI3/C# on Windows, SwiftUI on macOS, GTK4/Rust on Linux).

Current version: 0.4.0

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

- Each platform has its own native UI — ensure feature parity across all three
- If `#[cfg(target_os = ...)]` is unavoidable in the Rust core, ALL platforms must have equivalent impl
- Never ship a feature that works on one OS but silently fails on another

## Version Bumping (MANDATORY)

Update version in `core/Cargo.toml` → `version = "x.y.z"`.

After commit: `git tag v0.3.X && git push origin v0.3.X` to trigger Release.

## CI/CD Pipeline

### Workflows
- **ci.yml** — Runs on push/PR to main/staging: `cargo fmt --check`, `cargo clippy --features local-stt -- -D warnings`, `cargo test --lib --features local-stt`, Linux GTK4 lint
- **staging-native.yml** — Builds all 3 native UIs in parallel:
  - Windows: Rust DLL (`--features local-stt-vulkan`) + .NET build + C# tests + zip
  - macOS: Rust static lib (`--features local-stt-metal`) + Xcode build + DMG
  - Linux: cargo build + clippy + AppImage (default feature `local-stt` = CPU)
- **release.yml** — Runs on tag push (`v*`): builds all 3 native platforms. Publishes GitHub Release.

### Build Requirements
- **CMake** required on all platforms (whisper-rs compiles whisper.cpp from source)
- macOS: `brew install cmake` or Xcode CLI tools
- Windows: `choco install cmake` or via get-cmake GitHub Action
- Linux CI: `sudo apt-get install cmake`

### Release Process
1. Commit changes to `main` (or merge feature branch)
2. Bump version in `core/Cargo.toml`, commit
3. Update `CHANGELOG.md` — move [Unreleased] to new version
4. Push to origin
5. `git tag v0.4.X && git push origin v0.4.X`
6. Wait for release workflow to complete (~15 min)
7. Users get auto-update notification

### Pre-Push Checklist (MANDATORY — run ALL before every push)
- `cargo fmt --check` — clean
- `cargo clippy --features local-stt -- -D warnings` — zero warnings (CI treats warnings as errors!)
- `cargo test --lib --features local-stt` — all pass
- Version updated in `core/Cargo.toml`
- `CHANGELOG.md` updated if releasing
- Feature branch merged (if applicable)
- Native UI builds are CI-only (platform-specific) — no local pre-push requirement

## Native UI Architecture

Shared Rust core with platform-native UIs connected via C FFI.

### Phase status
1. **Phase 0** — Rust C FFI layer (`ffi.rs`) — **COMPLETE** (30+ functions, 246 tests)
2. **Phase 1** — Windows WinUI3/C# — **IMPLEMENTED** (41 C# tests, local STT toggle, model download)
3. **Phase 2** — macOS SwiftUI — **IMPLEMENTED** (STT settings, model download, history view, onboarding)
4. **Phase 3** — Linux GTK4/Rust — **IMPLEMENTED** (builds on CI, AppImage available)

### FFI layer
- `ffi.rs` exports 30+ C functions, uses global `OnceLock<AppState>`
- Original 18 functions + 10 new (model management + history)
- See `docs/dev/native-ui-plan.md` for gap analysis between platform UIs

## Architecture Quick Reference

```
Rust Core (core/src/)            → lib.rs, audio.rs, preprocess.rs, transcribe.rs, llm.rs,
                                  provider.rs, keystore.rs, error.rs, hotkey.rs,
                                  local_stt.rs, history.rs, filler.rs
                                ↕ C FFI (ffi.rs — 30+ exported functions)
Windows UI (platforms/windows/) → WinUI 3 / C# (.NET 8), P/Invoke to dimmy_lib.dll
macOS UI (platforms/macos/)     → SwiftUI, FFI bridge via DimmyFFI.h to libdimmy_lib.a
Linux UI (platforms/linux/)     → GTK4 + libadwaita (Rust), direct crate dependency on dimmy_lib
```

### Cargo Feature Flags
- `local-stt` (default) — enables whisper-rs for local offline transcription
- `local-stt-metal` — macOS GPU acceleration (Apple Silicon Neural Engine)
- `local-stt-vulkan` — Windows/Linux cross-vendor GPU acceleration
- `local-stt-cuda` — NVIDIA GPU acceleration

## Audio Pipeline — CRITICAL

See `docs/dev/audio-pipeline.md` for full details. Key rules:

- **NEVER feed zero-amplitude samples to dagc (AGC)**. It produces ALL NaN permanently.
- VAD grace period must NOT emit silence frames — only delay `in_speech→false` transition.
- All audio output must be checked for NaN/Inf and clamped.
- `process_buffer()` calls `process()` ONCE with all samples — this means the entire recording goes through a single VAD→AGC pass.

## Known Bugs & Lessons Learned

See `docs/dev/known-bugs.md` for the full registry. Check it before touching:
- Audio preprocessing (preprocess.rs)
- macOS FFI (hotkey.rs)
- Platform-specific native UI code

## Provider System

- Provider enum in `provider.rs`: Groq, OpenAI, OpenRouter, Gemini, Deepgram, Anthropic, Custom, **Local**
- Cloud providers auto-detected from URL (`from_url()`); Local is set explicitly via `stt_mode` config
- Each provider has `max_file_bytes()` for chunking decisions (Local = `usize::MAX`)
- STT routing in `transcribe.rs`: OpenAI-compatible (multipart), Deepgram (raw body), Gemini (base64 JSON), **Local (whisper-rs direct)**
- LLM routing in `llm.rs`: OpenAI-compatible (chat completions), Anthropic (Messages API)
- `stt_mode` config field: `"cloud"` (default for upgrades) or `"local"` (offline, no API key needed)

## Local STT (whisper-rs)

- Offline transcription via whisper.cpp, gated behind `local-stt` Cargo feature
- Models: GGML format, downloaded on demand from HuggingFace to `dirs::data_dir()/dimmy/models/`
- Default model: `ggml-base-q8_0.bin` (78 MB)
- Available: Tiny (42 MB), Base (78 MB), Small (181 MB), Medium (514 MB)
- GPU acceleration: Metal on macOS (Apple Silicon), Vulkan on Windows (all GPUs)
- Input: f32 16kHz mono samples from existing audio pipeline (ProcessedAudio → downsample → whisper)
- FFI functions: `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`

## Transcription History

- SQLite database with FTS5 full-text search (`history.rs`)
- DB file: `~/.config/dimmy/history.db` (macOS/Linux) or `%APPDATA%\dimmy\history.db` (Windows)
- Auto-saves after each successful transcription
- FFI functions: `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`

## Filler Removal

- Post-transcription cleanup of speech disfluencies (`filler.rs`)
- 6 languages: Italian, English, Spanish, French, German, Portuguese
- Regex-based with word boundary matching, case insensitive
- Applied to both local and cloud transcriptions when `filler_removal_enabled: true`

## API Key Storage

- **Always uses local AES-256 encrypted file** (`~/.config/dimmy/keys.enc`)
- Key derived from SHA-256(username + hostname + salt), machine-specific
- No OS popups, no admin needed on any platform
- OS keyring (macOS Keychain, Windows Credential Manager) kept as read-only fallback for migration
- The `use_keyring` config field is forced to `false` — toggle removed from all platform UIs

## Conventions

- CLAUDE.md is in .gitignore (not committed)
- Feature branches: `feat/description` or `fix/description`
- Merge to main with `--no-ff` for clear history
- Commit messages: `feat:`, `fix:`, `chore:`, `docs:`, `test:`, `style:`
- One concern per commit — don't bundle unrelated changes

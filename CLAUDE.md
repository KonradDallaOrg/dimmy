# Dimmy — Project Instructions

## What Dimmy Does

Cross-platform voice transcription overlay. Records audio via global hotkey, transcribes via STT providers (Groq, OpenAI, Deepgram, Gemini), optionally post-processes with LLM, pastes result into active app. Built as a shared Rust core library with native UIs per platform (WinUI3/C# on Windows, SwiftUI on macOS, GTK4/Rust on Linux).

Current version: 0.3.64

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

ALWAYS update BOTH files in the same commit:
- `src-tauri/Cargo.toml` → `version = "x.y.z"`
- `src-tauri/tauri.conf.json` → `"version": "x.y.z"`

CI enforces consistency — mismatch fails Lint.

After commit: `git tag v0.3.X && git push origin v0.3.X` to trigger Release.

## CI/CD Pipeline

### Workflows
- **ci.yml** — Runs on push/PR to main/staging: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --lib`, version match check
- **staging-release.yml** — Runs on push to staging: lint + test + build all platforms, creates pre-release `staging-latest`
- **staging-native.yml** — Builds all 3 native UIs in parallel:
  - Windows: .NET build + C# tests + zip artifact
  - macOS: Rust static lib + Xcode build + DMG
  - Linux: cargo build + clippy + AppImage
- **release.yml** — Runs on tag push (`v*`): builds all platforms. Publishes GitHub Release with auto-updater JSON.

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
- Native UI builds are CI-only (platform-specific) — no local pre-push requirement

## Native UI Architecture

Shared Rust core with platform-native UIs connected via C FFI.

### Phase status
1. **Phase 0** — Rust C FFI layer (`ffi.rs`) — **COMPLETE** (40+ tests, assertions, NaN safety)
2. **Phase 1** — Windows WinUI3/C# — **IMPLEMENTED** (41 C# tests, builds, runs)
3. **Phase 2** — macOS SwiftUI — **IMPLEMENTED** (builds, runs, 1 test file)
4. **Phase 3** — Linux GTK4/Rust — **IMPLEMENTED** (builds on CI, AppImage available, needs runtime testing)

### FFI layer
- `ffi.rs` exports 20+ C functions, uses global `OnceLock<AppState>`
- See memory file `phase0_ffi_status.md` for full checklist
- Gap analysis between platforms in memory `native_ui_plan.md`

## Architecture Quick Reference

```
Rust Core (src-tauri/src/)      → lib.rs, audio.rs, preprocess.rs, transcribe.rs, llm.rs,
                                  provider.rs, keystore.rs, error.rs, hotkey.rs
                                ↕ C FFI (ffi.rs — 20+ exported functions)
Windows UI (native-ui/windows/) → WinUI 3 / C# (.NET 8), P/Invoke to dimmy_lib.dll
macOS UI (native-ui/macos/)     → SwiftUI, FFI bridge via DimmyFFI.h to libdimmy_lib.a
Linux UI (native-ui/linux/)     → GTK4 + libadwaita (Rust), direct crate dependency on dimmy_lib
```

## Audio Pipeline — CRITICAL

See memory file `audio_pipeline.md` for full details. Key rules:

- **NEVER feed zero-amplitude samples to dagc (AGC)**. It produces ALL NaN permanently.
- VAD grace period must NOT emit silence frames — only delay `in_speech→false` transition.
- All audio output must be checked for NaN/Inf and clamped.
- `process_buffer()` calls `process()` ONCE with all samples — this means the entire recording goes through a single VAD→AGC pass.

## Known Bugs & Lessons Learned

See memory file `known_bugs.md` for the full registry. Check it before touching:
- Audio preprocessing (preprocess.rs)
- macOS FFI (hotkey.rs)
- Platform-specific native UI code

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

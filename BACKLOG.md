# Dimmy Backlog

> Features and improvements tracked by priority. Items in **v1.0 MVP** are the current focus.
> Everything else is deferred. This file is the single source of truth for what's planned.

## v1.0 MVP (Current Sprint)

> **Goal:** Working macOS + Windows with local transcription, history, filler removal, onboarding.
> **Plan:** `docs/superpowers/plans/2026-04-08-local-stt-mvp.md`

### Local STT (whisper-rs in Rust core)
- [ ] Add `whisper-rs` dependency with platform feature flags (Metal/Vulkan)
- [ ] Add `Provider::Local` variant to provider enum
- [ ] Implement `transcribe_local()` in transcribe.rs
- [ ] Model download manager (on-demand from HuggingFace, progress callback)
- [ ] New config fields: `stt_mode`, `local_model`, `local_model_path`
- [ ] FFI functions: `dimmy_download_model`, `dimmy_get_model_status`, `dimmy_list_local_models`
- [ ] Default model: `ggml-base-q8_0.bin` (78 MB)

### Transcription History (SQLite in Rust core)
- [ ] Add `rusqlite` dependency with `bundled` feature
- [ ] History module: SQLite + FTS5 virtual table
- [ ] CRUD: save, recent, search, delete, stats
- [ ] FFI functions: `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`
- [ ] Auto-save after each transcription

### Filler Removal (Rust core)
- [ ] Add `regex` dependency
- [ ] Filler module: 6 languages (it, en, es, fr, de, pt)
- [ ] Applied after transcription, before LLM
- [ ] Config: `filler_removal_enabled` (default: true)

### macOS UI Updates
- [ ] Settings: STT mode toggle (Local / Cloud)
- [ ] Model download progress UI
- [ ] History view (search + date grouping)
- [ ] Onboarding: add model download step
- [ ] Local STT as default (cloud on-demand)
- [ ] Cloud features (API keys, provider selection) visible only when cloud mode selected

### Windows UI Updates
- [x] Settings: Local STT toggle + model download
- [ ] **History view tab** (P/Invoke declarations exist, UI not yet built — parity gap with macOS)
- [ ] **Models tab** (model browsing UI — macOS has it, Windows only has inline download button)
- [x] Keep ALL existing features unchanged
- [x] Local STT as additional option (cloud remains default for now)

### CI/CD Updates
- [ ] macOS: build with `--features local-stt-metal`
- [ ] Windows: build with `--features local-stt-vulkan`
- [ ] Install CMake in CI runners
- [ ] Cache whisper.cpp compilation

---

## Should Have (v1.1)

### macOS Polish (port from dimmy-new)
- [ ] BlobGlowView — audio-reactive morphing blob glow
- [ ] Dashboard layout (Home stats, History, Config, About)
- [ ] Permission polling (10s interval, force-cancel on revocation)
- [ ] Session chaining for recordings >60s
- [ ] Simplified settings (fewer tabs)
- [ ] @MainActor strict concurrency adoption

### Release Process
- [ ] Update CHANGELOG.md with every release (follow [Keep a Changelog](https://keepachangelog.com/) format)
- [ ] Move [Unreleased] section to new version header on tag push
- [ ] Automate changelog validation in CI (optional: use `git-cliff` or `changelog-enforcer` action)

### Documentation & Context Engineering
- [x] Review and update CLAUDE.md to reflect local STT architecture, new modules (filler, history, local_stt), updated config fields — rewritten 2026-04-21 as slim TOC-style playbook
- [x] Fix CLAUDE.md memory file path references — all paths verified and pointing to `docs/dev/*` correctly
- [x] Update README.md architecture diagram to include local STT path and new modules — mermaid diagram reflects LOCAL/CLOUD split and all modules
- [x] Add per-platform docs — created `platforms/{windows,macos,linux}/README.md`. (README convention rather than per-platform CLAUDE.md; simpler discoverability.)
- [x] Document key architectural decisions — captured as "Decision log" table in `docs/ARCHITECTURE.md`. (Formal `docs/adr/` directory deferred — table is sufficient at current scale.)
- [x] Review all docs/dev/ files for coherence — done 2026-04-21
- [x] Create `docs/BUILD.md`, `docs/RELEASING.md`, `docs/dev/modules.md`, `docs/dev/windows-ci.md`, `CONTRIBUTING.md` — new canonical homes for scattered content
- [x] Add synthetic WAV transcription smoke check — shipped as **tier-1 FFI integration harness** in `core/tests/ffi_e2e.rs` (feeds JFK sample + silent/noise fixtures through the full FFI, mocks cloud HTTP with wiremock). 6 tests, cross-platform Win/Mac/Linux, runs on every PR via `.github/workflows/e2e-tests.yml`. See [`docs/dev/testing.md`](docs/dev/testing.md).

### Cross-Platform
- [ ] Launch at login (macOS: ServiceManagement, Windows: registry/startup folder)
- [ ] Auto-update notification with download link
- [ ] Sound feedback on recording start/stop (optional)
- [ ] VoiceOver / screen reader accessibility

### Onboarding safety — provider mismatch after Cloud setup
> **Observed (2026-04-23):** Existing-user ran Windows onboarding, picked **Cloud + Groq key**. Config also had pre-existing `llm_enabled=true`, `llm_api_url=api.anthropic.com`, `llm_use_same_key=true` (Rust default). Post-onboarding, every transcription triggered `HTTP 401 invalid x-api-key` because LLM post-processing sent the Groq key to Anthropic. Workaround: user manually set `llm_use_same_key=false`. Onboarding didn't warn, didn't coerce.
>
> Impact: any user whose existing config has LLM cloud provider ≠ STT cloud provider + `llm_use_same_key=true` at time of onboarding Cloud setup. New installs are safe (`llm_enabled=false` by default). All platforms potentially affected as onboarding adds Cloud-choice flows — same risk will land on macOS/Linux when their onboarding is similarly shaped.
>
> **Options:**
> - **A (preferred):** defensive coerce in onboarding code-behind. When Cloud is picked + `api_key` written, also inspect `llm_enabled` + `llm_api_url`; if `llm_enabled=true` AND `llm_api_url` provider ≠ new STT provider, force `llm_use_same_key=false`. Localized (~20 lines C# in `OnboardingWindow.xaml.cs::PersistModelChoice`), non-breaking, no other paths affected.
> - **B:** coerce in Rust core `dimmy_set_config_json` — when `api_url` or `api_key` changes, if `llm_api_url` points to a different provider and `llm_use_same_key=true`, force it to `false`. Cross-platform, but touches shared core (higher blast radius).
> - **C:** change default `llm_use_same_key: true → false` in `lib.rs::AppConfig::default`. Breaking for existing users who relied on the default.
>
> Prefer **A**. Cross-apply the same check when macOS/Linux onboarding adds an analogous Cloud step.
- [ ] Implement Option A: provider-mismatch coercion in `OnboardingWindow.xaml.cs::PersistModelChoice` (Windows)
- [ ] Mirror to macOS onboarding Cloud step when added
- [ ] Mirror to Linux onboarding Cloud step when added

### Local STT Improvements
- [ ] Model size picker in settings (tiny/base/small/medium)
- [ ] GPU/CPU toggle in advanced settings
- [ ] Streaming partial results via segment callbacks
- [ ] Model integrity verification (SHA checksum)

### Local LLM Enhancement (offline, on-device)
> **Feasibility study:** `docs/dev/local-llm-feasibility.md` (2026-04-12)
> **Tested:** Gemma 4 E2B Q4_K_M on T600 4GB — works, 3-4s per enhancement, good quality Italian
- [ ] Add `llama-cpp-2` crate behind `local-llm` feature flag (+metal/vulkan/cuda variants)
- [ ] `local_llm.rs`: model catalogue, download (GGUF from HuggingFace), WhisperContext-style cache
- [ ] `llm.rs`: routing branch `if llm_mode == "local"` → call `local_llm::generate()`
- [ ] Disable thinking mode in inference params (critical for Gemma 4 — avoids 20s hidden reasoning)
- [ ] Reinforce "keep same language" in PREAMBLE for small models
- [ ] FFI: `dimmy_list_llm_models`, `dimmy_download_llm_model`, `dimmy_llm_model_exists`
- [ ] Config fields: `llm_mode` (cloud/local), `local_llm_model`
- [ ] Reuse `preferred_gpu_device()` for GPU selection
- [ ] Default model: Gemma 4 E2B Q4_K_M (7.2 GB, Apache 2.0, 140+ languages)
- [ ] Platform UIs: LLM model dropdown in settings (Windows, macOS, Linux)
- [ ] CI: add `local-llm-vulkan` (Windows), `local-llm-metal` (macOS) to build matrix

### Other LLM Improvements
- [ ] Custom system prompt templates
- [ ] Translation improvements (auto-detect source language)

---

## Could Have (v2.0+)

### Architecture
- [x] Monorepo restructure: `src-tauri/` -> `core/`, `native-ui/` -> `platforms/`
- [ ] Root Cargo.toml workspace manifest
- [ ] ADRs in `docs/adr/`
- [ ] UniFFI codegen to replace hand-written FFI
- [ ] Split CLAUDE.md into `.claude/rules/` hierarchy

### macOS Optimizations
- [ ] WhisperKit as macOS-specific fast path (native ANE, ~2x faster)
- [ ] Apple SpeechAnalyzer integration (macOS 26+, zero-dependency)
- [ ] CoreML model pre-compilation for faster first-run

### Features
- [ ] Plugin system for post-processing
- [ ] Keyboard shortcut to paste last N transcripts
- [ ] Export history (JSON, CSV, plain text)
- [ ] Multiple profiles (work/personal with different settings)
- [ ] Speaker diarization (who said what)
- [ ] Real-time live captions overlay mode

### Linux
- [ ] GTK4 UI parity with macOS/Windows features
- [ ] Wayland hotkey support
- [ ] PipeWire audio backend
- [ ] Flatpak distribution

---

## Won't Have (Explicitly Out of Scope)

- Full text editor (we paste into active apps)
- Video/screen recording
- Mobile app (iOS/Android)
- Cloud sync between devices
- Collaborative/shared transcription
- Browser extension
- Electron/web UI (native only)

---

## Completed

- [x] Rust C FFI layer (Phase 0) — 20+ functions, 40+ tests
- [x] Windows WinUI3/C# (Phase 1) — 41 C# tests, full onboarding
- [x] macOS SwiftUI (Phase 2) — builds, runs, onboarding flow
- [x] Linux GTK4/Rust (Phase 3) — builds on CI, AppImage
- [x] Multi-provider cloud STT (Groq, OpenAI, Deepgram, Gemini, Custom)
- [x] LLM post-processing (13 styles, 5 tones, translation)
- [x] Audio preprocessing (VAD, AGC, highpass filter)
- [x] Dual-backend keystore (AES-256-GCM + OS keyring)
- [x] Pill overlay with 5 waveform styles, 6 border styles
- [x] CI/CD pipeline (lint, test, build, release on all 3 platforms)

# Architecture

> **One page. Everything you need to understand the system.**
> For per-module reference, see [`dev/modules.md`](dev/modules.md).
> For the audio DSP pipeline, see [`dev/audio-pipeline.md`](dev/audio-pipeline.md).

## The shape of Dimmy

Dimmy is a **shared Rust core** with **three native UIs** — one per operating system. Every feature (recording, transcription, LLM post-processing, history, keystore) lives in the core. Each UI is a thin layer that calls into the core and renders platform-native chrome.

```
┌──── Windows (WinUI 3 / C# / .NET 8) ────┐
│         P/Invoke → dimmy_lib.dll        │
├──── macOS (SwiftUI / Xcode) ────────────┤
│         C FFI → libdimmy_lib.a (static) │
├──── Linux (GTK4 + libadwaita / Rust) ───┤
│         Rust crate dep → dimmy_lib      │
└──────────────────┬──────────────────────┘
                   │
         ┌─────────▼──────────┐
         │   Rust Core        │
         │   core/src/        │
         └─────────┬──────────┘
                   │
   ┌───────┬───────┼───────┬─────────┐
   ▼       ▼       ▼       ▼         ▼
 cpal  whisper  reqwest  rusqlite  keystore
 (mic)  (STT)   (cloud)  (history)  (AES)
```

## Directory map

```
pai-voice/
├── core/                     Rust core — all business logic
│   ├── src/
│   │   ├── lib.rs            Config, AppState, module exports
│   │   ├── ffi.rs            30+ C exports (cdylib surface)
│   │   ├── audio.rs          Mic capture via cpal
│   │   ├── preprocess.rs     VAD + AGC + highpass (see audio-pipeline.md)
│   │   ├── transcribe.rs     Cloud STT routing + chunking
│   │   ├── local_stt.rs      whisper-rs integration + model download
│   │   ├── llm.rs            LLM post-processing router
│   │   ├── local_llm.rs      llama-cpp-4 integration (optional)
│   │   ├── provider.rs       Provider enum + URL validation
│   │   ├── keystore.rs       AES-256 key storage
│   │   ├── history.rs        SQLite + FTS5 history
│   │   ├── filler.rs         Filler word removal (6 languages)
│   │   ├── hotkey.rs         Global hotkey (platform-specific)
│   │   └── error.rs          Typed error hierarchy
│   └── Cargo.toml            Single source of truth for feature flags
│
├── platforms/
│   ├── windows/              WinUI 3 / C# / .NET 8
│   │   ├── Dimmy.Windows/    Main app (XAML + C#)
│   │   └── Dimmy.Windows.Tests/  41 tests
│   ├── macos/                SwiftUI
│   │   ├── Dimmy/            Main app (DimmyApp.swift, Views/, Managers/)
│   │   └── DimmyTests/       XCTest suite
│   └── linux/                GTK4 + libadwaita (Rust)
│       └── src/              hotkey, pill, settings, tray, waveform
│
├── .github/workflows/        ci.yml, staging-native.yml, release.yml, test-install.yml
├── docs/                     You are here
│   ├── ARCHITECTURE.md       This file
│   ├── BUILD.md              Build commands per platform
│   ├── RELEASING.md          Release runbook
│   ├── dev/                  Canonical dev reference
│   │   ├── audio-pipeline.md     DSP pipeline + VAD state machine
│   │   ├── development-practices.md  Negative space + TDD (MANDATORY)
│   │   ├── known-bugs.md         Root-cause registry
│   │   ├── modules.md            Per-module reference
│   │   ├── native-ui-plan.md     FFI + per-platform status
│   │   ├── windows-ci.md         10 CI invariants (paid in blood)
│   │   └── local-llm-feasibility.md  Feasibility study (2026-04-12)
│   └── superpowers/          Historical implementation plans + specs
│       ├── plans/            Big-feature task lists
│       └── specs/            Per-platform UI design specs
└── CHANGELOG.md              Release notes (Keep a Changelog)
```

## Layers & responsibilities

### 1. UI layer (per-platform, thin)
- Render the pill overlay, settings window, onboarding
- Listen to system events (tray click, global hotkey feedback from core)
- Call into core via FFI/P/Invoke/crate
- **Never** owns state the core also tracks. Never writes `config.json` directly — the core is the single writer.

### 2. FFI boundary (`core/src/ffi.rs`)
- C ABI exports, stable across platform UIs
- Marshals string buffers, ints, JSON blobs
- Owns a global `OnceLock<AppState>` — this is the app's process-wide singleton
- Asserts on every entry (non-null pointers, valid UTF-8, bounds checks)

### 3. Core (`core/src/`)
- Owns all state: config, keys, active recording, model cache, history DB
- Owns all I/O: mic, filesystem, network, SQLite
- No platform-specific code except behind `#[cfg(target_os = ...)]` in `hotkey.rs` and window-attach paths

### 4. External
- **cpal** — audio capture
- **whisper.cpp** (via `whisper-rs`) — local STT, compiled from source via CMake
- **llama.cpp** (via forked `llama-cpp-4`) — optional local LLM, compiled from source via CMake
- **HTTP providers** — Groq, OpenAI, Deepgram, Gemini, Anthropic, OpenRouter
- **OS keyring** — read-only fallback for migration; primary storage is the encrypted file

## Data flow: one recording, end to end

```
Hotkey press
   ↓ (platform hotkey.rs → FFI callback)
AudioCommand::Start → audio.rs spawns cpal stream
   ↓
48kHz f32 samples buffered
   ↓ (second hotkey press or timeout)
AudioCommand::Stop → samples moved to RawAudio
   ↓
preprocess.rs: clamp → highpass → VAD+hysteresis → AGC → clamp
   ↓ ProcessedAudio
   ├── stt_mode == "local":
   │     downsample_to_16k() → whisper-rs full() → String
   └── stt_mode == "cloud":
         estimate_wav_size → [chunk if > provider limit] → to_wav_payload()
         → transcribe_audio() over HTTPS → String
   ↓
filler.rs removes disfluencies (if enabled)
   ↓
llm.rs post-processes (if style != Off) → String
   ↓
history.rs SAVES to SQLite (always)
   ↓
FFI callback → UI paints "done" → UI sends Ctrl+V / Cmd+V to focused window
```

**Every arrow in that diagram has NaN/Inf/bounds assertions.** See [`dev/development-practices.md`](dev/development-practices.md).

## State & config: single-writer rule

- `config.json` lives in `~/.config/dimmy/` (Linux/macOS) or `%APPDATA%\dimmy\` (Windows)
- **Only the Rust core writes it.** UIs send updates via `dimmy_set_config_json()` → core validates → core persists.
- API keys are never in `config.json`. They live in `~/.config/dimmy/keys.enc` (AES-256-GCM, key derived from `SHA-256(username + hostname + salt)`).
- OS keyring exists as read-only fallback for migration from earlier versions. `use_keyring` config field is forced to `false`.

## FFI surface (stable)

30+ exported C functions in three groups:

- **Lifecycle** — `dimmy_init`, `dimmy_shutdown`, `dimmy_check_audio_health`
- **Config & keys** — `dimmy_get_config_json`, `dimmy_set_config_json`, `dimmy_get_api_key`, `dimmy_set_api_key`
- **Recording & transcription** — `dimmy_start_recording`, `dimmy_stop_recording`, `dimmy_transcribe_callback`
- **Models** — `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`
- **History** — `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`
- **Hotkey** — 7 functions for the platform-specific keyboard-hook bridge

All string I/O is UTF-8. All JSON blobs are validated on entry. See `core/src/ffi.rs` for signatures.

## Decision log (short form)

| Decision | Why |
|---|---|
| C FFI over UniFFI | Simpler, zero-codegen, platform UIs are hand-written anyway |
| whisper-rs over WhisperKit | Cross-platform; WhisperKit is macOS-only |
| AES-256 file over OS keyring | No admin prompts, no popups on first use, machine-specific key |
| Native UIs over Electron/Tauri WebView | Platform-consistent UX, better perf, smaller installers |
| Negative Space Programming | Assertions in release builds catch corruption immediately; silent failures cost hours |
| MSVC 14.50+ required on Windows CI | 14.44 miscompiles `ggml-vulkan` state init (see `dev/windows-ci.md` I1) |
| Velopack installer bundles VC Redist via `--framework vcredist143-x64` | Single source of truth for VC runtime (System32), avoids ABI mismatches |

## Where to go next

- **Building:** [`BUILD.md`](BUILD.md)
- **Releasing:** [`RELEASING.md`](RELEASING.md)
- **Writing code:** [`dev/development-practices.md`](dev/development-practices.md) — mandatory philosophy
- **Touching audio code:** [`dev/audio-pipeline.md`](dev/audio-pipeline.md) + [`dev/known-bugs.md`](dev/known-bugs.md)
- **Touching Windows CI:** [`dev/windows-ci.md`](dev/windows-ci.md) — READ BEFORE editing any workflow
- **Per-module deep dive:** [`dev/modules.md`](dev/modules.md)

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
│   │   ├── ffi.rs            132 C exports (cdylib surface; ABI snapshot via abi_snapshot.rs)
│   │   ├── audio.rs          Mic + WASAPI loopback capture via cpal (multi-stream)
│   │   ├── aec.rs            WebRTC AEC3 — Mix-mode echo cancellation
│   │   ├── dfn.rs            DeepFilterNet noise suppression (scaffolding, deferred)
│   │   ├── preprocess.rs     VAD + AGC + highpass + file-load path (see audio-pipeline.md)
│   │   ├── transcribe.rs     Cloud STT routing + chunking
│   │   ├── local_stt.rs      whisper-rs integration + resumable model download
│   │   ├── parakeet.rs       Parakeet TDT v3 (ONNX) — Win/Linux + CPU Mac
│   │   ├── parakeet_fluid.rs Parakeet via FluidAudio CoreML — Mac Apple Neural Engine
│   │   ├── chunked_stt.rs    Realtime chunked Parakeet worker (5 s window + dedup)
│   │   ├── meeting.rs        Long-form meeting mode + pause/resume + post-process
│   │   ├── call_detector.rs  Auto-detect meetings (mic-in-use + app inference) → record nudge (Win + Mac)
│   │   ├── consent.rs        Recording-consent notice text + consent.jsonl audit (GDPR / all-party)
│   │   ├── deepgram_stream.rs Realtime streaming dictation over Deepgram WebSocket (true streaming)
│   │   ├── process_loopback.rs Per-process WASAPI loopback (Phase 5a, Win-only)
│   │   ├── llm.rs            LLM post-processing router + adaptive thinking dispatch
│   │   ├── local_llm.rs      llama-cpp-4 integration (optional)
│   │   ├── download.rs       Resumable + SHA-256-verified model downloads (LLM/whisper/parakeet)
│   │   ├── claude_code.rs    Anthropic subscription LLM via local `claude` CLI (no API key)
│   │   ├── codex.rs          OpenAI/ChatGPT subscription LLM via local `codex` CLI
│   │   ├── claude_desktop.rs Claude Desktop MCP bridge (patches config + spawns dimmy-mcp)
│   │   ├── notion.rs         Notion REST client — send recaps to a page/database
│   │   ├── catalog.rs        Embedded model-catalog.json (single source for cloud models)
│   │   ├── dfn3.rs           DeepFilterNet v3 noise suppression (feature local-dfn)
│   │   ├── provider.rs       Provider enum + URL validation
│   │   ├── app_rules.rs      Per-app LLM style override resolution
│   │   ├── keystore.rs       AES-256 key storage
│   │   ├── history.rs        SQLite + FTS5 history (v2 schema)
│   │   ├── filler.rs         Filler word removal (6 languages)
│   │   ├── hotkey.rs         Global hotkey (platform-specific)
│   │   ├── autostart.rs      Cross-platform launch-at-login (auto-launch crate)
│   │   ├── license.rs        Ed25519 license token verification (offline)
│   │   ├── gpu_health.rs     GPU sentinel + sticky known-bad marker
│   │   ├── gpu_diag.rs       ggml log-callback capture + Vulkan env snapshot
│   │   ├── telemetry/        PostHog + Sentry pipeline (events.rs, sentry_pipeline.rs)
│   │   └── error.rs          Typed error hierarchy
│   ├── src/bin/              Standalone binaries (gated by features)
│   │   ├── dimmy_smoke.rs    Installer FFI smoke (libloading)
│   │   ├── parakeet_smoke.rs / _bench.rs / _fluid_smoke.rs / chunked_smoke.rs
│   │   ├── recap_one_shot.rs Offline LLM recap from existing transcripts.txt
│   │   ├── license_cli.rs    License-server e2e CLI
│   │   └── bench_local.rs / test_local_llm.rs / bench_llm_quality.rs
│   └── Cargo.toml            Single source of truth for feature flags
│
├── platforms/
│   ├── windows/              WinUI 3 / C# / .NET 8
│   │   ├── Dimmy.Windows/        Main app (XAML + C#)
│   │   ├── Dimmy.Windows.Tests/  xUnit unit tests (ViewModels, Services)
│   │   └── Dimmy.Windows.UiTests/ FlaUI UIA3 smoke tests (see testing.md)
│   ├── macos/                SwiftUI
│   │   ├── Dimmy/            Main app (DimmyApp.swift, Views/, Managers/, Services/)
│   │   │   └── Views/Meeting/    MeetingViewModel + 7 sub-views (idle/recording/
│   │   │                         processing/done/sidebar/playback/recap)
│   │   └── DimmyTests/       XCTest suite (XCTest target wired into pbxproj)
│   └── linux/                GTK4 + libadwaita (Rust)
│       └── src/              hotkey, pill, settings, tray, waveform
│
├── core/tests/               Rust integration tests
│   ├── ffi_e2e.rs            Tier-1 end-to-end: pre-recorded PCM → FFI → assert
│   ├── meeting_pause_resume.rs   Pause/resume idempotency + stop-while-paused
│   ├── parakeet_long_file.rs Diagnostic: per-chunk RMS / NaN counts on real WAVs
│   ├── parakeet_e2e.rs       Parakeet ONNX path end-to-end
│   ├── preprocess_properties.rs  proptest invariants (NaN-free, clamped, monotone len)
│   ├── abi_snapshot.rs       Cross-platform symbol diff vs golden fixture
│   ├── v2_ffi.rs / v2_followups.rs  v2 config-field round-trip + retention
│   ├── llm_request_shape.rs  Asserts the exact JSON body each provider gets
│   ├── llm_flows.rs / live_models.rs  Tier-A live LLM matrix + model smoke (manual, #[ignore])
│   ├── telemetry_coverage.rs Telemetry hygiene gate (every Event emitted or reserved)
│   └── stress_tests.rs       Offline stress (no API calls)
│
├── .github/workflows/        ci.yml, staging-auto-update.yml, staging-tester.yml,
│                             release.yml, test-install.yml, e2e-tests.yml
├── docs/                     You are here
│   ├── ARCHITECTURE.md       This file
│   ├── BUILD.md              Build commands per platform
│   ├── RELEASING.md          Release runbook
│   ├── dev/                  Canonical dev reference
│   │   ├── audio-pipeline.md     DSP pipeline + VAD + Mix mode + file-load
│   │   ├── development-practices.md  Negative space + TDD (MANDATORY)
│   │   ├── known-bugs.md         Root-cause registry
│   │   ├── modules.md            Per-module reference
│   │   ├── native-ui-plan.md     FFI + per-platform status
│   │   ├── windows-ci.md         10 CI invariants (paid in blood)
│   │   ├── testing.md            Test pyramid: unit / tier-1 / tier-2 / tier-3
│   │   ├── system-audio-capture-tests.md  PR #45 hardening inventory
│   │   ├── telemetry-implementation.md    PostHog + Sentry implementation
│   │   ├── licensing-{poc,prod,flow}.md   Licensing v2 architecture
│   │   ├── parakeet-local-stt.md / stt-benchmark-*.md  Parakeet docs
│   │   └── local-llm-feasibility.md       Feasibility study (2026-04-12)
│   └── superpowers/          Historical implementation plans + specs (decay-prone)
│       ├── plans/            Big-feature task lists
│       ├── specs/            Per-platform UI design specs
│       └── handoffs/         Time-bound cross-session handoffs
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

## Data flow: dictation (one recording, end to end)

```
Hotkey press
   ↓ (platform hotkey.rs → FFI callback)
AudioCommand::Start → audio.rs spawns cpal mic stream (always-mix forces
                       a parallel WASAPI loopback stream too on Win)
   ↓
48kHz f32 samples buffered
   │  (in Mix mode: aec.rs worker subtracts loopback → mic - speaker_echo)
   ↓ (second hotkey press or timeout)
AudioCommand::Stop → samples moved to RawAudio
   ↓
preprocess.rs: clamp → highpass → VAD+hysteresis → AGC → clamp
   ↓ ProcessedAudio
   ├── stt_mode == "local":
   │     downsample_to_16k() → whisper-rs (or parakeet/parakeet_fluid) → String
   └── stt_mode == "cloud":
         estimate_wav_size → [chunk if > provider limit] → to_wav_payload()
         → transcribe_audio() over HTTPS → String
   ↓
filler.rs removes disfluencies (if enabled)
   ↓
app_rules.rs::resolve(captured_app_id) → optional style override
   ↓
llm.rs post-processes (if style != Off) → String
   ↓
history.rs SAVES to SQLite (v2 schema: enhanced text + audio path + word ts)
   ↓
FFI callback → UI paints "done" → UI sends Ctrl+V / Cmd+V to focused window
```

## Data flow: meeting mode (long-form)

```
Meeting Start
   ↓ dimmy_meeting_start (creates UUID dir under <config>/meetings/<id>/)
   ↓
meeting.rs worker thread spawns
   │   - cpal mic + (Win) loopback streams keep filling audio_buffer
   │   - AEC3 worker (aec.rs) zero-pads ref ring if loopback is empty
   │   - Worker drains chunks (default 15 s, configurable) and either:
   │       streaming-WAV write to audio.wav (16 kHz int16)
   │     + chunked transcribe (cloud OR Parakeet/whisper, same backend
   │       routing as dictation) → transcripts.txt line per chunk
   │     + meta.json updated with last_chunk_ts (live)
   │   ↓
   │   PAUSE: dimmy_meeting_pause → worker skips drain/write/STT;
   │          on resume the paused window is excluded; transcripts.txt
   │          gets a [paused] line at the seam
   │   ↓
Meeting Stop (or pill Stop while meeting active)
   ↓ dimmy_meeting_stop / save_post_process
   ↓
Recap pipeline (UI-side MeetingPostProcessService, Win + Mac mirrors):
   ↓ - read transcripts.txt
   ↓ - llm.rs::process_raw_prompt with the 11-section structured prompt
   ↓     (recap_model_override → URL-heuristic fallback)
   ↓ - Anthropic Opus 4.7+ uses thinking.type=adaptive (no budget_tokens)
   ↓
recap.md + actions.json land in <config>/meetings/<id>/
   ↓
.recording marker deleted on clean stop (presence at startup → recovery)
```

**Every arrow in either diagram has NaN/Inf/bounds assertions.** See [`dev/development-practices.md`](dev/development-practices.md). The Mix-mode AEC ring buffers cap at 1 s headroom (`MAX_RING_SAMPLES = 48_000`) — older samples are dropped with a delay-estimator resync rather than blocking.

## State & config: single-writer rule

- `config.json` lives in `~/.config/dimmy/` (Linux/macOS) or `%APPDATA%\dimmy\` (Windows)
- **Only the Rust core writes it.** UIs send updates via `dimmy_set_config_json()` → core validates → core persists.
- API keys are never in `config.json`. They live in `~/.config/dimmy/keys.enc` (AES-256-GCM, key derived from `SHA-256(username + hostname + salt)`).
- OS keyring exists as read-only fallback for migration from earlier versions. `use_keyring` config field is forced to `false`.

## FFI surface (stable)

~76 exported C functions, snapshotted in `core/tests/fixtures/abi_exports.txt`
and diff-tested by `abi_snapshot.rs` so any silent rename / drop / mangling
fails the PR. The host UIs depend on the exact set; regenerate the golden
in the same PR as the consumer.

Grouped:

- **Lifecycle** — `dimmy_init`, `dimmy_shutdown`, `dimmy_check_audio_health`, `dimmy_get_version`, `dimmy_build_flavor`, `dimmy_set_event_callback`
- **Config & keys** — `dimmy_get_config_json`, `dimmy_set_config_json`, `dimmy_has_api_key`, `dimmy_list_devices_json`
- **Recording & transcription** — `dimmy_start_recording` (rc -7 = meeting active), `dimmy_stop_recording`, `dimmy_cancel_recording`, `dimmy_is_recording`, `dimmy_get_amplitude`, `dimmy_get_loopback_amplitude`, `dimmy_transcribe_file` (rc -1..-8)
- **Meeting** — `dimmy_meeting_start`, `_stop`, `_save_post_process`, `_list_orphans`, `_is_active`, `_pause`, `_resume`, `_is_paused`
- **LLM** — `dimmy_llm_call_raw` (raw prompt for recap), `dimmy_cycle_llm_style`, `dimmy_cycle_llm_tone`
- **App rules** — `dimmy_set_app_context`, `dimmy_clear_app_context`
- **Models** — `dimmy_list_local_models`, `dimmy_list_llm_models`, `dimmy_download_model`, `dimmy_download_llm_model`, `dimmy_*_exists`
- **Parakeet** — `dimmy_parakeet_bundle_present`, `_download_bundle`, `_warmup`
- **History v2** — `dimmy_history_save`, `_recent`, `_search`, `_delete`, `_stats`, `_update_enhanced`, `_update_audio`, `_update_word_timestamps`
- **Hotkey** — 7 functions for the platform-specific keyboard-hook bridge
- **Autostart** — `dimmy_autostart_set_enabled`, `_is_enabled`
- **GPU diagnostics** — `dimmy_gpu_get_status`, `_clear_known_bad`
- **Telemetry** — `dimmy_telemetry_set_enabled` / `_is_enabled` / `_status` / `_anonymous_id` / `_reset_anonymous_id` / `_set_crash_enabled` / `_is_crash_enabled`
- **Licensing** — `dimmy_license_status_json`, `_refresh`, `_clear`, `_devices_list`, `_billing_portal_url`
- **Stats** — `dimmy_update_stats`

All string I/O is UTF-8. All JSON blobs are validated on entry. See
`core/src/ffi.rs` for signatures and `core/tests/fixtures/abi_exports.txt`
for the canonical export list.

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

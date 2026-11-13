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
dimmy/
├── core/                     Rust core — all business logic
│   ├── src/
│   │   ├── lib.rs            Config, AppState, module exports, shared helpers (config dir, write_atomic, log)
│   │   ├── ffi.rs            230 `dimmy_*` C exports (cdylib surface; golden list in tests/fixtures/abi_exports.txt)
│   │   ├── audio.rs          Mic + system-loopback capture via cpal (multi-stream), device follow
│   │   ├── capture.rs        Who owns the shared capture buffers (dictation vs meeting)
│   │   ├── aec.rs            WebRTC AEC3 — Mix-mode echo cancellation
│   │   ├── dfn.rs            RNNoise denoiser (nnnoiseless); only the denoise_offline tool calls it
│   │   ├── dfn3.rs           DeepFilterNet v3 (feature local-dfn); only the denoise_offline tool calls it
│   │   ├── gtcrn.rs          GTCRN denoiser on the 16 kHz STT input (feature denoise-gtcrn, off unless DIMMY_GTCRN=1)
│   │   ├── silero.rs         Silero VAD for the realtime chunk gate (bundled model)
│   │   ├── preprocess.rs     VAD + AGC + highpass + route decision + file-load path (see audio-pipeline.md)
│   │   ├── transcribe.rs     Cloud STT routing + chunking, local engine dispatch
│   │   ├── local_stt.rs      whisper-rs integration + resumable model download + GPU backend decision
│   │   ├── coreml_encoder.rs whisper Core ML encoder bundle (Mac Neural Engine)
│   │   ├── parakeet.rs       Parakeet TDT v3 (ONNX) — Win/Linux + CPU Mac; dispatch to parakeet_fluid
│   │   ├── parakeet_fluid.rs Parakeet via FluidAudio CoreML — Mac Apple Neural Engine
│   │   ├── qwen_asr.rs       Qwen3-ASR via llama.cpp mtmd (feature local-stt-qwen)
│   │   ├── qwen_fluid.rs     Qwen3-ASR via FluidAudio CoreML — Mac Apple Neural Engine
│   │   ├── whistle.rs        Whistle CPU-only STT (engine loaded with libloading)
│   │   ├── nemotron.rs       Nemotron 3.5 ASR Streaming (engine loaded with libloading)
│   │   ├── diarize.rs        Speaker diarization (Nemotron-3-Diarization, ONNX)
│   │   ├── lang_detect.rs    Which language a recording is in (whisper language probabilities)
│   │   ├── chunked_stt.rs    Realtime chunked transcription worker (backend-agnostic, dedup)
│   │   ├── file_stream.rs    Streaming file load: decode → mono → highpass → chunks
│   │   ├── dictation_session.rs  One state machine for dictation presses and releases
│   │   ├── meeting.rs        Long-form meeting mode: capture thread + transcription thread, pause/resume
│   │   ├── transcriber_health.rs Is the transcriber of a meeting producing text
│   │   ├── call_detector.rs  Auto-detect meetings (mic-in-use + app inference) → record nudge (Win + Mac)
│   │   ├── call_media.rs     Is the call still connected (media sockets)
│   │   ├── calendar.rs       Which appointment a meeting was + roster for the recap
│   │   ├── consent.rs        Recording-consent notice text + consent.jsonl audit (GDPR / all-party)
│   │   ├── deepgram_stream.rs Realtime streaming dictation over Deepgram WebSocket (true streaming)
│   │   ├── openai_stream.rs  Realtime streaming dictation over OpenAI Realtime WebSocket
│   │   ├── stream_bounds.rs  Time limits shared by the streaming workers
│   │   ├── capture_sessions.rs The call's mics and outputs (Win-only)
│   │   ├── device_events.rs  OS device notifications → audio thread
│   │   ├── llm.rs            LLM post-processing router + adaptive thinking dispatch
│   │   ├── local_llm.rs      llama-cpp-4 integration (optional)
│   │   ├── download.rs       Resumable + SHA-256-verified model downloads (shared by every model)
│   │   ├── download_center.rs One queue for every on-device model download + delete
│   │   ├── claude_code.rs    Anthropic subscription LLM via local `claude` CLI (no API key)
│   │   ├── codex.rs          OpenAI/ChatGPT subscription LLM via local `codex` CLI
│   │   ├── gemini_cli.rs     Google account LLM via local `gemini` CLI
│   │   ├── kimi_cli.rs       Kimi Code membership LLM via local `kimi` CLI (ACP)
│   │   ├── kimi_mcp.rs       Registers dimmy-mcp with Kimi Code
│   │   ├── cli_runner.rs     Runs a local CLI with a deadline and a process-tree kill
│   │   ├── win_paths.rs      Finds just-installed CLI binaries (Win-only)
│   │   ├── claude_desktop.rs Claude Desktop MCP bridge (installs dimmy-mcp as an extension)
│   │   ├── notion.rs         Notion REST client — send recaps to a page/database
│   │   ├── confluence.rs     Confluence Cloud client — send recaps to a wiki page
│   │   ├── telegram.rs       Telegram Saved Messages inbox source (feature telegram)
│   │   ├── catalog.rs        Embedded model-catalog.json (single source for cloud models)
│   │   ├── provider.rs       Provider enum + URL validation + KeyringScope
│   │   ├── app_rules.rs      Per-app LLM style override resolution
│   │   ├── keystore.rs       AES-256 key storage
│   │   ├── history.rs        SQLite + FTS5 history (v2 schema)
│   │   ├── filler.rs         Filler word removal (6 languages)
│   │   ├── hotkey.rs         Global hotkey (platform-specific)
│   │   ├── hotkey_recorder.rs Recording a new shortcut (rules, conflicts, warnings)
│   │   ├── autostart.rs      Cross-platform launch-at-login (auto-launch crate)
│   │   ├── license.rs        Ed25519 license token verification (offline) + licensing-server client
│   │   ├── legal_doc.rs      Licence + third-party notices as blocks the hosts draw
│   │   ├── hardware.rs       GPU / memory facts for the local-vs-cloud choice
│   │   ├── setup.rs / setup_cli.rs / setup_prefs.rs  Agent setup (`dimmy-setup`)
│   │   ├── gpu_health.rs     GPU sentinel + sticky known-bad marker
│   │   ├── gpu_diag.rs       ggml log-callback capture + Vulkan env snapshot
│   │   ├── win_qos.rs        Exempts local inference from Windows EcoQoS throttling
│   │   ├── sim_audio.rs      Meeting audio from WAV files (feature sim-audio, test builds only)
│   │   ├── telemetry/        PostHog + Sentry pipeline (events, client, sanitize, sentry_pipeline, crash reports)
│   │   ├── error.rs          Typed error hierarchy
│   │   └── main.rs           Stub; the hosts load the library
│   ├── src/bin/              Standalone binaries (36 files; most gated by features)
│   │   ├── dimmy_smoke.rs    Installer FFI smoke (libloading)
│   │   ├── dimmy_setup.rs    `dimmy-setup` launcher (feature setup-launcher)
│   │   ├── parakeet_smoke.rs / _bench.rs / _fluid_smoke.rs / chunked_smoke.rs
│   │   ├── nemotron_smoke.rs / whistle_smoke.rs / qwen_asr_smoke.rs
│   │   ├── recap_one_shot.rs Offline LLM recap from existing transcripts.txt
│   │   ├── license_cli.rs    License-server e2e CLI
│   │   ├── bench_local.rs / test_local_llm.rs / bench_llm_quality.rs
│   │   └── …                 A/B and probe tools (denoise_ab, denoise_offline, vad_ab, window_ab, stt_race, …)
│   ├── assets/               Bundled files: THIRD_PARTY_NOTICES.md, ggml-silero-v6.2.0.bin, gtcrn_simple.onnx, consent/
│   └── Cargo.toml            Single source of truth for feature flags
│
├── assets/model-catalog.json Cloud model catalog, compiled into the core (catalog.rs)
├── mcp-server/               `dimmy-mcp` binary (MCP server for Claude Desktop and Kimi Code)
│
├── platforms/
│   ├── windows/              WinUI 3 / C# / .NET 8
│   │   ├── Dimmy.Windows/        Main app (XAML + C#)
│   │   ├── Dimmy.Windows.Tests/  xUnit unit tests (ViewModels, Services)
│   │   └── Dimmy.Windows.UiTests/ FlaUI UIA3 smoke tests (see testing.md)
│   ├── macos/                SwiftUI
│   │   ├── Dimmy/            Main app (DimmyApp.swift, Views/, Managers/, Services/)
│   │   │   └── Views/Meeting/    MeetingViewModel + sub-views (idle, recording,
│   │   │                         processing, done, sidebar, playback bar, speakers)
│   │   └── DimmyTests/       XCTest suite (XCTest target wired into pbxproj)
│   └── linux/                GTK4 + libadwaita (Rust)
│       └── src/              hotkey, onboarding/, pill_window, settings/, state,
│                             telemetry_consent, terms, text_injector, tray, waveform
│
├── core/tests/               Rust integration tests
│   ├── ffi_e2e.rs            Tier-1 end-to-end: pre-recorded PCM → FFI → assert
│   ├── meeting_pause_resume.rs   Pause/resume idempotency + stop-while-paused
│   ├── audio_hardening.rs    Dictation preprocess-route regression tests
│   ├── call_detect_scenarios.rs  Call-detector scenarios
│   ├── parakeet_long_file.rs Diagnostic: per-chunk RMS / NaN counts on real WAVs
│   ├── parakeet_e2e.rs       Parakeet ONNX path end-to-end
│   ├── preprocess_properties.rs  proptest invariants (NaN-free, clamped, monotone len)
│   ├── abi_snapshot.rs       Symbol diff vs golden fixture (not run by any workflow)
│   ├── v2_ffi.rs / v2_followups.rs  v2 config-field round-trip + retention
│   ├── setup_cli.rs          `dimmy-setup` commands
│   ├── llm_request_shape.rs  Asserts the exact JSON body each provider gets
│   ├── llm_flows.rs / live_models.rs  Tier-A live LLM matrix + model smoke (manual, #[ignore])
│   ├── telemetry_coverage.rs Telemetry hygiene gate (every Event emitted or reserved)
│   ├── stress_tests.rs       Offline stress (no API calls)
│   └── …                     gen_windows_keys, gtcrn_builder_probe, import_and_verify,
│                             live_model_downloads, local_llm_thinking, migrate_keys, repro_peaks_crash
│
├── .github/workflows/        ci.yml, staging-auto-update.yml, staging-tester.yml,
│                             release.yml, test-install.yml, e2e-tests.yml,
│                             engine-nemotron.yml, agent-preview.yml
├── docs/                     You are here
│   ├── ARCHITECTURE.md       This file
│   ├── BUILD.md              Build commands per platform
│   ├── RELEASING.md          Release runbook
│   ├── dev/                  Canonical dev reference
│   │   ├── audio-pipeline.md     DSP pipeline + VAD + Mix mode + file-load
│   │   ├── development-practices.md  Negative space + TDD (MANDATORY)
│   │   ├── known-bugs.md         Root-cause registry
│   │   ├── modules.md            Per-module reference
│   │   ├── model-inventory.md    Local models, sources, licences
│   │   ├── meeting-audio-durability.md  Capture thread vs transcription thread
│   │   ├── native-ui-plan.md     FFI + per-platform status
│   │   ├── windows-ci.md         11 CI invariants (paid in blood)
│   │   ├── testing.md            Test pyramid: unit / tier-1 / tier-2 / tier-3
│   │   ├── system-audio-capture-tests.md  PR #45 hardening inventory
│   │   ├── telemetry-implementation.md    PostHog + Sentry implementation
│   │   ├── licensing-{poc,prod}.md        Licensing v2 architecture
│   │   ├── parakeet-local-stt.md / stt-benchmark-*.md  Parakeet docs
│   │   ├── nemotron.md / diarization.md / qwen3-asr-2026-09-09.md  Other local engines
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
- Checks its arguments on entry (a null pointer or invalid UTF-8 returns an error code)
- Sends state changes to the host through one event callback (`dimmy_set_event_callback`, `emit_event`)

### 3. Core (`core/src/`)
- Owns all state: config, keys, active recording, model cache, history DB
- Owns all I/O: mic, filesystem, network, SQLite
- Platform-specific code sits behind `#[cfg(target_os = ...)]`, in about 25 modules (for example `hotkey.rs`, `device_events.rs`, `capture_sessions.rs`, `win_qos.rs`, `win_paths.rs`, `claude_desktop.rs`, `gpu_diag.rs`)

### 4. External
- **cpal** — audio capture
- **whisper.cpp** (via `whisper-rs`) — local STT, compiled from source via CMake
- **llama.cpp** (via `llama-cpp-4`, upstream `eugenehp/llama-cpp-rs`) — optional local LLM and Qwen3-ASR, compiled from source via CMake
- **ONNX Runtime** (via `ort`) — Parakeet, diarization, GTCRN
- **Engines loaded at run time** (`libloading`) — Whistle (`needle`) and Nemotron (NeMo-Speech.cpp), downloaded with their models
- **HTTP providers** — Groq, OpenAI, Deepgram, Gemini, Anthropic, OpenRouter, Together, Fireworks, Kimi
- **Local CLIs for subscription LLM** — `claude`, `codex`, `gemini`, `kimi`
- **OS keyring** — read-only fallback for migration; primary storage is the encrypted file

## Data flow: dictation (one recording, end to end)

```
Hotkey press
   ↓ (hotkey.rs → `hotkey` event {binding, edge} → host → dimmy_dictation_input
   ↓  → dimmy_start_recording)
AudioCommand::Start { source: Mic } → audio.rs opens the cpal mic stream
                       (dictation is mic-only; no loopback, no AEC)
   ↓
48kHz f32 samples buffered (resampled from the device rate)
   │  (with streaming_dictation on: Deepgram / OpenAI WebSocket, Nemotron
   │   stream or the chunked worker emit `stt_chunk` events while recording)
   ↓ (release, second press, or the 600 s hard stop)
dimmy_stop_recording → AudioCommand::Stop → samples moved to RawAudio
   ↓
preprocess.rs, by preprocess_route():
   ├── local STT  → full: clamp → highpass → VAD+hysteresis → AGC → clamp
   │                (guarded: falls back when the result is worse)
   ├── cloud STT  → highpass only
   └── disabled   → raw
   ↓ ProcessedAudio
   ├── stt_mode == "local":
   │     downsample_to_16k() → whisper-rs, Parakeet, Qwen3-ASR, Whistle or
   │     Nemotron (resolve_local_backend) → String
   └── stt_mode == "cloud":
         estimate_wav_size → [chunk if > provider limit] → to_wav_payload()
         → transcribe_audio() over HTTPS → String
   ↓
filler.rs removes disfluencies (if enabled)
   ↓
history.rs SAVES the raw text to SQLite (v2 schema: app context, style)
   ↓
`transcript_ready` event + return value → host
   ↓
host calls dimmy_process_with_llm → app_rules.rs::resolve(app context)
   → optional style override → llm.rs post-processes (if style != Off)
   ↓
host calls dimmy_history_update_enhanced, then pastes
   (Ctrl+V / Cmd+V to the focused window)
```

## Data flow: meeting mode (long-form)

```
Meeting Start
   ↓ dimmy_meeting_start (creates UUID dir under <config>/meetings/<id>/)
   ↓
meeting.rs spawns two threads (capture is Mix: mic + system audio)
   │   - cpal mic + loopback streams fill the buffers at 48 kHz (Win: WASAPI
   │     loopback; Mac: the Swift host pushes system audio through
   │     dimmy_push_loopback_audio)
   │   - AEC3 worker (aec.rs) zero-pads ref ring if loopback is empty
   │   - CAPTURE thread (worker_loop): reads the buffers, writes three
   │     tracks — audio (mix), audio_mic, audio_system — as Ogg/Vorbis
   │     (WAV fallback; WAV on Linux), and hands each window (default 15 s,
   │     configurable; 0.5 s for Nemotron) to a bounded queue with try_send.
   │     It never waits; a full queue drops the window (transcript, not audio)
   │   - TRANSCRIPTION thread (stt_thread_loop): transcribes each window
   │     (cloud OR local, same backend routing as dictation) →
   │     transcripts.txt line per chunk + meeting_chunk events
   │   - meta.json (schema 2) written at start: stage "recording",
   │     started_at, device_sample_rate
   │   ↓
   │   PAUSE: dimmy_meeting_pause → capture appends are gated and the
   │          worker skips drain/write/STT; on resume the paused window is
   │          excluded; transcripts.txt gets a [paused] line at the seam
   │   ↓
Meeting Stop (or pill Stop while meeting active)
   ↓ dimmy_meeting_stop: tracks finalized first, then the transcriber joined;
   ↓ meta.json stage → "stopped"
   ↓
Recap pipeline (UI-side MeetingPostProcessService, Win + Mac mirrors):
   ↓ - read transcripts.txt
   ↓ - dimmy_llm_call_raw with the 11-section structured prompt →
   ↓     ffi.rs resolves recap_model_override → llm.rs::process_raw_prompt
   ↓ - Anthropic Opus 4.7+ uses thinking.type=adaptive (no budget_tokens)
   ↓ - dimmy_meeting_save_post_process
   ↓
recap.md + actions.json land in <config>/meetings/<id>/; stage → "recapped"
   ↓
.recording marker deleted on clean stop (presence at startup → recovery)
```

**Every arrow in either diagram has NaN/Inf/bounds assertions.** See [`dev/development-practices.md`](dev/development-practices.md). The Mix-mode AEC ring buffers cap at 1 s headroom (`MAX_RING_SAMPLES = 48_000`) — older samples are dropped with a delay-estimator resync rather than blocking.

## State & config: single-writer rule

- `config.json` lives in the config dir, `dirs::config_dir()/<namespace>`: `~/.config/dimmy/` (Linux), `~/Library/Application Support/dimmy/` (macOS) or `%APPDATA%\dimmy\` (Windows). The namespace is `dimmy` unless the build sets `DIMMY_CONFIG_NAMESPACE` (`lib.rs::config_dir_name`); hosts read it with `dimmy_config_dir_name()`.
- **Only the Rust core writes it.** UIs send updates via `dimmy_set_config_json()` → core validates → core persists.
- API keys are never in `config.json`. They live in `keys.enc` in the same dir (AES-256-CTR + HMAC-SHA256, key derived from `SHA-256(username + hostname + salt)`; stops the file being read on another computer, not malware running as the same user).
- OS keyring exists as read-only fallback for migration from earlier versions. `use_keyring` config field is forced to `false`.

## FFI surface (stable)

230 exported `dimmy_*` C functions in `core/src/ffi.rs` at v0.7.15 (two of
them only in test or debug builds). The golden list
`core/tests/fixtures/abi_exports.txt` holds 221 symbols and is compared by
`core/tests/abi_snapshot.rs`, but no workflow runs that test and the
fixture lacks seven current exports (see `dev/modules.md`, `ffi.rs`). The
host UIs depend on the exact set; regenerate the golden in the same PR as
the consumer.

Grouped (the main entries; `ffi.rs` is the full list):

- **Lifecycle** — `dimmy_init`, `dimmy_shutdown`, `dimmy_check_audio_health`, `dimmy_get_version`, `dimmy_build_flavor`, `dimmy_config_dir_name`, `dimmy_set_event_callback`, `dimmy_host_log`
- **Config & keys** — `dimmy_get_config_json`, `dimmy_set_config_json`, `dimmy_has_api_key`, `dimmy_save_llm_provider_key`, `dimmy_list_devices_json`
- **Recording & transcription** — `dimmy_start_recording` (rc -7 = meeting active), `dimmy_stop_recording`, `dimmy_cancel_recording`, `dimmy_is_recording`, `dimmy_dictation_input`, `dimmy_dictation_phase`, `dimmy_get_amplitude`, `dimmy_get_loopback_amplitude`, `dimmy_push_loopback_audio`, `dimmy_transcribe_file` (rc -1, -2, -5..-8)
- **Meeting** — `dimmy_meeting_start`, `_stop`, `_save_post_process`, `_list_orphans`, `_recover_interrupted`, `_is_active`, `_active_dir`, `_pause`, `_resume`, `_is_paused`, `_set_title`, `_set_recap_status`, `_import`, `_retranscribe`, `_rename_speaker`, `dimmy_meetings_dir`
- **LLM** — `dimmy_process_with_llm` (dictation enhancement), `dimmy_llm_call_raw` (raw prompt for recap), `dimmy_command_transform` (command mode), `dimmy_cycle_llm_style`, `dimmy_cycle_llm_tone`
- **Subscription CLIs** — `dimmy_claude_code_*`, `dimmy_codex_*`, `dimmy_gemini_cli_*`, `dimmy_kimi_*`
- **App rules** — `dimmy_set_app_context`, `dimmy_clear_app_context`
- **Models** — `dimmy_model_catalog_json`, `dimmy_list_local_models`, `dimmy_list_llm_models`, `dimmy_download_model`, `dimmy_download_llm_model`, `dimmy_*_exists`, `dimmy_model_download_enqueue` / `_cancel` / `_snapshot_json`, `dimmy_model_delete`
- **Parakeet** — `dimmy_parakeet_bundle_present`, `_download_bundle`, `_warmup`, `_transcribe`
- **Other local engines** — `dimmy_qwen_asr_*`, `dimmy_whistle_*`, `dimmy_nemotron_*`, `dimmy_diarization_*`, `dimmy_coreml_*`, `dimmy_detect_audio_language`, `dimmy_hardware_json`
- **History v2** — `dimmy_history_save`, `_recent`, `_search`, `_delete`, `_stats`, `_update_enhanced`, `_update_audio`, `_update_word_timestamps`
- **Custom dictionary** — `dimmy_user_dict_add`, `_remove`, `_list_json`
- **Hotkey** — `dimmy_hotkey_install`, `_set`, `_set_command`, `_set_meeting`, `_combos_conflict`, and the recorder: `dimmy_hotkey_recorder_start`, `_stop`, `_is_active`, `_key`
- **Call detection & consent** — `dimmy_call_*`, `dimmy_consent_*`
- **Integrations** — `dimmy_claude_desktop_*`, `dimmy_kimi_mcp_*`, `dimmy_notion_*`, `dimmy_confluence_*`, `dimmy_calendar_*`, `dimmy_telegram_*`
- **Autostart** — `dimmy_autostart_set_enabled`, `_is_enabled`
- **GPU diagnostics** — `dimmy_gpu_get_status`, `_clear_known_bad`
- **Telemetry** — `dimmy_telemetry_set_enabled` / `_is_enabled` / `_status` / `_anonymous_id` / `_reset_anonymous_id` / `_set_crash_enabled` / `_is_crash_enabled` / `_track_typed` / `_capture_feedback` / `_capture_host_exception`, `dimmy_crash_report_*`
- **Licensing & legal** — `dimmy_license_status_json`, `_refresh`, `_clear`, `_devices_list`, `_billing_portal_url`, `_request_trial`, `_redeem`, `_checkout_url`, `_plan_change`; `dimmy_legal_document`, `dimmy_license_text`, `dimmy_third_party_notices`, `dimmy_terms_version`
- **Agent setup** — `dimmy_setup_cli`
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

# Changelog

All notable changes to Dimmy are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.10] - 2026-04-19

### Added
- **Forensic logging across the whisper inference path** — previously the
  log trail ended at `[LocalSTT] Model cached successfully` followed by
  ggml's `whisper_backend_init_gpu: no GPU found`, making it impossible to
  tell whether a subsequent silent process abort happened in
  `create_state`, during `whisper_full`, or in segment extraction. Dimmy
  now emits a line on entry and exit of each of those three phases,
  including `n_threads`, `single_segment`, and sample count. Lines are
  flushed synchronously per the existing `crate::log` semantics, so the
  last line before a C++ abort pins the crash site for post-mortem.

## [0.6.9] - 2026-04-19

### Added
- **Sticky GPU known-bad marker with driver fingerprint (Windows/Linux)** —
  v0.6.8 recovered from a ggml-vulkan abort within a single relaunch, but
  the recovery state was session-scoped: every cold start re-tried the GPU
  path and crashed again before falling back. Dimmy now persists a
  `.gpu_known_bad` record next to the session-scoped sentinel, including a
  fingerprint of the Vulkan loader environment (vulkan-1.dll size +
  registered ICDs on Windows; ICD JSON files on Linux). Subsequent cold
  starts compare the current fingerprint against the recorded one: match →
  stay on CPU without crashing; mismatch (driver/ICD updated) → clear the
  marker and give the GPU another chance automatically. Settings → Debug
  surfaces the status and a "Retry GPU on next launch" button that wipes
  the marker manually. macOS path is a no-op (Metal does not need it).
- **ggml debug logging toggle (Settings → Debug)** — the per-tensor /
  per-layer dumps from whisper + llama load are now suppressed by default,
  cutting cold-start log volume by ~80%. The toggle re-enables them when
  diagnosing a model load. INFO/WARN/ERROR continue to flow at all times.
- **`dimmy_gpu_get_status` / `dimmy_gpu_clear_known_bad` FFI** — JSON status
  reader and one-shot clear for native UIs to surface the GPU recovery
  state and the manual-retry action.

## [0.6.8] - 2026-04-19

### Fixed
- **Local STT/LLM on hosts where ggml-vulkan aborts inside its own init**
  — on dual-boot Windows installs where the same hardware has a partially
  broken driver stack, `WhisperContext::new_with_params` and
  `LlamaBackend::init()` could still abort the process *after* our sentinel
  forced `use_gpu(false)`. Root cause: whisper.cpp/llama.cpp call
  `ggml_backend_init_by_type(CPU)` → `ggml_backend_registry` singleton →
  `ggml_backend_vk_reg()` → `ggml_vk_instance_init()` unconditionally, so
  the `use_gpu` flag on params doesn't prevent Vulkan driver code from
  running. The CPU fallback was effectively identical to the GPU path on
  these machines.

  Fix: when the GPU backend is declared `Unavailable` (sentinel, env var,
  or probe failure), also set `VK_DRIVER_FILES` + `VK_ICD_FILENAMES` to a
  non-existent path. The Vulkan loader then reports zero ICDs, ggml-vulkan
  logs "No devices found" and returns early without touching driver code.
  Re-ordered `llm_cache::generate` so `gpu_backend_status()` runs before
  `LlamaBackend::init()` — the env var must be set before llama.cpp
  triggers its backend registry.

## [0.6.7] - 2026-04-19

### Fixed
- **macOS CI build (v0.6.6 hotfix)** — v0.6.6 assumed Apple clang emitted
  the same `c_int` alias for `ggml_log_level` as MSVC, but Apple clang on
  arm64 actually emits `c_uint` (same as gcc/Linux). Only Windows is the
  outlier. Flipped the cfg so `GgmlLogLevel` is `c_int` on Windows and
  `c_uint` everywhere else.

## [0.6.6] - 2026-04-19

### Fixed
- **CI build on Linux / macOS (v0.6.5 hotfix)** — `ggml_log_level` is a
  bindgen-generated C enum whose Rust alias is `c_int` on Windows (MSVC
  defaults enums to signed int) but `c_uint` on Linux/macOS (gcc and
  Apple clang default to unsigned for non-negative enums). The v0.6.5
  callback signature hard-coded `i32`, which matched Windows only.
  Introduce `GgmlLogLevel` as a cfg-conditional alias so the fn-pointer
  type matches what each platform's bindgen emits.

## [0.6.5] - 2026-04-19

### Added
- **GPU diagnostic logging** — when the GPU backend is first queried, Dimmy now:
  - Installs log callbacks on both `whisper.cpp` and `llama.cpp` so their
    internal ggml messages (including the error text that precedes a
    process abort) land in `dimmy.log` with a `[ggml <LEVEL>]` prefix.
  - Logs a Vulkan environment snapshot: `vulkan-1.dll` path/size, registered
    Vulkan ICDs (from `HKLM\Software\Khronos\Vulkan\Drivers`), and
    `TdrDelay`/`TdrDdiDelay` registry values. Linux logs the `.json` files
    under `/etc/vulkan/icd.d` and `/usr/share/vulkan/icd.d`.
  - Makes post-mortem analysis of GPU crashes possible without extra tooling:
    the last words of ggml before an abort are now in the user's log file.

## [0.6.4] - 2026-04-19

### Fixed
- **GPU crash recovery (Windows/Linux)** — on some machines with Vulkan-capable
  discrete GPUs, `ggml-vulkan` aborts the process during whisper/llama model init
  (not a recoverable Rust error). The app would restart in a loop whenever the
  user chose local STT or local LLM. Added a sentinel file in the config dir that
  is written before any GPU init attempt and deleted on success; if a subsequent
  launch still sees the sentinel, the backend is forced to CPU for that session
  so the app remains usable. Drivers or settings fixed between runs will allow
  the GPU path to be retried automatically.
- **Windows UI clipped on high-DPI displays** — Settings and Onboarding windows
  were resized in raw physical pixels, so at 150%/200% scaling the windows were
  too small and toggles/buttons were cut off. Windows now resize using logical
  DIPs scaled by the monitor DPI (`WindowHelper.ResizeLogical`).
- **Settings "Advanced" toggle overlap** — on narrow window widths the Advanced
  toggle overlapped the scrollable content. The layout now stacks the toggle
  above the content instead of absolute-positioning it over the panel.

## [0.5.2] - 2026-04-12

### Fixed
- **Vulkan GPU auto-detection** — on multi-GPU laptops (e.g. Intel iGPU + NVIDIA dGPU), whisper.cpp defaulted to device 0 (integrated GPU), causing crashes during inference. Now auto-enumerates Vulkan physical devices and selects the first discrete GPU.
- Added Large-v3-Turbo models (Q5, Q8) and Distil-Large-v3.5 models (Q5, Q8) to the local STT model catalogue

### Added
- `preferred_gpu_device()` — Vulkan device enumeration via raw FFI (Windows + Linux), zero new dependencies
- `GGML_VK_DEVICE` env var override for power users / CI to force a specific GPU device index
- Log output lists all Vulkan devices with type (Integrated/Discrete/Virtual/CPU) at first model load

## [0.4.0] - 2026-04-08

### Added
- **Local offline transcription** via whisper.cpp (whisper-rs) — no API keys required
  - macOS: Metal GPU acceleration on Apple Silicon
  - Windows: Vulkan GPU acceleration (NVIDIA, AMD, Intel)
  - CPU fallback on all platforms
  - Default model: Whisper Base Q8 (78 MB, downloaded on first use)
  - 4 model sizes available: Tiny (42 MB), Base (78 MB), Small (181 MB), Medium (514 MB)
- **Transcription history** with full-text search (SQLite + FTS5)
  - Auto-saved after every transcription
  - Searchable from native UI
  - Stats: total words, sessions, duration
- **Filler word removal** for 6 languages (it, en, es, fr, de, pt)
  - Removes "basically", "you know", "cioè", "praticamente", etc.
  - Enabled by default, configurable in settings
- New config fields: `stt_mode` (local/cloud), `local_model`, `filler_removal_enabled`
- FFI functions for model management: `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`
- FFI functions for history: `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`
- `Provider::Local` variant for local STT routing
- CI/CD: platform-specific feature flags (Metal on macOS, Vulkan on Windows)
- BACKLOG.md for feature tracking
- CHANGELOG.md (this file)

### Changed
- Default STT mode is "cloud" for safe upgrades (local mode requires model download first)
- `dimmy_start_recording` skips API key check when in local mode
- Filler removal applied to both local and cloud transcriptions
- **API key storage simplified**: always uses local AES-256 encrypted file (no OS popups, no admin needed). OS keyring kept as read-only fallback for migrating existing keys.
- Removed "Use System Keychain" toggle from macOS, Windows, and Linux settings

### Removed
- `enigo` dependency (unused — native UIs handle text injection)
- `arboard` dependency from core (unused — native UIs handle clipboard; Linux UI has its own)

### Fixed
- README.md referenced non-existent `tauri.conf.json` in pre-push checklist
- `.gitignore` contradicted itself on `docs/superpowers/` directory
- Default `stt_mode` changed from "local" to "cloud" to prevent broken first recording on upgrade (model not yet downloaded)

## [0.3.65] - 2026-04-08

### Changed
- Replaced video with pill-states overview image in README
- Removed Tauri remnants, updated README with native pill screenshots

## [0.3.64] - 2026-04-07

### Changed
- Documentation rewrite + security fixes

[0.4.0]: https://github.com/KonradDallaOrg/dimmy/compare/v0.3.65...v0.4.0
[0.3.65]: https://github.com/KonradDallaOrg/dimmy/compare/v0.3.64...v0.3.65
[0.3.64]: https://github.com/KonradDallaOrg/dimmy/releases/tag/v0.3.64

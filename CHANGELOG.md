# Changelog

All notable changes to Dimmy are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.19] - 2026-04-20

### Fixed
- **Linker gate couldn't find `dumpbin.exe`.** v0.6.18 built
  `dimmy_lib.dll` successfully (cargo finished, toolchain 14.51
  confirmed active) but the post-build gate step called bare
  `dumpbin` via `& dumpbin`, and `dumpbin.exe` wasn't in PATH —
  the previous step's `vcvars64.bat` activation was scoped to its
  `shell: cmd` subshell only. Fix locates dumpbin under
  `$env:VS2026_PATH\VC\Tools\MSVC\<ver>\bin\Hostx64\x64\` and
  invokes via absolute path.

## [0.6.18] - 2026-04-20

### Fixed
- **Explicit `exit 0` at end of VS 2026 install step.** v0.6.17 ran the
  whole script cleanly (printed "DONE") then still exited 1 —
  chocolatey's exit 3010 (reboot-required) leaves `$LASTEXITCODE=3010`,
  and pwsh 7.3+ with `$PSNativeCommandUseErrorActionPreference=$true`
  (the default on GitHub Actions pwsh wrapper) propagates that to the
  step exit code regardless of what runs after. Adding `exit 0` at the
  end of the PowerShell block overrides the inherited code.

## [0.6.17] - 2026-04-20

### Fixed
- **Silent exit 1 after VS 2026 detection.** v0.6.16 reached the
  toolchain verification (`VS 2026 MSVC toolchain: 14.51.36231 at
  C:\Program Files (x86)\Microsoft Visual Studio\18\Insiders`) then
  aborted with exit 1 and no exception text. Replaced the
  `[version]` cast + `echo >> $env:GITHUB_ENV` with explicit regex
  parsing + `Add-Content -Encoding utf8`. Added Write-Host
  breadcrumbs before each potentially-throwing operation to expose
  which line trips next time.

## [0.6.16] - 2026-04-19

### Fixed
- **`vswhere` missing `-prerelease` flag hid the just-installed VS 2026
  preview.** v0.6.15 choco install succeeded (`Chocolatey installed 5/5
  packages`, VS 2026 BuildTools v118.6.0.117102900-preview1 deployed)
  but the post-install `vswhere -version "[18.0,19.0)"` query returned
  nothing — vswhere filters out preview releases by default. Added
  `-prerelease` to every VS 2026 lookup. Also replaced the 3 s sleep
  with a 60 s poll loop because Installer registration can lag the
  choco `installed` report.

## [0.6.15] - 2026-04-19

### Fixed
- **`choco install` of the VS 2026 BuildTools preview needs `--pre`.**
  v0.6.14 failed with `visualstudio2026buildtools-preview not installed.
  The package was not found with the source(s) listed` — chocolatey
  silently omits prerelease packages unless `--pre` is passed. Added
  the flag to both release.yml and staging-native.yml.

## [0.6.14] - 2026-04-19

### Fixed
- **VS 2026 BuildTools install via chocolatey instead of
  `aka.ms/vs/18`.** v0.6.13 tried to download
  `https://aka.ms/vs/18/release/vs_buildtools.exe` but Microsoft has not
  registered the `aka.ms/vs/18/*` short-URLs yet — they all 302-redirect
  to Bing search results, so the "bootstrapper" ended up being ~63 KB
  of HTML and the installer aborted with "The file or directory is
  corrupted and unreadable". Chocolatey hosts
  `visualstudio2026buildtools-preview` (published 2025-12-22), whose
  install script internally fetches the signed bootstrapper from
  Microsoft's CDN. Switching the CI step to `choco install
  visualstudio2026buildtools-preview -y --package-parameters "..."`
  sidesteps the aka.ms gap entirely.

## [0.6.13] - 2026-04-19

### Fixed
- **Windows build succeeds with MSVC 14.50 via side-by-side VS 2026
  BuildTools install.** v0.6.12 hit the expected wall: the pinned
  `windows-2025` runner image ships VS 2022 Enterprise only (MSVC
  14.44), and `setup.exe update` cannot bridge major Visual Studio
  versions, so the pre-build gate aborted. `windows-2025` has no
  path to 14.50 short of installing VS 2026 separately. New Windows
  build step downloads `vs_buildtools.exe` from
  `aka.ms/vs/18/release` and installs the VCTools + VC.Tools.x86.x64
  components to a side-by-side VS 2026 install. The Rust cargo step
  activates that toolchain via `vcvars64.bat` in a cmd shell, scoped
  to the step so subsequent .NET / MSBuild / AppxPackage steps
  continue to use VS 2022 Enterprise (which has the UWP workloads
  VS 2026 BuildTools lacks). The post-build linker-version gate
  from v0.6.12 still enforces `dumpbin /headers` linker >= 14.50,
  so any future regression that drops the toolchain fails CI loudly.
- **Locate VS AppxPackage tools now pins to VS 2022 explicitly.** With
  VS 2026 BuildTools installed alongside, a bare `vswhere -latest`
  would return VS 2026 (newer) which lacks AppxPackage tasks. The
  step now uses `-version "[17.0,18.0)"` to select VS 2022 regardless
  of install ordering.

### Known
- v0.6.12 tag was pushed with the pre-build gate in place but CI
  aborted before producing a Windows build. GitHub release v0.6.12
  exists with Linux + macOS artifacts only. Do not upgrade to v0.6.12
  on Windows; v0.6.13 supersedes it.

## [0.6.12] - 2026-04-19

### Fixed
- **Windows installer crashed on first transcription at
  `whisper_backend_init_gpu` — MSVC 14.44 linker miscompiles whisper.cpp
  Vulkan state init.** v0.6.11 addressed a related-but-wrong ABI theory
  around the bundled VC runtime; removing the bundle reproduced the crash
  identically, proving the runtime wasn't the cause. An empirical DLL swap
  against a locally-built `dimmy_lib.dll` (same source commit, MSVC 14.50
  linker) ran clean end-to-end on the same machine, pinning the bug to
  MSVC 14.44 codegen around `ggml-vulkan`'s per-state backend allocation.
  Fix has three parts:
  1. Windows CI runners pinned to `windows-2025` which ships MSVC 14.50+.
  2. A pre-build step verifies `VC\Tools\MSVC\<newest>` is >= 14.50 and
     invokes the VS Installer to update if not, aborting with a clear
     error message otherwise.
  3. A post-build gate parses the PE header of `dimmy_lib.dll` via
     `dumpbin /headers` and fails the workflow if linker version < 14.50,
     preventing another silent ship of a known-broken build.
- **Stopped co-locating `msvcp140.dll` / `vcruntime140.dll` in the
  installer folder.** Velopack `--framework vcredist143-x64` already
  installs the official Microsoft VC Redist at setup time (lands in
  System32), and Windows DLL search order makes a second co-located copy
  either redundant (when versions match) or actively harmful (when the
  bundled copy shadows a newer System32 ABI — this was the whole v0.6.10
  crash cause). `verify-self-contained.ps1` updated to reflect the
  delegation.

### Details
- `test-install.yml` still only probes 15 s of startup; it does NOT yet
  exercise `dimmy_stop_recording`, which is why both v0.6.10 and v0.6.11
  shipped with this latent break. Extending it to round-trip a synthetic
  WAV through the FFI before ticking the release green is tracked for
  v0.6.13.

## [0.6.11] - 2026-04-19

### Fixed
- **Windows installer crashed in `dimmy_stop_recording` with
  `AccessViolationException`** — ABI mismatch between the Rust DLL and the
  bundled Visual C++ runtime. The CI step that copies `vcruntime140.dll` /
  `msvcp140.dll` into the publish folder walked the entire `VC` tree with
  `Get-ChildItem -Recurse ... | Select-Object -First 1`, which in practice
  returned the oldest redistributable package shipped with Visual Studio
  (e.g. `14.29.30157.0` from 2021). `dimmy_lib.dll` was linked against the
  current compiler toolchain (14.4x+) so imports that existed only in the
  newer `msvcp140.dll` resolved against the older co-located copy — the
  process dereferenced a null vtable entry deep inside whisper.cpp and
  segfaulted. The step now pins to `VC\Tools\MSVC\<newest>\bin\Hostx64\x64`,
  i.e. the exact toolchain the compiler used, so bundled and linked
  runtimes always match. Affected every self-contained installer produced
  by `staging-native.yml` and `release.yml`.

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

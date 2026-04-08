# Changelog

All notable changes to Dimmy are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- Default STT mode is now "local" (offline) — cloud providers available on-demand
- `dimmy_start_recording` skips API key check when in local mode
- Filler removal applied to both local and cloud transcriptions

### Removed
- `enigo` dependency (unused — native UIs handle text injection)
- `arboard` dependency from core (unused — native UIs handle clipboard; Linux UI has its own)

### Fixed
- README.md referenced non-existent `tauri.conf.json` in pre-push checklist
- `.gitignore` contradicted itself on `docs/superpowers/` directory

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

# Dimmy — Claude playbook

> **You are an AI agent working on Dimmy.** This file is your load-bearing context. It is committed to the repo so any Claude Code session starts here.
>
> **Humans:** you want [`README.md`](README.md) (what Dimmy is) or [`CONTRIBUTING.md`](CONTRIBUTING.md) (how to hack on it).

## What Dimmy is (one paragraph)

Cross-platform voice-transcription overlay. Records audio via global hotkey, transcribes locally (whisper.cpp, default) or via cloud STT (Groq, OpenAI, Deepgram, Gemini). Optionally post-processes with an LLM, removes filler words, saves to history, pastes into the focused app. Shared Rust core (`core/`) + one native UI per OS: WinUI 3 on Windows, SwiftUI on macOS, GTK4 on Linux. Current version: see `core/Cargo.toml`.

## Navigation — read these when relevant

| Topic | Doc |
|---|---|
| System architecture, directory map, FFI | [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) |
| Build commands (all platforms, feature flags) | [`docs/BUILD.md`](docs/BUILD.md) |
| Cutting a release | [`docs/RELEASING.md`](docs/RELEASING.md) |
| Development philosophy (mandatory reading) | [`docs/dev/development-practices.md`](docs/dev/development-practices.md) |
| Audio DSP pipeline, VAD, AGC rules | [`docs/dev/audio-pipeline.md`](docs/dev/audio-pipeline.md) |
| Known bugs — read before touching audio, macOS FFI, Windows transparency | [`docs/dev/known-bugs.md`](docs/dev/known-bugs.md) |
| Per-module reference (providers, STT, LLM, history, etc.) | [`docs/dev/modules.md`](docs/dev/modules.md) |
| **Windows CI invariants — MUST read before editing any workflow** | [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md) |
| **Testing strategy — tiers, fixtures, how to run + extend** | [`docs/dev/testing.md`](docs/dev/testing.md) |
| Native UI status across platforms | [`docs/dev/native-ui-plan.md`](docs/dev/native-ui-plan.md) |
| Local LLM feasibility study | [`docs/dev/local-llm-feasibility.md`](docs/dev/local-llm-feasibility.md) |
| **Telemetry implementation (PostHog + Sentry)** | [`docs/dev/telemetry-implementation.md`](docs/dev/telemetry-implementation.md) |
| **Licensing v2 PoC — local server, Ed25519 tokens, 7 test scenarios** | [`docs/dev/licensing-poc.md`](docs/dev/licensing-poc.md) |
| **User-facing privacy policy** | [`PRIVACY.md`](PRIVACY.md) |
| Per-platform notes | [`platforms/windows/README.md`](platforms/windows/README.md), [`platforms/macos/README.md`](platforms/macos/README.md), [`platforms/linux/README.md`](platforms/linux/README.md) |

## Development philosophy — MANDATORY

These rules are non-negotiable. Full rationale lives in [`docs/dev/development-practices.md`](docs/dev/development-practices.md).

### Negative Space Programming
Every function asserts preconditions and postconditions **in production code**. Use `assert!()`, not `debug_assert!()`. The absence of a crash is the proof of correctness. We prefer crashes in prod over silent corruption.

- Assert inputs at function entry (non-zero, non-empty, valid range)
- Assert outputs before return (finite, expected length)
- Assert invariants at state transitions
- Assert postconditions after complex operations (total samples preserved, no NaN)

### Test-Driven Development
Every bug fix:
1. Write a test that reproduces the exact failure
2. Verify it fails
3. Write the minimal fix
4. Verify the test passes
5. Verify no regressions

Every new feature: test describing desired behaviour → fail → implement → pass.

### Cross-platform parity
Every feature, fix, or change MUST work identically on Windows, macOS, Linux. If `#[cfg(target_os = ...)]` is unavoidable, every platform has an equivalent impl. Never ship a feature that works on one OS and silently fails on another.

### Production stability
- Clamp all audio samples to `[-1.0, 1.0]`
- Check for NaN/Inf after every DSP operation
- Truncate error bodies to 200 chars (prevents key/PII leak)
- Timeout all HTTP requests (`30s + 1s/MB`, capped at 600s)
- Validate URLs before use (HTTPS only, localhost exception)

### Less is more
- Don't add error handling, fallbacks, or validation for scenarios that can't happen
- Don't design for hypothetical future requirements — three similar lines beat a premature abstraction
- Default to no comments. Add one only when the WHY is non-obvious (hidden constraint, subtle invariant, workaround for a specific bug)
- Don't explain WHAT the code does — well-named identifiers do that

## Pre-push checklist — run BEFORE every push

**One-time setup** after cloning: activate the committed git hooks
with `./scripts/install-hooks.sh`. The pre-commit hook runs
`cargo fmt --check` on every commit that touches `core/**.rs` and
refuses unformatted code — kills the recurring "cherry-picked
unformatted code → CI Format step red" loop at the source.

From `core/`:

```bash
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm
# tier-1 FFI integration (cross-platform, ~3 s once fixtures are cached):
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1
```

CI treats clippy warnings as errors. CI uses the same feature flags — mismatching will go green locally and red in CI. If you touched Linux UI, also `cd platforms/linux && cargo clippy -- -D warnings && cargo test`. If you touched Windows UI / onboarding / XAML, also run the FlaUI smoke tests (see [`docs/dev/testing.md`](docs/dev/testing.md)).

## v2 surfaces — what shipped on `feat/v2-unified` (2026-05)

The current `staging` is **v0.6.29** with the v2 unified feature set.
A fresh Claude session needs to know these modules + FFI surfaces
exist before touching anything:

| Surface | Rust module | FFI entries | UI mirror (Win) |
|---|---|---|---|
| **App-context rules** — per-app LLM style override | `app_rules.rs` | `dimmy_set_app_context`, `dimmy_clear_app_context` | `Helpers/AppContextCapture.cs` (rich: HWND + focus drift), `ViewModels/AppRuleViewModel.cs` |
| **History v2 schema** — enhanced text, audio path, app process, word timestamps, retention | `history.rs` (idempotent ALTER TABLE) | `dimmy_history_recent`, `_search`, `_update_enhanced`, `_update_audio`, `_update_word_timestamps` | `ViewModels/HistoryItemViewModel.cs`, History detail panel in `SettingsWindow.xaml` |
| **File load** (drop / picker → transcribe, local OR cloud) | extends `ffi.rs` (chunked + cloud branch) | `dimmy_transcribe_file` (rc -1..-8) | `Helpers/Win32FileDialog.cs`, `Helpers/Win32DropTarget.cs` (UIPI bypass), `Helpers/WavPeaks.cs`, ConfirmLargeFileAsync |
| **Meeting mode** — long-form record + LLM recap | `meeting.rs` (worker thread, streaming WAV, transcripts.txt) | `dimmy_meeting_start/_stop/_save_post_process/_list_orphans/_is_active` | `Views/MeetingWindow.xaml` (poll 2 s) |
| **LLM raw** (bypass dictation prompt for recap) | `llm.rs::process_raw_prompt` | `dimmy_llm_call_raw` | meeting recap auto-trigger |
| **Parakeet TDT v3 STT** + word timestamps | `parakeet.rs::transcribe_with_word_timestamps` | inside `dimmy_transcribe_file` | (Mac uses `parakeet_fluid.rs` via FluidAudio) |
| **App icons from .exe** — alpha-preserved 256×256 PNGs | (no Rust) | (no FFI) | `Helpers/IconExtractor.cs` (`IShellItemImageFactory` + `GetDIBits` ARGB) |
| **Pill / Taskbar / JumpList** | (no Rust) | (no FFI) | `Services/TaskbarService.cs`, `JumpListService.cs`, `CommandPipeServer.cs` |

Hardening status: see [`docs/dev/v2-test-hardening-plan.md`](docs/dev/v2-test-hardening-plan.md) — the new surfaces above shipped without coverage; that plan is the next-up TDD pass.

## Decision tree — where does this change go?

| You want to... | Go here |
|---|---|
| Add a cloud STT provider | `core/src/provider.rs` + routing in `transcribe.rs` |
| Add an LLM post-processing style | `core/src/llm.rs` + UI dropdown in each platform |
| Add a config field | `core/src/lib.rs` (struct) → `ffi.rs` getter/setter → each platform UI |
| Add an FFI entry | `core/src/ffi.rs` + `core/tests/v2_ffi.rs` (round-trip) + each platform's interop wrapper |
| Add an app-rule trigger | `core/src/app_rules.rs::resolve` + UI capture (Win: `Helpers/AppContextCapture.cs`, Mac: `NSWorkspace`) |
| Fix audio bug | Reproduce in `preprocess.rs` test FIRST, read `docs/dev/audio-pipeline.md`, then fix |
| Add a meeting feature | `core/src/meeting.rs` (worker + transcripts.txt) + UI window + LLM raw handoff |
| Touch the file-load pipeline | `dimmy_transcribe_file` in `ffi.rs` — rc table is contractual, don't renumber |
| Touch macOS FFI | Read `docs/dev/known-bugs.md` MACOS-001/002/003 first |
| Touch Windows CI / installer | Read `docs/dev/windows-ci.md` — 10 invariants, all paid in blood |
| Add a test | Read `docs/dev/testing.md` (tier definitions) + `docs/dev/v2-test-hardening-plan.md` (Phase 7+8 hardening) |
| Add a doc | `docs/dev/` (permanent) or `docs/superpowers/handoffs/` (time-bound). DO NOT link handoffs from `CLAUDE.md` — they decay; this file describes the codebase, not work-in-progress. |

## Critical invariants (beyond the philosophy)

### Audio pipeline
- **NEVER feed zero-amplitude samples to dagc (AGC).** It produces permanent NaN corruption. See `docs/dev/audio-pipeline.md` and `docs/dev/known-bugs.md` AUDIO-001.
- VAD grace period must NOT emit silence frames — only delay the `in_speech → false` transition.
- `process_buffer()` calls `process()` ONCE with all samples. The entire recording goes through a single VAD → AGC pass.
- Always NaN/Inf-check and clamp audio output.

### Config & keys — single-writer rule
- **Only the Rust core writes `config.json`.** UIs send updates via `dimmy_set_config_json()` and re-read. The C# / Swift / Rust-UI layers NEVER write the config file directly.
- API keys live in `~/.config/dimmy/keys.enc` (AES-256-GCM, machine-specific key derivation). **Never in `config.json`.** The `use_keyring` config field is forced to `false` — keyring is read-only fallback only.

### Windows CI
All 10 invariants live in [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md) — paid in blood, MUST be read before editing `.github/workflows/` or `platforms/windows/`. Run `/windows-ci-preflight` before pushing any workflow change.

### Versioning
- Update `core/Cargo.toml` → `version = "x.y.z"` for every release.
- Full runbook: [`docs/RELEASING.md`](docs/RELEASING.md).

### Telemetry — privacy hard rules
- **NEVER** include user content (transcribed text, prompt text, custom LLM prompt, microphone device name, file paths, hostname, username, IP) in any PostHog property or Sentry message. The `looks_like_secret` filter is a safety net, not a substitute for review.
- Provider names (groq/openai/anthropic/...) are categorical enums and OK to send.
- Counts, durations, error categories are OK to send.
- Adding a new event: add an `Event` variant in `core/src/telemetry/events.rs`, wire the emit, add a unit test, and update [`docs/dev/telemetry-implementation.md`](docs/dev/telemetry-implementation.md) + [`PRIVACY.md`](PRIVACY.md) if a new category of data is collected.

## Conventions

- **Branches:** `feat/<thing>` or `fix/<thing>`
- **Commit messages:** `feat:`, `fix:`, `chore:`, `docs:`, `test:`, `style:` — one concern per commit
- **Merge:** `--no-ff` into `staging` so history shows feature-branch shape
- **Release:** `main` is fast-forwarded from `staging` at release time
- **CLAUDE.md is committed** (this file). It IS the playbook. Keep it slim — link to `docs/` for detail.

## Executing actions with care (AI-specific)

Before running destructive or shared-state actions, check with the user:
- Force-pushing any branch (especially `main`)
- `git reset --hard`, overwriting uncommitted changes
- Deleting files or branches
- Pushing tags (they trigger release.yml)
- Any change to `.github/workflows/` — read `docs/dev/windows-ci.md` first, then ask

Local, reversible actions (editing files, running tests, committing to a feature branch) are fine without asking. When uncertain, transparently state what you'll do and confirm.

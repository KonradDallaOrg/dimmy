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

From `core/`:

```bash
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm
# tier-1 FFI integration (cross-platform, ~3 s once fixtures are cached):
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1
```

CI treats clippy warnings as errors. CI uses the same feature flags — mismatching will go green locally and red in CI. If you touched Linux UI, also `cd platforms/linux && cargo clippy -- -D warnings && cargo test`. If you touched Windows UI / onboarding / XAML, also run the FlaUI smoke tests (see [`docs/dev/testing.md`](docs/dev/testing.md)).

## Decision tree — where does this change go?

| You want to... | Go here |
|---|---|
| Add a cloud STT provider | `core/src/provider.rs` + routing in `transcribe.rs` |
| Add an LLM post-processing style | `core/src/llm.rs` + UI dropdown in each platform |
| Add a config field | `core/src/lib.rs` (struct) → `ffi.rs` getter/setter → each platform UI |
| Fix audio bug | Reproduce in `preprocess.rs` test FIRST, read `docs/dev/audio-pipeline.md`, then fix |
| Touch macOS FFI | Read `docs/dev/known-bugs.md` MACOS-001/002/003 first |
| Touch Windows CI / installer | Read `docs/dev/windows-ci.md` — 10 invariants, all paid in blood |
| Add a doc | Put it in `docs/dev/`. Don't duplicate content in `CLAUDE.md` or `README.md` — link. |

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
- `dimmy_lib.dll` linker version MUST be ≥ 14.50 (14.44 miscompiles `ggml-vulkan`). CI gate uses `dumpbin /headers`.
- VS 2026 BuildTools and VS 2022 are installed side-by-side. VS 2022 is needed for MrtCore PRI generation (`dotnet publish`).
- Velopack `vpk pack ... --framework vcredist143-x64` installs VC Redist to System32. Do NOT bundle `vcruntime140.dll` / `msvcp140.dll` in the publish folder.
- All 10 invariants: [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md).

### Versioning
- Update `core/Cargo.toml` → `version = "x.y.z"` for every release.
- Full runbook: [`docs/RELEASING.md`](docs/RELEASING.md).

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

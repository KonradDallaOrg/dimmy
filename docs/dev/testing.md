# Testing Dimmy

> How we test this codebase, what each layer catches, and how to run and extend it. Read this before opening a PR that touches the Rust core, the Windows/macOS UI, or the audio pipeline.

## The pyramid

```
                      ┌──────────────┐
                      │   tier 3     │   Full install → dictate
                      │  (deferred)  │   VB-CABLE + virtual audio, end-to-end
                   ┌──┴──────────────┴──┐
                   │      tier 2        │  UI automation
                   │                    │  FlaUI/UIA3 on Windows (see below)
                   │                    │  XCTest on macOS (Vassi, follow-up)
                ┌──┴────────────────────┴──┐
                │         tier 1           │ FFI integration
                │                          │ Pre-recorded PCM → FFI → assert
                │                          │ Cross-platform: Win / Mac / Linux
             ┌──┴──────────────────────────┴──┐
             │            unit                │ Rust + C# unit tests
             │                                │ cargo test --lib, dotnet test
             └────────────────────────────────┘
```

**Bugs that reached production and are now caught by tier 1**:
- v0.6.10 FFI ABI mismatch (installer crashed on first transcription)
- `set_single_segment(true)` → empty transcript on clips shorter than ~30 s
- Onboarding Cloud path writing a Groq STT key while pre-existing LLM config pointed to Anthropic with `llm_use_same_key=true` → silent 401 on every dictation

## Tier 1 — FFI integration (Rust)

**What it tests.** Pre-recorded PCM fed directly into the audio buffer via a test-only FFI (`dimmy_inject_pcm_for_test`, gated by `--features test-ffi`), then the normal `dimmy_stop_recording` runs the whole pipeline (preprocess → STT → filler → LLM). Cloud HTTP is mocked with `wiremock-rs`.

**Where.** `core/tests/ffi_e2e.rs` — 6 tests, cross-platform, all green in ~3 s.

**Fixtures** (downloaded on first run to `core/target/test-fixtures/`, gitignored, cached in CI):
- `jfk.wav` — [whisper.cpp/samples](https://github.com/ggerganov/whisper.cpp/raw/master/samples/jfk.wav) (MIT, 11 s English)
- `ggml-tiny.en-q8_0.bin` — [HuggingFace ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en-q8_0.bin) (MIT, ~40 MB)

**Run locally** (Windows — requires the project toolchain: LLVM, Ninja, Vulkan SDK, MSVC env):

```bash
cd core
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --nocapture --test-threads=1
```

`--release` is required on GHA windows-2025 (debug-mode whisper.cpp crashes with `STATUS_ILLEGAL_INSTRUCTION` on some runners). Locally you can drop `--release` if you have a capable CPU — debug compiles faster.

`--test-threads=1` is required because these tests share Dimmy's process-wide `GLOBAL_STATE`. The `serial_test` attribute already serializes them; the flag is belt-and-suspenders.

**Add a new test.** Pick a scenario that should be caught before the installer ships. Follow the existing pattern: `ensure_init()`, `set_config(...)` to a minimal JSON, `transcribe_pcm(samples, sr)`, assert on substring or behaviour. For cloud provider mismatches, mount a `wiremock::MockServer` per test.

**Do not ignore a failing test to unblock a push.** The test exists because the bug reached production before. If you genuinely believe the test is wrong, rewrite it; don't `#[ignore]` it.

## Tier 2 — Windows UI smoke (FlaUI)

**What it tests.** Launches the built `Dimmy.Windows.exe`, drives the onboarding wizard via UIA3, and asserts on the automation tree. 5 tests, ~41 s locally.

**Where.** `platforms/windows/Dimmy.Windows.UiTests/` — xUnit project, `net8.0-windows`, depends on `FlaUI.Core` + `FlaUI.UIA3`.

**Tool choice** (as of 2026-04):
- **FlaUI** — chosen. UIA3-native, actively maintained (v5.0.0 Feb 2025), C#-native (matches our stack).
- **WinAppDriver** — rejected. Last release 2020; protocol deprecated.
- **Appium Windows** — rejected. Still proxies to the abandoned WinAppDriver.exe.
- **Playwright desktop** — rejected. Electron/CDP only; no WinUI 3 support.

**Run locally**:

```bash
# 1. Build Dimmy.Windows first (needs the Rust DLL in core/target/release/)
cd core
cargo build --release --lib --features local-stt
cd ../platforms/windows/Dimmy.Windows
dotnet build Dimmy.Windows.csproj -c Release

# 2. Run UI tests
cd ../Dimmy.Windows.UiTests
dotnet test -c Debug --logger "console;verbosity=normal"
```

The tests kill any running Dimmy process before each run and delete `%APPDATA%\dimmy\.onboarding_done` to start from the fresh onboarding state. They do NOT complete onboarding (so the marker stays absent; your manual onboarding state is not affected).

**AutomationIds contract.** Tests find controls via `AutomationProperties.AutomationId`. When you add new UI that needs test coverage, add an ID in XAML:

```xml
<Button AutomationProperties.AutomationId="SomeUniqueId" ... />
```

Existing IDs (keep stable — renaming breaks tests):
- `OnboardingGetStartedButton`
- `OnboardingLocalCard` / `OnboardingCloudCard`
- `OnboardingGroqKeyBox`
- `OnboardingChoiceBackButton` / `OnboardingChoiceContinueButton`

**Headless note.** GHA `windows-2025` runners don't reliably render windows to the desktop. Tests that rely on mouse clicks at screen coordinates (e.g. clicking a `Border` which has no `InvokePattern`) may pass locally but fail in CI. Prefer UIA patterns (`AsButton().Invoke()`, `AsTextBox().Enter(...)`) over `element.Click()`.

## Tier 3 — Full install → dictate (deferred)

Not shipped. Would validate the whole stack including WASAPI capture via a virtual audio cable (VB-CABLE) and clipboard paste assertion. Tier 1 already covers most of what would go wrong; tier 3 only catches capture-layer regressions specific to real microphone devices. Revisit if a capture-specific bug slips past tier 1.

## CI workflows

**Production pipeline — do NOT touch** (see [`windows-ci.md`](windows-ci.md) for invariants):
- `ci.yml` — fast Rust lint/test + Linux GTK4 lint; runs on every push and PR
- `staging-native.yml` — full installer build on push to `staging`
- `release.yml` — tagged release builds
- `test-install.yml` — clean-Windows install test, triggered by release workflows

**Additive testing pipeline**:
- `e2e-tests.yml` — tier 1 FFI (matrix Win/Mac/Linux) + tier 2 Windows UI smoke. Triggers on `pull_request` and `workflow_dispatch`. Does not push to releases, does not overlap with production workflows.

**Fail-fast ordering**: `windows-ui-smoke` has `needs: ffi-integration`, so cheap FFI tests run first; UI job skips if tier 1 fails.

## Pre-push checklist

Before pushing any branch that touches Rust core, C# code, or XAML, run locally:

```bash
# Rust
cd core
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1

# C# (Windows)
cd ../platforms/windows/Dimmy.Windows.Tests
dotnet test
cd ../Dimmy.Windows.UiTests
dotnet test    # requires Dimmy.Windows + dimmy_lib.dll built first
```

A CI cycle is 10-20 minutes; this checklist runs in under 5 min locally and catches ~95 % of what CI would fail on.

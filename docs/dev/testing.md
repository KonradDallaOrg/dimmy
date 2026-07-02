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

**Bugs that reached production and are now caught by the test pyramid**:
- v0.6.10 FFI ABI mismatch (installer crashed on first transcription) — caught at PR time by tier 1.5 ABI snapshot + installer FFI smoke.
- `set_single_segment(true)` → empty transcript on clips shorter than ~30 s — caught by tier 1 short-clip test.
- Onboarding Cloud path writing a Groq STT key while pre-existing LLM config pointed to Anthropic with `llm_use_same_key=true` → silent 401 on every dictation — caught by tier 1 provider-mismatch test.
- AUDIO-001 dictation case (zero-amplitude samples → permanent NaN in dagc AGC) — caught by tier 1.5 preprocess proptest.
- AUDIO-001 file-load case (97 % of a 95-min WAV destroyed by AGC NaN) — caught by `preprocess::tests::file_load_long_silence_does_not_corrupt_subsequent_audio` + 7 sibling unit tests, plus the diagnostic-style `parakeet_long_file.rs`.
- AUDIO-003 AEC ref-ring starvation (Mix mode hung on systems with no active loopback) — caught by `aec::tests::worker_processes_mic_when_ref_ring_empty`.
- LLM-001 Anthropic Opus 4.7+ rejecting `thinking.type=enabled` + `budget_tokens` — caught by the 6 dispatch tests in `llm.rs::tests`.
- Meeting pause/resume idempotency + stop-while-paused deadlock — caught by `core/tests/meeting_pause_resume.rs` (4 integration tests).
- `dimmy_start_recording` rc=-7 contract drift between Rust and C# / Swift hosts — caught by `ffi::tests::start_recording_blocked_when_meeting_active`.

## Tier 1 — FFI integration (Rust)

**What it tests.** Pre-recorded PCM fed directly into the audio buffer via a test-only FFI (`dimmy_inject_pcm_for_test`, gated by `--features test-ffi`), then the normal `dimmy_stop_recording` runs the whole pipeline (preprocess → STT → filler → LLM). Cloud HTTP is mocked with `wiremock-rs`.

**Where.** `core/tests/ffi_e2e.rs` — 12 tests, cross-platform. Plus
`core/tests/meeting_pause_resume.rs` (4 tests, exercises the
`dimmy_meeting_pause/_resume/_is_paused` FFI contract + worker
behaviour while paused), `core/tests/audio_hardening.rs` (4 tests —
dictation route-aware preprocess + BUG A/B guardrails, see below) and
`core/tests/parakeet_long_file.rs` (1 diagnostic test on real long
WAVs, skips cleanly when fixture unavailable; survives in tree as a
regression early-warning for future preprocess changes).

**Audio hardening (`core/tests/audio_hardening.rs`)** — regression tests
for the two silent dictation bugs (known-bugs.md AUDIO-004): the LOCAL
Full route transcribes real speech through the make-it-worse guard, the
CLOUD route delivers a non-empty body to the provider, quiet attenuated
speech still transcribes locally, and a synthesized medium file
(jfk×4, ~44 s) transcribes via `dimmy_transcribe_file`. The route
*selection* itself is pinned deterministically by the `preprocess_route`
unit test; the capture-ratio guard is unit-tested in `telemetry::`
(it's inert in the injection harness — no real capture timing).

Coverage groups:
- **Local STT**: jfk sample, silent input, short clip (<30s, set_single_segment guard), long clip (>30s, segmentation guard), preprocess pipeline end-to-end.
- **Cloud STT**: mocked 200, mocked 401, provider mismatch (Groq STT key + Anthropic LLM URL).
- **Cloud LLM** (`dimmy_process_with_llm`): mocked 200 rewrite, mocked 401 → original text fallback, mocked 500 → original text fallback.
- **Config round-trip**: 30-field set/get verification — the contract every native UI relies on.

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

**Config-dir isolation (MANDATORY, enforced in code).** Test builds can NEVER touch the real `%APPDATA%/dimmy`: under `cfg(test)` or the `test-ffi` feature, `config_dir_path()` resolves to `DIMMY_TEST_CONFIG_DIR` (set per-process by each harness's `ensure_init()`), falls back to a per-process temp dir for unit tests, and `dimmy_init` REFUSES to run in `test-ffi` binaries when the env var is missing. Added 2026-07-02 after a local `cargo test` overwrote a live install's config.json (shortcut, LLM settings and device were clobbered by test fixtures). If you add a new integration test file, copy the `ensure_init()` from `ffi_e2e.rs` — the env-set lines are load-bearing. `meeting_pause_resume.rs` is gated on `test-ffi` for the same reason (it writes meeting dirs): run it with `--features test-ffi`.

**Add a new test.** Pick a scenario that should be caught before the installer ships. Follow the existing pattern: `ensure_init()`, `set_config(...)` to a minimal JSON, `transcribe_pcm(samples, sr)`, assert on substring or behaviour. For cloud provider mismatches, mount a `wiremock::MockServer` per test.

**Do not ignore a failing test to unblock a push.** The test exists because the bug reached production before. If you genuinely believe the test is wrong, rewrite it; don't `#[ignore]` it.

## Tier 1.5 — ABI snapshot + preprocess properties + installer FFI smoke

Three additional Rust integration suites that run alongside tier 1. They guard regression classes that the tier-1 happy-path tests don't surface.

### ABI snapshot (`core/tests/abi_snapshot.rs`)

Builds `dimmy_lib` as a cdylib and parses the resulting shared library cross-platform via the `object` crate (PE / ELF / Mach-O). Extracts the sorted set of `dimmy_*` exports and diffs against the golden file at `core/tests/fixtures/abi_exports.txt`. Any silent rename, drop, or accidental mangling fails the PR before the installer ships — the upstream guard for the v0.6.10 ABI-mismatch class.

```bash
# Run normally
cargo test --release --test abi_snapshot --features local-stt

# Intentionally update the golden after adding/removing an FFI export
UPDATE_ABI=1 cargo test --release --test abi_snapshot --features local-stt
```

**The fixture must be updated in the same PR as every UI** (C# / Swift / Rust GTK) that consumes the changed symbol. Do not regenerate without a corresponding consumer update.

### Preprocess properties (`core/tests/preprocess_properties.rs`)

`proptest`-driven property tests for `preprocess::process_buffer`. Three properties guard the audio invariants that have shipped as bugs in the past:
1. Arbitrary `Vec<f32>` in `[-2.0, 2.0]` → output finite, clamped to `[-1.0, 1.0]`.
2. All-zero input → no NaN/Inf in output (AUDIO-001 guard).
3. Near-rail sinusoidal signal → still clamped, AGC must not amplify past the rails.

Run for 16 / 44.1 / 48 kHz on every property. Fast (~1.5 s) — proptest defaults to ~256 cases per property.

```bash
cargo test --release --test preprocess_properties --features local-stt
```

### Installer FFI smoke (`core/src/bin/dimmy_smoke.rs`)

A standalone binary that runtime-loads `dimmy_lib.{dll,dylib,so}` via `libloading` and calls every shipping `dimmy_*` export with safe, side-effect-light arguments. Exits 0 if every call returns a sensible code without panicking. Catches the v0.6.10 ABI-mismatch class at PR time without needing a Setup.exe — runs against the freshly-built cdylib in CI.

Gated behind the `smoke-test` feature so default builds don't pull `libloading`.

```bash
# Build
cargo build --release --bin dimmy_smoke --features smoke-test

# Run (cwd must contain dimmy_lib.dll on Windows; LD_LIBRARY_PATH on Unix)
cd target/release && ./dimmy_smoke
```

Ships as a separate `ffi-smoke` job in `e2e-tests.yml`, matrix Win/Mac/Linux, `needs: ffi-integration` for fail-fast ordering.

The release-critical `test-install.yml` is intentionally not modified by this gate. A follow-up can extend `dimmy_smoke` to load the *installed* DLL after Setup.exe runs.

## Tier A — live model / LLM tests (manual, `#[ignore]`, NOT CI)

Real network + API keys (repo `.env`). The ONLY reliable way to check model behaviour — never gate CI on them.

- **`core/tests/live_models.rs`** — drives `llm::process_raw_prompt` against every cloud model in `assets/model-catalog.json`; catches dead/renamed model ids.
- **`core/tests/llm_flows.rs`** — the LLM flow matrix + catalog sweep (style · translate · command · security across providers, plus a per-model sweep). Dedicated guide: [`llm-flows-testing.md`](llm-flows-testing.md).

```bash
cargo test --test llm_flows --features local-llm -- --ignored --nocapture --skip flows_local_gguf
```

## Other Rust suites (run on plain `cargo test`)

- **`llm_request_shape.rs`** — asserts the exact JSON body (system prompt, temperature, thinking mode, headers) each provider receives.
- **`telemetry_coverage.rs`** — hygiene gate: every `Event` variant is emitted somewhere or explicitly reserved.
- **`v2_ffi.rs` / `v2_followups.rs`** — v2 FFI round-trip (app context, file transcribe, meeting-active, history hooks, raw LLM call) + retention / orphan recovery.
- **`meeting_pause_resume.rs`** — pause/resume idempotency + stop-while-paused.
- **`stress_tests.rs`** — offline stress (30 min / 1 h recordings, NaN/Inf audio, memory pressure), no API calls.
- **`parakeet_long_file.rs`** — diagnostic regression guard for `dimmy_transcribe_file` on >10 min WAVs (AUDIO-001 file-load case).

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
- `staging-auto-update.yml` — full installer build on push to `staging`
- `release.yml` — tagged release builds
- `test-install.yml` — clean-Windows install test, triggered by release workflows

**Additive testing pipeline**:
- `e2e-tests.yml` — tier 1 FFI (matrix Win/Mac/Linux) + tier 2 Windows UI smoke + cross-platform FFI smoke (matrix Win/Mac/Linux). Triggers on `pull_request` and `workflow_dispatch`. Does not push to releases, does not overlap with production workflows.

**Fail-fast ordering**: `windows-ui-smoke` and `ffi-smoke` both have `needs: ffi-integration`, so cheap FFI tests run first; UI / smoke jobs skip if tier 1 fails.

## Pre-push checklist

Before pushing any branch that touches Rust core, C# code, or XAML, run locally:

```bash
# Rust
cd core
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1
cargo test --release --test abi_snapshot --features local-stt
cargo test --release --test preprocess_properties --features local-stt
cargo test --release --test meeting_pause_resume --features local-stt,test-ffi -- --test-threads=1

# C# (Windows)
cd ../platforms/windows/Dimmy.Windows.Tests
dotnet test
cd ../Dimmy.Windows.UiTests
dotnet test    # requires Dimmy.Windows + dimmy_lib.dll built first

# Swift (macOS)
cd ../../macos
xcodebuild test -project Dimmy.xcodeproj -scheme Dimmy \
  -destination "platform=macOS"
```

A CI cycle is 10-20 minutes; this checklist runs in under 5 min locally and catches ~95 % of what CI would fail on. The Mac XCTest target is wired into `Dimmy.xcodeproj` (productType `bundle.unit-test`, ad-hoc signed); it currently runs structured-recap prompt + parser, recap-model picker round-trip, history-row formatting, AppState language/preset round-trip.

## Hardening pass for `feat/system-audio-capture`

Separate doc with the per-test rationale + manual-sweep checklist:
[`docs/dev/system-audio-capture-tests.md`](system-audio-capture-tests.md). Read it before relaxing or removing any of the new tests — each one corresponds to a real shipped bug or near-miss.

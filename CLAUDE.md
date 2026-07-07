# Dimmy — Claude playbook

> **You are an AI agent working on Dimmy.** This file is your load-bearing context. It is committed to the repo so any Claude Code session starts here.
>
> **Humans:** you want [`README.md`](README.md) (what Dimmy is) or [`CONTRIBUTING.md`](CONTRIBUTING.md) (how to hack on it).

## What Dimmy is (one paragraph)

Cross-platform voice-transcription overlay. Records audio via global hotkey, transcribes locally (whisper.cpp or Parakeet) or via cloud STT (Groq, OpenAI, Deepgram, Gemini, …), with optional realtime streaming dictation (Deepgram WebSocket). Optionally post-processes with an LLM — cloud via API key OR a **subscription** (Claude Code / Codex CLI), or local via llama.cpp — removes filler words, saves to history, and pastes into the focused app. Also does **command mode** (transform the selected text in place), **meeting mode** (long-form record → recap, with auto-detect + recording-consent), and integrates with **Claude Desktop (MCP)** and **Notion**. Shared Rust core (`core/`) + one native UI per OS: WinUI 3 on Windows, SwiftUI on macOS, GTK4 on Linux. Current version: see `core/Cargo.toml`.

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
| **Known-good baseline (v0.6.66) — as-built behavior + FREEZE invariants of the load-bearing features (meeting/system-audio, recap, shortcuts, API/providers, dictation)** | [`docs/dev/known-good-baseline.md`](docs/dev/known-good-baseline.md) |
| **Windows CI invariants — MUST read before editing any workflow** | [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md) |
| **Testing strategy — tiers, fixtures, how to run + extend** | [`docs/dev/testing.md`](docs/dev/testing.md) |
| Native UI status across platforms | [`docs/dev/native-ui-plan.md`](docs/dev/native-ui-plan.md) |
| Local LLM feasibility study | [`docs/dev/local-llm-feasibility.md`](docs/dev/local-llm-feasibility.md) |
| **Telemetry implementation (PostHog + Sentry)** | [`docs/dev/telemetry-implementation.md`](docs/dev/telemetry-implementation.md) |
| **Licensing v2 PoC — local server, Ed25519 tokens, 7 test scenarios** | [`docs/dev/licensing-poc.md`](docs/dev/licensing-poc.md) |
| **Licensing flow — state machine + sequence diagrams (ground truth)** | [`docs/dev/licensing-flow.md`](docs/dev/licensing-flow.md) |
| **Licensing prod — Cloudflare Worker + Stripe production setup** | [`docs/dev/licensing-prod.md`](docs/dev/licensing-prod.md) |
| **Staging tester guide — what to test in the side-by-side staging install** | [`docs/dev/staging-testing.md`](docs/dev/staging-testing.md) |
| **Claude Code subscription backend (LLM via local `claude` CLI)** | [`docs/dev/claude-code-backend.md`](docs/dev/claude-code-backend.md) |
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

### CUPID + anti-enshittification (house rule)
Filter every feature, refactor, and "let's also add X" through two screens BEFORE writing code:

- **Anti-enshittification (Doctorow):** does this serve the actual user, or is it surface-bloat that dilutes Dimmy's focus? If it's "nice to have" / "for completism" / "what if someone…", say so out loud and don't ship. Saying *"I don't think we should build this"* is a contribution.
- **CUPID (Dan North — joinable properties, not rules; see https://dannorth.net/cupid-for-joyful-coding/):**
  - **C**omposable — does it play with the existing pieces (Rust core, FFI, host UI) or need its own bespoke pipeline?
  - **U**nix-philosophy — one thing, well. A module / FFI entry / button that *also* does X and Y is a smell.
  - **P**redictable — behaviour matches the name + the user's mental model; no surprise side-effects.
  - **I**diomatic — fits the codebase (event callbacks, single-writer config, assertions over silent fallbacks). New code should look like the codebase, not like a different project bolted on.
  - **D**omain-based — Dimmy is a voice-overlay app. Anything that doesn't speak the domain (capture, transcription, recap, history, hotkey, meeting) needs strong justification.

In practice: default to the smaller option when a request is ambiguous (one card vs seven, one button vs a wizard, one field vs five). Refactors get the same filter — an "improvement" that adds layers without serving the user is still bloat. When existing code violates the filter, propose simplification (remove, merge, inline) rather than building on top.

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

### Mac pre-flight — MANDATORY when touching `platforms/macos/**` or Mac-facing FFI

`scripts/dev/preflight-mac.sh` is the single canonical pre-push for the
Mac path. It rebuilds the Rust static lib with the Mac frozen feature
set, runs `xcodebuild`, **and launches the .app for 5 s** so the
runtime `SelfTests.runAtLaunch` assertions fire before the DMG ships.

`xcodebuild` alone is **not enough** — release.yml only compiles, it
never launches. A stale `SelfTests` assertion (e.g. "Onboarding has 4
steps" after a new step lands; "LLM preset URL must be HTTPS" after
the synthetic `claude-code://` preset lands) compiles fine and ships a
DMG that **crashes on the user's first launch**. Burned 2026-05-13 —
v0.6.39-rc1 had to be deleted from GitHub Releases and recut.

Rule: if you change `OnboardingContainerView.totalSteps`, `LlmPreset`,
`SttPreset`, `MacLlmStyles`, `PillTranslateLanguages`, `Info.plist`
(esp. SUFeedURL), or any other thing `SelfTests` pins, **update
SelfTests in the same commit and run `preflight-mac.sh`**. The script
is the safety net; the rule is the actual fix.

### Windows local DLL build — feature flag set is FROZEN

**`cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan`** is the canonical local Windows build for `dimmy_lib.dll`. Dropping any of these features = silently breaks production code paths:
- `local-stt-vulkan` → whisper.cpp Vulkan STT (used by dictation when `stt_mode=local && local_stt_backend=whisper`, by meeting STT chunks, by file-load).
- `local-stt-parakeet` → Parakeet TDT v3 STT (used by dictation chunked-stt worker when `local_stt_backend=parakeet`, default for many users; ALSO referenced by meeting follow-ups in v2).
- `local-llm-vulkan` → llama.cpp Vulkan LLM (used for local recap, local rewrite, future meeting-recap-local path).

**NEVER** rebuild with a subset just because the diff "doesn't touch parakeet" or "the recap is cloud-only". The user's *runtime config* decides which path runs — drop the feature and the path becomes a silent trap (`local model: parakeet inference requires the local-stt-parakeet cargo feature` looped on every chunk while telemetry says `transcription.failed`). Burned 2026-05-07 twice (meeting empty transcript, then dictation empty transcript). The feature set was frozen after the second incident.

**RULE (user-explicit, 2026-05-07): If the user has not EXPLICITLY asked for a feature to be removed, KEEP IT. Always rebuild with the full set above. The user wants all features always — "le voglio tutte. sempre."** Removing a feature on your own initiative is a regression, not an optimization.

### Windows C# host build — output path depends on `-p:Platform=x64`

**`dotnet build platforms/windows/Dimmy.Windows/Dimmy.Windows.csproj -c Debug -p:Platform=x64`** is the canonical local C# build. **`-p:Platform=x64` is not optional.**

- Without it: output lands in `bin/Debug/net8.0-windows10.0.19041.0/win-x64/Dimmy.Windows.dll`
- With it (correct): output lands in `bin/x64/Debug/net8.0-windows10.0.19041.0/win-x64/Dimmy.Windows.dll`

The .exe Velopack installs / that you `Start-Process` on a dev box always lives under `bin/x64/Debug/...`. If you build to the default path you'll get green compile output but the .exe keeps loading the **previous** Dimmy.Windows.dll forever. Symptom is non-obvious: new code changes appear to do nothing at runtime, log lines you just added never appear, and you waste hours chasing a phantom regression. Burned 2026-05-21 during the Claude MCP wizard debugging — three rebuilds in a row went to the wrong path before catching the discrepancy via `ls` timestamp diff.

When the user reports "the code change didn't take effect", FIRST diagnostic is to `ls -la bin/x64/Debug/.../Dimmy.Windows.dll` and compare LastWriteTime to your source edits. If the DLL is older than your edits, the build is going to the wrong directory.

Also: `--no-incremental` is cheap insurance when chasing "did my change really get into the binary?" — it forces MSBuild to recompile rather than trust its caching layer.

## v2 surfaces — what shipped on `feat/v2-unified` + `feat/system-audio-capture` (2026-05)

v2 landed at v0.6.31 (2026-05); the tree is now **v0.6.63** (check `core/Cargo.toml`
for the live number) and a lot more shipped on top — see "Surfaces shipped since v2"
below. The two merges that defined the v2 base:

- `feat/v2-unified` (Apr–May 2026) — app rules, history v2, file load,
  meeting mode, Parakeet local STT, taskbar / jumplist / pill chrome.
- `feat/system-audio-capture` (May 2026, PR #45) — always-mix capture,
  meeting pause/resume, AEC3, recap-model override, pill ↔ meeting
  routing, MeetingWindow lifecycle decoupling, file-load preprocess fix.
- `staging-mac-v2-parity` (PR #46) — Mac port of the meeting / pause /
  recap-model surface to keep cross-platform parity.

A fresh Claude session needs to know these modules + FFI surfaces exist
before touching anything:

### Core / dictation surfaces (`feat/v2-unified`)

| Surface | Rust module | FFI entries | UI mirror |
|---|---|---|---|
| **App-context rules** — per-app LLM style override | `app_rules.rs` | `dimmy_set_app_context`, `dimmy_clear_app_context` | Win: `Helpers/AppContextCapture.cs` (HWND + focus drift), `ViewModels/AppRuleViewModel.cs`. Mac: `MacRulesPage.swift` |
| **History v2 schema** — enhanced text, audio path, app process, word timestamps, retention | `history.rs` (idempotent ALTER TABLE) | `dimmy_history_recent`, `_search`, `_update_enhanced`, `_update_audio`, `_update_word_timestamps` | Win: `ViewModels/HistoryItemViewModel.cs`, History detail panel in `SettingsWindow.xaml` |
| **File load** (drop / picker → transcribe, local OR cloud) | extends `ffi.rs` + uses `preprocess::process_buffer_for_file_load` (highpass-only, no AGC) | `dimmy_transcribe_file` (rc -1..-8) | Win: `Helpers/Win32FileDialog.cs`, `Helpers/Win32DropTarget.cs` (UIPI bypass), `Helpers/WavPeaks.cs`, ConfirmLargeFileAsync |
| **Meeting mode** — long-form record + LLM recap | `meeting.rs` (worker, streaming WAV, transcripts.txt, recovery marker) | `dimmy_meeting_start/_stop/_save_post_process/_list_orphans/_is_active` | Win: `Views/MeetingWindow.xaml` (lifecycle decoupled). Mac: `Views/Meeting/MeetingViewModel.swift` + 7 sub-views |
| **LLM raw** (bypass dictation prompt for recap) | `llm.rs::process_raw_prompt` (Anthropic adaptive thinking, Gemini Pro auto-think) | `dimmy_llm_call_raw` | Auto-fired by `MeetingPostProcessService` after stop |
| **Parakeet TDT v3 STT** + word timestamps | `parakeet.rs::transcribe_with_word_timestamps`, realtime via `chunked_stt.rs` | inside `dimmy_transcribe_file`, `dimmy_parakeet_warmup`, `_bundle_present`, `_download_bundle` | Win: bundled `onnxruntime.dll` + Parakeet picker. Mac: `parakeet_fluid.rs` via FluidAudio (Apple Neural Engine, 100–300× RTF) |
| **App icons from .exe** — alpha-preserved 256×256 PNGs | (no Rust) | (no FFI) | Win: `Helpers/IconExtractor.cs` (`IShellItemImageFactory` + `GetDIBits` ARGB) |
| **Pill / Taskbar / JumpList** | (no Rust) | (no FFI) | Win: `Services/TaskbarService.cs`, `JumpListService.cs`, `CommandPipeServer.cs`, `UiPreferences.cs` |

### Meeting / system-audio-capture surfaces (`feat/system-audio-capture`)

| Surface | Rust module | FFI entries | UI mirror |
|---|---|---|---|
| **Always-mix capture** — pill + meeting force `AudioSource::Mix`, AEC tolerant of empty ref | `audio.rs`, `aec.rs` (10 ms frames, 480 samples @ 48 kHz, capped rings) | (existing capture FFI) | C# `AudioSource` enum kept for backward-compat with old `config.json` |
| **Meeting pause/resume** — gap-skip semantics | `meeting.rs::MeetingSession::{pause,resume,is_paused}` | `dimmy_meeting_pause`, `dimmy_meeting_resume`, `dimmy_meeting_is_paused` (1=flipped, 0=no-op, -1=lock failure) | Win: `MeetingWindow.xaml.cs::Pause_Click` (E768 ↔ E769 glyph). Mac: pause button on meeting window |
| **Pill blocked when meeting active** | `dimmy_start_recording` returns -7 | C#/Swift hosts treat as silent no-op |
| **AEC3 acoustic echo cancellation** | `aec.rs` (WebRTC AEC3 via `aec3 = 0.2`) | (none — internal worker) | (none) |
| **DeepFilterNet noise suppression** — DEFERRED | `dfn.rs` (stub gated by `local-dfn` feature) | (none) | (none) |
| **Per-process loopback** — Phase 5a SCAFFOLDING (BT/HFP unblocker) | `process_loopback.rs` (Win-only, `spawn_process_capture` returns Err) | (none yet) | (none yet) |
| **Recap-model override** | (config field, picked by `process_raw_prompt`) | inside `dimmy_set_config_json` | Win + Mac: curated dropdown in Advanced settings |
| **Pill ↔ Meeting recap pipeline** | (no Rust) | (no FFI — uses existing meeting FFI) | Win: `Services/MeetingPostProcessService.cs`. Mac: `Services/MeetingPostProcessService.swift` (mirror) |

Hardening status: see [`docs/dev/system-audio-capture-tests.md`](docs/dev/system-audio-capture-tests.md) for the 40 net-new tests landed with PR #45 (23 Rust unit + 4 integration + 13 C# xUnit) and the manual-sweep checklist for what isn't automated. Earlier `feat/v2-unified` hardening plan: [`docs/dev/v2-test-hardening-plan.md`](docs/dev/v2-test-hardening-plan.md).

### Surfaces shipped since v2 (v0.6.32 → 0.6.63, May–Jun 2026)

A fresh session must know these exist too — all in the shared core, so each is automatically cross-platform (Win C# + Mac Swift mirror the same FFI).

| Surface | Rust module | FFI entries | Notes |
|---|---|---|---|
| **Command mode** — transform the SELECTED text in place (not paste-only); CASE-A instruction vs CASE-B replacement | `llm.rs::{build_command_transform_prompt,build_command_generate_prompt}` | `dimmy_command_transform` | Win: `Services/SelectionCaptureService.cs` + `UiaSelectionReader.cs` (UIA-first). Mac: AX selection read. Auth = same dispatch as recap (api_key / subscription) |
| **Claude Code subscription** LLM (Anthropic Pro/Team/Max via local `claude` CLI) | `claude_code.rs` | `dimmy_claude_code_status`/`_spawn_login`/`_ping`/`_recheck`/`_diagnostics`/`_binary_path`/`_node_status` | Synthetic `claude-code://` URL routes `process_text`+`process_raw_prompt` via `claude --print`. Dimmy never reads `~/.claude/credentials.json`. See [`docs/dev/claude-code-backend.md`](docs/dev/claude-code-backend.md) |
| **Codex subscription** LLM (OpenAI/ChatGPT via local `codex` CLI) | `codex.rs` | `dimmy_codex_status`/`_spawn_login`/`_ping`/`_recheck`/`_diagnostics`/`_binary_path` | Sibling of `claude_code.rs`; `codex exec -` with prompt on stdin. **On one tested ChatGPT account a sub served only its default model (`gpt-5.5`); other ids → HTTP 400 (tier-dependent, not generalized).** See [`docs/dev/codex-backend.md`](docs/dev/codex-backend.md) |
| **Claude Desktop MCP bridge** | `claude_desktop.rs` + `mcp-server/` (`dimmy-mcp` binary) | `dimmy_claude_desktop_status`/`_install`/`_uninstall` | Patches `claude_desktop_config.json`; the MCP server exposes 6 tools (incl. `dimmy_search` over meetings) |
| **Call / meeting auto-detect** + stop-suggestion | `call_detector.rs` (pure state machine) | `dimmy_call_signal`/`_signal_sys`/`_signal_session_ended`/`_signal_response`/`_set_tracked_origin`/`_meeting_started_external`/`_detector_state` | Host polls audio-session state → signals; core decides nudge/suppress. Win: `CallDetectionService.cs` + `CallNudgeWindow`. Mac: `CallDetectionManager` |
| **Recording consent** gate (GDPR / all-party) | `consent.rs` | `dimmy_consent_text`, `dimmy_consent_log_event` | Localized modal + announcement (6 langs) + append-only `consent.jsonl` audit. Host decides WHEN to show it |
| **Streaming dictation** — realtime typing as you speak | `deepgram_stream.rs` (cloud WS) + `chunked_stt.rs` (local) | (internal — `STREAMING` static in `ffi.rs`, `stt_chunk` events) | `streaming_dictation` config follows STT mode: cloud→Deepgram WS, local→chunked typing. Host injects delta at cursor |
| **Custom dictionary** — bias STT toward user terms | `user_dict` storage (in `lib.rs`/`ffi.rs`) | `dimmy_user_dict_add`/`_remove`/`_list_json` | Win + Mac Settings page + add-via-hotkey |
| **Model catalog** — single source of truth for cloud models | `catalog.rs` (`include_str!` `assets/model-catalog.json`) | `dimmy_model_catalog_json` | Win (System.Text.Json) + Mac (Codable) read the same compiled bytes → no per-OS model drift |
| **Resumable + integrity-checked model downloads** | `download.rs` | (backs `dimmy_download_model` / `dimmy_download_llm_model`) | Range/If-Range resume + SHA-256/magic verify; see modules.md |
| **Notion integration** — send recaps to a page/database | `notion.rs` | `dimmy_notion_has_token`/`_set_token`/`_test_connection`/`_search`/`_send_recap` | User's own internal integration token (AES keystore, `KeyringScope::NotionToken`) |
| **Obsidian / folder export** — write `recap.md` to a sync folder | (host-only, no Rust) | (none) | Win: `RecapExportService`. Mac: `tryExportRecap` |

## Decision tree — where does this change go?

| You want to... | Go here |
|---|---|
| Add a cloud STT provider | `core/src/provider.rs` + routing in `transcribe.rs` |
| Add an LLM post-processing style | `core/src/llm.rs` + UI dropdown in each platform |
| Add a config field | `core/src/lib.rs` (struct) → `ffi.rs` getter/setter → each platform UI |
| Add an FFI entry | `core/src/ffi.rs` + `core/tests/v2_ffi.rs` (round-trip) + each platform's interop wrapper |
| Add an app-rule trigger | `core/src/app_rules.rs::resolve` + UI capture (Win: `Helpers/AppContextCapture.cs`, Mac: `NSWorkspace`) |
| Fix audio bug | Reproduce in `preprocess.rs` test FIRST, read `docs/dev/audio-pipeline.md`, then fix |
| Add a meeting feature | `core/src/meeting.rs` (worker + transcripts.txt) + UI window + LLM raw handoff. Lifecycle is decoupled from UI: closing the window doesn't stop the recording — `MEETING` static lives in `ffi.rs` |
| Touch meeting pause/resume | `meeting.rs::MeetingSession::{pause,resume,is_paused}` + FFI rc contract (1 / 0 / -1). Worker excludes paused window from `audio.wav` and adds `[paused]` line in `transcripts.txt` |
| Touch the AEC / Mix-mode echo path | `core/src/aec.rs` — 10 ms frames @ 48 kHz, ref ring zero-padding when loopback empty. Reference signal comes from cpal loopback, capture from mic |
| Touch the file-load pipeline | `dimmy_transcribe_file` in `ffi.rs` — rc table is contractual, don't renumber. Uses `preprocess::process_buffer_for_file_load` (highpass-only). Do NOT call full `RawAudio::preprocess` — AGC NaN destroys long files |
| Touch the dictation preprocess route | `preprocess::preprocess_route` is the single source of truth (cloud→highpass, local→full-guarded, disabled→raw). Local runs `process_buffer_guarded` (make-it-worse fallback). Dictation captures Mic-only; meeting captures Mix. See AUDIO-004 + `docs/dev/audio-pipeline.md` (route-aware section). Regression tests: `core/tests/audio_hardening.rs`. NEVER route cloud through full VAD/dagc — it degrades quiet audio to "Ah!" |
| Pick a recap model | `core/src/llm.rs::process_raw_prompt` honours `recap_model_override` first, then URL heuristic. Anthropic Opus 4.7+ / Sonnet 5+ use `thinking.type=adaptive`; older Sonnets use `budget_tokens` |
| Add a Parakeet feature | `core/src/parakeet.rs` (Win/Linux ONNX path) + `parakeet_fluid.rs` (Mac ANE, gated by `local-stt-parakeet-fluid`). Realtime chunking lives in `chunked_stt.rs` |
| Touch a model download (LLM / whisper / parakeet) | `core/src/download.rs` is the shared helper: `download_resumable` (async, used by `local_llm` + `local_stt`) and `verify_file` (sync, used by `parakeet`). Resume = `Range`/`If-Range`; integrity = SHA-256 (from HF LFS ETag) + magic-byte; corrupt `.part` is deleted so the retry restarts clean. Don't reimplement per-model. `sha2` is non-optional. |
| Touch LLM dispatch / prompts (style·translate·command) | `core/src/llm.rs`: `process_text` (enhancement) + `process_raw_prompt` (command/recap); local mirrors in `local_llm.rs`. Translate uses `lang_name(code)` (NAME, not bare ISO). `strip_output_scaffolding` cleans cloud output (`[TRANSCRIPTION]`, `<think>…</think>`, ChatML). gpt-5/o-series → `max_completion_tokens.max(8192)` or output is EMPTY. Verify with `core/tests/llm_flows.rs` (see `docs/dev/llm-flows-testing.md`). |
| Resolve an LLM / recap API key | `ffi.rs` reads `KeyringScope::Llm(vendor)` then falls back to the SAME vendor's `Stt(vendor)` key (vendor derived from `llm_url`). One key per provider works for STT+LLM+command. The unified Providers card writes the key to every scope a vendor supports; its "Connected" badge counts EITHER scope. |
| Touch macOS FFI | Read `docs/dev/known-bugs.md` MACOS-001/002/003 first |
| Touch Windows CI / installer | Read `docs/dev/windows-ci.md` — 10 invariants, all paid in blood |
| Touch the recap auth dispatch | `core/src/ffi.rs::dimmy_llm_call_raw` — `recap_auth_method` is INDEPENDENT of `llm_auth_method`; defensive guard already falls back to `api_key` when `subscription` is requested with a non-Claude model. Win mirror in `SettingsWindow.xaml.cs::RefreshAuthIntegrationStatus`, Mac in `MacOutputPage.swift::recapSubscriptionActive`. |
| Save anything in C# Settings → ToJson | `SettingsViewModel.cs::ToJson` — **identity fields (api_url, api_model, llm_api_url, llm_api_model, local_model, selected_device) emit ONLY when non-empty** (`if-empty-omit` pattern). Adding a new identity-class field? Mirror that pattern, otherwise a transient empty VM will wipe the saved value via `dimmy_set_config_json`. Burned 2026-05-16 (config.json wiped after a recap_model_override save). |
| Derive %APPDATA% / config dir in host UI | Use `BuildInfo.ConfigDirName` (Win) / `appState.configDirURL` (Mac), which read from `dimmy_config_dir_name()` FFI. **NEVER** derive from `IsStaging` / build flavor — the two are decoupled. |
| Trigger a release / pre-release | See the **Release pipelines** table below. Wrong trigger = wrong licensing endpoint (Stripe Live vs Test). |
| Add a test | Read `docs/dev/testing.md` (tier definitions) + `docs/dev/v2-test-hardening-plan.md` (Phase 7+8 hardening) |
| Add a doc | `docs/dev/` (permanent) or `docs/superpowers/handoffs/` (time-bound). DO NOT link handoffs from `CLAUDE.md` — they decay; this file describes the codebase, not work-in-progress. |

## Critical invariants (beyond the philosophy)

### Audio pipeline
- **NEVER feed zero-amplitude samples to dagc (AGC).** It produces permanent NaN corruption. See `docs/dev/audio-pipeline.md` and `docs/dev/known-bugs.md` AUDIO-001.
- VAD grace period must NOT emit silence frames — only delay the `in_speech → false` transition.
- `process_buffer()` calls `process()` ONCE with all samples. The entire recording goes through a single VAD → AGC pass.
- Always NaN/Inf-check and clamp audio output.

### Windows icon assets — FROZEN, do NOT swap with brand-kit-latest
The shipping `dimmy.ico`, `dimmy-logo.png`, `dimmy-tray-{dark,light}-{idle,recording,transcribing,processing,completing,paused}.ico` (12 + idle alias) are **SOLID-fill silhouettes** sourced from the older brand-kit revision (`~/Pictures/dimmy-brand/windows/icon-1024-edge.png` + `icon-1024-edge-white.png`). The current brand-kit-latest ships `icon-1024.png` / `icon-1024-white.png` which are **outline-only thin renders** — they downscale to 1px strokes at the 16-24px taskbar / tray sizes and visually vanish.

**Rule:** never replace these assets with the `icon-1024.png` / `icon-1024-white.png` from any brand-kit refresh. If you must regenerate, run `scripts/dev/bake-win-tray-icons.py` (uses the solid sources via `DIMMY_TRAY_WHITE_SRC` + `DIMMY_APP_GRADIENT_SRC` env overrides). Burned 2026-05-30 twice — first bake used the thin outline source → user reported "icone PICCOLE si vedono di MERDA"; second pass used the solid edge source → fixed. The `ICONS.md` next to the assets must stay in sync.

### Config & keys — single-writer rule
- **Only the Rust core writes `config.json`.** UIs send updates via `dimmy_set_config_json()` and re-read. The C# / Swift / Rust-UI layers NEVER write the config file directly.
- API keys live in `~/.config/dimmy/keys.enc` (AES-256-GCM, machine-specific key derivation). **Never in `config.json`.** The `use_keyring` config field is forced to `false` — keyring is read-only fallback only.

### Windows CI
All 10 invariants live in [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md) — paid in blood, MUST be read before editing `.github/workflows/` or `platforms/windows/`. Run `/windows-ci-preflight` before pushing any workflow change.

**🚨 `cargo clean --release` was dropped from Win build steps 2026-05-14** (`staging-auto-update.yml`, `staging-tester.yml`, `release.yml`). The `Swatinem/rust-cache@v2` restore is now load-bearing — saves 20-25 min per Win build. Safety net: the `dumpbin /headers` linker-version gate right after `cargo build` aborts if the produced DLL was linked with `< 14.50` (catches stale-cache miscompiles). **If a Win release ever ships a broken DLL** (linker gate red, `test-install.yml` failing, user-reported crash on Win), **first diagnostic is to re-add `cargo clean --release`** in the build step and re-run the workflow. Then investigate why the rust-cache didn't invalidate via fingerprint. User has explicitly accepted this trade-off — don't silently revert.

### Release pipelines — which workflow does what

Three workflows publish artifacts; they look similar but speak to
**different licensing endpoints + Stripe accounts**. Picking the wrong
trigger ships a binary that bills real money instead of Test mode (or
vice versa). Memorise:

| Workflow | Trigger | flavor | `DIMMY_LICENSE_PUBKEY` | server URL | packId / Velopack channel | config dir | Stripe |
|---|---|---|---|---|---|---|---|
| **`staging-auto-update.yml`** | push to `staging` | `staging` | `avlM65...` hardcoded inline | **license-staging**.dimmy.app | `Dimmy-Staging` / channel `staging` (since 2026-06-16) | `dimmy-staging` (set via `DIMMY_CONFIG_NAMESPACE`) | Test |
| **`staging-tester.yml`** | tag `v*-staging*` (e.g. `v0.6.46-staging.1`) | `staging` | `avlM65...` hardcoded inline | **license-staging**.dimmy.app | `Dimmy-Staging` / channel `staging` | `dimmy-staging` (separate, set via `DIMMY_CONFIG_NAMESPACE`) | Test |
| **`release.yml`** | tag `v*` *not* matching `-staging` (e.g. `v0.6.46-rc1` or `v0.6.46`) | prod (empty) | `${{ secrets.DIMMY_LICENSE_PUBKEY }}` | **license**.dimmy.app | `Dimmy` / channel default (`prerelease` flag selects stable vs rc) | `dimmy` | **Live** |

**The packId is the structural firewall (since 2026-06-16).** Both staging workflows ship packId `Dimmy-Staging` + Velopack channel `staging`; `release.yml` ships packId `Dimmy`. The Win client (`UpdateService`) uses `GithubSource`, which filters releases by the installed packId AND channel — so a prod `Dimmy` install (stable or prerelease) can NEVER see a `Dimmy-Staging` build, and a `Dimmy-Staging` install (which forces `prerelease=true` because all its releases are GitHub-prereleases) can NEVER see a prod build. `staging-auto-update.yml` packs a monotonic `X.Y.Z-staging.<run_number>` so every staging push is a distinct version the staging install auto-updates to; `staging-tester.yml` is the signed, tag-pinned snapshot on the same channel. This replaced the old footgun where `staging-auto-update.yml` shipped packId `Dimmy` and leaked into the prod prerelease channel (burned 2026-06-16). **Mac/Sparkle staging auto-update is NOT yet on this model — staging DMGs are manual-download; aligning Sparkle is a follow-up.**

**🔐 SIGNED WINDOWS BUILDS — runner + SimplySign prerequisite (since 2026-06-24).** `staging-tester.yml` (and, once wired, `release.yml`) include a `sign-windows` job that runs on the **self-hosted runner `PC-KDALLA`** (label `dimmy-sign`, on Konrad's PC) to Authenticode-sign the Windows binaries with the **Certum cloud cert** (thumbprint `DD0AD19CFEB75B2D02A58363CA17CF0ED16BFDB4`, SHA256). The cloud key is non-exportable / has no headless login → **signing is local-by-design, NOT on GitHub-hosted runners.**

> **BEFORE pushing any signed Windows tag (`v*-staging.N`; later `v*-rcN` / `v*`): (1) the self-hosted runner must be ONLINE (`./run.cmd` → "Listening for Jobs"), and (2) SimplySign Desktop must be OPEN + LOGGED IN (session ~3h).** If not, the `sign-windows` job fails — but `build-windows` already uploaded UNSIGNED assets, so the release still works; just log into SimplySign and re-run the `sign-windows` job. Manual fallback: `scripts/dev/firma-release.ps1 -Tag <tag>` signs the downloaded installer post-hoc. Full setup in memory `reference_windows_code_signing_certum`.

Practical consequences:

- **A `v*-rcN` tag triggers `release.yml`, NOT staging.** Cutting `v0.6.46-rc1` builds against PROD endpoints. Velopack channel `prerelease` users see it; if anyone clicks "Buy plan" Stripe Live bills them. Recommended trial-only testing on rcN — `Start trial` and `/api/activate` magic-link redemption are free in both modes.
- **Staging is a separate Velopack track (`Dimmy-Staging` / channel `staging`), structurally invisible to the prod `Dimmy` prerelease channel.** `GithubSource` filters releases by the installed packId AND channel, so the isolation is by IDENTITY, not by withholding files. `staging-auto-update.yml` therefore uploads the full Velopack set (`Setup.exe` + `Portable.zip` + `Dimmy-Staging-*-full.nupkg` + `releases.staging.json`) to `staging-latest` — a staging install auto-updates across the staging channel with no user action. The OLD bug (burned 2026-06-16): `staging-auto-update.yml` shipped packId `Dimmy` (same as prod) + a manifest + a `vpk`-stripped clean `0.6.59` that OUTRANKED `0.6.59-rc1`, so a paying prod user on the prerelease channel auto-updated INTO a staging binary (license verifies against the staging pubkey → silent free/trial demotion; "Buy plan" → Stripe Test). **Invariant: packId `Dimmy` is prod-only (`release.yml`); every staging build is packId `Dimmy-Staging` + channel `staging`. NEVER ship a flavor=staging build under packId `Dimmy`.**
- **`v*-staging.N` is the tester path.** `staging-tester.yml` produces a side-by-side install (packId `Dimmy-Staging`) that coexists with a prod install on the same machine; talks to license-staging + Stripe Test so the full pay flow can be exercised without a real charge.
- **Flavor ≠ config dir.** Since 2026-05-16 the config dir is keyed off `DIMMY_CONFIG_NAMESPACE` (default `dimmy`), not `DIMMY_BUILD_FLAVOR`. A flavor=staging build that ships under the prod packId (i.e. `staging-auto-update.yml`) shares the prod `Roaming\dimmy\` so a *manually sideloaded* staging build reads the same data as the prod install. (This is NOT a license to auto-update prod users into it — see the prerelease-channel invariant above; staging-auto-update withholds the Velopack manifest precisely so that can't happen.) Only `staging-tester.yml` sets `DIMMY_CONFIG_NAMESPACE=dimmy-staging` (paired with packId `Dimmy-Staging`).
- **`license-client` cargo feature is mandatory** in every release pipeline. Without it the Rust core short-circuits `LicenseStatus::Unrestricted` regardless of the embedded pubkey, and the binary ships with the DEV badge + free everything. All three workflows pass `--features ...,license-client` explicitly; a fresh contributor `cargo build` (no env) still defaults to no `license-client` so it builds cleanly without `DIMMY_LICENSE_PUBKEY`.
- **C# `BuildInfo.ConfigDirName` reads from FFI**, not from the flavor. After PR #70 + the 2026-05-16 onboarding-restart fix, the host UI calls `dimmy_config_dir_name()` to learn which dir to use. **Never derive the dir from `IsStaging` in C# / Swift**.

Full runbook + recovery procedures: [`docs/RELEASING.md`](docs/RELEASING.md). When in doubt about which trigger you want, re-read this table.

### Versioning — MANDATORY VERSION CHECK BEFORE ANY RELEASE WORK

**STOP. Before you bump, tag, build a release, or even open a release PR — check what version is already out there.** `core/Cargo.toml` on a branch can be stale (rc1 of a version that's already been final-released, or a number behind because someone else shipped while the branch was open). Bumping based only on Cargo.toml content has caused at least one rollback (2026-05-13: bumped `0.6.37-rc1` → `0.6.37` while `v0.6.38-rc1` had already been tagged the night before; had to cancel the in-flight Staging Release and force-bump to `0.6.38` mid-pipeline).

**Pre-release checklist — run all three, every time:**

1. `gh release list --limit 10` — what's the most recent GitHub release/pre-release?
2. `git tag --sort=-version:refname | head -10` — what's the highest tag?
3. `cat core/Cargo.toml | head -5` — what does the source-of-truth say?

The next version is `max(github_releases, git_tags) + 1 patch`. If Cargo.toml is lower, **bump it FIRST**, separate commit, before doing anything release-shaped (PR merge to staging, tag push, release.yml trigger).

If you're about to bump and `gh release list` shows a higher version than what you were going to write, **stop and reconcile**. Don't ship a version number lower than what's published. Don't ship a duplicate. Don't reuse a -rc tag for a different commit.

- Update `core/Cargo.toml` → `version = "x.y.z"` for every release (separate commit, message starts with `chore(release): bump`).
- Full runbook: [`docs/RELEASING.md`](docs/RELEASING.md).

### No FFI-state polling rule

**Don't poll the Rust core to mirror state in the UI.** Use the event
callback channel (`dimmy_set_event_callback` ⇒ Win
`AppViewModel.HandleEvent`, Mac `DimmyCore.handleEvent`,
Linux `DimmyCore::on_event`). When you need the host UI to react to a
state change in the Rust core (meeting active, recording started,
chunk progress, …), have the Rust side `emit_event(...)` exactly once
per transition; subscribers update local state from the envelope.
Polling timers (`DispatcherTimer` / `NSTimer` / `Timer.scheduledTimer`
/ `tokio::time::interval`) that call `dimmy_*_is_*()` on a fixed
interval are forbidden — they waste CPU/battery, add perceived
latency, and pick arbitrary cadences ("500 ms" with no rationale)
that hide the real design question (which is "what events does the
core emit?").

**Documented exceptions** (legitimate timer use):

- Continuous sampling that has no "event" semantics — amplitude /
  waveform sampling at 12-30 fps for the live VU meter; recording
  clock tick at 1 Hz for the elapsed-time label. The data is a
  continuous stream, not a state transition.
- User-initiated UI animations — drag-scroll edge-of-list timer,
  popover fade, tooltip hover delay. The user is actively driving
  the timer; idle ⇒ no timer.
- OS APIs without notifications — macOS `AXIsProcessTrustedWithOptions`
  (TCC permissions) requires polling because Apple ships no
  notification API for trust-state transitions. Document the reason
  inline.

**To replace a polling timer with an event:**

1. Add a Rust emit: small helper like `emit_meeting_state_event(active, paused)` that wraps the call to `emit_event("name", "{...}")`.
2. Call it in every state-transition branch of the Rust handler. One emit per transition, exactly.
3. Add a `case "name":` in the host `HandleEvent` / `handleEvent` /
   `on_event` switch; map payload fields to observable view-model
   properties.
4. Replace the polling tick body with the same logic, hooked off
   `PropertyChanged` (Win) / `@Published` (Mac, Combine) / signal
   (Linux GTK4).
5. Add Rust unit tests verifying the emit happens (capture the
   callback into a test slot, assert the JSON envelope shape).

Pattern reference: meeting state polling on the pill, replaced in
PR #48 — see commits + tests for the canonical shape.

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

### Branching from staging — MANDATORY

When starting a new feature/fix branch, you MUST base it on the **latest** `origin/staging`, not on a stale local checkout. The standard sequence:

```bash
git fetch origin staging --force   # force-update origin/staging in case local was behind
git checkout -b feat/new-thing origin/staging
```

If the user asks you to start work without specifying a base, **ask explicitly**: "Branch this off latest `origin/staging`, or off another base (`main`, a specific tag, an existing feature branch)?". Don't guess — branching off the wrong base produces invisible regressions: features that landed on staging yesterday simply don't exist in the new branch, the user trips over the missing functionality hours later, and the rescue is a non-trivial merge with conflicts.

Burned 2026-05-10 on `feat/notion-integration`: branched off a local `origin/staging` that was 7 commits behind GitHub's `staging` (PR #47 had merged the night before — drag-reorder rewrite, event-driven meeting state, recap helpers, STT dedup). The user found out only when drag-reorder crashed in the Notion-integration build because the WinUI built-in drag was still in place. Recovery required committing in-flight Notion work + merging origin/staging mid-stream + resolving conflicts in `SettingsWindow.xaml.cs`, `MeetingWindow.xaml.cs`, `PillWindow.xaml.cs`.

If you're already on a branch that's diverged from staging, the recovery is the same: **merge origin/staging into your branch** (not rebase — preserves the feature-branch shape we already use for PRs). Cherry-picking is a fallback only when merge conflicts would be intractable.

## Executing actions with care (AI-specific)

Before running destructive or shared-state actions, check with the user:
- Force-pushing any branch (especially `main`)
- `git reset --hard`, overwriting uncommitted changes
- Deleting files or branches
- Pushing tags (they trigger release.yml)
- Any change to `.github/workflows/` — read `docs/dev/windows-ci.md` first, then ask

Local, reversible actions (editing files, running tests, committing to a feature branch) are fine without asking. When uncertain, transparently state what you'll do and confirm.

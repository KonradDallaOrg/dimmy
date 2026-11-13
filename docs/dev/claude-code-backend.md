# Claude Code subscription as an LLM backend

**Status, checked against the code on 2026-11-11:**

- The trigger is `auth_method == "subscription"` on the real Anthropic URL: `llm_auth_method` for the dictation rewrite and command mode, `recap_auth_method` for the recap (empty means `api_key`, it never inherits; `ffi.rs::dimmy_llm_call_raw`). The synthetic `claude-code://default` URL still routes in the core (`claude_code::is_claude_code_url`), but there is no "Claude-Code" preset any more: both hosts rewrite that URL to `https://api.anthropic.com/v1/messages` + `subscription` on load (`SettingsViewModel.cs`, `AppState.swift`).
- In `llm.rs` (`process_text`, `process_raw_prompt`) the Claude branch is the LAST subscription branch: `subscription` on a Gemini, OpenAI or Kimi URL goes to that vendor's CLI first. For a recap, `subscription` with a model that is not Claude and a URL that is not OpenAI or Kimi falls back to `api_key` (`dimmy_llm_call_raw`).
- One call is `claude_code::run_blocking(prompt, model, timeout, quick)`. When the CLI's `--help` lists `--include-partial-messages` it runs `claude --print --output-format stream-json --verbose --include-partial-messages` (`run_blocking_streaming`); otherwise `claude --print --output-format text`. Both add `LEAN_ARGS` when `--help` lists all of them, `--effort low` + `MAX_THINKING_TOKENS=0` when `quick`, and `--model <id>` when the model is not empty. The prompt is always on stdin.
- It streams: text deltas go out as `llm_stream` events (`start` / `delta` / `thinking` / `end`), the answer is the CLI's final `result` line.
- A dictation that will end in a Claude CLI rewrite prewarms the process at recording start (`ffi.rs::prewarm_claude_for_dictation`, called from `dimmy_start_recording`).
- Timeouts: 60 s rewrite (`process_text`), 600 s recap and command mode (`process_raw_prompt`), 15 s ping (`dimmy_claude_code_ping`), 30 s `dimmy-setup login test claude`. `cli_runner.rs` enforces the deadline and kills the whole process tree.

First shipped on `feat/anthropic-subscription-login` (2026-05-13). Win + Mac parity. Linux: subprocess path works (PATH walk) but no Settings card yet (GTK4 work pending).

## What this is

An authentication choice for the Anthropic provider (first shipped as a provider preset named **Claude-Code**) that routes LLM calls (style rewrite, command mode, meeting recap) through Anthropic's official `claude` CLI binary running on the user's machine, using the user's logged-in Pro / Team / Max subscription instead of consuming API-key credits.

There is **no STT change**. Anthropic doesn't ship an audio-transcription API; Dimmy continues to use Groq / OpenAI / Deepgram / Gemini / Whisper-local / Parakeet for speech-to-text exactly as before. The subscription path is LLM-only.

## Why subprocess and not OAuth

Anthropic does **not** publish an OAuth flow for third-party apps to act on behalf of a Claude subscription. The only sanctioned consumer of those credentials is the official `claude` CLI (a.k.a. Claude Code), which:

- Handles browser-based OAuth itself.
- Stores the token in `~/.claude/credentials.json` (or `.credentials.json` on some platforms), or on macOS in the login Keychain under service `Claude Code-credentials`. `claude_code::has_credentials` probes all three for existence only.
- Exposes a `--print` mode that takes a prompt on stdin and writes the response to stdout.

We piggyback on that. Dimmy never reads `~/.claude/credentials.json`, never sees the token, never participates in the OAuth dance. Every LLM call runs `claude --print --model <id>` as a subprocess, pipes the prompt to stdin, reads the answer from stdout (as `stream-json` when the CLI supports it; a dictation rewrite can use a process started at hotkey-down, see "Speed" below).

This is intentional and is a **privacy hard rule** (CLAUDE.md): touching the credentials file would expand Dimmy's threat model unnecessarily.

## Architecture

```
┌──────────────┐    config.json    ┌───────────────────┐
│   Settings   │ ────────────────▶ │ llm_auth_method = │
│   UI         │                   │  "subscription"   │
│              │                   │ (legacy: llm_api_ │
│              │                   │  url = "claude-   │
│              │                   │  code://default") │
└──────────────┘                   └─────────┬─────────┘
                                             │
                                             ▼ process_text / process_raw_prompt
                          ┌─────────────────────────────────────────┐
                          │ llm.rs                                   │
                          │   (Gemini, Codex, Kimi CLI checks first) │
                          │   if auth_method == "subscription"       │
                          │      || is_claude_code_url(api_url):     │
                          │     spawn_blocking(run_blocking)         │
                          │   else:                                  │
                          │     validate_url + HTTP request          │
                          └─────────────────────────────────────────┘
                                             │
                                             ▼
                          ┌─────────────────────────────────────────┐
                          │ claude_code.rs::run_blocking             │
                          │   detect_binary() → known install dirs, │
                          │     PATH, Windows registry PATH, then   │
                          │     the login shell (Mac/Linux)         │
                          │   has_credentials() → file existence,   │
                          │     or the macOS Keychain entry         │
                          │   Command::new(binary)                  │
                          │     --print                             │
                          │     --output-format stream-json         │
                          │       --verbose --include-partial-      │
                          │       messages   (or: text, old CLI)    │
                          │     LEAN_ARGS, quick args, --model      │
                          │   stdin ← prompt (never argv)           │
                          │   stdout → response (caller)            │
                          │   stderr → local log only               │
                          │   cli_runner: deadline, then kill the   │
                          │     process tree (60 s rewrite,         │
                          │      600 s recap / command)             │
                          └─────────────────────────────────────────┘
```

The URL scheme `claude-code://default` is **synthetic** — it never hits the network. The check in `llm.rs` short-circuits **before** `validate_url`, which would otherwise reject it as non-HTTPS. The hosts no longer write it; they set `llm_auth_method` and keep the Anthropic URL.

## Speed: what every call passes, and why (measured 2026-11-07)

A dictation rewrite on Haiku 4.5 took 8-12 s. Three causes, three fixes:

| Cause | Fix | Where |
|---|---|---|
| The CLI loaded the user's own setup (CLAUDE.md, plugins, hooks, MCP servers, tools) into every call: ~44k input tokens for one dictation against ~4k. It also saved each dictation in the user's `claude --resume` history. | `LEAN_ARGS`: `--tools "" --setting-sources "" --strict-mcp-config --disable-slash-commands --no-session-persistence`. **All or nothing**, gated on `--help`: `--tools ""` alone measured ~330k tokens. | `claude_code::lean_args` |
| Claude Code turns extended thinking on by default: 12 of 13 saved dictations carried a thinking block, ~7 s before the first word. | `run_blocking(.., quick = true)` for dictation rewrites and pings: `MAX_THINKING_TOKENS=0` (turns thinking off where the model allows it, e.g. Haiku 4.5) and `--effort low` when `--help` lists it (the 5.5 models always reason adaptively; low effort trims it). Recap and command mode pass `false` and keep thinking. | `QUICK_ENV`, `quick_args` |
| ~0.85 s of CLI startup and ~0.75 s of CLI shutdown after the answer was already out. | **Prewarm**: `dimmy_start_recording` starts `claude --input-format stream-json` when the dictation will end in a Claude CLI rewrite (`should_prewarm`); the rewrite hands it the prompt as one user message. One process per dictation, so no context carries over; a stale one (other model, >10 min, exited) is retired by closing its stdin, never killed (it may be writing `~/.claude.json`). **Early return**: the answer goes back on the `result` line, the CLI exits on a reaper thread. | `prewarm`, `take_warm`, `run_blocking_streaming` |

Result on real dictations (measured 2026-11-07, Haiku 4.5): 8-12 s → **~1.4 s** (model ~1.2 s). Codex and Gemini CLI are untouched: neither prewarm nor `quick` applies to them yet. (Kimi has a `quick` flag of its own and no prewarm, see `kimi-backend.md`.)

Each streaming call logs one line with `warm_age_ms`, `first_line_ms`, `first_text_ms`, `result_ms`, `answered_ms`, the CLI's own `cli_ms` / `api_ms`, `tokens_in` and the 5-hour quota utilisation. In stream-json mode a refusal (usage limit, auth) comes only in the result line, stderr stays empty: it is logged as `[ClaudeCode] error result: …` (200 chars).

## The 3 states the UI surfaces

| FFI return | `ClaudeCodeStatus` | UI label (Mac card)                        | Sign-in button (Mac card) |
|------------|--------------------|--------------------------------------------|--------------------|
| 0          | `Ready`            | "✓ Logged in. Using `claude` at `<path>`." | "Re-sign in"       |
| 1          | `NotLoggedIn`      | "Claude Code installed but not logged in." | "Sign in via browser" |
| 2          | `NotInstalled`     | "Claude Code CLI not detected."            | "Install Claude Code first" (disabled) |

The Windows card uses its own wording for the same three states ("Connected — using `<path>`…", "…installed but not signed in…", "…not detected. Click Set up wizard…") and offers the `ClaudeConnectDialog` wizard instead of a disabled button.

The sign-in button spawns `claude /login`:

- **Win:** `cmd /c start "" claude /login` — new console window, detached.
- **Mac:** `crate::run_in_new_terminal_window("<path> /login", "claude-login")` — writes `<config_dir>/installers/claude-login.command` and opens it with `/usr/bin/open`, so Terminal shows a new window with the URL. No AppleScript (the earlier `osascript` route failed without the Automation permission).
- **Linux:** plain background spawn (too many terminal emulators to dispatch).

After spawn, the UI polls `dimmy_claude_code_status()` every 2 s for 3 min. Once it flips to `Ready`, the polling loop emits `claude_code.login_completed { outcome: "success" }` and stops.

## Failure modes + the error_category mapping

`ClaudeCodeError` maps to categorical PostHog labels via `claude_code::error_category()`:

| Rust variant       | Telemetry category | Cause                                               |
|--------------------|--------------------|-----------------------------------------------------|
| `NotInstalled`     | `not_installed`    | binary missing from all candidate paths             |
| `NotLoggedIn`      | `not_logged_in`    | no non-empty credentials file and (macOS) no Keychain entry |
| `Spawn(String)`    | `spawn`            | exec failure (permissions, broken binary, no shell) |
| `Timeout`          | `timeout`          | model didn't return within 60 s (rewrite) / 600 s (recap) / 15 s (ping) |
| `NonZeroExit{...}` | `exit_nonzero`     | CLI returned non-zero — rate-limit, auth expired, …  |
| `InvalidUtf8`      | `invalid_utf8`     | stdout wasn't UTF-8 (defensive — should never fire) |

**Security:** `ClaudeCodeError::Display` deliberately strips the inner error text from `Spawn(_)` and the `stderr_excerpt` from `NonZeroExit`. Both could contain transcript fragments echoed back by the model. The full detail lands in `dimmy.log` (local file the user controls), never in telemetry or Sentry.

## PostHog events

| Event name                       | Properties                                                          | Fired from                          |
|----------------------------------|---------------------------------------------------------------------|-------------------------------------|
| `claude_code.status_probed`      | `status: "ready"\|"not_logged_in"\|"not_installed"`                 | `dimmy_claude_code_status()` (FFI)  |
| `claude_code.login_spawned`      | (none)                                                              | `claude_code::spawn_login()` on success |
| `claude_code.login_completed`    | `outcome: "success"\|"timeout"\|"spawn_failed"`                     | Host polling loop (Win/Mac)         |
| `claude_code.invocation`         | `kind: "rewrite"\|"recap"\|"test"`, `processing_ms_bucket`, `success: bool`, `error_category` | `llm::process_text`, `process_raw_prompt`, `dimmy_claude_code_ping()` |

All properties are categorical / bucketed — never the prompt text, model output, binary path, stderr, hostname, or path fragments.

`processing_ms_bucket` uses the shared `sanitize::bucket_processing_ms` helper: `lt_500 | 500_2000 | 2000_10000 | 10000_60000 | ge_60000`.

## Sentry

Inherits the global aggressive scrubbing from `telemetry/sentry_pipeline.rs::redact_prose`:

- `LlmError::Display` never includes the HTTP body (or, here, the CLI stderr).
- `enable_logs: false` so the Rust `log` crate output doesn't surface in Sentry events.
- The allow-list prefix check in `redact_prose` does NOT cover any string starting with model output, so even if a panic message somehow embedded one, it would be replaced with `"<redacted: prose content>"`.

## Settings (was: a preset)

The first version exposed a provider preset (`"Claude-Code"`, `claude-code://default`, `claude-opus-4-7`). That preset no longer exists on either host. Today:

- Win: the user keeps an Anthropic model and turns on "Use Anthropic subscription" (`LlmUseSubscriptionCard`) or "Use Anthropic subscription for recap" (`RecapUseSubscriptionCard`). Both are shown only when the provider or recap model is Anthropic and the CLI is ready (`SettingsWindow.xaml.cs::RefreshAuthIntegrationStatus`). The status card and the sign-in button are the Anthropic card on the Integrations page (`AnthropicIntegration*`), with a setup wizard in `ClaudeConnectDialog`.
- Mac: an Authentication picker (`api_key` / `subscription`) in `MacOutputPage.swift` sets `appState.llmAuthMethod`. `MacClaudeCodeCard` is on `MacIntegrationsPage.swift`. `SelfTests` asserts every LLM preset URL is HTTPS.
- Both hosts migrate a saved `claude-code://` URL to the Anthropic URL + `llm_auth_method = "subscription"` when they load the config.

## Test Connection button

Calls `dimmy_claude_code_ping()` which runs `claude --print` (`run_blocking`, no `--model`, `quick = true`) with a fixed `"reply with the single word: pong"` prompt (so the user's transcripts can never end up on the wire here). 15 s timeout. Returns elapsed_ms on success or one of the 6 error categories. Emits `claude_code.invocation { kind: "test" }` so we can see how often users actually exercise the test path.

## Config round-trip safety

`claude-code://default` is preserved verbatim through `dimmy_set_config_json` → in-memory state → on-disk `config.json` → next-launch load. Locked in by `config_round_trip_preserves_claude_code_url` in `core/tests/v2_ffi.rs`. The validation that rejects non-HTTPS LLM URLs (`Provider::validate_url`) is only invoked **after** the `is_claude_code_url` short-circuit in both `process_text` and `process_raw_prompt`, so the core never rewrites or rejects the synthetic scheme. The hosts do rewrite it, on purpose, when they load the config (see "Settings" above).

## What doesn't work / out of scope

- **Model choice (this one DOES work).** On a live Max account (2026-06-20) `--model` accepted `opus` / `sonnet` / `haiku` and full ids like `claude-opus-4-8` (only `fable` is a special-access program). Contrast: on one tested ChatGPT account, Codex accepted only its account-default model — see [`codex-backend.md`](codex-backend.md).
- **STT via subscription.** Not possible — Anthropic offers no audio API.
- **Streaming (this one DOES work now).** With a CLI that lists `--include-partial-messages`, `run_blocking_streaming` reads `stream-json` and emits `llm_stream` events (`start`, `delta`, `thinking`, `end`), which both hosts render. The returned answer is still the CLI's final `result` line. The HTTP path streams too in `process_raw_prompt` (recap, command mode), for Anthropic and OpenAI-compatible endpoints (`llm.rs::read_anthropic_sse`, `read_openai_sse`); Gemini-native and the HTTP dictation rewrite (`process_text`) do not. An older CLI falls back to the synchronous `--output-format text` call.
- **System-prompt isolation.** `claude --print` doesn't distinguish system vs user prompts; we glue them into a single prompt with a `---` separator. The model treats the leading block as instructions in practice.
- **Token usage metering.** No way to surface "you've used N tokens this month" — the CLI doesn't expose Anthropic billing in a stable form.
- **Multi-account.** Whichever account is currently logged into the local CLI is the one Dimmy uses. Switching accounts is `claude logout && claude /login` from a terminal.

## Where to look in code

| Concern                          | File                                                         |
|----------------------------------|--------------------------------------------------------------|
| Subprocess + status enum         | `core/src/claude_code.rs`                                    |
| Process run, deadline, tree kill | `core/src/cli_runner.rs`                                     |
| LLM dispatch short-circuit       | `core/src/llm.rs` (`process_text`, `process_raw_prompt`)     |
| FFI exports                      | `core/src/ffi.rs` (`dimmy_claude_code_status / _binary_path / _spawn_login / _ping / _recheck / _diagnostics / _node_status`), prewarm in `prewarm_claude_for_dictation` |
| Win UI                           | `platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml{.cs}`, `ClaudeConnectDialog.xaml{.cs}` |
| Mac UI                           | `platforms/macos/Dimmy/Views/Settings/MacClaudeCodeCard.swift` (on `MacIntegrationsPage.swift`) + `MacOutputPage.swift` (Authentication picker) |
| Win interop                      | `platforms/windows/Dimmy.Windows/Interop/DimmyNative.cs`     |
| Mac interop                      | `platforms/macos/Dimmy/Managers/DimmyCore.swift`             |
| FFI C header                     | `platforms/macos/Dimmy/DimmyFFI.h`                           |
| Event taxonomy                   | `core/src/telemetry/events.rs` (4 `ClaudeCode*` variants)    |
| Tests                            | `core/src/claude_code.rs::tests`, `core/tests/v2_ffi.rs::config_round_trip_preserves_claude_code_url` |

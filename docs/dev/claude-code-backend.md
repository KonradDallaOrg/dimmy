# Claude Code subscription as an LLM backend

**Status:** shipped on `feat/anthropic-subscription-login` (2026-05-13). Win + Mac parity. Linux: subprocess path works (PATH walk) but no Settings card yet (GTK4 work pending).

## What this is

A new LLM provider preset — **Claude-Code** — that routes every LLM call (style rewrite, meeting recap) through Anthropic's official `claude` CLI binary running on the user's machine, using the user's logged-in Pro / Team / Max subscription instead of consuming API-key credits.

There is **no STT change**. Anthropic doesn't ship an audio-transcription API; Dimmy continues to use Groq / OpenAI / Deepgram / Gemini / Whisper-local / Parakeet for speech-to-text exactly as before. The subscription path is LLM-only.

## Why subprocess and not OAuth

Anthropic does **not** publish an OAuth flow for third-party apps to act on behalf of a Claude subscription. The only sanctioned consumer of those credentials is the official `claude` CLI (a.k.a. Claude Code), which:

- Handles browser-based OAuth itself.
- Stores the token in `~/.claude/credentials.json` (or `.credentials.json` on some platforms — both checked).
- Exposes a `--print` mode that takes a prompt on stdin and writes the response to stdout.

We piggyback on that. Dimmy never reads `~/.claude/credentials.json`, never sees the token, never participates in the OAuth dance. Every LLM call spawns `claude --print --model <id>` as a subprocess, pipes the prompt to stdin, reads the answer from stdout.

This is intentional and is a **privacy hard rule** (CLAUDE.md): touching the credentials file would expand Dimmy's threat model unnecessarily.

## Architecture

```
┌──────────────┐    config.json    ┌───────────────────┐
│   Settings   │ ────────────────▶ │  llm_api_url =    │
│   UI         │                   │  "claude-code://  │
│              │                   │   default"        │
└──────────────┘                   └─────────┬─────────┘
                                             │
                                             ▼ process_text / process_raw_prompt
                          ┌─────────────────────────────────────────┐
                          │ llm.rs                                   │
                          │   if is_claude_code_url(api_url):        │
                          │     spawn_blocking(run_blocking)         │
                          │   else:                                  │
                          │     validate_url + HTTP request          │
                          └─────────────────────────────────────────┘
                                             │
                                             ▼
                          ┌─────────────────────────────────────────┐
                          │ claude_code.rs::run_blocking             │
                          │   detect_binary() → PATH walk + common  │
                          │     install dirs (~/.claude/local,      │
                          │     /opt/homebrew/bin, %LOCALAPPDATA%)  │
                          │   has_credentials() → file existence    │
                          │   Command::new(binary)                  │
                          │     .arg("--print")                     │
                          │     .arg("--output-format text")        │
                          │     .arg("--model").arg(model)          │
                          │   stdin ← prompt (never argv)           │
                          │   stdout → response (caller)            │
                          │   stderr → local log only               │
                          │   poll try_wait every 100 ms            │
                          │   kill if past timeout (60 s rewrite,   │
                          │      600 s recap)                       │
                          └─────────────────────────────────────────┘
```

The URL scheme `claude-code://default` is **synthetic** — it never hits the network. The check in `llm.rs` short-circuits **before** `validate_url`, which would otherwise reject it as non-HTTPS.

## The 3 states the UI surfaces

| FFI return | `ClaudeCodeStatus` | UI label                                   | Sign-in button     |
|------------|--------------------|--------------------------------------------|--------------------|
| 0          | `Ready`            | "✓ Logged in. Using claude at `<path>`."   | "Re-sign in"       |
| 1          | `NotLoggedIn`      | "Claude Code installed but not logged in." | "Sign in via browser" |
| 2          | `NotInstalled`     | "Claude Code CLI not detected."            | "Install first" (disabled) |

The sign-in button spawns `claude /login`:

- **Win:** `cmd /c start "" claude /login` — new console window, detached.
- **Mac:** `osascript -e 'tell Terminal to do script "<path> /login"'` — new Terminal window so the user sees the URL.
- **Linux:** plain background spawn (too many terminal emulators to dispatch).

After spawn, the UI polls `dimmy_claude_code_status()` every 2 s for 3 min. Once it flips to `Ready`, the polling loop emits `claude_code.login_completed { outcome: "success" }` and stops.

## Failure modes + the error_category mapping

`ClaudeCodeError` maps to categorical PostHog labels via `claude_code::error_category()`:

| Rust variant       | Telemetry category | Cause                                               |
|--------------------|--------------------|-----------------------------------------------------|
| `NotInstalled`     | `not_installed`    | binary missing from all candidate paths             |
| `NotLoggedIn`      | `not_logged_in`    | credentials.json missing or empty                   |
| `Spawn(io::Error)` | `spawn`            | exec failure (permissions, broken binary, no shell) |
| `Timeout`          | `timeout`          | model didn't return within 60 s (rewrite) / 600 s (recap) / 15 s (ping) |
| `NonZeroExit{...}` | `exit_nonzero`     | CLI returned non-zero — rate-limit, auth expired, …  |
| `InvalidUtf8`      | `invalid_utf8`     | stdout wasn't UTF-8 (defensive — should never fire) |

**Security:** `ClaudeCodeError::Display` deliberately strips the inner `io::Error` text from `Spawn(_)` and the `stderr_excerpt` from `NonZeroExit`. Both could contain transcript fragments echoed back by the model. The full detail lands in `dimmy.log` (local file the user controls), never in telemetry or Sentry.

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

## Settings preset

- Win: `SettingsViewModel.cs` exposes `("Claude-Code", "claude-code://default", "claude-opus-4-7")` in the provider dropdown. Picking it makes `ClaudeCodeStatusCard` visible in the Output page.
- Mac: `AppState.swift` exposes `LlmPreset(id: "claude-code", apiUrl: "claude-code://default", model: "claude-opus-4-7")`. `MacOutputPage` injects `MacClaudeCodeCard` when `appState.llmApiUrl.hasPrefix("claude-code://")`.

The card lives next to the API-key card. Selecting Claude-Code hides the API-key card (no key needed) and shows the status card instead.

## Test Connection button

Calls `dimmy_claude_code_ping()` which spawns `claude --print` with a fixed `"reply with the single word: pong"` prompt (so the user's transcripts can never end up on the wire here). 15 s timeout. Returns elapsed_ms on success or one of the 6 error categories. Emits `claude_code.invocation { kind: "test" }` so we can see how often users actually exercise the test path.

## Config round-trip safety

`claude-code://default` is preserved verbatim through `dimmy_set_config_json` → in-memory state → on-disk `config.json` → next-launch load. Locked in by `config_round_trip_preserves_claude_code_url` in `core/tests/v2_ffi.rs`. The validation that rejects non-HTTPS LLM URLs (`Provider::validate_url`) is only invoked **after** the `is_claude_code_url` short-circuit in both `process_text` and `process_raw_prompt`, so the synthetic scheme can't be rewritten or rejected.

## What doesn't work / out of scope

- **Model choice (this one DOES work).** A Claude subscription serves the whole family — `opus` / `sonnet` / `haiku` and full ids like `claude-opus-4-8` all work via `--model` (verified against a live Max account, 2026-06-20; only `fable` is a special-access program). This is the opposite of the Codex/ChatGPT subscription, which serves only its account-default model — see [`codex-backend.md`](codex-backend.md).
- **STT via subscription.** Not possible — Anthropic offers no audio API.
- **Streaming.** `claude --print` is synchronous request/response. The HTTP path supports the same (no streaming UI yet).
- **System-prompt isolation.** `claude --print` doesn't distinguish system vs user prompts; we glue them into a single prompt with a `---` separator. The model treats the leading block as instructions in practice.
- **Token usage metering.** No way to surface "you've used N tokens this month" — the CLI doesn't expose Anthropic billing in a stable form.
- **Multi-account.** Whichever account is currently logged into the local CLI is the one Dimmy uses. Switching accounts is `claude logout && claude /login` from a terminal.

## Where to look in code

| Concern                          | File                                                         |
|----------------------------------|--------------------------------------------------------------|
| Subprocess + status enum         | `core/src/claude_code.rs`                                    |
| LLM dispatch short-circuit       | `core/src/llm.rs` (`process_text`, `process_raw_prompt`)     |
| FFI exports                      | `core/src/ffi.rs` (`dimmy_claude_code_status / _binary_path / _spawn_login / _ping`) |
| Win UI                           | `platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml{.cs}` |
| Mac UI                           | `platforms/macos/Dimmy/Views/Settings/MacClaudeCodeCard.swift` + `MacOutputPage.swift` |
| Win interop                      | `platforms/windows/Dimmy.Windows/Interop/DimmyNative.cs`     |
| Mac interop                      | `platforms/macos/Dimmy/Managers/DimmyCore.swift`             |
| FFI C header                     | `platforms/macos/Dimmy/DimmyFFI.h`                           |
| Event taxonomy                   | `core/src/telemetry/events.rs` (4 `ClaudeCode*` variants)    |
| Tests                            | `core/src/claude_code.rs::tests`, `core/tests/v2_ffi.rs::config_round_trip_preserves_claude_code_url` |

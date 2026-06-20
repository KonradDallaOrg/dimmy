# Codex (ChatGPT subscription) LLM backend

Sibling of [`claude-code-backend.md`](claude-code-backend.md). When the
Output -> LLM (or Recap) "Use ChatGPT subscription" toggle is on and the
provider is OpenAI, Dimmy routes `process_text` / `process_raw_prompt`
through the local `codex` CLI (`codex exec -`) instead of the OpenAI HTTP
API. Auth is whatever the local `codex` CLI is logged into; Dimmy never
reads OpenAI credentials.

Code lives in `core/src/codex.rs`; dispatch short-circuit in
`core/src/llm.rs` (the `codex_openai_sub` branch in both `process_text`
and `process_raw_prompt`).

## Model entitlement — the subscription gotcha

**Observed on ONE live ChatGPT account (not generalized):** that account's
Codex accepted only its account-default model (`gpt-5.5`); every other id
was rejected with HTTP 400, including the OpenAI *API* model ids the
Output picker offers (gpt-5-mini, gpt-4o, ...).

```
ERROR 400 invalid_request_error:
"The 'gpt-5-mini' model is not supported when using Codex with a ChatGPT account."
```

> Scope caveat: this was a single account (tier unknown). A higher Codex
> tier (Pro / Team / Enterprise) MAY expose more models — untested. Do
> not treat the single-model limit as universal; treat it as "the picked
> model can be rejected by the subscription, so a rejected model must
> fail gracefully / fall back to the account default."

Verified empirically against that account (codex v0.140.0, 2026-06-20):

| `codex exec -m <model>`      | result |
|------------------------------|--------|
| *(omitted)*                  | OK — resolves to account default `gpt-5.5` |
| `gpt-5.5`                    | OK |
| `gpt-5`, `gpt-5.1`, `gpt-5-high` | rejected (400, "not supported with a ChatGPT account") |
| `gpt-5-codex`, `gpt-5.5-codex`, `gpt-5-codex-high` | rejected (400) |
| `o3`                         | rejected (400) |
| `gpt-5-mini`                 | rejected (400) |

So with a ChatGPT subscription the usable set is effectively just the
account default (`gpt-5.5` today). The other model names require an
OpenAI **API key** (pay-per-token), not the subscription. The CLI syntax
to pick a model (`-m` / `--model`) is correct and unchanged; the limit is
the subscription **entitlement**, not the flag.

### Why command mode can fail with "HTTP 1" / rc -1

If the user has an OpenAI API model (e.g. gpt-5-mini) selected in the
Output picker AND flips the subscription toggle, the `codex_openai_sub`
branch forwards that model to `codex exec -m gpt-5-mini`, codex returns
400, the CLI exits 1, and Dimmy surfaces "cloud transform failed: HTTP 1".
The audio / selection / prompt are all fine — it is purely the model id
the subscription won't serve. Picking nothing model-specific (so codex
uses its default) is the working path.

## Contrast: Claude Code subscription is NOT restricted this way

The Anthropic side (`claude_code.rs`, `claude --print --model <m>`) lets a
subscription pick across the family. Verified same day against a live Max
account:

| `claude --model <m>` | result |
|----------------------|--------|
| `opus`               | OK |
| `sonnet`             | OK |
| `haiku`              | OK |
| `claude-opus-4-8` (full id) | OK |
| `fable`              | unavailable — special-access program, not a tier limit |

So the recap-with-Opus-4.8-subscription path works as picked; only Codex
has the single-model entitlement quirk above.

## Where to look in code

| Concern                    | File |
|----------------------------|------|
| Subprocess + status enum   | `core/src/codex.rs` |
| LLM dispatch short-circuit | `core/src/llm.rs` (`process_text`, `process_raw_prompt`, `codex_openai_sub`) |
| FFI exports                | `core/src/ffi.rs` (`dimmy_codex_status / _binary_path / _spawn_login / _ping / _recheck / _diagnostics`) |
| Win UI                     | `platforms/windows/Dimmy.Windows/Views/SettingsWindow.xaml{.cs}` |
| Mac UI                     | `platforms/macos/Dimmy/Views/Settings/` + `MacOutputPage.swift` |
| Event taxonomy             | `core/src/telemetry/events.rs` (`Codex*` variants) |

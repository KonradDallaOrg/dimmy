# Kimi integration: feasibility and plan (2026-11-09)

Status: **plan only, nothing built.** Branch `feat/kimi-subscription`, cut from
`origin/staging` @ `0d456207`. Everything below was checked against
Moonshot's own docs on 2026-11-09. Items marked **SPIKE** still need a run
on a real Kimi account before any code is written.

## What the user asked

Use Kimi the way Dimmy already uses Claude Code and Codex: the user's own
Kimi subscription for dictation rewrite (LLM), command mode and meeting
recap, plus a Kimi Desktop integration like the Claude Desktop one. Also look
at Qwen and GLM.

## What Kimi offers (official sources)

| Surface | Facts that matter to Dimmy |
|---|---|
| **Kimi Code CLI** (`kimi`, package `@moonshot-ai/kimi-code`) | `kimi -p` runs one prompt, no TUI, answer on stdout, thinking + tool progress on stderr. Print mode also reads the prompt from **stdin**. `--output-format text\|stream-json`. `-m <alias>`. `kimi login` = device-code OAuth, exit 0/1. Data root `~/.kimi-code` (`KIMI_CODE_HOME`), OAuth creds in `credentials/<name>.json` (0600). `kimi acp` = Agent Client Protocol over stdio (JSON-RPC). Install: `irm https://code.kimi.com/kimi-code/install.ps1 \| iex` / `curl … install.sh \| bash` / npm. **Windows needs Git for Windows** (it uses Git Bash). |
| Agent restriction | `--agent-file <md>` with frontmatter `tools: []` = **no tools at all**. Print mode otherwise runs tools under the `auto` policy, so without this a dictated sentence could run a command. |
| Env knobs | `KIMI_MODEL_THINKING_EFFORT` (low..max), `KIMI_CODE_NO_AUTO_UPDATE`, `KIMI_DISABLE_TELEMETRY`, `KIMI_LOOP_MAX_STEPS_PER_TURN`. |
| **Kimi Code Desktop** (Win + Mac, 1.0 on 2026-09-17, now 1.3.2) | Same engine as the CLI. **Reads the same `~/.kimi-code/mcp.json`** (official Desktop "Settings and Extensions" page); new servers apply to new sessions. |
| **Kimi Code membership** | Included from Plus (new plans) / Andante (old). Rolling 5 h window + monthly pool, shared with Kimi chat and across all devices and keys. Model ids depend on tier (`k3`, `k3-256k`, `kimi-for-coding`, `kimi-for-coding-highspeed`): same trap as Codex, so by default we do not pass `-m`. |
| Kimi Code API key | Up to 5 keys per member, for third-party tools. OpenAI-compatible `https://api.kimi.ai/coding/v1` (CN `api.kimi.com`), Anthropic-compatible `…/coding/`. **Honest User-Agent required**: changing it can suspend the membership. |
| **Kimi Platform** (pay per token) | Normal API key, OpenAI-compatible. Enterprise and commercial use belongs here. |

### The rule that shapes the plan

Kimi Code Community Guidelines: *"Kimi Code subscriptions are for personal
interactive use only"*, and "Don't use Kimi Code for non-interactive
automation" (examples: "scripted batch execution or data annotation
pipelines"). Enforcement: they cap concurrent requests first.

How Dimmy's flows read against that rule:

| Flow | Who triggers it | Verdict |
|---|---|---|
| Dictation rewrite | the user, one hotkey press, their own text | OK. One person, one action. |
| Command mode | the user, on their own selection | OK |
| Meeting recap after stop | the user stops their own meeting | OK, one call per meeting |
| Telegram inbox recap | the user shares one voice note | Borderline but per-item and user-initiated. Allowed, never in bulk. |
| Re-recap of orphans, retranscribe-all, any backfill loop | Dimmy, in bulk | **Never on the subscription.** Fall back to API key, or ask. |
| MCP bridge (Kimi Desktop/CLI calls Dimmy tools) | Kimi's own client | No quota of ours used. Clean. |

So the subscription path is feasible. It needs one structural guard: the
dispatcher must know whether a call is user-initiated (see Phase 2, item 6).

## Qwen and GLM: verdict

| | Qwen | GLM (Z.ai) |
|---|---|---|
| Own CLI | Qwen Code (`qwen -p`, `-o json`), works headless | none (ZCode IDE). Users run Claude Code pointed at Z.ai |
| Subscription for a third-party app | Free Qwen OAuth **ended 2026-04-15**. Alibaba Coding Plan (`sk-sp-` keys): "only for use in AI programming tools and OpenClaw-type agents", workflow and automation platforms excluded | GLM Coding Plan: "limited to the officially supported tools"; Dimmy is not on the list |
| Already reachable in Dimmy | Qwen3.x via Together, Fireworks, Groq (catalog) | GLM-5.2 via Together, Fireworks (catalog) |

**Recommendation: don't build either.** A subscription integration would break
their plan terms, and the user's account would pay for it. This is the
Gemini/Antigravity lesson again (`gemini_cli.rs` header). Pay-per-token
access already works through the catalog. A direct DashScope or Z.ai
provider adds nothing until someone asks for it.

## Plan

### Phase 0: SPIKE on a real Kimi account (~1 h, needs the user's login)

Run these on this PC (and MacinCloud for the Mac path). Write the results here.

1. `echo <prompt> | kimi -p --output-format stream-json`: which line holds the answer, the error and the usage? Do quota or auth refusals go to stdout or stderr? Exit codes?
2. A ~120 KB recap prompt on stdin on Windows (npm `.cmd` shim and the native install). No argv limit hit?
3. `--agent-file dimmy-agent.md` (`tools: []`) together with `-p`: accepted? Dictate "list the files in this folder" and confirm no tool runs.
4. Without `-m`: which model answers? Which `-m` ids does this tier accept? (Codex lesson.)
5. Latency of a 50-word rewrite, cold, with `KIMI_MODEL_THINKING_EFFORT=low` and without. Compare with Claude's 1.4 s.
6. Does `-p` save a session under `~/.kimi-code`? Is it keyed by cwd? (Privacy: transcripts would sit in the user's Kimi history.)
7. Which file means "logged in" (existence check only, we never read it)? Does `kimi login` work from a console we spawn?
8. Kimi Desktop: add a `dimmy` entry to `~/.kimi-code/mcp.json`, open a new session, check that the 6 Dimmy tools appear and `dimmy_search` works.
9. Only if 5 is slow: is `kimi acp` usable as a prewarmed process (the Claude prewarm pattern)?

Stop and report if 3 fails (no way to switch tools off) or 6 shows sessions we can't keep out of the user's history.

### Phase 1: Moonshot as an HTTP provider (small, no ToS questions)

Covers Kimi Platform keys and Kimi Code keys. Both are OpenAI-compatible, so
this reuses the existing HTTP path.

1. `provider.rs`: `Provider::Moonshot`, matching `api.moonshot.ai`/`.cn`, `api.kimi.ai`, `api.kimi.com`. Tag `moonshot`, `is_moonshot()`, `from_tag`, `KeyringScope` follow from the enum.
2. `assets/model-catalog.json`: `moonshot` provider with Platform models plus a "Kimi Code (membership key)" entry on `…/coding/v1`. Win and Mac pickers read the catalog, so no per-OS model list.
3. `User-Agent: Dimmy/<version>` on these requests (required by Kimi, and honest anyway).
4. Unified Providers card: logo + key field (Win `SettingsWindow`, Mac `MacOutputPage`).
5. Tests: `from_url` cases, catalog lookup, `config_round_trip` for the URL, `llm_flows` test against a mock.

### Phase 2: Kimi Code CLI as a subscription backend (the Claude/Codex mirror)

Fourth sibling: `core/src/kimi_cli.rs`, built on `cli_runner.rs`, with the same shape as `codex.rs`.

1. `detect_binary` (PATH, `~/.kimi-code/bin`, npm global, `%LOCALAPPDATA%`), `has_credentials` (existence only), `status()` → Ready / NotLoggedIn / NotInstalled (+ NoGitBash on Windows if the spike shows that is a separate failure).
2. `run_blocking(prompt, model, timeout, quick)`: prompt on **stdin**, `-p` + `--output-format stream-json`, `--agent-file` pointing at a bundled `tools: []` agent, neutral temp cwd, env `KIMI_CODE_NO_AUTO_UPDATE=1`, `KIMI_MODEL_THINKING_EFFORT=low` when `quick`. No `-m` unless the user picked one the spike shows the tier serves.
3. `spawn_login`: visible console running `kimi login` (Win `cmd /c start`, Mac Terminal via osascript, Linux plain spawn).
4. `llm.rs`: branch in `process_text` and `process_raw_prompt`, triggered by `kimi-cli://default` OR `auth_method == "subscription"` + `Provider::Moonshot`. It goes before the Claude branch, like the Codex one.
5. `ffi.rs`: `dimmy_kimi_status/_binary_path/_spawn_login/_ping/_recheck/_diagnostics`; the recap guard in `dimmy_llm_call_raw` learns Moonshot; `abi_exports.txt`, `DimmyFFI.h`, `DimmyNative.cs`, `DimmyCore+Kimi.swift`.
6. **Interactive-only guard**: a `CallOrigin { User, Background }` passed down to `process_raw_prompt`. On `Background` + Kimi subscription, fall back to the API key, or skip with a clear message. One small enum, also useful for the Claude path.
7. Telemetry: `KimiCli{StatusProbed,LoginSpawned,LoginCompleted,Invocation}`, categorical only. Update `telemetry-implementation.md` + `PRIVACY.md`.
8. UI: "Use Kimi subscription" toggle when the provider is Moonshot, plus a connect dialog. Win `KimiConnectDialog` + `SettingsWindow.KimiCli.cs`; Mac `MacKimiCliCard` + `KimiConnectSheet` + SelfTests, then `preflight-mac.sh`; pbxproj checked with `check-pbxproj.py`.
9. `docs/dev/kimi-backend.md` (sibling of `codex-backend.md`) with the spike table.

### Phase 3: Kimi Desktop / CLI MCP bridge (the Claude Desktop mirror)

One file serves both Kimi clients: `~/.kimi-code/mcp.json` (`mcpServers.dimmy = {command: <dimmy-mcp>, args: []}`).

1. `core/src/kimi_desktop.rs`: detect Desktop/CLI, patch `mcp.json` (merge, never rewrite other entries, keep a backup, same as `claude_desktop.rs`), uninstall, status.
2. FFI `dimmy_kimi_mcp_status/_install/_uninstall`; a card in Integrations on Win + Mac.
3. The `dimmy-mcp` binary needs no change if the spike confirms Kimi speaks plain stdio MCP.

### Order and size

Phase 0 → 1 → 2 → 3. Phase 1 is about a day. Phase 2 is roughly the size of
the Gemini CLI change (2.3k lines, Win only) plus the Mac port. Phase 3 is
~1 day. Each phase is its own PR to `staging`.

## Open questions for the user

- Phase 2 on by default for Moonshot users, or behind a switch like `gemini_cli_enabled`? The "personal interactive use" wording argues for a visible opt-in with one line of explanation.
- Ask Kimi support in writing whether a dictation app driving `kimi -p` on the user's own action counts as interactive use? Their guidelines invite exactly that question.

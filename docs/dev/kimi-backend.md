# Kimi Code (Kimi membership) LLM backend

Fourth sibling of [`claude-code-backend.md`](claude-code-backend.md) and
[`codex-backend.md`](codex-backend.md). Kimi sits in the LLM step only
(dictation rewrite, command mode, meeting recap). It does not do
speech-to-text. Plan and feasibility (and why Qwen and GLM were not built):
[`kimi-integration-plan.md`](kimi-integration-plan.md).

## Two ways in, one provider

`Provider::Kimi` (tag `kimi`) is the Kimi Code membership.

| Auth | Route | Trigger |
|---|---|---|
| API key (the membership's up-to-5 keys) | HTTP, OpenAI-compatible `https://api.kimi.ai/coding/v1/chat/completions` (CN `api.kimi.com`) | `llm_api_url` / recap model on the Kimi URL, `auth_method = "api_key"` |
| Subscription | the local `kimi` CLI over ACP | `auth_method = "subscription"` + Kimi URL, or the synthetic `kimi-cli://default` |

Both LLM reqwest clients send an honest `User-Agent: Dimmy/<version>`. Kimi
suspends members whose clients fake their identity.

Moonshot pay-per-token Platform keys are left out on purpose: Kimi K3 is
already reachable through Together and Fireworks.

## Transport: `kimi acp`, not `kimi -p`

Read from the MIT source of `@moonshot-ai/kimi-code` 2.5.2 (2026-11-09).
`-p` takes the prompt from argv only, which caps a recap at 8191 characters
through an npm `.cmd` shim. ACP (ndjson JSON-RPC on stdio) gives:

- the prompt on stdin, any length;
- the tier's real model list (`configOptions.model`). A picked model the
  tier does not offer is dropped and the CLI default answers, so the Codex
  "HTTP 1" trap cannot happen here;
- `thinking` off for quick rewrites (recap and command mode keep it);
- `session/delete` after each call, so dictations do not pile up in the
  user's Kimi history;
- every `session/request_permission` answered `cancelled`, no fs or
  terminal capability, neutral temp cwd: no tool runs;
- a prompt that starts with `/` is a slash command in ACP, so `guard_slash`
  makes sure ours never does.

Sequence: `initialize` → `session/new {cwd, mcpServers: []}` (`-32000` =
auth required) → `session/set_config_option` (model, thinking, mode) →
`session/prompt` (text in `session/update` `agent_message_chunk`;
`stopReason` end_turn / refusal / cancelled / max_tokens) →
`session/delete`.

Known cost not measured yet: the user's global `~/.kimi-code/AGENTS.md`
and their own MCP servers still load in our sessions (ACP has no switch).

## Install and sign-in (the wizard does it)

The user installs nothing by hand. Windows `KimiConnectDialog`, Mac
`KimiConnectSheet`:

1. **Windows only: Git for Windows.** The CLI runs its shell through Git
   Bash (`locateWindowsGitBash`: `KIMI_SHELL_PATH`, next to `git.exe` on
   PATH, `Program Files\Git`, `%LOCALAPPDATA%\Programs\Git`, mirrored in
   `kimi_cli::git_bash_present`). One button runs
   `winget install --id Git.Git -e --source winget` in a visible terminal.
2. **The CLI.** One button runs `dimmy_kimi_install_command` (PowerShell
   `irm https://code.kimi.ai/kimi-code/install.ps1 | iex`, or
   `curl -fsSL https://code.kimi.ai/kimi-code/install.sh | bash`, the global channel: `code.kimi.com` writes region `mainland-cn` and the sign-in then fails for a kimi.ai account) in a
   visible terminal. It lands in `~/.kimi-code/bin`, which detection checks
   directly, so no PATH refresh is needed.
3. **Sign in.** `kimi login` (device code) in a visible terminal. Dimmy
   checks that `~/.kimi-code/credentials/*.json` exists (never reads it).
4. **Test.** `dimmy_kimi_ping` + `dimmy_kimi_models_json`.

The wizard does not switch the rewrite or the recap to Kimi by itself. The
user picks a Kimi Code model and turns on "Use Kimi subscription" (Windows)
or Authentication → Subscription (Mac).

## Terms: personal interactive use only

Kimi Code Community Guidelines: subscriptions are "for personal interactive
use only", no "scripted batch execution". Every Dimmy call is one user
action: one dictation, one command, one recap after one meeting. As of this
writing no code path calls the LLM over many items in a loop (checked:
every `dimmy_llm_call_raw` call site on both hosts, every
`process_raw_prompt` caller in the core). **If you add a bulk path (re-recap
all, backfill), it must not use the Kimi subscription.** The plan's
`CallOrigin` guard was not built because nothing needs it yet.

## FFI

| Entry | Contract |
|---|---|
| `dimmy_kimi_status` | 0 ready, 1 installed-not-signed-in, 2 not installed |
| `dimmy_kimi_recheck` | drop the cached lookup, then status |
| `dimmy_kimi_binary_path` | length, 0 not installed, -1 bad buffer |
| `dimmy_kimi_diagnostics` | JSON: candidate paths, credentials / Git Bash presence |
| `dimmy_kimi_install_command` | the vendor's install line for this OS |
| `dimmy_kimi_git_bash_present` | 1 present (always 1 off Windows) |
| `dimmy_kimi_spawn_login` | 0 spawned, -1 not installed, -2 spawn failure |
| `dimmy_kimi_ping` | ms > 0; -1 not installed, -2 not signed in, -3 spawn, -4 timeout, -5 protocol, -6 reported (usage limit), -7 refused, -8 empty |
| `dimmy_kimi_models_json` | `[{"id","name"}]`; -3 not installed / signed in, -4 CLI failed. Blocks up to 30 s |

| `dimmy_kimi_mcp_status` | JSON `{kimi_found, registered, binary_exists, config_path}` |
| `dimmy_kimi_mcp_install(binary)` | 0 done; -1 no home, -2 binary missing, -3 `mcp.json` not valid JSON (left untouched), -4 write failed |
| `dimmy_kimi_mcp_uninstall` | 1 removed, 0 nothing to remove, negative on error |

Host event `kimi_cli.login_completed {outcome}` goes through
`dimmy_telemetry_track_typed`.

## MCP bridge: Kimi reads Dimmy (Phase 3)

`core/src/kimi_mcp.rs`. Kimi Code CLI and Kimi Code Desktop read the same
`~/.kimi-code/mcp.json`. The toggle "Let Kimi Code read your meetings" on
the Kimi card (both hosts, shown when `~/.kimi-code` exists) writes one key,
`mcpServers.dimmy` (`dimmy-<ns>` for staging), pointing at the shipped
`dimmy-mcp` (Win: next to `Dimmy.exe`; Mac: `Contents/Resources`). Rules:
every other entry is kept; a file that is not a JSON object is refused,
never overwritten; the first change to an existing file keeps a copy as
`mcp.json.dimmy-backup`; the write is tmp + rename.

Unlike the Claude Desktop extension, the binary is referenced in place,
not copied. Velopack's `current\` folder and `/Applications/Dimmy.app` are
stable across updates.

Side effect: Kimi loads user MCP servers in every session, including the
ones Dimmy opens for the subscription, so with the bridge on each Kimi
call also starts `dimmy-mcp`. Its tools are never approved there.

### Kimi Code Desktop wizard and "Open in Kimi Code"

Integrations has a **Kimi Code Desktop** card (Win + Mac) with a 3-step
wizard, the sibling of the Claude Desktop one: (1) install Kimi Code
Desktop (Windows: one button, `winget install --id
MoonshotAI.KimiCodeDesktop`; both: link to kimi.com/code), (2) connect
dimmy-mcp in `mcp.json`, (3) open a new Kimi session and wait for the
heartbeat. Our `mcp.json` entry sets `DIMMY_MCP_CLIENT=kimi`, so dimmy-mcp
writes `mcp.heartbeat.kimi` and a running Claude Desktop (plain
`mcp.heartbeat`) cannot fake the check. Detection: Windows looks for a
"Kimi Code" folder (not the Kimi app beside it) with a "kimi" .exe under `%LOCALAPPDATA%\Programs` or
`%ProgramFiles%`; macOS for `*Kimi Code*.app` in `/Applications` or
`~/Applications`.

The meeting toolbar shows an official-Kimi-mark button when Kimi Code
Desktop is installed. Kimi Code's only desktop deep link is
`kimi-code://open?root=<dir>&new=1` (verified in the 2.5.2 binary; no
prompt pre-fill), so the button opens a new chat in the meeting's folder
and puts the recap prompt on the clipboard: the MCP prompt when Dimmy is
connected, a `transcripts.txt` prompt otherwise.

### Kimi Work (the Kimi app): Dimmy as a personal plugin

This is the Integrations card ("Kimi Work") on both hosts. Kimi Work runs a
Kimi Code engine of its own ("daimon") and starts LOCAL MCP servers declared
by **personal plugins**. Verified on Kimi 3.2.24 for Windows (2026-11-09):

- A plugin written straight into Kimi's installed list
  (`<daimon-share>/daimon/runtime/kimi-code/home/plugins/installed.json` +
  `managed/<id>/`) is parsed ("local plugins parsed") but never activated:
  Kimi's server-side list (`gateway installed=…`) decides. Don't do that.
- Kimi's own Plugin Builder, importing a local folder, writes exactly one
  file: `<daimon-share>/daimon/plugin-market/personal/<id>.json`, a record
  whose `sourcePath` points at the folder. The plugin then shows in
  Plugins → Personal; Install copies it in, and a Work chat starts the
  declared MCP server (dimmy-mcp, heartbeat `mcp.heartbeat.kimi-work`).
- So "Add to Kimi" (`kimi_mcp::register_work_plugin_at`, inside
  `dimmy_kimi_mcp_install`) writes `<config dir>/kimi-plugin/<id>/`
  (`kimi.plugin.json` with `hostKind: local` + `mcpServers`, `icon.png`,
  `locales/it-IT.json`, `iconUrl` = dimmy.app/brand/icon-512.png) and that
  record. The user presses Install in Kimi; the wizard watches
  `installed.json` and the heartbeat. `<daimon-share>` is
  `%APPDATA%\kimi-desktop\daimon-share` on Windows; on macOS the same
  layout under Application Support is assumed, not yet seen on a Mac.
- Deep links: `kimi-work://home` opens Work; nothing pre-fills a prompt
  (`kimi://chat?prompt=` and six other variants tested, signed in) and
  `kimi-work://plugin?id=` does not open a plugin page from outside. The
  meeting button therefore opens Work with the recap prompt on the
  clipboard. With only Kimi Code Desktop installed it opens that instead,
  in the meeting folder.
- Install: winget `MoonshotAI.Kimi` (Windows), kimi.ai/products/download.

## Setup CLI

`dimmy-setup profile apply … --recap kimi-sub [--recap-model k3]` and
`dimmy-setup login status|start|test kimi`.

## Open: live check on a real Kimi account

Everything above was written from the CLI source and tested against a
scripted ACP fake (`kimi_cli::tests`). Still to run on a real account:
ping, rewrite latency (Claude's is 1.4 s), the recap of a long meeting,
the model list, that no tool runs, that `session/delete` leaves no session
behind, that Kimi Code Desktop lists the 6 Dimmy tools after the bridge is
on and `dimmy_search` works, and that the catalog ids (`k3`, `kimi-for-coding`,
`kimi-for-coding-highspeed`) match what `/coding/v1/models` returns.

## Where to look in code

| Concern | File |
|---|---|
| Detection, ACP client, login, install | `core/src/kimi_cli.rs` |
| LLM dispatch | `core/src/llm.rs` (Kimi branch before the Claude one, in `process_text` and `process_raw_prompt`) |
| FFI + recap guard | `core/src/ffi.rs` (`dimmy_kimi_*`, `dimmy_llm_call_raw`) |
| Setup profile / login | `core/src/setup.rs`, `core/src/setup_cli.rs` |
| Win UI | `Views/KimiConnectDialog.xaml{.cs}`, `Views/SettingsWindow.KimiCli.cs`, `RefreshAuthIntegrationStatus` in `SettingsWindow.xaml.cs` |
| Mac UI | `Managers/DimmyCore+Kimi.swift`, `Views/Settings/MacKimiCliCard.swift`, `KimiConnectSheet.swift`, `MacOutputPage.swift` |
| Telemetry | `core/src/telemetry/events.rs` (`KimiCli*`) |

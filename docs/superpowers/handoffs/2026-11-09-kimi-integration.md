# Handoff: Kimi integration (2026-11-09)

Branch `feat/kimi-subscription`, cut from `origin/staging` @ `0d456207`.
Work-PC worktree: `E:\dimmy-wt\kimi`. Plan + feasibility:
[`docs/dev/kimi-integration-plan.md`](../../dev/kimi-integration-plan.md).

## Status (2026-11-09, evening)

Steps 1-7 below are DONE on this branch (commits after `03ec029c`):
compiled, `cargo clippy -D warnings` clean, 1409 lib tests + 63 `v2_ffi`
tests green, ABI snapshot regenerated, setup `kimi-sub` profile +
`login … kimi`, Windows wizard/card/toggles (600 C# tests green), macOS
parity (`xcodebuild` OK and the app launched without a SelfTests trip on
MacinCloud), docs (`docs/dev/kimi-backend.md`, telemetry, PRIVACY,
CLAUDE.md), Phase 3 MCP bridge (`kimi_mcp.rs` + a toggle on both cards).
No Linux UI work (user: no Linux).

**Only step 8 is open: the live check on the user's real Kimi account.**
Nothing was run against a real `kimi` binary yet.

## (history) First thing on the other PC

**The Rust changes have NEVER been compiled.** The only clippy attempt on the
work PC failed on argument parsing before cargo started (C: was at 1.4 GB
and another agent held the shared target). Start with:

```bash
cd core
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm kimi
cargo test --lib --features local-stt,local-llm provider
cargo test --lib --features local-stt,local-llm llm::tests::kimi
```

Expect small compile fixes (closure borrows in `kimi_cli::converse`,
`json[...] = bool.into()` in `ffi.rs`, any exhaustive `match` on
`Provider` elsewhere that now lacks `Provider::Kimi`).

## What the user asked (2026-11-09, verbatim intent)

"Go straight like a train until we have Kimi like Claude and Anthropic."
Kimi CLI subscription like Claude Code, the connect **wizard**, and Kimi API
keys (the membership's up-to-5 keys) for recap / LLM output. They won't
install or run anything themselves: the wizard must do it in one click.
Kimi does NOT do speech-to-text. It sits in the LLM step only (rewrite,
command mode, recap), same place as Claude.

## Decisions taken (and why)

- **One provider, `Provider::Kimi` (tag `kimi`) = Kimi Code membership.**
  Two auth paths like Anthropic: API key (HTTP, OpenAI-compatible
  `https://api.kimi.ai/coding/v1/chat/completions`, CN `api.kimi.com`) or
  `auth_method = "subscription"` → the `kimi` CLI. Moonshot pay-per-token
  Platform keys were left out on purpose: Kimi K3 is already reachable via
  Together/Fireworks (less is more).
- **Transport = `kimi acp` (Agent Client Protocol, ndjson JSON-RPC on
  stdio), NOT `kimi -p`.** Read from the MIT source of
  `@moonshot-ai/kimi-code` 2.5.2: `-p` takes the prompt only from argv (8191
  chars through an npm shim: recaps don't fit). ACP gives: prompt on stdin,
  the tier's real model list (`configOptions.model`, which avoids the Codex
  "HTTP 1" trap), `thinking` off for quick rewrites, `session/delete` (no
  dictations left in the user's Kimi history), and permission requests we
  answer `cancelled` (default mode is `manual`).
- A prompt starting with `/` is a slash command in ACP → `guard_slash`.
- Honest `User-Agent: Dimmy/<ver>` added to both LLM reqwest clients
  (Kimi suspends members whose clients fake their identity).
- Terms: Kimi Code is "personal interactive use only". Every Dimmy call is
  one user action; never call the Kimi sub from a bulk loop. Qwen and GLM
  coding plans exclude apps like Dimmy: not built (see the plan doc).

## Verified facts (Kimi Code CLI 2.5.2 source, 2026-11-09)

- Native install: `irm https://code.kimi.com/kimi-code/install.ps1 | iex` /
  `curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash` →
  `~/.kimi-code/bin/kimi[.exe]` (`KIMI_INSTALL_DIR`), user PATH only.
  npm package needs Node ≥ 22.19 (this PC has 22.2).
- **Windows: Git for Windows is required** (`locateWindowsGitBash`:
  `KIMI_SHELL_PATH`, beside `git.exe` on PATH, `Program Files\Git`,
  `%LOCALAPPDATA%\Programs\Git`). Mirrored in `kimi_cli::git_bash_present`.
- Login: `kimi login` (device code), also `kimi acp --login`. Tokens in
  `~/.kimi-code/credentials/<name>.json` (`KIMI_CODE_HOME`). We check
  existence only.
- ACP: `initialize` → `session/new {cwd, mcpServers}` (error `-32000` =
  auth required) → `session/set_config_option {configId: model|thinking|mode}`
  → `session/prompt` (text arrives as `session/update` `agent_message_chunk`;
  `stopReason` end_turn / refusal / cancelled / max_tokens) → `session/delete`.
- Kimi Code Desktop (Win+Mac) reads the same `~/.kimi-code/mcp.json`
  (`mcpServers.<name> = {command, args, env, enabled, ...}`) → Phase 3.
- User-global `~/.kimi-code/AGENTS.md` / `~/.agents/AGENTS.md` and user MCP
  servers still load in our sessions (no ACP switch for that). Measure the
  startup cost on a real account.

## Done in this branch (uncompiled, see above)

| File | What |
|---|---|
| `core/src/kimi_cli.rs` (new) | detection, status, `run_blocking` over ACP, `list_models`, `spawn_login`, `install_command`, `git_bash_present`, `PROVIDER_URL = "kimi-cli://default"`, 20 tests on a scripted ACP fake |
| `core/src/provider.rs` | `Provider::Kimi`, `from_url` (api.kimi.ai / api.kimi.com), `as_str`/`from_tag` "kimi", `default_llm_url`, `is_kimi()`, test |
| `assets/model-catalog.json` | provider `kimi` "Kimi Code", models `k3`, `kimi-for-coding`, `kimi-for-coding-highspeed` (ids from Kimi's membership guide, NOT checked against `/coding/v1/models`) |
| `core/src/llm.rs` | Kimi branch in `process_text` (quick, 60 s) and `process_raw_prompt` (600 s), before the Claude branch; `track_kimi`, `map_kimi_err`; UA; tests |
| `core/src/ffi.rs` | `dimmy_kimi_status/_diagnostics/_binary_path/_install_command/_git_bash_present/_spawn_login/_ping/_models_json/_recheck`; recap guard knows Kimi; `has_kimi_key/_llm_key/_recap_key` in config JSON; `kimi_cli.login_completed` host event |
| `core/src/telemetry/{events,report}.rs` | `KimiCli{StatusProbed,LoginSpawned,LoginCompleted,Invocation}` |

## Left to do, in order

1. **Compile + tests green** (above). Then regenerate the ABI snapshot:
   `UPDATE_ABI=1 cargo test --release --test abi_snapshot --features local-stt`
   (`core/tests/fixtures/abi_exports.txt`, 9 new `dimmy_kimi_*` symbols).
2. `core/tests/v2_ffi.rs`: config round-trip for `kimi-cli://default` and
   for the Kimi Code URL with `llm_auth_method=subscription`.
3. `setup.rs` / `setup_cli.rs`: a `kimi-sub` recap/LLM profile next to
   `codex-sub`, `login status|start|test kimi`, install line.
4. **Windows UI** (mirror Codex exactly):
   - `Interop/DimmyNative.cs`: P/Invoke for the 9 entries.
   - `Views/KimiConnectDialog.xaml(.cs)`: 3 pages like
     `CodexConnectDialog`. Page 1 checks `dimmy_kimi_git_bash_present`
     (offer `winget install --id Git.Git -e`), then the one-click install
     (`dimmy_kimi_install_command` in a visible PowerShell); page 2 sign-in
     (`dimmy_kimi_spawn_login`, poll `dimmy_kimi_recheck` 2 s × 3 min, emit
     `kimi_cli.login_completed`); page 3 test (`dimmy_kimi_ping`) + model list
     (`dimmy_kimi_models_json`, off the UI thread).
   - `SettingsWindow.xaml` Integrations card "Kimi (Kimi Code)" + a
     `SettingsWindow.KimiCli.cs` partial, like the Codex card.
   - `Services/ProviderCatalog.cs`: `kimi` in the LLM/recap provider lists,
     `ProviderInfo("kimi", "Kimi Code", "Ki", <color>, "https://www.kimi.com/code/console", ...)`.
     `SettingsWindow.xaml.cs` URL→vendor maps (lines ~2232/2248) +
     `has_kimi_key`/`has_kimi_llm_key` in the key-flag tables (~2716/2729);
     `SettingsViewModel.cs` key-flag dicts (~597/722) and the
     "Use Kimi subscription" toggle where the Output/Recap auth radio sits.
   - `Assets/Providers/kimi.svg`; `Dimmy.Windows.Tests/ProviderCatalogTests.cs`.
   - Build: `dotnet build ... -c Debug -p:Platform=x64` (see CLAUDE.md).
5. **Mac UI** (parity is mandatory): `DimmyFFI.h`, `DimmyCore+Kimi.swift`,
   `MacKimiCliCard.swift`, `KimiConnectSheet.swift`, `AppState.swift`
   (provider enum + URL map + `hasKimiKey`), `ProviderCatalog.swift`,
   `ProviderTagging.swift`, `MacOutputPage.swift`, `MacIntegrationsPage.swift`,
   `provider-kimi` asset, pbxproj (run `scripts/dev/check-pbxproj.py`),
   SelfTests, then `scripts/dev/preflight-mac.sh` (MacinCloud).
6. Docs: `docs/dev/kimi-backend.md` (sibling of `codex-backend.md`),
   `docs/dev/telemetry-implementation.md`, `PRIVACY.md` (new categorical
   events), CLAUDE.md surfaces table row.
7. Phase 3: `core/src/kimi_desktop.rs` registers `dimmy-mcp` in
   `~/.kimi-code/mcp.json` (merge, backup, never touch other entries) + FFI
   + Integrations card. Same pattern as `claude_desktop.rs`.
8. **Live check with a real Kimi account** (the user's): ping, rewrite
   latency (compare Claude's 1.4 s), recap of a long meeting, model list,
   that no tool runs, that `session/delete` leaves no session behind, and
   that the catalog ids are what `/coding/v1/models` returns.

## Build environment notes (work PC)

- C: was at 1.4 GB. Keep cargo off C:: `CARGO_TARGET_DIR`, `CARGO_HOME`,
  `TEMP`, and `LOCALAPPDATA` (llama.cpp's `llcb` cache) all on E:.
  `E:\dimmy-build\agent` is used by other agents; this branch used
  `E:\dimmy-build\kimi`.
- Calling cargo through a PowerShell script with `--` swallowed it: run
  cargo directly, or pass `'--'` quoted.

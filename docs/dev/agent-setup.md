# Agent setup: `dimmy-setup`

Lets an AI agent the user already trusts (Claude Code, Codex, Gemini CLI)
install, configure and test Dimmy from a terminal, end to end, by following
`https://dimmy.app/install.md`. The agent asks the user what they want; the
user only does what no agent can do for them: OS permissions and logins.

## Shape

One implementation, in the Rust core:

```
core/src/setup.rs          pure logic: schema, strict validation, profiles,
                           system check, diag redaction, app-running lock
core/src/setup_cli.rs      argument parsing + command dispatch, JSON out
ffi.rs  dimmy_setup_cli(argc, argv) -> exit code
```

Two thin entry points, so the installers carry no second copy of the core:

| OS | How the agent runs it | Entry point |
|---|---|---|
| Windows | `%LOCALAPPDATA%\Dimmy\current\dimmy-setup.exe <cmd>` | `core/src/bin/dimmy_setup.rs`: console exe, loads `dimmy_lib.dll` from its own folder, calls `dimmy_setup_cli` |
| macOS | `/Applications/Dimmy.app/Contents/MacOS/Dimmy --setup <cmd>` | Swift `main` sees `--setup` before `NSApplication` starts, calls `dimmy_setup_cli`, exits |

## Rules

- **Output is JSON on stdout**, one object per command (`{"ok":true,...}` or
  `{"ok":false,"error":{"code":"...","message":"...","fix":"..."}}`). Progress
  for long operations is one JSON object per line. Exit codes: `0` ok, `1`
  failed, `2` bad usage (including rejected settings: the request was
  wrong, not the machine), `3` Dimmy is running and the command writes
  (`config set`, `profile apply`, `onboarding done`, `models download`).
- **The core stays the single writer.** Writing commands refuse while the app
  runs (exit 3, `fix: "dimmy-setup app quit"`). The app holds an exclusive
  lock on `<config_dir>/.app.lock` for its lifetime; the CLI probes it.
- **Invalid values are rejected, never replaced.** `dimmy_set_config_json`
  quietly maps an unknown value to a default, which is right for a UI and
  wrong for an agent. `setup::validate_patch` runs first and returns every
  problem with the allowed values.
- **No secret ever travels through the agent.** No command takes a key as an
  argument or prints one. Keys are entered by the user in Dimmy's own window.
- **No telemetry from the CLI** in v1.
- `--config-dir <path>` points the whole run at a throwaway directory, for
  testing on a machine that has a real install. Only valid before init.
  Whisper, Qwen, Whistle and local LLM files live in the shared models
  folder, not the config dir, so `models download` still lands there.
- The CLI never calls `dimmy_init`. `ffi::init_headless` loads config and
  keystore only (no audio thread, hotkeys, telemetry, history database or
  key migration), and writes go through the app's own
  `dimmy_set_config_json`, then are read back: a value the core adjusted
  is reported as `not_applied`, not hidden.

## Commands (v1 = Windows-testable now, v2 = needs the running app)

| Command | Does | v |
|---|---|---|
| `info` | version, flavor, platform, config dir, app installed/running | 1 |
| `schema` | every setting the agent may set: type, allowed values, default, one plain sentence | 1 |
| `check` | OS, CPU arch, RAM, free disk, GPU fitness + known-bad, mics, Claude/Codex/Gemini CLI state, which keys exist (booleans), license, onboarding done, **recommended profile** | 1 |
| `config get` | current settings, no secrets | 1 |
| `config set k=v ...` / `--json` | strict-validated patch, `--dry-run` shows the diff | 1 |
| `profile apply --use meeting\|dictation\|both --lang ... --stt local\|cloud ... --recap ...` | one consistent set of settings from a few answers, `--dry-run` | 1 |
| `models list` / `models download <id>` | catalog with sizes and presence; download checks free space first, streams progress | 1 |
| `login status\|start\|test <claude\|codex\|gemini>` | `test` is a real round trip, not "credentials exist" | 1 |
| `key status` | which providers have a key, per scope | 1 |
| `key test <provider>` | real round trip with the stored key | 1 (LLM), 2 (STT) |
| `key ask <provider>` | opens Dimmy's key window | 2 |
| `test stt --file <wav>` / `test recap` | real transcription / real recap call | 1 |
| `test mic`, `test meeting --seconds N` | levels per track + transcript | 2 |
| `perm status\|open\|wait <perm>` (macOS) | TCC belongs to the app, so status needs the running app | 2 |
| `onboarding status\|done` | marks onboarding done so the app opens on a "configured by your AI" summary | 1 |
| `diag [--lines N]` | versions, settings without secrets, redacted log tail, recognised error categories | 1 |
| `app status\|start\|quit` | | 1 |

v2 needs a request/response channel to the running app (Windows' command pipe
is one-way today; macOS has none). Tracked separately.

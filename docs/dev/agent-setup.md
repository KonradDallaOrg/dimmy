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
  (`config set`, `profile apply`, `onboarding done`, `models download`,
  `models prepare`).
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
| `schema` | every user setting: type, allowed values, default, one plain sentence; `stored_by: "app"` for the ones the app keeps outside `config.json` (see below) | 1 |
| `check` | OS, CPU arch, RAM, free disk, GPU fitness + known-bad, mics, Claude/Codex/Gemini CLI state, which keys exist (booleans), license, onboarding done, **recommended profile**, `warnings` (`no_microphone` when no input device is found; always present, may be empty) | 1 |
| `config get` | current settings, no secrets | 1 |
| `config set k=v ...` / `--json` | strict-validated patch, `--dry-run` shows the diff | 1 |
| `profile apply --use meeting\|dictation\|both --lang ... --stt local\|cloud ... --recap ...` | one consistent set of settings from a few answers, `--dry-run` | 1 |
| `models list` / `models download <id>` | catalog with sizes and presence; download checks free space first, streams progress. On a Mac `parakeet:fp32` is the FluidAudio Core ML bundle (466 MB), the one the app runs; `coreml:<whisper file>` rows are whisper's optional Neural Engine encoder | 1 |
| `models prepare [<id>]` | runs the one-time Core ML compile now (default: the configured engine's model), so the first meeting does not pay it. `needed:false` where nothing compiles (Windows, Linux, whisper without an encoder) | 1 |
| `login status\|start\|test <claude\|codex\|gemini>` | `test` is a real round trip, not "credentials exist" | 1 |
| `key status` | which providers have a key, per scope | 1 |
| `key test <provider>` | real round trip with the stored key | 1 (LLM), 2 (STT) |
| `key ask <provider>` | opens Dimmy's key window | 2 |
| `test stt --file <wav>` / `test recap` | real transcription / real recap call | 1 |
| `test mic`, `test meeting --seconds N` | levels per track + transcript | 2 |
| `perm status` | each permission: `granted`, `denied`, `not_determined`, `restricted`, `unknown` or `not_applicable`, plus `required_granted`. macOS: read from the app's `permissions.json` with `stale_secs` (or `known:false` before the app's first run). Windows: the microphone privacy switches straight from the registry | 1 |
| `perm open microphone\|accessibility\|input_monitoring\|system_audio` | opens that exact System Settings pane (macOS) or `ms-settings:privacy-microphone` (Windows; the others are `not_applicable`) | 1 |
| `perm guide` (macOS) | opens Dimmy's permission guide window (writes `<config_dir>/.show-permission-guide`, then `open`s the app) | 1 |
| `last` | the latest dictation (time, `age_secs`, duration, chars/words, no text) and the latest meeting (`age_secs`, duration, `transcript_nonempty`, size of each audio track, `recap_present`, warnings from the log). Read-only, so it works while the app runs. Use it to check a test, never the user's word | 1 |
| `onboarding status\|done` | marks onboarding done so the app opens on a "configured by your AI" summary | 1 |
| `diag [--lines N]` | versions, settings without secrets, redacted log tail, recognised error categories | 1 |
| `app status\|start\|quit` | | 1 |

## Every setting, including the ones the app stores

`schema` lists every user setting. `setup::NOT_SETTINGS` names the
`config.json` fields that are not choices (connection state written by the
Notion and Confluence sign-in, caches, counters, window anchors, migrated
knobs); a test makes each new `AppConfig` field land in one list or the
other.

Some settings never reach `config.json`: the app reads them itself at
launch. They carry `"stored_by": "app"` in `schema`, and `config get` /
`config set` handle them like the rest (`core/src/setup_prefs.rs`):

| OS | Store | Keys |
|---|---|---|
| Windows | `<config_dir>/ui_prefs.json`, merged (unknown properties are kept) | `dict_add_hotkey`, `command_hotkey`, `meeting_hotkey`, `update_channel`, `recap_export_folder`, `theme`, `pill_show_on_startup`, `pill_show_on_hotkey`, `show_taskbar_icon`, `tray_icon_always_visible` |
| macOS | the app's UserDefaults, via `defaults write <bundle id>` | `shortcut`, `dict_add_hotkey`, `command_hotkey`, `meeting_hotkey`, `update_channel`, `recap_export_folder`, `pill_visible`, `show_in_dock`, `show_in_menu_bar` |

- On macOS the dictation `shortcut` is the app's `shortcutEncoded`; the
  `config.json` field is never read there, so it is not offered.
- Hotkeys use one grammar on both systems (`ctrl+alt+m`; `opt`/`option`
  = `alt`, `cmd` = `win`; `fn` alone on macOS). The Mac encodings
  (`HotkeyCombo.encoded`, the `ModifierShortcut` bitmask) are mirrored and
  unit-tested in `setup_prefs`. `""` turns an optional hotkey off; on macOS
  that deletes the key and sets `meetingHotkeyDefaultApplied`, otherwise
  the app restores its default meeting shortcut.
- Same rule as `config.json`: written only while the app is closed (exit 3),
  because both apps read these once at launch and save their own copy.

## Permissions on macOS

macOS answers a permission question for the process RESPONSIBLE for the
caller. For `Dimmy --setup` that is the assistant which launched it, so the
CLI cannot read Dimmy's grants. The app writes what it sees instead:

```
<config_dir>/permissions.json
{"updated_at": "<RFC 3339>", "platform": "macos",
 "microphone": "granted|denied|not_determined|restricted",
 "accessibility": "granted|denied",
 "input_monitoring": "granted|denied|unknown",
 "system_audio": "granted|denied|unknown"}
```

Written at launch and on every change (`PermissionsManager`), atomically.
System audio has no status API: it is what the last meeting observed
(audio arrived or not), `unknown` before one.

When `.onboarding_done` exists the Mac treats the install as configured by
an assistant: the marker is kept (so `onboarding status` stays true), and a
missing permission opens the small permission guide instead of the whole
onboarding. Both "Reset onboarding" actions delete the marker.

## Quitting and starting on macOS

The CLI is the app binary (`<X>.app/Contents/MacOS/<X>`), and a test build
can be named "Dimmy Agent". So the bundle is derived from the running
executable, its `CFBundleIdentifier` read with `defaults read`, quit with
`tell application id "<id>" to quit`, started with `open <bundle>`.

## Core ML on macOS

Two different compiles, both once per Mac:

- **Parakeet** (the Mac's default engine) runs through FluidAudio on the
  Neural Engine. Its download (`models download parakeet:fp32`) already
  loads the model, which compiles it; `models prepare parakeet:fp32` runs a
  warm-up inference. The CLI is the app binary, so the system's compiled
  model cache it fills is the app's.
- **Whisper** can use an optional Core ML encoder (`coreml:<file>`). Its
  compile is the 347 s stall of 2026-09-14; the app prepares it in the
  background, `models prepare <file>` does it during setup instead.

v2 needs a request/response channel to the running app (Windows' command pipe
is one-way today; macOS has none). Tracked separately.

# Command hotkey on the Rust hook (toggle + PTT) — handover

Branch: `feat/command-hotkey-ptt` (off `feat/command-mode-hotkey`).
Date: 2026-06-02. Win-only so far; Mac core is ready, Mac UI is not.

## What changed and why

The dedicated command-mode hotkey used to run on Win32 `RegisterHotKey`,
which delivers key-DOWN only. That forced it to be **toggle-only** and
**modifier+key only** (no `Win+Alt`-style modifier-only combos). It also
could not follow the dictation Push-to-Talk / Toggle setting.

It now runs on the **same Rust low-level keyboard hook** as the dictation
shortcut, so it inherits:

- both **Toggle and PTT** (it reads the same `ShortcutMode` as dictation),
- **every combo** the dictation hotkey supports, including modifier-only
  (`Win+Alt`, `Ctrl+Shift`) and 2-mod+key (`Ctrl+Shift+X`).

The two hotkeys are two independent bindings on one hook (`DICT` + `CMD` in
`core/src/hotkey.rs`), so the proven dictation path is byte-for-byte the
same logic, just parameterised over a `Binding` struct.

## Surfaces

| Layer | File | Change |
|---|---|---|
| Core | `core/src/hotkey.rs` | `Binding` struct (codes + down-flags + combo_active + event). Two statics `DICT`/`CMD`. `Binding::process()` is the original state machine, now per-binding. `set_command_shortcut`, `take_command_event`, `combos_conflict`. Windows `keyboard_proc` + macOS `tap_callback` feed BOTH bindings. |
| FFI | `core/src/ffi.rs` | `dimmy_hotkey_set_command`, `dimmy_hotkey_take_command_event`, `dimmy_hotkey_combos_conflict`. |
| Win interop | `Interop/DimmyNative.cs` | P/Invoke decls for the 3 new entries. |
| Win svc | `Services/HotkeyService.cs` | `CommandHotkeyPressed`/`Released` events, `SetCommandShortcut`, poll the command event in the same loop (release gated by `PttMode`). |
| Win app | `App.xaml.cs` | Removed the `RegisterHotKey`-based `CommandHotkeyService`. `OnCommandHotkeyPressed`/`OnCommandHotkeyReleased` mirror the dictation toggle/PTT handlers but set `CommandOneShot`. `ReregisterCommandHotkey` -> `SetCommandShortcut`. |
| Win settings | `SettingsWindow.xaml(.cs)` | Command recorder dropped `RequireKey` (modifier-only now valid). On change, rejects a combo that conflicts with the dictation or dictionary shortcut (`dimmy_hotkey_combos_conflict`) with a toast. Load the saved combo verbatim. |
| Deleted | `Services/CommandHotkeyService.cs` | The whole RegisterHotKey implementation. |

## Conflict rule

Two combos conflict when one keyset is a subset of the other (a combo fires
whenever all its keys are held, so `Ctrl+Space` would also fire while you
press `Ctrl+Shift+Space`). The command recorder rejects a combo that
conflicts with either the dictation shortcut or the dictionary shortcut.
Disabled (empty) never conflicts.

## Tests

- `core/src/hotkey.rs`: 6 new unit tests (press/release for 2-mod and
  mod+key, auto-repeat idempotency, unconfigured-ignores, set/clear CMD
  independent of DICT, conflict subset/equal/distinct). The 12 existing
  dictation tests are unchanged and green.
- `core/tests/v2_ffi.rs`: FFI round-trip for set_command + take_command +
  combos_conflict.
- All 571 lib tests (`--test-threads=1`) + 301 Win xUnit green. Win DLL
  (frozen feature set) + C# host (x64) build clean.

## MANUAL test matrix (do this on the running build)

Set the command hotkey in Settings -> Shortcut -> "Command mode shortcut".
Try both `ShortcutMode = Toggle` and `Push-to-Talk` (the command hotkey
follows whichever is set for dictation).

1. **Toggle + modifier+key** (`Ctrl+Shift+X`): select text, press once
   (pill amber, recording), speak, press again -> selection transformed.
2. **Toggle + no selection**: press, speak ("write a haiku about cats"),
   press again -> generated text inserted at the caret.
3. **PTT + modifier-only** (`Win+Alt`): hold, speak, release -> command
   runs. Confirm Start Menu does NOT open on release (suppression).
4. **PTT + mod+key**: hold `Ctrl+Shift+X`, speak, release -> runs; the
   app must not receive a literal `X` keystroke.
5. **Dictation still works** unchanged in the same session (both bindings
   coexist) for Toggle and PTT.
6. **Conflict**: try setting the command hotkey equal to the dictation
   shortcut -> rejected with a toast, stays disabled.
7. **Clear**: the Clear button disables the command hotkey (back to
   pill-menu-only command mode).

## Mac parity (TODO, not in this branch)

The shared core already drives both bindings on the macOS `CGEventTap`
(`DICT.process` + `CMD.process` in `tap_callback`). Mac just needs to:

- call `dimmy_hotkey_set_command` from the Mac settings when the user sets
  a command combo (mirror of `SetCommandShortcut`),
- poll `dimmy_hotkey_take_command_event` alongside `dimmy_hotkey_take_event`
  in the Mac hotkey poller, routing pressed/released to the command
  start/stop with the same `commandOneShot` flag,
- use `dimmy_hotkey_combos_conflict` to reject overlapping combos.

No Rust changes needed on the Mac side; it is purely Swift wiring.

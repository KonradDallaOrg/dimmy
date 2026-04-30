# platforms/windows

The Windows native UI. WinUI 3 + C# / .NET 8, calling the Rust core via P/Invoke.

- **Big picture:** [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md)
- **Build:** [`../../docs/BUILD.md`](../../docs/BUILD.md#windows)
- **CI invariants (READ BEFORE TOUCHING WORKFLOWS):** [`../../docs/dev/windows-ci.md`](../../docs/dev/windows-ci.md)

## What lives here

```
Dimmy.Windows/                   Main app
├── App.xaml, App.xaml.cs        WinUI App shell + jump-list arg parsing
├── Program.cs                   Entry point
├── Views/
│   ├── PillWindow.xaml(.cs)        Floating overlay (recording UI)
│   ├── SettingsWindow.xaml(.cs)    Settings + Pill / Tasks / Visibility cards
│   ├── OnboardingWindow.xaml(.cs)  Welcome flow
│   └── TaskbarAnchorWindow.xaml    Invisible 1×1 anchor — owns the taskbar entry
│       └─ click → TogglePill, right-click → jump list
├── ViewModels/                  MVVM layer (AppViewModel, SettingsViewModel)
├── Services/
│   ├── TrayService.cs              System-tray icon (small, near clock)
│   ├── TaskbarService.cs           Taskbar overlay icon + amplitude bar (ITaskbarList3)
│   ├── JumpListService.cs          Right-click menu (ICustomDestinationList) + AUMI
│   ├── CommandPipeServer.cs        Named-pipe IPC for jump-list commands
│   ├── UiPreferences.cs            Win-only UI prefs (ui_prefs.json)
│   ├── HotkeyService.cs            Low-level keyboard hook → AppViewModel
│   ├── TextInjectionService.cs     Cmd+V via SendInput
│   └── TranscriptionService.cs     FFI orchestration around dimmy_*
├── Interop/
│   └── DimmyNative.cs           P/Invoke declarations → dimmy_lib.dll
├── Helpers/                     WinUI glue, DPI, transparency
├── Converters/                  XAML value converters
├── Strings/                     Localizable resources
├── Assets/                      Icons, pill graphics
├── app.manifest                 PerMonitorV2 DPI awareness + COM
├── Dimmy.Windows.csproj         The project file
└── NuGet.config                 Feed config

Dimmy.Windows.Tests/             41 C# tests
Dimmy.Windows.UiTests/           FlaUI UIA3 smoke tests
installer.nsi                    NSIS installer config (legacy — Velopack is now canonical)
verify-self-contained.ps1        CI gate: asserts the publish folder contains only what it should
test-in-sandbox.ps1              Local clean-install smoke test (Windows Sandbox)
diagnose-install.ps1             Debug script for "why is the installed app not launching"
```

The **build script at the repo root** — `build-windows.ps1` — is the one-shot for local contributors. CI inlines its own steps for toolchain control.

## Runtime facts

- **DLL entry point:** `dimmy_lib.dll` is loaded via `DimmyNative.cs` P/Invoke. Dropped next to `Dimmy.Windows.exe` in the publish folder.
- **Single-instance guard:** `Global\DimmySingleInstance` mutex. A second launch normally exits silently — exception: jump-list shortcuts re-launch the EXE with `--command <name>`, which is forwarded to the running instance via named pipe BEFORE the mutex check (see [Jump list + IPC](#jump-list--ipc-named-pipe)).
- **AppUserModelID:** `Dimmy` (set process-wide via `SetCurrentProcessExplicitAppUserModelID` and per-window via `SHGetPropertyStoreForWindow` — both required on Windows 11 unpackaged for the jump list to bind to our taskbar entry).
- **Configuration:** `%APPDATA%\dimmy\config.json`. **The Rust core is the only writer.** UI calls `dimmy_set_config_json()` and re-reads.
- **Win-only UI prefs:** `%APPDATA%\dimmy\ui_prefs.json` — small JSON store for the pill-visibility toggles (`PillShowOnStartup`, `PillShowOnHotkey`). Kept out of `config.json` because they're not cross-platform settings.
- **Keys:** `%APPDATA%\dimmy\keys.enc` (AES-256-GCM). Managed entirely by the Rust keystore.
- **Logs:** `%LOCALAPPDATA%\dimmy\logs\dimmy.log`, `crash.log`, `ptt.log`. Plus diagnostic `%TEMP%\dimmy_jumplist.log` and `%TEMP%\dimmy_startup.log` for boot-time issues.
- **Installer:** Velopack (`--framework vcredist143-x64`, `--packId Dimmy` — the AUMI must match the packId, otherwise the jump list won't appear). VC Redist goes to System32; the app folder stays lean. See I4 and I10 in [`windows-ci.md`](../../docs/dev/windows-ci.md).

## Quick dev loop

```powershell
# From this directory
dotnet build Dimmy.Windows\Dimmy.Windows.csproj -c Debug
# Run from VS or:
dotnet run --project Dimmy.Windows\Dimmy.Windows.csproj -c Debug
```

Tests:
```powershell
dotnet test Dimmy.Windows.Tests\Dimmy.Windows.Tests.csproj -c Release
```

For the Rust DLL build (needed once per Rust change), see [`../../docs/BUILD.md#windows`](../../docs/BUILD.md#windows).

## Taskbar anchor — state at a glance

The Windows taskbar button carries the recording-pipeline state visually, mirroring the macOS menu-bar status icon.

- **`TaskbarAnchorWindow`** — invisible 1×1 WinUI window kept always-minimised. Its only job is to register an HWND in the taskbar so the rest of the system has something to attach to. `WM_SYSCOMMAND/SC_RESTORE` is intercepted in a window subclass: when the user clicks the taskbar button, the anchor stays minimised and we forward `TaskbarClicked` → `App.TogglePill`.

- **`TaskbarService`** — wraps `ITaskbarList3`. Subscribes to `AppViewModel.PropertyChanged` and updates two visuals on every state transition:
  - **Overlay icon** (`SetOverlayIcon`) — small colored dot in the bottom-right of the taskbar button. Recording=red, Transcribing=blue, Processing=purple, Completing=green, Error=yellow, Idle=cleared.
  - **Progress bar** (`SetProgressState` + `SetProgressValue`) — colored bar across the bottom of the button. During Recording, the value is driven by `dimmy_get_amplitude()` polled at 12 Hz with the same display-AGC the pill uses → the bar pulses with your voice (free VU meter visible even when the pill is hidden). Other states use INDETERMINATE (Windows handles the animation).

- **State icons** are drawn in code at 32×32 with 4×4 super-sampled anti-aliasing, written to `%TEMP%\dimmy_taskbar_icons_v2\`, loaded via `LoadImage`. Windows downscales to the 16×16 overlay slot using bilinear filtering on the alpha channel — clean round dots, no jagged hexagons.

## Jump list + IPC (named pipe)

Right-click on the taskbar icon shows a custom jump list — the Windows analogue of the macOS Dock context menu.

**Categories:**
- **Tasks**: Toggle pill (no icon), Open Settings… (•••), Quit Dimmy (✕)
- **Style**: Off / Correct / Elaborate / Summarize / Professional, each with its color dot
- **Translate to**: No translation (grey) / English (USA composite) / Italiano / Français / Deutsch / Español, with vertical-stripe flag icons

**How clicks dispatch (round-trip):**
1. The user clicks an entry → Windows launches `Dimmy.Windows.exe --command set-style:elaborate` (a transient process).
2. `App.OnLaunched` parses `--command` BEFORE the single-instance mutex check.
3. `CommandPipeServer.TrySendCommand` opens a `NamedPipeClientStream` to `Global\DimmyCommand`, writes the command line, and exits.
4. The running instance's `CommandPipeServer.AcceptLoop` reads the line on a background task and dispatches via `HandleForwardedCommand` on the UI dispatcher.
5. `set-style:` / `set-translate:` commands write through `dimmy_set_config_json()` — same single-writer path the pill scroll handlers and tray submenu use.

**AUMI requirement (Windows 11):** Custom jump-list entries are silently dropped unless a Start-menu shortcut with the matching AUMI exists. Velopack creates one at install (`packId=Dimmy` → AUMI=Dimmy). For dev builds, `JumpListService.EnsureStartMenuShortcut()` writes a `Dimmy (Dev).lnk` pointing at the dev EXE on first launch (idempotent, recreates if the EXE path changes). Without this, right-click shows only system defaults (Pin / Close).

**Glyphs are drawn in code:** All jump-list icons (style dots, flag stripes, USA composite, X, three dots) are 32×32 BGRA written to `%TEMP%\dimmy_*_icons_v*\`. Mixing built-in shell icons (`imageres.dll,N`) with our brand glyphs looked stylistically jarring — consistent custom drawing reads better.

## Pill visibility / taskbar-only mode

Two toggles in **Settings → Pill overlay → Visibility** let the user run Dimmy without ever seeing the floating pill:

- **Show pill on app start** (default ON) — when off, `App.ShowPillAndHotkey` creates the pill but immediately calls `HidePill()`.
- **Show pill when recording** (default ON) — when off, `App.OnHotkeyPressed` no longer auto-shows the pill if it's hidden. Recording proceeds, status visible only on the taskbar icon.

Stored in `%APPDATA%\dimmy\ui_prefs.json` (small JSON; persistence happens in `App.OnUiPrefsRelevantPropertyChanged` which fires on `AppViewModel.PillShow*` changes). Out of `config.json` because they're Windows-only UI behaviours, not cross-platform settings — same reasoning macOS uses for its `showInDock` / `showInMenuBar` UserDefaults split.

## Tray + pill right-click menus

Both the tray icon (small, near the clock) and right-click on the pill itself show a `MenuFlyout` with:

- Status row + read-only Native (STT input lang) + Shortcut
- **Translate to →** submenu with all 7 targets, current value checkmarked (`ToggleMenuFlyoutItem`)
- **Style →** submenu with all 13 styles, current value checkmarked
- Show/Hide Pill, Settings…, Quit

The submenu items write via `dimmy_set_config_json()` and locally update `_vm.LlmTranslateTo` / `_vm.LlmStyle` so the pill reflects the change immediately without round-tripping through the config event loop.

## Platform-specific gotchas

- **Pill transparency.** Uses `WS_POPUP` style, `DWMWCP_DONOTROUND`, `DWMWA_COLOR_NONE`. Do NOT use `DwmExtendFrameIntoClientArea` with negative margins — adds glass effect. See `known-bugs.md` WIN-001.
- **Glow effect.** Single `Composition.DropShadow` — not stacked borders. GPU-accelerated.
- **Hotkey bridge.** Low-level keyboard hook (`SetWindowsHookEx`) with 7 FFI functions. Supports any combo.
- **PRI generation.** `dotnet publish` needs the UWP/AppxPackage workloads from VS 2022 (not VS 2026 BuildTools). If `resources.pri` is missing from the app dir, WinUI throws `XamlParseException` at `InitializeComponent()` and the app is headless. See I2.
- **DPI.** PerMonitorV2 awareness in `app.manifest`. Windows are sized in DIPs; multiply by monitor scale for pixel values.

## Debugging a shipped installer

If a user reports "installer runs but app doesn't launch":

1. Ask for `%LOCALAPPDATA%\dimmy\logs\dimmy_startup.log` and `crash.log`.
2. Check for `XamlParseException` → PRI regression (I2).
3. Check for `create_state` / `ggml-vulkan` crash → GPU backend issue. Sticky known-bad marker should have kicked in; if not, verify `%LOCALAPPDATA%\dimmy\known_bad_gpu.marker` was written.
4. Check linker version of the shipped `dimmy_lib.dll`:
   ```powershell
   & "C:\Path\To\dumpbin.exe" /headers "C:\Path\To\dimmy_lib.dll" | Select-String "linker version"
   ```
   Must be ≥ 14.50. If not, a broken build slipped through the CI gate — reopen [`windows-ci.md`](../../docs/dev/windows-ci.md) I1.

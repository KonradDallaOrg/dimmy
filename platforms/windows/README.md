# platforms/windows

The Windows native UI. WinUI 3 + C# / .NET 8, calling the Rust core via P/Invoke.

- **Big picture:** [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md)
- **Build:** [`../../docs/BUILD.md`](../../docs/BUILD.md#windows)
- **CI invariants (READ BEFORE TOUCHING WORKFLOWS):** [`../../docs/dev/windows-ci.md`](../../docs/dev/windows-ci.md)

## What lives here

```
Dimmy.Windows/                   Main app
├── App.xaml, App.xaml.cs        WinUI App shell
├── Program.cs                   Entry point
├── Views/                       XAML views (pill, settings, onboarding)
├── ViewModels/                  MVVM layer
├── Services/                    Tray, hotkey bridge, text injection
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
installer.nsi                    NSIS installer config (legacy — Velopack is now canonical)
verify-self-contained.ps1        CI gate: asserts the publish folder contains only what it should
test-in-sandbox.ps1              Local clean-install smoke test (Windows Sandbox)
diagnose-install.ps1             Debug script for "why is the installed app not launching"
NuGet.config                     Solution-level feed config
```

The **build script at the repo root** — `build-windows.ps1` — is the one-shot for local contributors. CI inlines its own steps for toolchain control.

## Runtime facts

- **DLL entry point:** `dimmy_lib.dll` is loaded via `DimmyNative.cs` P/Invoke. Dropped next to `Dimmy.Windows.exe` in the publish folder.
- **Single-instance guard:** `Global\DimmySingleInstance` mutex. A second launch pings the first instance and exits.
- **Configuration:** `%APPDATA%\dimmy\config.json`. **The Rust core is the only writer.** UI calls `dimmy_set_config_json()` and re-reads.
- **Keys:** `%APPDATA%\dimmy\keys.enc` (AES-256-GCM). Managed entirely by the Rust keystore.
- **Logs:** `%LOCALAPPDATA%\dimmy\logs\dimmy.log`, `crash.log`, `ptt.log`.
- **Installer:** Velopack (`--framework vcredist143-x64`). VC Redist goes to System32; the app folder stays lean. See I4 and I10 in [`windows-ci.md`](../../docs/dev/windows-ci.md).

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

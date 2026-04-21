# platforms/linux

The Linux native UI. GTK4 + libadwaita, in Rust, as a direct crate dependency on the Dimmy core.

- **Big picture:** [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md)
- **Build:** [`../../docs/BUILD.md`](../../docs/BUILD.md#linux)
- **Historical design spec:** [`../../docs/superpowers/specs/2026-03-25-linux-native-ui-design.md`](../../docs/superpowers/specs/2026-03-25-linux-native-ui-design.md)

## What lives here

```
Cargo.toml                       Crate manifest, depends on core/ via path
assets/                          SVG icons, .desktop file
src/
├── main.rs                      gtk4 App entry
├── state.rs                     Shared state (wraps core's AppState through FFI-less crate import)
├── pill_window.rs               The pill overlay (Wayland layer_shell on Wayland; override-redirect on X11)
├── settings/                    Settings window (libadwaita)
├── onboarding/                  First-run wizard
├── tray.rs                      StatusNotifierItem / AppIndicator
├── hotkey.rs                    X11 and Wayland (portal) hotkey plumbing
├── text_injector.rs             xdotool on X11, wtype / wl-copy on Wayland
└── waveform.rs                  GtkDrawingArea-based waveform renderer
```

No FFI boundary on Linux — the Linux UI crate imports the Rust core directly as a `path` dependency. This is the simplest of the three platforms.

## Runtime facts

- **Configuration:** `~/.config/dimmy/config.json`. Core is the only writer.
- **Keys:** `~/.config/dimmy/keys.enc` (AES-256-GCM).
- **Logs:** `~/.local/share/dimmy/logs/`.
- **Installer:** AppImage. CPU-only whisper.cpp (`local-stt` default feature, no Vulkan) for portability across distros with varying Vulkan loader availability.
- **User wanting GPU on Linux** rebuilds from source with `--features local-stt-vulkan`. Documented in BUILD.md.

## Quick dev loop

```bash
# From this directory
cargo build --release
./target/release/dimmy-linux

# Lint + test (mirrors CI)
cargo clippy -- -D warnings
cargo test
```

The Rust core is rebuilt automatically as part of this build (path dependency). No need to `cd core && cargo build` separately.

## Display server matrix

| Feature | X11 | Wayland |
|---|---|---|
| Pill overlay | Override-redirect window | `gtk4-layer-shell` layer-surface |
| Global hotkey | Raw XGrabKey (direct) | Global Shortcuts portal (user approves) |
| Text injection | `xdotool key` | `wtype` + `wl-copy` fallback |
| Tray | StatusNotifierItem via DBus | Same (DBus protocol, compositor-agnostic) |

On Wayland without Global Shortcuts portal support, the user must approve each hotkey combo via the compositor's prompt. Where the compositor doesn't implement the portal (older GNOME, some Sway setups), the hotkey feature degrades gracefully — the app works, but the hotkey can't be registered.

## Platform-specific gotchas

- **AppImage portability.** Default feature (`local-stt` CPU) is what ships. If you add a feature flag that pulls in a system library not present on Ubuntu 22.04, the AppImage won't run on older distros. Test on at least one older distro before changing the build-appimage step.
- **libadwaita theming.** The settings window uses libadwaita's stock styling — do not override colour schemes. Users' desktop theme bleeds through correctly only if the app stays theme-neutral.
- **GTK4 version.** Ubuntu 22.04 ships GTK 4.6; some newer APIs need 4.10+. Feature-gate any GTK 4.10+ calls and provide a fallback.

## Dependencies the user must install

Documented in [`../../docs/BUILD.md#linux`](../../docs/BUILD.md#linux). Summary:

```
libgtk-4-dev libadwaita-1-dev libasound2-dev libxdo-dev libdbus-1-dev pkg-config cmake
```

For the AppImage build step (in CI), the same packages are installed on `ubuntu-24.04`.

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
├── state.rs                     AppEvent enum + async_channel from background threads to the GTK loop
├── pill_window.rs               The pill overlay (gtk4-layer-shell layer-surface only when built with the
│                                optional `layer-shell` feature and the compositor supports it; otherwise
│                                plain GTK window hints)
├── settings/                    Settings window (AdwPreferencesWindow, 8 pages: general, shortcut,
│                                output, overlay, permissions, stats, debug, about; stats and debug
│                                appear with the Advanced toggle)
├── onboarding/                  First-run wizard (welcome, shortcut, try-it)
├── terms.rs, telemetry_consent.rs  Terms and statistics-consent dialogs at launch
├── tray.rs                      StatusNotifierItem via the `ksni` crate
├── hotkey.rs                    Backend detection only (portal on Wayland, "x11" otherwise). STUB:
│                                register_hotkey / unregister_hotkey do nothing yet
├── text_injector.rs             Paste method detection: wtype, then ydotool on Wayland; xdotool on X11;
│                                clipboard-only (arboard) as last resort
└── waveform.rs                  GtkDrawingArea-based waveform renderer
```

No FFI boundary on Linux — the Linux UI crate imports the Rust core directly as a `path` dependency (`dimmy_lib`). The core's own keyboard hook is a no-op on Linux (`core/src/hotkey.rs`), and `main.rs` only calls `detect_hotkey_backend()`, so this UI has no working global hotkey and no meeting window yet. Treat it as an experimental prototype.

## Runtime facts

- **Configuration:** `~/.config/dimmy/config.json`. Core is the only writer.
- **Keys:** `~/.config/dimmy/keys.enc` (AES-256-CTR + HMAC-SHA256).
- **Logs:** `~/.config/dimmy/dimmy.log` (the core writes it into the config dir).
- **Installer:** AppImage (`release.yml`, job `build-linux`). CPU-only whisper.cpp (`local-stt` default feature of the core, no Vulkan) for portability across distros with varying Vulkan loader availability.
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
| Pill overlay | Plain GTK window (type hints) | `gtk4-layer-shell` layer-surface, only in a `--features layer-shell` build (the AppImage build does not enable it) |
| Global hotkey | Planned: XGrabKey. Not implemented (stub) | Planned: Global Shortcuts portal (`ashpd` is a dependency). Not implemented (stub) |
| Text injection | `xdotool key` | `wtype`, then `ydotool`; clipboard-only if neither is installed |
| Tray | StatusNotifierItem via DBus | Same (DBus protocol, compositor-agnostic) |

The hotkey rows describe the intended design. Today `register_hotkey` only logs and returns Ok, so no global hotkey fires on either display server.

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

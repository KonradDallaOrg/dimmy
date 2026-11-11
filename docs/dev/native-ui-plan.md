# Native UI — status & platform equivalents

> **Status, checked against the code on 2026-11-11:** Windows (`platforms/windows/Dimmy.Windows`) and macOS (`platforms/macos/Dimmy`) are shipping native UIs. Linux (`platforms/linux`, GTK4 + libadwaita) exists and the release workflow builds an AppImage, but it is an experimental prototype: `platforms/linux/src/hotkey.rs` only detects the backend and its `register_hotkey` is a stub, the core's Linux keyboard hook (`core/src/hotkey.rs`) is a no-op, and there is no meeting UI. The Tauri/HTML `src/` tree this plan mentions no longer exists. Counts below were refreshed from the repo (221 `dimmy_*` exports in `core/tests/fixtures/abi_exports.txt`).

> This file was the original "Native UI Plan". All 4 phases are now implemented and shipping. It now serves as a **status reference + cross-platform equivalence map**. For per-platform dev notes, see `platforms/{windows,macos,linux}/README.md`. For the big-feature implementation plans that drove the rewrite, see `docs/superpowers/plans/` and `docs/superpowers/specs/`.

## Status Summary

| Phase | Platform | Status | Files | Tests |
|-------|----------|--------|-------|-------|
| 0 | Rust FFI (`ffi.rs`) | COMPLETE | 221 `dimmy_*` exports (ABI-snapshotted) | ~1,478 lib + ~199 integration `#[test]` attributes |
| 1 | Windows (WinUI3/C#) | IMPLEMENTED & shipping | ~134 .cs files | ~410 C# `[Fact]`/`[Theory]` attributes |
| 2 | macOS (SwiftUI) | IMPLEMENTED & shipping | — | 261 XCTest funcs |
| 3 | Linux (GTK4+libadwaita) | EXPERIMENTAL (builds on CI, AppImage built; hotkey stub, no meeting UI) | — | crate tests on CI |

## Architecture

```
macOS:    SwiftUI native app  ──┐
Windows:  WinUI3 / C# app    ──┤──→  Rust core (FFI / IPC)  ──→  STT/LLM providers
Linux:    GTK4 / Adwaita app  ──┘    (audio, preprocess, transcribe, llm, keystore)
```

The Rust core (`audio.rs`, `preprocess.rs`, `transcribe.rs`, `llm.rs`, `keystore.rs`, `provider.rs`) is shared by all hosts. The native UIs replaced the old `src/` (HTML/CSS/JS) + Tauri window management, which is gone from the repo.

## IPC Bridge (Rust ↔ Native UI)

Direct FFI — compile Rust core as `cdylib`, expose C API, call from Swift/C#/GTK. The Rust lib already has `crate-type = ["staticlib", "cdylib", "rlib"]`.

## Phase Order

1. **Phase 0** — Rust C FFI layer (`ffi.rs`) — COMPLETE (40+ tests, assertions, NaN safety)
2. **Phase 1** — Windows native (WinUI3/C#) — IMPLEMENTED (~410 C# tests, builds & runs in VS2026)
3. **Phase 2** — macOS native (SwiftUI) — IMPLEMENTED (builds & runs)
4. **Phase 3** — Linux native (GTK4+libadwaita) — PROTOTYPE (builds on CI, AppImage built; hotkey registration is a stub)

## Current state per platform

Windows and macOS ship with their own first-class native UI. Linux has an experimental GTK4 UI (see the status block above). For the per-platform tour, see the platform READMEs — those are the source of truth for what's there and how it's wired. This file is just the cross-platform map.

| Surface | Windows | macOS | Linux |
|---|---|---|---|
| Pill overlay | WinUI 3 `PillWindow` (transparent, topmost, tool window) | SwiftUI panel (transparent, draggable) | GTK4 keep-above window |
| System tray (right of clock) | `Shell_NotifyIcon` + WinUI `MenuFlyout` with submenus | `NSStatusItem` + native `NSMenu` with submenus | StatusNotifierItem (DBus) |
| Taskbar / Dock presence | Anchor window + `ITaskbarList3` overlay icon + amplitude bar (max of mic+sys) + jump list | Dock toggle (`showInDock` → `NSApp.setActivationPolicy`) + LSUIElement | (n/a — Linux has no Dock concept) |
| Settings UI | WinUI 3 `SettingsWindow` (NavigationView) | SwiftUI **Tahoe v3** (`MacSettingsContainerView`, 14 tabs, default ON) | GTK4 `Adw.PreferencesWindow` (8 pages) |
| Onboarding | WinUI 3 `OnboardingWindow` | SwiftUI `OnboardingContainerView` (6-step) | GTK4 onboarding |
| Meeting mode (long-form record + recap) | `Views/MeetingWindow.xaml` + `MarkdownRenderer` + `TranscriptRenderer` + `Services/MeetingPostProcessService.cs` | `Views/Meeting/MeetingViewModel.swift` + `MeetingIdleView` / `MeetingRecordingView` / `MeetingProcessingView` / `MeetingDoneView` / `MeetingSidebar` / `AudioPlaybackBar` / `WavPeaks` / `Services/MeetingPostProcessService.swift` | (not wired — meeting feature is Win + Mac only for now) |
| Pill ↔ meeting routing | The core's `meeting_state` event (`AppViewModel`) drives the pill and taskbar; `Services/MeetingPostProcessService.cs` triggers recap on pill Stop | `meeting_state` event via `DimmyCore.handleEvent` plus one initial sync in `Controllers/PillWindowController.swift`; Mac `MeetingPostProcessService.swift` mirror | (n/a) |
| Cross-platform UI prefs | `ui_prefs.json` + `config.json` | `UserDefaults` + `config.json` | `config.json` only (no UI-prefs store found in `platforms/linux/src`) |
| Update mechanism | Velopack (auto-update + delta) | DMG, updated in place by Sparkle (`Services/UpdateService.swift`) | AppImage (manual) |

The "all features must work cross-platform" rule from CLAUDE.md applies to **functionality**. Each platform may surface that functionality through idiomatic native chrome (NSMenu vs MenuFlyout vs popover menu). Today the Linux UI does not yet cover the meeting feature or a working global hotkey.

## Platform Equivalents

### Windows
| macOS | Windows |
|-------|---------|
| NSStatusItem | NotifyIcon (system tray) |
| NSPanel (floating) | WinRT Window (TopMost, transparent) |
| NSEvent global monitor | SetWindowsHookEx low-level keyboard hook (`core/src/hotkey.rs`) |
| NSPasteboard | Clipboard API |
| CGEvent (Cmd+V) | SendInput (Ctrl+V) |
| NSVisualEffectView | Mica / Acrylic (WinUI3) |
| AXIsProcessTrusted | Not needed (SendInput always works) |
| AVCaptureDevice (permission status only) | None: audio is captured by the Rust core (cpal), no Windows permission API is called |

### Linux
| macOS | Linux |
|-------|-------|
| NSStatusItem | StatusNotifierItem (DBus) / AppIndicator |
| NSPanel | GTK Window (keep-above, composited) |
| NSEvent global monitor | Planned: XGrabKey (X11) / Global Shortcuts portal (Wayland); not implemented, `platforms/linux/src/hotkey.rs` is a stub |
| NSPasteboard | `arboard` crate (`platforms/linux/src/text_injector.rs`) |
| CGEvent | xdotool (X11) / wtype, then ydotool (Wayland); clipboard-only as last resort |
| NSVisualEffectView | GTK CSS + compositor |

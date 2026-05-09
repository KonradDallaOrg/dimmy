# Native UI — status & platform equivalents

> This file was the original "Native UI Plan". All 4 phases are now implemented and shipping. It now serves as a **status reference + cross-platform equivalence map**. For per-platform dev notes, see `platforms/{windows,macos,linux}/README.md`. For the big-feature implementation plans that drove the rewrite, see `docs/superpowers/plans/` and `docs/superpowers/specs/`.

## Status Summary

| Phase | Platform | Status | Files | Tests |
|-------|----------|--------|-------|-------|
| 0 | Rust FFI (`ffi.rs`) | COMPLETE | ~76 exports (ABI-snapshotted) | ~411 lib + ~88 integration |
| 1 | Windows (WinUI3/C#) | IMPLEMENTED & shipping | ~40 | ~100 C# tests |
| 2 | macOS (SwiftUI) | IMPLEMENTED & shipping | — | 69 XCTest funcs |
| 3 | Linux (GTK4+libadwaita) | IMPLEMENTED & shipping | — | crate tests on CI |

## Architecture

```
macOS:    SwiftUI native app  ──┐
Windows:  WinUI3 / C# app    ──┤──→  Rust core (FFI / IPC)  ──→  STT/LLM providers
Linux:    GTK4 / Adwaita app  ──┘    (audio, preprocess, transcribe, llm, keystore)
```

The Rust core (`audio.rs`, `preprocess.rs`, `transcribe.rs`, `llm.rs`, `keystore.rs`, `provider.rs`) stays as-is. Each native UI replaces the current `src/` (HTML/CSS/JS) + Tauri window management.

## IPC Bridge (Rust ↔ Native UI)

Direct FFI — compile Rust core as `cdylib`, expose C API, call from Swift/C#/GTK. The Rust lib already has `crate-type = ["staticlib", "cdylib", "rlib"]`.

## Phase Order

1. **Phase 0** — Rust C FFI layer (`ffi.rs`) — COMPLETE (40+ tests, assertions, NaN safety)
2. **Phase 1** — Windows native (WinUI3/C#) — IMPLEMENTED (41 C# tests, builds & runs in VS2026)
3. **Phase 2** — macOS native (SwiftUI) — IMPLEMENTED (builds & runs)
4. **Phase 3** — Linux native (GTK4+libadwaita) — IMPLEMENTED (builds on CI, AppImage available)

## Current state per platform

All three platforms are shipping with their own first-class native UI. For the per-platform tour, see the platform READMEs — those are the source of truth for what's there and how it's wired. This file is just the cross-platform map.

| Surface | Windows | macOS | Linux |
|---|---|---|---|
| Pill overlay | WinUI 3 `PillWindow` (transparent, topmost, tool window) | SwiftUI panel (transparent, draggable) | GTK4 keep-above window |
| System tray (right of clock) | `Shell_NotifyIcon` + WinUI `MenuFlyout` with submenus | `NSStatusItem` + native `NSMenu` with submenus | StatusNotifierItem (DBus) |
| Taskbar / Dock presence | Anchor window + `ITaskbarList3` overlay icon + amplitude bar (max of mic+sys) + jump list | Dock toggle (`showInDock` → `NSApp.setActivationPolicy`) + LSUIElement | (n/a — Linux has no Dock concept) |
| Settings UI | WinUI 3 `SettingsWindow` (NavigationView) | SwiftUI **Tahoe v3** (`MacSettingsContainerView`, 9 pages, default ON) | GTK4 `Adw.PreferencesWindow` |
| Onboarding | WinUI 3 `OnboardingWindow` | SwiftUI `OnboardingContainerView` (4-step) | GTK4 onboarding |
| Meeting mode (long-form record + recap) | `Views/MeetingWindow.xaml` + `MarkdownRenderer` + `TranscriptRenderer` + `Services/MeetingPostProcessService.cs` | `Views/Meeting/MeetingViewModel.swift` + `MeetingIdleView` / `MeetingRecordingView` / `MeetingProcessingView` / `MeetingDoneView` / `MeetingSidebar` / `AudioPlaybackBar` / `WavPeaks` / `Services/MeetingPostProcessService.swift` | (not wired — meeting feature is Win + Mac only for now) |
| Pill ↔ meeting routing | `App.xaml.cs` polls `dimmy_meeting_is_active` (every 500 ms) + `Services/MeetingPostProcessService.cs` triggers recap on pill Stop | `Controllers/PillWindowController.swift` 500 ms `NSTimer` + Mac `MeetingPostProcessService.swift` mirror | (n/a) |
| Cross-platform UI prefs | `ui_prefs.json` + `config.json` | `UserDefaults` + `config.json` | gsettings + `config.json` |
| Update mechanism | Velopack (auto-update + delta) | DMG (manual) | AppImage (manual) |

The "all features must work cross-platform" rule from CLAUDE.md applies to **functionality**. Each platform may surface that functionality through idiomatic native chrome (NSMenu vs MenuFlyout vs popover menu), but no feature is exclusive to one OS.

## Platform Equivalents

### Windows
| macOS | Windows |
|-------|---------|
| NSStatusItem | NotifyIcon (system tray) |
| NSPanel (floating) | WinRT Window (TopMost, transparent) |
| NSEvent global monitor | RegisterHotKey / SetWindowsHookEx |
| NSPasteboard | Clipboard API |
| CGEvent (Cmd+V) | SendInput (Ctrl+V) |
| NSVisualEffectView | Mica / Acrylic (WinUI3) |
| AXIsProcessTrusted | Not needed (SendInput always works) |
| AVCaptureDevice | MediaCapture API |

### Linux
| macOS | Linux |
|-------|-------|
| NSStatusItem | StatusNotifierItem (DBus) / AppIndicator |
| NSPanel | GTK Window (keep-above, composited) |
| NSEvent global monitor | XDotool / XCB / libinput |
| NSPasteboard | xclip / wl-copy |
| CGEvent | xdotool key / wtype |
| NSVisualEffectView | GTK CSS + compositor |

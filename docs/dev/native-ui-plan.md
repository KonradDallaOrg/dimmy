# Native UI Plan

Replacing the Tauri WebView frontend with native UIs per platform.

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

1. **Phase 0** — Rust C FFI layer (`ffi.rs`) — COMPLETED (40 tests, assertions, NaN safety)
2. **Phase 1** — Windows native (WinUI3/C#) — IN PROGRESS
3. **Phase 2** — macOS native (SwiftUI)
4. **Phase 3** — Linux native (GTK4)

## SwiftUI Mockup (in `mockup/dimmy-new/`)

### What's production-ready (60%):
- AppState (ObservableObject, all published properties)
- Menu bar (NSStatusItem + NSPopover with status/settings)
- Pill overlay (NSPanel, borderless, floating, transparent, draggable)
- Pill animations (rainbow gradient border, pulsing glow, waveform bars)
- Hotkey detection (global + local NSEvent monitors, double-tap logic)
- Push-to-talk vs Toggle mode (visual distinction: blue pulsing vs green steady + stop button)
- 4-step onboarding (Welcome → Permissions → Shortcut → Try It)
- Settings (6 tabs: General, Shortcut, Output, Overlay, Permissions, About)
- Text injection (clipboard save/restore + CGEvent Cmd+V simulation)
- Dark/light mode (NSVisualEffectView + .hudWindow material)
- Position persistence (UserDefaults)

### What's placeholder (40%):
- Audio recording → replaced by AudioSimulator (fake waveform + funny Italian text)
- STT transcription → no provider integration
- LLM post-processing → no integration
- Some settings toggles hardcoded (Launch at login, Show in Dock, Overlay opacity/style)
- Preference persistence incomplete (only shortcut + position saved)

### Key design specs:
- Pill sizes: Idle 120×36pt (30% opacity) → Recording 200-220×44pt (100%, rainbow border)
- Waveform: 7 bars, 12fps update, smooth interpolation
- Completion: green checkmark, spring animation, 1.0s
- Global scale factor: 1.15× (all UI elements 15% larger than default)
- Menu bar: dynamic icon (outline idle, filled+red recording, check+green completing)

## Critical Gaps (SwiftUI mockup vs WebView production)

**Tier 1 — Missing core functionality (must fix before replacing WebView):**
1. Provider/model selector (Groq 3, OpenAI 3, Deepgram 2, Gemini 2, Custom) — users can't change STT provider
2. API key management in settings — only onboarding has key entry
3. Full LLM styles (13 vs only 3 in mockup) + tone + translate + custom prompt
4. LLM provider/endpoint/model/key management
5. Audio device selector
6. Chunk streaming UI + chunk dots progress
7. Transcription prompt field
8. Stats display (words, time, saved)
9. Pill states: transcribing, LLM processing, error (only idle/recording/completing in mockup)
10. Pill elements: dot colors (13 per style), timer, device name, status text

**Tier 2 — UI gaps:**
- Compact mode toggle, keyring toggle, realtime preview, audio debug, LLM logging
- Onboarding skips provider/key/language/style setup (only 4 steps vs WebView 6)
- i18n (hardcoded English in mockup)

**New in mockup (not in WebView — keep these):**
- Launch at login toggle
- Show in Dock toggle
- Post-processing toggles (filler words, punctuation, capitalization)
- Clipboard restore toggle
- Paste method selector (Cmd+V vs keystrokes)
- Idle opacity selector
- Border animation style selector (Rainbow, Blue pulse, Green, None)
- Waveform style selector (Bars, Line, Dots)
- Reset position button
- Pill intro rainbow glow during onboarding

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

# platforms/macos

The macOS native UI. SwiftUI + Xcode, calling the Rust core via C FFI (static link).

- **Big picture:** [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md)
- **Build:** [`../../docs/BUILD.md`](../../docs/BUILD.md#macos)
- **Known FFI gotchas:** [`../../docs/dev/known-bugs.md`](../../docs/dev/known-bugs.md) (MACOS-001, MACOS-002, MACOS-003)

## What lives here

```
Dimmy/                           Main app
├── DimmyApp.swift               @main entry — Settings scene branches on
│                                appState.useTahoeSettings (default true)
├── AppDelegate.swift            NSApplicationDelegate — lifecycle, applyActivationPolicy
├── DimmyFFI.h                   C header for the Rust core FFI
├── Views/
│   ├── PillView.swift               Floating overlay
│   ├── MenuBarPopover.swift         Legacy SwiftUI popover (unreferenced — see below)
│   ├── OnboardingContainerView.swift
│   └── Settings/
│       ├── MacAtoms.swift           Tahoe v3 building blocks (MacTile, MacRow,
│       │                            MacGroupLabel, MacNote, MacKeycap, …)
│       ├── MacSettingsContainerView.swift   v3 sidebar + 9 tabs + toolbar
│       ├── MacHomePage.swift / MacVoicePage.swift / MacOutputPage.swift
│       ├── MacPillPage.swift / MacShortcutPage.swift / MacPrivacyPage.swift
│       ├── MacRulesPage.swift / MacAboutPage.swift / MacAdvancedPage.swift
│       ├── MacPermissionsPage.swift
│       └── *Settings*View.swift     Legacy v1/v2 (used when useTahoeSettings=false)
├── Controllers/
│   └── StatusBarController.swift    NSStatusItem owner — native NSMenu (NOT popover)
│                                    with Translate-to / Style submenus
├── Managers/                    Services: HotkeyManager, DimmyCore, PermissionsManager
├── State/                       AppState ObservableObject + UserDefaults persistence
│                                (showInDock, showInMenuBar, useTahoeSettings, etc.)
├── Utilities/                   Extensions, helpers, SelfTests
├── Assets.xcassets/             App icons, ClaudeMark, DimmyLogo
├── Dimmy.entitlements           Microphone, Accessibility, Accessory mode
├── Info.plist                   LSUIElement=true (accessory), privacy usage strings

Dimmy.xcodeproj/                 Xcode project
DimmyTests/                      XCTest unit tests
dmg-assets/                      DMG installer background + layout
```

## Runtime facts

- **FFI linkage:** the Rust core is compiled to `libdimmy_lib.a` (static). Xcode links it into the app binary. `DimmyFFI.h` is the bridging header.
- **Target:** `aarch64-apple-darwin` (Apple Silicon). Intel builds are not shipped.
- **Configuration:** `~/.config/dimmy/config.json`. The Rust core is the only writer.
- **Mac-only UI prefs:** `UserDefaults` (NOT in `config.json`) for OS-specific behaviours that don't cross platforms — `showInDock`, `showInMenuBar`, `useTahoeSettings`. Same reasoning Windows uses for `ui_prefs.json`.
- **Keys:** `~/.config/dimmy/keys.enc` (AES-256-GCM).
- **Logs:** `~/Library/Logs/dimmy/`.
- **Installer:** DMG (see [`dmg-assets/`](dmg-assets) for layout). First launch needs right-click → Open or `xattr -d com.apple.quarantine /Applications/Dimmy.app` because we don't have a Developer ID.

## Quick dev loop

```bash
# 1. Rebuild the Rust core (only when core/ changes)
cd ../../core
cargo build --release --lib --target aarch64-apple-darwin --features local-stt-metal,local-llm-metal
rm -f target/aarch64-apple-darwin/release/libdimmy_lib.dylib   # force static link

# 2. Open Xcode and Cmd+R
cd ../platforms/macos
open Dimmy.xcodeproj
```

Tests: `Cmd+U` in Xcode, or `xcodebuild test -project Dimmy.xcodeproj -scheme Dimmy -destination "platform=macOS"`.

## Platform-specific gotchas

- **`objc_msgSend` must NOT be declared variadic.** Stack-based args on ARM64 cause PAC failure at runtime. CI builds cross-compile and don't catch it. Declare as `fn objc_msgSend()` (no args) and `std::mem::transmute` to typed pointers per call signature. See MACOS-001.
- **`kCFTypeDictionaryKeyCallBacks`** must be `static ... : [u8; 0]`, not `u8`. Use `.as_ptr()`. See MACOS-002.
- **Explicit framework links.** `hotkey.rs` needs CoreGraphics and CoreFoundation explicitly listed in the `#[link(framework = ...)]` attrs.
- **macOS 26 Tahoe compatibility.** `tao 0.34.5` crashes on macOS 26 with `transparent: true` (upstream tao#1171). The project disabled that flag and sets transparency manually. Do NOT re-enable `transparent: true` until tao upstream fixes. See MACOS-003.
- **`local-llm-metal` requires `dynamic-link`.** llama.cpp dylibs are bundled into `Dimmy.app/Contents/Frameworks/` and codesigned. If you build outside Xcode and the dylibs aren't present, llama.cpp won't find symbols at runtime. Always build macOS via Xcode.
- **Permissions.** Microphone (always needed). Accessibility (needed only for the paste-on-transcribe path; without it, the user would have to paste manually). Onboarding polls `AXIsProcessTrusted()` every 10 s.

## Session chaining

Recordings > 60 s are transparently chunked at the SwiftUI level. This is a UX-layer thing; the Rust core sees one long buffer regardless.

## Dock / menu bar

`LSUIElement=true` in `Info.plist` ships the app as an accessory by default — no Dock icon, lives in the menu bar (`NSStatusItem`).

`AppDelegate.applyActivationPolicy()` toggles between `.regular` (in Dock + Cmd-Tab) and `.accessory` (menu-bar only) based on `appState.showInDock`. `appState.showInMenuBar` independently shows/hides the `NSStatusItem` itself. At least one of the two should be on so the user can always reach the app — the UI doesn't enforce this hard-block (it's a soft "if both off, you only have the pill"). Both toggles live in **Settings → Advanced → Appearance** in the Tahoe v3 UI.

The status-bar menu is a **native `NSMenu`** (rebuilt on every open via `NSMenuDelegate.menuNeedsUpdate`) — NOT an `NSPopover` like the v1 implementation. Items:
- Status row (disabled label)
- Native: `<STT input lang>` (read-only — input language stays in Settings → Voice)
- **Translate to →** submenu of all translate targets (LLM output target, current value checkmarked via `NSMenuItem.state = .on`)
- **Style →** submenu of `LlmStyle.allCases` with checkmarks
- Shortcut: `<combo>` (read-only)
- Settings… (⌘,) and Quit Dimmy (⌘Q)

Pickers write `appState.llmTranslateTo` / `appState.llmStyle` and persist via `DimmyCore.shared.setConfig(appState.toRustConfig())` — same single-writer path the rest of the UI uses.

`MenuBarPopover.swift` is the legacy SwiftUI popover replaced by the native menu. It's left in the source tree (unreferenced, no allocations) to avoid touching `Dimmy.xcodeproj`'s file references — can be removed via Xcode UI in a follow-up.

## Tahoe v3 settings

The redesigned settings UI (default ON via `useTahoeSettings`). 9 pages wired through `MacSettingsContainerView.swift`:

| Page | Purpose |
|---|---|
| Home | Hero + stats + current setup snapshot |
| Voice | STT mode, provider, API key, mic device, audio processing |
| Output | LLM mode, style chip-flow, tone, translate, custom prompt, paste options |
| Pill | Live preview, position, border + waveform style |
| Shortcut | Hotkey display, mode (PTT/Toggle), HotkeyStatus badge |
| Privacy | Telemetry toggles, anonymous ID, feedback form |
| App rules | Per-app style rules (foreground capture not yet wired) |
| About | Hero icon, "Check for updates…", release notes link |
| Advanced | Show in Dock + Show in menu bar, Metal acceleration, Diagnostics, Reset |
| Permissions | Mic / Accessibility status + grant flow |

Building blocks live in `MacAtoms.swift` (`MacTile`, `MacRow`, `MacGroupLabel`, `MacNote`, `MacKeycap`, …) — the look is curated for macOS 26 Tahoe (Liquid-glass sidebar, chevron toolbar, browser-model navigation history). The legacy `*SettingsView.swift` files are still in the tree for the `useTahoeSettings=false` fallback (which QA can pin to validate regressions).

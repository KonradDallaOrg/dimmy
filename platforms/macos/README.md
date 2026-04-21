# platforms/macos

The macOS native UI. SwiftUI + Xcode, calling the Rust core via C FFI (static link).

- **Big picture:** [`../../docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md)
- **Build:** [`../../docs/BUILD.md`](../../docs/BUILD.md#macos)
- **Known FFI gotchas:** [`../../docs/dev/known-bugs.md`](../../docs/dev/known-bugs.md) (MACOS-001, MACOS-002, MACOS-003)

## What lives here

```
Dimmy/                           Main app
├── DimmyApp.swift               @main entry, scene configuration
├── AppDelegate.swift            NSApplicationDelegate — lifecycle hooks
├── DimmyFFI.h                   C header for the Rust core FFI
├── Views/                       SwiftUI views (PillView, SettingsView, OnboardingView)
├── Controllers/                 Non-UI control flow
├── Managers/                    Services: hotkey, tray, text injection, audio
├── State/                       AppState ObservableObject + persisted preferences
├── Utilities/                   Extensions, helpers
├── Assets.xcassets/             App icons, colour sets, imagesets
├── Dimmy.entitlements           Microphone, Accessibility, Accessory mode
├── Info.plist                   LSUIElement (accessory), privacy usage strings

Dimmy.xcodeproj/                 Xcode project
DimmyTests/                      XCTest unit tests
dmg-assets/                      DMG installer background + layout
```

## Runtime facts

- **FFI linkage:** the Rust core is compiled to `libdimmy_lib.a` (static). Xcode links it into the app binary. `DimmyFFI.h` is the bridging header.
- **Target:** `aarch64-apple-darwin` (Apple Silicon). Intel builds are not shipped.
- **Configuration:** `~/.config/dimmy/config.json`. The Rust core is the only writer.
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

`LSUIElement = true` — the app is an accessory, no Dock icon. Lives in the menu bar (`NSStatusItem`). Settings open in an `NSPopover` or a detached `NSWindow` depending on user preference.

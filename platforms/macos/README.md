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
├── AppDelegate.swift            NSApplicationDelegate — lifecycle, applyActivationPolicy.
│                                Single-instance guard bypassed under XCTest
│                                (XCTestConfigurationFilePath env var).
├── DimmyFFI.h                   C header for the Rust core FFI
├── Views/
│   ├── PillView.swift               Floating overlay
│   ├── MeetingView.swift            Meeting window host (delegates to Meeting/ subviews)
│   ├── Meeting/                     v0.6.31 long-form meeting UI
│   │   ├── MeetingViewModel.swift           State machine + FFI orchestration
│   │   ├── MeetingIdleView.swift            Pre-record state
│   │   ├── MeetingRecordingView.swift       Timer / chunks / Pause / Stop / Back-to-live
│   │   ├── MeetingProcessingView.swift      Stepwise checkmarks + spinner
│   │   ├── MeetingDoneView.swift            Recap cards (the 11 sections in
│   │   │                                    MeetingPostProcessService.sectionKeys)
│   │   │                                    + transcript playhead
│   │   ├── MeetingSidebar.swift             Past meetings list (search + delete)
│   │   ├── MeetingSpeakers.swift            Speaker model + DiarizationService
│   │   ├── MeetingAmplitudeAGC.swift        Live amplitude normalisation
│   │   ├── AudioPlaybackBar.swift           AVAudioPlayer + click-drag seek
│   │   └── WavPeaks.swift                   Centre-mirrored amplitude peaks
│   │                                        (400 buckets, AudioPlaybackBar)
│   ├── MenuBarPopover.swift         Legacy SwiftUI popover (unreferenced — see below)
│   ├── Onboarding/                  OnboardingContainerView (6 steps: welcome,
│   │                                permissions, shortcut, model_download, try_it,
│   │                                stay_updated)
│   ├── Wizards/                     CommandWizardView, MeetingWizardView
│   └── Settings/
│       ├── MacAtoms.swift           Tahoe v3 building blocks (MacTile, MacRow,
│       │                            MacGroupLabel, MacNote, MacKeycap, …)
│       ├── MacSettingsContainerView.swift   v3 sidebar + 14 tabs + toolbar
│       ├── MacHomePage.swift / MacVoicePage.swift / MacOutputPage.swift (recap-model picker)
│       ├── MacProvidersPage.swift / MacIntegrationsPage.swift / MacLicensePage.swift
│       ├── MacPillPage.swift / MacShortcutPage.swift / MacPrivacyPage.swift
│       ├── MacRulesPage.swift / MacAboutPage.swift / MacAdvancedPage.swift (tab "Debug")
│       ├── MacPermissionsPage.swift
│       ├── Mac*Card.swift / *ConnectSheet.swift   Integration cards and connect sheets
│       └── *Settings*View.swift     Legacy v1/v2 (used when useTahoeSettings=false);
│                                    HistorySettingsView is still the Recordings tab of v3
├── Controllers/
│   ├── StatusBarController.swift    NSStatusItem owner — native NSMenu (NOT popover)
│   │                                with Translate-to / Style submenus
│   ├── PillWindowController.swift   Pill window. Meeting state arrives through the
│   │                                `meeting_state` event (DimmyCore.handleEvent) plus
│   │                                one initial sync at init — no polling timer
│   └── MeetingWindowController.swift Window lifecycle (decoupled from FFI —
│                                    closing doesn't stop the recording)
├── Services/
│   └── MeetingPostProcessService.swift  Recap pipeline mirror of Win impl
│                                (also CallDetectionManager, SystemAudioCaptureService,
│                                UpdateService (Sparkle), TelegramService, …)
├── Managers/                    HotkeyManager (rc=-7 silent no-op), DictHotkeyManager,
│                                TextInjector, DimmyCore (+V2, +License, +CallDetect, … extensions)
├── State/                       AppState ObservableObject + UserDefaults persistence
│                                (showInDock, showInMenuBar, useTahoeSettings,
│                                recap_model_override, etc.). Also holds PermissionsManager.
├── Utilities/
│   └── RecapModel.swift             Curated picker list + resolve fallthrough
├── Assets.xcassets/             App icons, ClaudeMark, DimmyLogo
├── Dimmy.entitlements           audio-input, screencapture, disable-library-validation
├── Info.plist                   LSUIElement=true (accessory), privacy usage strings,
│                                Sparkle keys (SUFeedURL)

Dimmy.xcodeproj/                 Xcode project (XCTest target wired in pbxproj —
                                 productType bundle.unit-test, ad-hoc signing)
DimmyTests/                      XCTest unit tests (261 test funcs in 21 files), e.g.
                                 AppStateTests, MeetingPostProcessServiceTests,
                                 RecapModelTests, MeetingHistoryRowTests,
                                 SystemAudioProcessTapDecisionTests
dmg-assets/                      DMG installer background + layout
```

## Runtime facts

- **FFI linkage:** the Rust core is compiled to `libdimmy_lib.a` (static). Xcode links it into the app binary. `DimmyFFI.h` is the bridging header.
- **Target:** `aarch64-apple-darwin` (Apple Silicon). Intel builds are not shipped.
- **Configuration:** `~/Library/Application Support/dimmy/config.json` (`dirs::config_dir()` on macOS; the folder name comes from `DimmyCore.configDirName`, so the staging tester build uses `dimmy-staging`). The Rust core is the only writer.
- **Mac-only UI prefs:** `UserDefaults` (NOT in `config.json`) for OS-specific behaviours that don't cross platforms — `showInDock`, `showInMenuBar`, `useTahoeSettings`. Same reasoning Windows uses for `ui_prefs.json`.
- **Keys:** `~/Library/Application Support/dimmy/keys.enc` (AES-256-CTR + HMAC-SHA256).
- **Logs:** `~/Library/Application Support/dimmy/dimmy.log` (the core writes it into the config dir). A few card error strings still say `~/Library/Logs/Dimmy/dimmy.log`; that path is not written.
- **Installer:** DMG (see [`dmg-assets/`](dmg-assets) for layout), updated in place by Sparkle (`Services/UpdateService.swift`, `SUFeedURL` in `Info.plist`). `release.yml` signs with a Developer ID identity and notarizes when the `MACOS_CERT_P12` secret is present; without it the DMG is ad-hoc signed and first launch needs right-click → Open or `xattr -d com.apple.quarantine /Applications/Dimmy.app`.

## Quick dev loop

```bash
# 1. Rebuild the Rust core (only when core/ changes)
cd ../../core
cargo build --release --lib --target aarch64-apple-darwin --features local-stt-metal,local-stt-coreml,local-llm-metal,local-stt-parakeet-coreml,local-stt-parakeet-fluid,local-dfn,license-client,telegram,local-stt-qwen
rm -f target/aarch64-apple-darwin/release/libdimmy_lib.dylib   # force static link

# 2. Open Xcode and Cmd+R
cd ../platforms/macos
open Dimmy.xcodeproj
```

The feature list above is `MAC_FEATURES` from `scripts/dev/preflight-mac.sh`. Before pushing, run that script: it rebuilds the static lib with this set, runs `xcodebuild` and launches the app so the `SelfTests.runAtLaunch` assertions fire. For a signed-in-place dev install see [`../../docs/dev/macos-development.md`](../../docs/dev/macos-development.md).

Tests: `Cmd+U` in Xcode, or `xcodebuild test -project Dimmy.xcodeproj -scheme Dimmy -destination "platform=macOS"`.

## Platform-specific gotchas

- **`objc_msgSend` must NOT be declared variadic.** Stack-based args on ARM64 cause PAC failure at runtime. CI builds cross-compile and don't catch it. Declare as `fn objc_msgSend()` (no args) and `std::mem::transmute` to typed pointers per call signature. See MACOS-001. (No `objc_msgSend` call remains in the repo today; the rule applies if one is added.)
- **`kCFTypeDictionaryKeyCallBacks`** must be `static ... : [u8; 0]`, not `u8`. Use `.as_ptr()`. See MACOS-002. (Likewise, no use remains today.)
- **Explicit framework links.** `hotkey.rs` needs CoreGraphics and CoreFoundation explicitly listed in the `#[link(framework = ...)]` attrs.
- **macOS 26 Tahoe compatibility (historical).** `tao` is no longer a dependency of any `Cargo.toml` in the repo (the SwiftUI app owns all windows), so the old `tao` / `transparent: true` crash from the Tauri era (MACOS-003 in `known-bugs.md`) cannot occur here.
- **`local-llm-metal` requires `dynamic-link`.** llama.cpp dylibs are bundled into `Dimmy.app/Contents/Frameworks/` and codesigned. If you build outside Xcode and the dylibs aren't present, llama.cpp won't find symbols at runtime. Always build macOS via Xcode.
- **Permissions.** Microphone (always needed). Accessibility (needed only for the paste-on-transcribe path; without it, the user would have to paste manually). `PermissionsManager` (in `State/AppState.swift`) re-reads microphone, Accessibility and Input Monitoring state every 5 s and whenever the app becomes active (macOS has no notification for trust changes).

## Long recordings

The SwiftUI layer does not chunk audio. Chunked transcription lives in the Rust core (`chunked_stt.rs` for chunked and streaming dictation, `meeting.rs` for meetings).

## Dock / menu bar

`LSUIElement=true` in `Info.plist` ships the app as an accessory by default — no Dock icon, lives in the menu bar (`NSStatusItem`).

`AppDelegate.applyActivationPolicy()` toggles between `.regular` (in Dock + Cmd-Tab) and `.accessory` (menu-bar only) based on `appState.showInDock`. `appState.showInMenuBar` independently shows/hides the `NSStatusItem` itself. At least one of the two stays on: the `AppState` setters turn the other one back on if both would be off. Both toggles live in **Settings → Pill overlay → Visibility** in the Tahoe v3 UI (`MacPillPage.swift`).

The status-bar menu is a **native `NSMenu`** (rebuilt on every open via `NSMenuDelegate.menuNeedsUpdate`) — NOT an `NSPopover` like the v1 implementation. Items:
- Status row (disabled label)
- Native: `<STT input lang>` (read-only — input language stays in Settings → Voice)
- **Translate to →** submenu of all translate targets (LLM output target, current value checkmarked via `NSMenuItem.state = .on`)
- **Style →** submenu of `LlmStyle.allCases` with checkmarks
- Command, Shortcut: `<combo>` (read-only), show/hide pill, start/stop recording, Open Meeting…, start/stop meeting recording
- Settings… (⌘,) and Quit Dimmy (⌘Q)

Pickers write `appState.llmTranslateTo` / `appState.llmStyle` and persist via `DimmyCore.shared.setConfig(appState.toRustConfig())` — same single-writer path the rest of the UI uses.

`MenuBarPopover.swift` is the legacy SwiftUI popover replaced by the native menu. It's left in the source tree (unreferenced, no allocations) to avoid touching `Dimmy.xcodeproj`'s file references — can be removed via Xcode UI in a follow-up.

## Tahoe v3 settings

The redesigned settings UI (default ON via `useTahoeSettings`). 14 tabs wired through `MacSettingsContainerView.swift` (`MacSettingsTab`). Pill overlay, App rules, Recordings and Debug show only while the sidebar "Advanced" toggle (`appState.showAdvanced`) is on; the Windows nav gates the same four pages:

| Page | Purpose |
|---|---|
| Home | Hero + stats + current setup snapshot, file transcribe, meeting mode, system group |
| Voice | STT mode, provider, mic device, audio processing, speakers (diarization), custom dictionary, vocabulary |
| Output | LLM mode and provider, rewrite style, meeting storage + recap export, meeting recap model, pasteboard, advanced LLM |
| Providers & keys | One card per AI provider, one key per provider (`MacProvidersPage`) |
| Pill overlay | Live preview, position, border + waveform style, Show in Dock + Show in menu bar |
| Shortcut | Recording / dictionary / command / meeting shortcuts, mode (PTT/Toggle), HotkeyStatus badge |
| Recordings | History list with Raw / Enhanced text and audio replay, audio retention (`HistorySettingsView`) |
| Integrations | Claude, OpenAI, Gemini and Kimi subscription cards, Calendar, Notion, Confluence, Claude Desktop (MCP) |
| Privacy & data | Telemetry toggles, anonymous ID, feedback form, AI transparency, resources |
| App rules | Per-app style rules; the foreground app is captured by `Helpers/AppContextCapture.swift` and sent with `dimmy_set_app_context` |
| License | Buy / activate / devices (`MacLicensePage`) |
| About | Hero icon, "Check for updates…", update channel, setup wizards, resources, legal |
| Permissions | Mic / Accessibility status + grant flow (Mac-only) |
| Debug (`MacAdvancedPage`) | Auto-recap, Metal acceleration note, Diagnostics, Reset |

Building blocks live in `MacAtoms.swift` (`MacTile`, `MacRow`, `MacGroupLabel`, `MacNote`, `MacKeycap`, …) — the look is curated for macOS 26 Tahoe (Liquid-glass sidebar, chevron toolbar, browser-model navigation history). The legacy `*SettingsView.swift` files are still in the tree for the `useTahoeSettings=false` fallback (which QA can pin to validate regressions).

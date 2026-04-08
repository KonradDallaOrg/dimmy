# Linux Native UI (Phase 3) — Design Spec

## Goal

Build a native Linux UI for Dimmy using GTK4 + libadwaita in Rust (gtk4-rs). Full feature parity with Windows (WinUI3/C#) and macOS (SwiftUI). Primary target: Ubuntu + GNOME. Must handle Wayland restrictions.

## Architecture

### Direct Rust access — no FFI

Unlike Windows/macOS which call the C FFI layer (`ffi.rs`), the Linux UI is written in Rust and calls business logic directly:

```
platforms/linux/ (gtk4-rs)
    ↓ direct Rust calls
core/src/ → AppState, audio.rs, transcribe.rs, llm.rs, preprocess.rs
```

No JSON serialization, no C string conversion, no buffer management. Full access to `Result<T, E>`, `String`, `Arc<Mutex<T>>`.

### Tauri dependency isolation (CRITICAL)

`core/Cargo.toml` has `tauri` as a hard dependency. The Linux crate must NOT pull in Tauri transitively.

**Solution: feature-gate Tauri in `core/Cargo.toml`**

```toml
[features]
default = ["tauri-runtime"]
tauri-runtime = ["tauri", "tauri-plugin-clipboard-manager"]

[dependencies]
tauri = { version = "2", features = ["tray-icon", "devtools"], optional = true }
tauri-plugin-clipboard-manager = { version = "2", optional = true }
```

All `#[tauri::command]` functions and the `run()` entry point in `lib.rs` go behind `#[cfg(feature = "tauri-runtime")]`. The business logic modules (`audio.rs`, `preprocess.rs`, `transcribe.rs`, `llm.rs`, `provider.rs`, `error.rs`, `keystore.rs`) and `AppState` remain unconditional.

The Linux crate depends on:
```toml
dimmy_lib = { path = "../../core", default-features = false }
```

This gives access to all business logic without Tauri, GTK, or WebKit.

### AppState initialization for Linux

Today `AppState` is constructed inside `lib.rs::run()` which is Tauri-specific. The Linux binary needs an equivalent init path.

**Solution**: Extract a public `AppState::new_standalone()` method that:
1. Loads config from `~/.config/dimmy/config.json` (XDG on Linux)
2. Initializes keystore (libsecret/GNOME Keyring via `use_keyring`, or encrypted file)
3. Spawns audio capture thread (cpal)
4. Returns `AppState` ready for use

The existing `dimmy_init()` in `ffi.rs` and `run()` in `lib.rs` can be refactored to call this same `new_standalone()` internally.

### Async bridge: tokio ↔ GTK main loop

The Rust backend uses `tokio` for async HTTP (STT, LLM). GTK4 has its own `glib::MainLoop`. These must not block each other.

**Pattern:**
```
main.rs:
  1. Create tokio runtime (multi-thread) in background thread
  2. Create glib::MainContext::channel() → (Sender<AppEvent>, Receiver<AppEvent>)
  3. Rust async operations (transcribe, LLM) run on tokio, send results via Sender
  4. GTK main loop receives via Receiver, updates UI on main thread

AppEvent enum:
  - RecordingStarted
  - AmplitudeUpdate(f32)
  - TranscriptionProgress { current: u32, total: u32 }
  - TranscriptionComplete(String)
  - LlmComplete(String)
  - Error(String)
  - StyleChanged(String)
  - ToneChanged(String)
```

This replaces the FFI's `dimmy_set_event_callback`. No C function pointers — typed Rust enum over a glib channel.

### Crate structure

```
platforms/linux/
├── Cargo.toml              # depends on dimmy_lib (default-features = false)
├── src/
│   ├── main.rs             # entry: init tokio + gtk4, create AppState, launch app
│   ├── app.rs              # DimmyApplication (AdwApplication subclass)
│   ├── state.rs            # AppStateBridge: glib channel, AppEvent enum, sync logic
│   ├── pill_window.rs      # floating overlay (gtk4-layer-shell)
│   ├── waveform.rs         # custom DrawingArea widget (5 styles)
│   ├── settings/
│   │   ├── mod.rs          # AdwPreferencesWindow container
│   │   ├── general.rs      # General tab (language, API key, theme, advanced STT)
│   │   ├── shortcut.rs     # Shortcut tab (recorder + mode picker)
│   │   ├── output.rs       # Output tab (LLM style/provider/clipboard)
│   │   ├── overlay.rs      # Overlay tab (position, border, waveform style)
│   │   ├── permissions.rs  # Permissions tab (mic, text injection tools)
│   │   ├── stats.rs        # Stats tab (words, time, time saved)
│   │   ├── debug.rs        # Debug tab (simulation, audio health, state dump)
│   │   └── about.rs        # About tab (version, update check, links)
│   ├── onboarding/
│   │   ├── mod.rs          # AdwCarousel-based wizard container
│   │   ├── welcome.rs      # Step 1: welcome + description
│   │   ├── shortcut.rs     # Step 2: shortcut recorder + mode
│   │   └── tryit.rs        # Step 3: live test with pill
│   ├── tray.rs             # system tray via ksni or zbus fallback
│   ├── hotkey.rs           # global hotkeys (portal + X11 fallback)
│   └── text_injector.rs    # clipboard + simulated Ctrl+V
├── assets/
│   ├── dimmy.svg           # scalable app icon
│   ├── dimmy-22.png        # tray icon 22x22
│   ├── dimmy-24.png        # tray icon 24x24
│   ├── dimmy-48.png        # app icon 48x48
│   ├── dimmy-recording.svg # tray: recording state
│   ├── dimmy-done.svg      # tray: done state
│   └── com.dimmy.app.desktop  # XDG .desktop file
```

### Build

```bash
cd platforms/linux
cargo build --release
# Output: target/release/dimmy-linux
```

Cargo.toml dependency:
```toml
[dependencies]
dimmy_lib = { path = "../../core", default-features = false }
gtk4 = "0.9"
libadwaita = "0.7"
gtk4-layer-shell = "0.4"
ksni = "0.2"                    # StatusNotifierItem (tray) — fallback: zbus direct
ashpd = "0.10"                  # xdg-desktop-portal (hotkeys, permissions)
x11rb = { version = "0.13", optional = true }  # X11 hotkey fallback
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
log = "0.4"
env_logger = "0.11"

[features]
default = ["x11-fallback"]
x11-fallback = ["x11rb"]
```

Note: exact versions TBD at implementation time; these are indicative.

## UI Components — Feature Parity Map

### 1. Pill Overlay Window

**What it does**: Floating transparent capsule showing recording state, waveform, timer.

**Implementation**: `gtk4-layer-shell` positions the window as a Wayland layer surface (always-on-top, no focus steal, all workspaces). Falls back to standard GtkWindow hints on X11.

**Minimum compositor**: GNOME 44+ (Mutter layer-shell support), KDE Plasma 5.27+, Sway, wlroots-based.

**States** (identical to Windows/macOS):

| State | Visual | Border | Duration |
|-------|--------|--------|----------|
| Idle | Circle with colored dot | User-selected style | Persistent |
| Idle (hover) | Expands: language + shortcut + gear icon | Same | While hovering |
| Recording | Capsule: waveform + timer (MM:SS) + stop btn | Animated (rainbow or solid pulse) | Until stop |
| Transcribing | Capsule: spinner + "Transcribing..." + chunk counter | Blue | Until done |
| Processing | Capsule: spinner + "Processing..." | Purple | Until done |
| Completing | Circle: green checkmark | Green | 1.2s auto |
| Error | Capsule: error icon + truncated message | Red | 3s auto |

**Interactions**:
- Drag to reposition (save position in config)
- Hover idle: expand to show info
- Right-click: context menu (Settings, Hide)
- Scroll on dot: cycle LLM styles
- Scroll on language: cycle languages

**Waveform widget** (custom `gtk4::DrawingArea` with Cairo):
- Bars: 7 vertical bars, center-weighted heights, weights [0.3, 0.5, 0.7, 1.0, 0.7, 0.5, 0.3]
- Bars Center: bars grow from center line
- Bars Round: 4px wide with rounded corners
- Line: smooth polyline connecting 7 sample points
- Dots: 5 pulsing circles, weights [0.4, 0.7, 1.0, 0.7, 0.4]
- Animation: 12Hz update timer, per-bar random jitter for organic motion
- Height range: 3–16px for bars, 3–10px for dots

**Border animation**:
- Rainbow: rotating angular gradient via Cairo (360 rotation, 2.5s loop)
- Solid colors: pulsing opacity (0.5–1.0) via `adw::TimedAnimation`
- Colors: Blue #38bdf8, Green #4ade80, Purple #a78bfa, Orange #fb923c, None #3c3c3c

**LLM style dot colors** (for pill and settings):
| Style | Hex |
|-------|-----|
| off | #41B0B1 |
| correct | #2dd4bf |
| summarize | #fbbf24 |
| elaborate | #4ade80 |
| comprehensible | #38bdf8 |
| professional | #f472b6 |
| prompt | #a78bfa |
| genz | #e879f9 |
| boomer | #f97316 |
| emoji | #facc15 |
| acronyms | #22d3ee |
| imbruttito | #ef4444 |
| custom | #fb923c |

**Transparency**:
- GTK4 + Layer Shell supports transparent backgrounds natively
- CSS: `window { background: transparent; }` + RGBA drawing in Cairo

### 2. Settings Window

**What it does**: 8-tab preferences window with Advanced toggle.

**Implementation**: `adw::PreferencesWindow` with `adw::PreferencesPage` per tab. Sidebar navigation via libadwaita's built-in tab system.

**Tabs and fields** (exact parity with Windows/macOS):

#### Tab 1: General
- Language: `adw::ComboRow` — Auto, Italiano, English, Espanol, Francais, Deutsch, Portugues
- API Key: `adw::PasswordEntryRow` with status indicator (green check / red x)
- Theme: `adw::ComboRow` — Auto, Light, Dark (sets `adw::StyleManager` color scheme)
- Launch at login: `adw::SwitchRow` (via XDG autostart .desktop file)
- **Advanced section** (`adw::ExpanderRow` or conditional visibility):
  - STT Provider: `adw::ComboRow` — 11 presets (Groq x3, OpenAI x3, Deepgram x2, Gemini x2, Custom)
  - Custom URL: `adw::EntryRow` (visible when Custom selected)
  - Custom model: `adw::EntryRow` (visible when Custom selected)
  - Prompt/Vocabulary: `gtk4::TextView` in scrolled window
  - Audio input device: `adw::ComboRow` (populated from Rust)
  - Microphone volume: `adw::ActionRow` + `gtk4::Scale` (10-100%, step 5)
  - Preprocessing: `adw::SwitchRow`
  - Chunk streaming: `adw::SwitchRow`
  - Use keyring: `adw::SwitchRow`

#### Tab 2: Shortcut
- Shortcut recorder: custom widget
  - **Wayland**: Opens xdg-desktop-portal GlobalShortcuts dialog (compositor-managed)
  - **X11**: Direct key capture (listen for key events, display combo)
  - Preset buttons: common combos (Ctrl+Alt, Ctrl+Shift, Alt+Shift, Super)
  - Validation: 2+ modifiers OR F-key
- Mode: `adw::ComboRow` — Push-to-Talk / Toggle

#### Tab 3: Output
- LLM Enhancement toggle: `adw::SwitchRow`
- LLM Style: `adw::ComboRow` — 13 styles with colored dots
- LLM Provider: `adw::ComboRow` — 8 presets (Groq, OpenAI, OpenRouter x2, Gemini, Anthropic x2, Custom)
- Custom URL/model: `adw::EntryRow` (visible when Custom)
- Use same API key: `adw::SwitchRow`
- LLM API Key: `adw::PasswordEntryRow` (visible when separate key)
- Keep in clipboard: `adw::SwitchRow`
- **Advanced**:
  - Tone: `adw::ComboRow` — None, Formal, Friendly, Concise, Academic
  - Translate to: `adw::ComboRow` — None, EN, IT, DE, FR, ES
  - Custom prompt: `gtk4::TextView` (visible when style = custom)

#### Tab 4: Overlay
- Show overlay: `adw::SwitchRow` — toggle pill visibility
- Idle opacity: `adw::ComboRow` — Nearly invisible, Subtle, Visible
- Position: `adw::ComboRow` — Top Right, Top Left, Bottom Right, Bottom Left, Bottom Center, Top Center
- Reset position: `gtk4::Button`
- Border style: `adw::ComboRow` — Rainbow, Blue, Green, Purple, Orange, None
- Waveform style: `adw::ComboRow` — Bars, Bars Center, Bars Round, Line, Dots

#### Tab 5: Permissions (Linux-specific)
- Microphone: status check (PipeWire/PulseAudio device access)
- Text injection tools: status check for `wtype`/`ydotool` (Wayland) or `xdotool` (X11)
  - If missing: show install instructions per distro (apt/dnf/pacman)
  - `ydotool` note: requires `input` group membership or root
- Autostart: status of XDG autostart file
- "Open System Settings" button for audio settings

#### Tab 6: Stats (Advanced only)
- Total words dictated: `adw::ActionRow` with large number
- Total speaking time: `adw::ActionRow` formatted as Xh Ym / Xm Ys
- Time saved estimate: `adw::ActionRow` with formula explanation
- Formula: `words * (1/40 - 1/150) * 60` seconds

#### Tab 7: Debug (Advanced only)
- Simulate recording cycle: `gtk4::Button` (8s test: record→transcribe→process→complete)
- Audio debug logging: `adw::SwitchRow`
- LLM request logging: `adw::SwitchRow`
- Audio health check: `gtk4::Button` + result display
- Current state dump: read-only info rows

#### Tab 8: About
- App icon (Dimmy logo)
- "Dimmy" title + version (from Cargo.toml via `env!("CARGO_PKG_VERSION")`)
- "Voice dictation that stays out of your way"
- Update check button (GitHub API)
- Links: GitHub repo, releases
- "Made with irony"

**Window size**: ~720x560px (matches Windows), resizable.

**Save/Cancel**: Bottom bar with Save (suggested action) + Cancel buttons. Save writes directly to AppState + config file via Rust.

### 3. Onboarding Wizard

**What it does**: First-run setup wizard, 3 steps.

**Implementation**: `adw::Window` with `adw::Carousel` (swipeable pages) + progress dots.

**Window size**: 520x440px, non-resizable.

**Steps**:

1. **Welcome**
   - Dimmy icon (64px)
   - "Dimmy" title
   - "Voice dictation that stays out of your way"
   - "Hold a shortcut, speak, release. Your words appear wherever you're typing."
   - "Get Started" button

2. **Shortcut**
   - "Your Shortcut" title
   - Shortcut recorder (same widget as settings — portal on Wayland, direct on X11)
   - Preset buttons
   - Mode picker: Push-to-Talk / Toggle
   - Back / Next

3. **Try It**
   - "Hold [SHORTCUT] and say something"
   - Live pill shown during test
   - Transcript display area
   - On success: green checkmark + "You're all set!"
   - "Start Using Dimmy" closes wizard, starts normal mode

Note: No permissions step needed. Linux doesn't require explicit mic permission grants like macOS — if PipeWire/PulseAudio is running, mic access works. Text injection tool availability is checked in Permissions settings tab.

### 4. System Tray

**What it does**: Status icon in system tray with context menu.

**Implementation**: `ksni` crate (Rust StatusNotifierItem via D-Bus). If `ksni` proves insufficient, fallback to implementing StatusNotifierItem protocol directly with `zbus` (already a transitive dependency via `ashpd`).

Works on:
- GNOME: requires AppIndicator extension (pre-installed on Ubuntu)
- KDE: native support
- XFCE/Cinnamon: native support

**Icons**: SVG primary, PNG fallback (22x22 and 24x24 for tray, 48x48 for app).

| State | Icon | Description |
|-------|------|-------------|
| Idle | dimmy.svg | Waveform circle |
| Recording | dimmy-recording.svg | Red dot / waveform |
| Transcribing | dimmy.svg (blue tint) | Processing indicator |
| Done | dimmy-done.svg | Green checkmark |

**Context menu** (matches Windows):
```
● Status (Ready / Recording...)
─────────────────
Language: [it/en/es/...]
Style: [off/correct/...]
Shortcut: [Ctrl+Alt/...]
─────────────────
Show/Hide Pill
Settings...
─────────────────
Quit Dimmy
```

**Left-click**: Toggle pill visibility.

### 5. Global Hotkeys

**What it does**: System-wide keyboard shortcut to start/stop recording.

**Implementation** (dual-path):

**Wayland** (primary): `ashpd` crate → xdg-desktop-portal `GlobalShortcuts` interface.
- App requests shortcut registration via D-Bus portal
- **GNOME 45+**: compositor shows its own dialog — user confirms/picks the shortcut
- **KDE 5.27+**: allows programmatic registration
- Portal handles all the Wayland security
- The shortcut recorder in onboarding/settings opens this portal dialog on Wayland (not direct key capture)

**X11** (fallback): `x11rb` crate → XGrabKey.
- For users running X11 session (still common on older Ubuntu, Debian)
- Direct key grab + key capture in recorder widget
- No portal needed

**Detection**: Check `$XDG_SESSION_TYPE` at startup → "wayland" or "x11".

### 6. Text Injection

**What it does**: Paste transcribed text into the active application.

**Implementation** (priority order):

**Wayland**:
1. `wtype` (primary) — uses `virtual-keyboard-unstable-v1` protocol, no root needed
2. `ydotool` (fallback) — requires `input` group or root (writes to `/dev/uinput`)
3. Clipboard-only (last resort) — copy to clipboard, user pastes manually

**X11**:
1. `xdotool` — simulates Ctrl+V keypress
2. Clipboard-only (fallback)

**Clipboard access**: `wl-copy`/`wl-paste` (Wayland) or `xclip` (X11). Can also use `arboard` Rust crate for cross-session clipboard.

Detection at startup: check which tools are available via `which`, log chosen method. Permissions tab shows status and install instructions.

If `keep_in_clipboard` is true: skip paste simulation, just copy.

## Wayland vs X11 Compatibility Matrix

| Feature | Wayland Solution | X11 Solution | Fallback |
|---------|-----------------|--------------|----------|
| Overlay positioning | gtk4-layer-shell | Window hints | Centered, user drags |
| Always on top | Layer: overlay | WM hints | Best-effort |
| Global hotkey | xdg-desktop-portal (GNOME 45+, KDE 5.27+) | XGrabKey | Settings prompt |
| Shortcut recorder | Portal dialog (compositor-managed) | Direct key capture | Manual entry |
| Text paste | wtype + wl-copy | xdotool + xclip | Clipboard only |
| Transparency | Native (GTK4 + CSS) | Native (GTK4 RGBA) | Opaque fallback |
| Tray icon | StatusNotifierItem | StatusNotifierItem | No tray, pill only |

## Packaging & Distribution

### AppImage (primary)
- Single file, runs on any distro
- Bundle: dimmy-linux binary + libadwaita/GTK4 libs + icon + .desktop file
- Tool: `linuxdeploy` + `linuxdeploy-plugin-gtk`
- User downloads, `chmod +x`, runs

### .deb (Ubuntu/Debian)
- For apt-based distros
- Package: binary + .desktop file + icon + systemd user service (optional autostart)
- Dependencies: `libgtk-4-1`, `libadwaita-1-0`, `pipewire`
- Recommends: `wtype` (Wayland paste), `xdotool` (X11 paste)

### CI Integration
- Add Linux build to `staging-native.yml` and `release.yml`
- Ubuntu 24.04 runner with GTK4/libadwaita dev packages
- Build steps: `apt install libgtk-4-dev libadwaita-1-dev` → `cargo build --release`
- Package: AppImage + .deb

### System requirements
- Ubuntu 22.04+ / Fedora 38+ / Debian 12+ / Arch (current)
- GTK 4.10+ and libadwaita 1.3+ (for PreferencesWindow features)
- GNOME 44+ recommended (layer-shell), GNOME 45+ for portal hotkeys
- PipeWire or PulseAudio (audio capture)
- Optional: wtype or ydotool (Wayland paste), xdotool (X11 paste)

## Testing Strategy

### Unit tests (Rust, in-crate)
- State bridge: verify AppEvent round-trips, glib channel delivery
- Hotkey detection: X11 vs Wayland path selection based on env
- Text injection: tool detection logic (which, PATH lookup)
- Config serialization round-trips

### Integration tests
- GTK widget creation (headless via `xvfb-run` for CI)
- Settings load/save cycle via AppState
- Onboarding flow state machine
- Pill state transitions (all 7 states)

### Manual testing matrix

| Distro | DE | Display | Priority |
|--------|----|---------|----------|
| Ubuntu 24.04 LTS | GNOME 46 | Wayland | P0 |
| Ubuntu 24.04 LTS | GNOME 46 | X11 | P1 |
| Fedora 40 | GNOME 46 | Wayland | P1 |
| Debian 12 | GNOME 43 | Wayland | P2 |
| Manjaro | KDE Plasma 6 | Wayland | P2 |

## Implementation Order

### Step 1: Scaffold + Tauri feature-gate
- Feature-gate Tauri in `core/Cargo.toml` (tauri-runtime feature)
- Add `#[cfg(feature = "tauri-runtime")]` to Tauri-specific code in `lib.rs`
- Create `AppState::new_standalone()` public constructor
- Create `platforms/linux/` crate with `dimmy_lib` (default-features = false)
- Verify `cargo build` compiles without Tauri
- Verify existing `cargo tauri build` still works (regression check)
- CI: add Linux build job

### Step 2: Hotkey + recording pipeline (validate Wayland first)
- Wayland portal hotkeys (ashpd) + X11 fallback (x11rb)
- Start/stop recording → AppState direct calls
- Text injection (wtype/ydotool/xdotool detection + execution)
- Validate the full pipeline: hotkey → record → transcribe → paste
- Console output first (no UI yet) — de-risks Wayland unknowns early

### Step 3: Pill overlay (core UX)
- Transparent window via gtk4-layer-shell
- 7-state machine (idle → recording → transcribing → processing → completing → error)
- Waveform widget (start with "Bars" style)
- Border animation (rainbow + solid colors)
- Drag to reposition
- Amplitude polling (12Hz) via glib channel from tokio

### Step 4: Settings window
- AdwPreferencesWindow with all 8 tabs
- All fields mapped to AppState (direct Rust read/write)
- Advanced toggle (hides/shows Debug, Stats tabs + advanced sections)
- Save/Cancel with direct Rust writes to config file

### Step 5: Onboarding wizard
- 3-step carousel
- Shortcut recorder widget (portal on Wayland, direct on X11)
- Live test with pill

### Step 6: System tray
- ksni StatusNotifierItem (or zbus fallback)
- Context menu (status, language, style, shortcut, settings, quit)
- Icon state changes per recording state

### Step 7: Remaining waveform styles + polish
- All 5 waveform styles (Bars, Bars Center, Bars Round, Line, Dots)
- All border animations smooth
- Hover effects on pill
- Scroll interactions (cycle style/language)
- Idle opacity setting

### Step 8: Packaging + CI
- AppImage build script (linuxdeploy)
- .deb package
- GitHub Actions Linux build job (Ubuntu 24.04 runner)
- Release workflow integration (tag → build → publish)
- .desktop file + icon installation paths

## Non-goals (explicitly out of scope)

- Flatpak/Snap packaging (can add later if requested)
- KDE-native look (GTK4 on KDE is acceptable, not alien)
- Wayland compositor-specific code (rely on portals and layer-shell only)
- Custom window decorations (use libadwaita defaults)
- i18n / translations (hardcoded English, same as macOS — can add gettext later)
- `core_api.rs` abstraction layer (YAGNI — call business logic directly)

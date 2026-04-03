# Dimmy Windows Native UI — Design Spec

**Date:** 2026-03-21
**Branch:** `feat/native-ui`
**Framework:** WinUI 3 + .NET 8 + Windows App SDK
**Reference:** macOS SwiftUI mockup (`mockup/dimmy-new/`)
**FFI:** Rust `dimmy.dll` (cdylib) via P/Invoke

---

## 1. Architecture

```
native-ui/windows/Dimmy.Windows/
├── Dimmy.Windows.csproj
├── App.xaml / App.xaml.cs           ← Lifecycle, single instance, startup routing
├── Assets/
│   ├── dimmy.ico                    ← Tray icon (16/32/48px)
│   ├── dimmy-recording.ico          ← Red variant
│   └── dimmy-done.ico               ← Green variant
├── Interop/
│   └── DimmyNative.cs               ← P/Invoke for all 20 FFI functions
├── Services/
│   ├── TrayService.cs               ← System tray (H.NotifyIcon or WinForms NotifyIcon)
│   ├── HotkeyService.cs             ← RegisterHotKey Win32 API
│   └── TextInjectionService.cs      ← Clipboard + SendInput(Ctrl+V)
├── ViewModels/
│   ├── AppViewModel.cs              ← Central state, wraps DimmyNative
│   ├── SettingsViewModel.cs
│   └── OnboardingViewModel.cs
├── Views/
│   ├── PillWindow.xaml              ← Transparent overlay, always-on-top
│   ├── OnboardingWindow.xaml        ← 3-step wizard
│   ├── SettingsWindow.xaml          ← NavigationView sidebar + pages
│   └── Controls/
│       ├── WaveformControl.xaml     ← Animated audio bars
│       └── ShortcutRecorder.xaml    ← Keyboard shortcut capture
├── Helpers/
│   └── WindowHelper.cs              ← Transparent window, drag, positioning
└── Strings/
    ├── en/Resources.resw
    └── it/Resources.resw
```

### Build flow

1. `cargo build --release --lib` → produces `dimmy.dll` (cdylib)
2. `dotnet build` in `native-ui/windows/` → produces `Dimmy.Windows.exe`
3. `dimmy.dll` is copied to output dir via `.csproj` post-build step

### Dependencies

- Windows App SDK 1.5+
- .NET 8
- H.NotifyIcon.WinUI (NuGet) for system tray
- CommunityToolkit.Mvvm (NuGet) for MVVM pattern

---

## 2. P/Invoke Layer (DimmyNative.cs)

Wraps all 20 FFI functions from `ffi.rs`:

```csharp
public static class DimmyNative
{
    private const string DLL = "dimmy.dll";

    // Lifecycle
    [DllImport(DLL)] public static extern int dimmy_init();
    [DllImport(DLL)] public static extern void dimmy_shutdown();

    // Callback
    public delegate void EventCallback(IntPtr jsonPtr);
    [DllImport(DLL)] public static extern void dimmy_set_event_callback(EventCallback cb);

    // Recording
    [DllImport(DLL)] public static extern int dimmy_start_recording();
    [DllImport(DLL)] public static extern int dimmy_stop_recording(byte[] buf, int bufLen);
    [DllImport(DLL)] public static extern void dimmy_cancel_recording();

    // Config
    [DllImport(DLL)] public static extern int dimmy_get_config_json(byte[] buf, int bufLen);
    [DllImport(DLL)] public static extern int dimmy_set_config_json([MarshalAs(UnmanagedType.LPUTF8Str)] string json);

    // Audio
    [DllImport(DLL)] public static extern float dimmy_get_amplitude();
    [DllImport(DLL)] public static extern int dimmy_list_devices_json(byte[] buf, int bufLen);

    // LLM
    [DllImport(DLL)] public static extern void dimmy_cycle_llm_style(int direction);
    [DllImport(DLL)] public static extern void dimmy_cycle_llm_tone(int direction);

    // Stats
    [DllImport(DLL)] public static extern int dimmy_update_stats(int words, double speakingSecs);

    // Utility
    [DllImport(DLL)] public static extern int dimmy_has_api_key();
    [DllImport(DLL)] public static extern int dimmy_is_recording();
}
```

Event callback receives JSON strings on a Rust thread. Must marshal to UI thread via `DispatcherQueue.TryEnqueue()`.

---

## 3. Onboarding (3 steps, macOS-style)

Window: 520×440px, centered, no resize, title bar hidden (ExtendsContentIntoTitleBar).

### Step 0: Welcome

- App icon: waveform circle (64px, accent color)
- Title: "Dimmy" (32px, bold, rounded)
- Subtitle: "Voice dictation that stays out of your way" (15px, secondary)
- Description: "Hold a shortcut, speak, release.\nYour words appear wherever you're typing." (13px, secondary)
- Button: "Get Started" (accent filled, 200px wide)
- Progress dots: 3 circles (8px), filled=current+done, unfilled=future

### Step 1: Shortcut

- Title: "Your Shortcut" (28px, bold)
- Subtitle: "Hold to dictate, release to paste" (14px, secondary)
- ShortcutRecorder control:
  - Inactive: shows keycaps (24px semibold) with "+" separators
  - Active (recording): orange background, "Press your shortcut..." text
  - Validation: 2+ modifiers OR single special key
  - Default: Win+Alt
- Mode selector: Push-to-Talk vs Toggle (segmented/radio)
  - Push-to-talk: "hold shortcut, release to paste"
  - Toggle: "double-tap to start, tap again to stop"
- Hint: "Click to change" (12px, tertiary)

### Step 2: Try It!

- Title: "Try it!" (28px, bold)
- Subtitle: "Hold [shortcut] and say something" (14px, secondary, dynamic shortcut display)
- Instruction: "Look at the pill overlay — it will animate while you speak" (12px, tertiary)
- Demo text field: rounded rect (80px height), placeholder "Waiting for your voice..."
- **If no API key**: after recording attempt, show inline message "Set up your API key to start transcribing" with button → opens Settings to General tab
- **If API key present**: real recording → transcription → text appears in demo field
- Success view (after transcription works):
  - Green checkmark icon (56px, spring animation)
  - "You're all set!" (24px, bold)
  - "Dimmy lives in your system tray.\nHold [shortcut] anywhere to dictate."
  - Button: "Start Using Dimmy" → close onboarding, show pill

### Transitions

- Step slides: horizontal slide + opacity fade (0.3s)
- Progress dots: accent=done, secondary opacity 0.3=future

---

## 4. Settings (macOS-style tabs, Advanced toggle)

Window: 620×440px, centered, resizable. NavigationView with left pane (sidebar).

### Global Advanced Toggle

A ToggleSwitch in the sidebar footer: "Advanced". When ON, shows additional controls in every tab + reveals Stats and Debug tabs.

### Tab: General

**Base:**

| Control | Type | Binding |
|---------|------|---------|
| Language | ComboBox | language (IT, EN, ES, FR, DE, PT) |
| Style | ComboBox | llm_style (Off + 12 styles) |
| Theme | Segmented (RadioButtons) | Auto / Light / Dark |
| Launch at login | ToggleSwitch | Windows startup registry |
| Show in taskbar | ToggleSwitch | Window ShowInTaskbar |
| API Key | PasswordBox + hint | api_key (per-provider) |

**Advanced additions:**

| Control | Type | Binding |
|---------|------|---------|
| Provider & Model | ComboBox | api_url + api_model (Groq whisper-large-v3-turbo, OpenAI whisper-1, Deepgram nova-3, Gemini, Custom) |
| Custom endpoint URL | TextBox | api_url (visible when Custom) |
| Custom model | TextBox | api_model (visible when Custom) |
| Prompt / Vocabulary | TextBox multiline | prompt |
| Chunk streaming | ToggleSwitch | chunk_streaming_enabled |
| Audio input device | ComboBox | selected_device (from dimmy_list_devices_json) |
| Preprocessing | ToggleSwitch | preprocessing_enabled |
| Use keyring | ToggleSwitch | use_keyring |
| Compact mode | ToggleSwitch | (UI-only, reduces pill to micro) |

### Tab: Shortcut

**Base:**

| Control | Type | Binding |
|---------|------|---------|
| Current shortcut | ShortcutRecorder control | shortcut |
| Mode | Segmented | shortcut_mode (toggle / hold) |

Same ShortcutRecorder as onboarding. Mode explanations with icons below selector.

### Tab: Output

**Base (from macOS mockup):**

| Control | Type | Binding |
|---------|------|---------|
| Style | ComboBox | llm_style (mirrors General) |
| Remove filler words | ToggleSwitch | (future) |
| Auto-punctuation | ToggleSwitch | (future) |
| Auto-capitalization | ToggleSwitch | (future) |
| Restore clipboard | ToggleSwitch | (future) |
| Paste method | ComboBox | Ctrl+V / Keystrokes |

**Advanced additions:**

| Control | Type | Binding |
|---------|------|---------|
| Tone | ComboBox | llm_tone (None, Formal, Friendly, Concise, Academic) |
| Translate to | ComboBox | llm_translate_to (None, EN, IT, DE, FR, ES) |
| LLM Provider | ComboBox | llm_api_url preset |
| Custom LLM endpoint | TextBox | llm_api_url (visible when Custom) |
| Custom LLM model | TextBox | llm_api_model |
| Use same API key | ToggleSwitch | llm_use_same_key |
| LLM API Key | PasswordBox | llm_api_key (visible when same key OFF) |
| Custom prompt | TextBox multiline | llm_custom_prompt (visible when style=Custom) |

### Tab: Overlay

**Base (from macOS mockup):**

| Control | Type | Binding |
|---------|------|---------|
| Show overlay | ToggleSwitch | (UI-only) |
| Default position | ComboBox | Top Right/Left, Bottom Right/Left/Center |
| Reset position | Button | Clears saved position |
| Idle opacity | ComboBox | Nearly invisible / Subtle / Visible |
| Border style | ComboBox | Rainbow / Blue pulse / Green / None |
| Waveform style | ComboBox | Bars / Line / Dots |

Helper: "You can always drag the pill to reposition it"

### Tab: About

- App icon (48px, accent)
- "Dimmy" (20px, bold, rounded)
- Version from Cargo.toml (12px, secondary)
- "Voice dictation that stays out of your way" (12px, secondary)
- Divider
- "Made with irony" (11px, tertiary)
- Check for updates button (future)

### Tab: Stats (Advanced only)

| Display | Source |
|---------|--------|
| Total words dictated | stats_total_words |
| Total speaking time | stats_total_speaking_secs |
| Time saved estimate | stats_total_speaking_secs × 3 |

### Tab: Debug (Advanced only)

| Control | Type | Binding |
|---------|------|---------|
| LLM log enabled | ToggleSwitch | llm_log_enabled |
| Audio debug | ToggleSwitch | audio_debug_enabled |

### Save/Cancel

Footer with Save + Cancel buttons (like WebView). Save calls `dimmy_set_config_json()`. Cancel discards and closes.

---

## 5. Pill Overlay

Transparent, always-on-top, borderless window. Draggable by background. Uses `DesktopAcrylicController` for blur effect.

### Window properties

- ExtendsContentIntoTitleBar: true
- IsAlwaysOnTop: true
- Background: transparent
- Size: 320×96px (280 content + 20px glow padding each side)
- No taskbar entry
- Show on all virtual desktops
- Draggable from any point

### States

#### Idle
- Dot (8px circle, colored by LLM style — 13 colors from STYLE_COLORS)
- Device name (12px, secondary)
- Settings gear icon button (right-click → context menu)
- Background: acrylic thin material
- Opacity: 0.5 default, 0.95 on hover (0.2s easeInOut)
- On hover: show language + shortcut labels

#### Recording
- Waveform: 7 bars (3px wide, 3–16px height, 1.5px radius, white 90% opacity)
- Bars update from `dimmy_get_amplitude()` polled at ~12 FPS
- Rainbow border: 2px stroke, 9-color gradient, rotates 360° in 2.5s (linear, infinite)
- Triple glow shadows (12/8/4px radius, HSV cycling with border)
- Timer display: MM:SS
- Stop button (toggle mode only): 12×12px white square, corner radius 2.5px
- Background: acrylic thick material (dense, opaque)

#### Transcribing
- Chunk dots progress (animated dots showing current/total chunks)
- Status text: "Transcribing..."

#### LLM Processing
- Status text: "Processing..."
- Spinning indicator

#### Completing
- Green checkmark icon (16px, bold)
- Spring animation: scale 0.3→1.0 (0.25s response, 0.6 damping)
- Green border (1.5px, 40% opacity) + green glow (radius 10)
- Duration: ~1.2s then return to idle

#### Error
- Red status text with error message (truncated 200 chars)
- Returns to idle after 3s

### Rainbow gradient (9 colors)

| Index | Hue | Sat | Bright |
|-------|-----|-----|--------|
| 0 | 0.00 | 0.7 | 1.0 |
| 1 | 0.08 | 0.8 | 1.0 |
| 2 | 0.15 | 0.7 | 1.0 |
| 3 | 0.35 | 0.7 | 0.95 |
| 4 | 0.52 | 0.6 | 1.0 |
| 5 | 0.62 | 0.7 | 1.0 |
| 6 | 0.75 | 0.6 | 1.0 |
| 7 | 0.85 | 0.6 | 1.0 |
| 8 | 0.95 | 0.7 | 1.0 |

### Drag & position persistence

- Drag via PointerPressed/Moved/Released on pill container
- Save position to Windows Registry or AppData JSON
- Default: bottom-right, 100px from screen edge
- Bounds checking: stay within main display visible area

### LLM style dot colors (from WebView)

```
off: #41B0B1, correct: #2dd4bf, summarize: #fbbf24,
elaborate: #4ade80, comprehensible: #38bdf8, professional: #f472b6,
prompt: #a78bfa, genz: #e879f9, boomer: #f97316,
emoji: #facc15, acronyms: #22d3ee, imbruttito: #ef4444, custom: #fb923c
```

---

## 6. System Tray

Using H.NotifyIcon.WinUI NuGet package.

### Icon states

| State | Icon | Tooltip |
|-------|------|---------|
| Idle | dimmy.ico (outline waveform) | "Dimmy — Ready" |
| Recording | dimmy-recording.ico (filled, red tint) | "Dimmy — Recording..." |
| Completing | dimmy-done.ico (checkmark, green) | "Dimmy — Done!" |

### Left-click behavior
Toggle pill window visibility (show/hide).

### Right-click context menu

```
┌─────────────────────┐
│ ● Ready             │  ← Status dot + text
│─────────────────────│
│ Language: Italiano   │
│ Style: Off           │
│ Mode: Push-to-Talk   │
│ Shortcut: Win+Alt    │
│─────────────────────│
│ Settings...          │
│ Quit Dimmy           │
└─────────────────────┘
```

---

## 7. Services

### HotkeyService

Uses Win32 `RegisterHotKey()` API via P/Invoke.

- Registers global hotkey on app start
- On hotkey press: start/stop recording via DimmyNative
- Double-tap detection: 0.4s window for toggle mode
- Minimum hold: 0.15s (prevent accidental triggers)
- Updates when user changes shortcut in settings

### TextInjectionService

1. Save current clipboard content
2. Set transcribed text to clipboard
3. `SendInput()` simulates Ctrl+V keypress
4. After 150ms delay, restore original clipboard

### TrayService

- Creates NotifyIcon with context menu
- Updates icon on recording state change
- Routes Settings/Quit menu clicks to AppViewModel

---

## 8. App Lifecycle

### Startup sequence

1. `App.OnLaunched()` → single instance check
2. `DimmyNative.dimmy_init()` → loads config, keys, spawns audio thread
3. `DimmyNative.dimmy_set_event_callback()` → register C# callback
4. Check `isOnboardingComplete` flag (AppData JSON or Registry)
   - If false → show OnboardingWindow
   - If true → show PillWindow
5. `TrayService.Initialize()` → system tray icon
6. `HotkeyService.Register()` → global hotkey

### Shutdown sequence

1. `HotkeyService.Unregister()` → remove global hotkey
2. `TrayService.Dispose()` → remove tray icon
3. `DimmyNative.dimmy_shutdown()` → save config

### Event callback handling

Rust calls the C# callback on a background thread. The callback:
1. Marshals `IntPtr` → UTF-8 string → JSON
2. Dispatches to UI thread via `DispatcherQueue.TryEnqueue()`
3. Updates AppViewModel properties (recording state, amplitude, chunk progress, errors)

---

## 9. Design Tokens (from macOS mockup, scaled 1.15×)

### Typography
- Title: 32px bold | Headline: 20.7px semibold | Body: 16.1px | Caption: 13.8px | Small: 12.65px

### Spacing
- Small: 9.2px | Medium: 16.1px | Large: 23px

### Corner radii
- Standard: 13.8px | Small: 9.2px

### Animation durations
- Hover: 0.2s easeInOut | Waveform: 0.2s easeOut | Spring: 0.25s/0.6 damping
- Rainbow: 2.5s linear infinite | Tab transitions: system default

---

## 10. i18n

Resource files (.resw) for English and Italian. All user-visible strings externalized. Same string keys as WebView i18n.js where applicable.

---

## 11. What's NOT in scope (Phase 1)

- Auto-updater (handled by Rust core + Tauri updater for now)
- Realtime preview
- Post-processing toggles (filler words, punctuation, capitalization) — UI present but non-functional placeholders
- Overlay opacity/border/waveform style selectors — UI present but fixed to defaults
- Launch at login — UI present but non-functional placeholder

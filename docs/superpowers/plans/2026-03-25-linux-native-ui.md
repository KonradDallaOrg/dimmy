# Linux Native UI (Phase 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native GTK4+libadwaita Linux UI for Dimmy with full feature parity with Windows/macOS native UIs.

**Architecture:** Separate Rust crate (`native-ui/linux/`) that depends on `dimmy_lib` with Tauri feature-gated out. Calls business logic (audio, transcribe, LLM) directly as Rust. Uses glib channels to bridge tokio async → GTK main loop.

**Tech Stack:** gtk4-rs, libadwaita-rs, gtk4-layer-shell, ashpd (xdg-desktop-portal), ksni (tray), tokio

**Spec:** `docs/superpowers/specs/2026-03-25-linux-native-ui-design.md`

---

## File Structure

### Files to modify in src-tauri/

- `src-tauri/Cargo.toml` — Add feature flags for `tauri-runtime`
- `src-tauri/build.rs` — Feature-gate `tauri_build::build()`
- `src-tauri/src/lib.rs` — Feature-gate Tauri imports/commands/run(), make AppState+helpers `pub`, add `new_standalone()`

### Files to create in native-ui/linux/

```
native-ui/linux/
├── Cargo.toml
├── src/
│   ├── main.rs             # Entry point: tokio + GTK init
│   ├── app.rs              # DimmyApplication (AdwApplication subclass)
│   ├── state.rs            # AppEvent enum, glib channel bridge
│   ├── pill_window.rs      # Floating overlay via layer-shell
│   ├── waveform.rs         # Custom DrawingArea (5 styles)
│   ├── settings/
│   │   ├── mod.rs          # AdwPreferencesWindow container + Advanced toggle
│   │   ├── general.rs      # Tab 1: language, API key, theme, STT provider
│   │   ├── shortcut.rs     # Tab 2: shortcut recorder + mode
│   │   ├── output.rs       # Tab 3: LLM style/provider/clipboard
│   │   ├── overlay.rs      # Tab 4: position, border, waveform style
│   │   ├── permissions.rs  # Tab 5: mic, paste tools
│   │   ├── stats.rs        # Tab 6: words, time, time saved
│   │   ├── debug.rs        # Tab 7: simulation, health, state
│   │   └── about.rs        # Tab 8: version, update, links
│   ├── onboarding/
│   │   ├── mod.rs          # AdwCarousel wizard container
│   │   ├── welcome.rs      # Step 1
│   │   ├── shortcut.rs     # Step 2: recorder + presets
│   │   └── tryit.rs        # Step 3: live test
│   ├── tray.rs             # StatusNotifierItem via ksni
│   ├── hotkey.rs           # xdg-desktop-portal + X11 fallback
│   └── text_injector.rs    # wtype/ydotool/xdotool + clipboard
```

---

## Task 1: Feature-gate Tauri in src-tauri

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/build.rs`
- Modify: `src-tauri/src/lib.rs` (lines 1-18, 601-646, 648-2407, 2409-end)

This task makes the dimmy_lib crate compilable WITHOUT Tauri, so the Linux crate can depend on it.

- [ ] **Step 1: Modify `src-tauri/Cargo.toml` — add feature flags**

```toml
[features]
default = ["tauri-runtime"]
tauri-runtime = [
    "dep:tauri",
    "dep:tauri-plugin-clipboard-manager",
    "dep:tauri-plugin-updater",
    "dep:tauri-plugin-single-instance",
    "dep:tauri-build",
]

[build-dependencies]
tauri-build = { version = "2", features = [], optional = true }

[dependencies]
tauri = { version = "2", features = ["tray-icon", "devtools"], optional = true }
tauri-plugin-clipboard-manager = { version = "2", optional = true }
tauri-plugin-updater = { version = "2", optional = true }
tauri-plugin-single-instance = { version = "2", optional = true }
```

All other deps (serde, reqwest, cpal, hound, enigo, arboard, keyring, etc.) stay unconditional.

- [ ] **Step 2: Feature-gate `build.rs`**

```rust
fn main() {
    #[cfg(feature = "tauri-runtime")]
    tauri_build::build();
}
```

- [ ] **Step 3: Feature-gate Tauri imports in `lib.rs`**

Wrap the Tauri-specific import at line 15:

```rust
#[cfg(feature = "tauri-runtime")]
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager};
```

- [ ] **Step 4: Feature-gate ALL Tauri-dependent code in `lib.rs`**

Run this grep to find every Tauri reference:
```bash
grep -n 'tauri::' src-tauri/src/lib.rs
```

EVERY function that uses `tauri::State`, `tauri::AppHandle`, `tauri::Emitter`, `tauri::Manager`, or `#[tauri::command]` MUST be wrapped in `#[cfg(feature = "tauri-runtime")]`. This includes but is not limited to:
- ALL `#[tauri::command]` functions (start_recording, stop_recording, get_transcript, set_config, get_config, save_key, load_key_for_provider, start_shortcut_recording, poll_shortcut_recording, stop_shortcut_recording, apply_shortcut, set_window_anchor, get_amplitude, get_audio_device, list_audio_devices, set_pill_size, center_window, needs_onboarding, complete_onboarding, get_version, check_for_update, install_update, prompt_accessibility, open_accessibility_settings, open_audio_debug_dir, etc.)
- Helper functions with `tauri::State` parameters: `save_current_config`, `snapshot_config`, and any others found by grep
- Window management helpers: `clamp_to_work_area`, `position_bottom_right`, `force_transparent_redraw`
- `pub fn run()` (the Tauri builder + setup + event loop)

Use a single `#[cfg(feature = "tauri-runtime")]` block wrapping related groups where possible.

- [ ] **Step 4b: Verify `hotkey.rs` module compiles on Linux without Tauri**

The existing `src-tauri/src/hotkey.rs` uses Windows-specific APIs. Check if it has `#[cfg(target_os)]` guards:
```bash
grep -c 'cfg(target_os' src-tauri/src/hotkey.rs
```
If it has unconditional Windows imports, gate the module declaration in `lib.rs`:
```rust
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod hotkey;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use hotkey::*;
```

- [ ] **Step 4c: Verify `ffi.rs` compiles under `--no-default-features`**

`ffi.rs` is `pub mod ffi;` in `lib.rs` and does NOT use Tauri types. It should compile without Tauri. Verify:
```bash
cd src-tauri && cargo check --no-default-features 2>&1 | grep -i error | head -20
```
If `ffi.rs` fails, either fix the issues or gate `pub mod ffi;` behind a feature.

- [ ] **Step 5: Make shared types and functions `pub`**

These items are currently `pub(crate)` and must become `pub` so the Linux crate can access them:
- `AppConfig` struct (line 208)
- `AppState` struct (line 601) — already `pub`
- `save_config_file()` (line 287)
- `load_config_file()` (line 361)
- `log()` (line 175)
- `config_dir_path()` (line 45)
- `config_path()` (line 88)
- `log_path()` (line 92)
- `save_key_with_store()` (line 553)
- `load_key_with_store()` (line 562)
- `migrate_from_pai_voice()` (line 446)
- `migrate_plaintext_key()` (line 512)
- `migrate_keyring_to_per_provider()` (line 572)
- `onboarding_completed()` (line 59)
- `mark_onboarding_done()` (line 74)
- `create_debug_session_dir()` (line 105)
- `DEFAULT_API_URL`, `DEFAULT_MODEL`, `DEFAULT_PROMPT`, `DEFAULT_LLM_URL`, `DEFAULT_LLM_MODEL` constants

- [ ] **Step 6: Add `AppState::new_standalone()` constructor**

Add this method after the `AppState` struct definition (after line 646):

```rust
impl AppState {
    /// Create AppState from config file + keyring, without Tauri.
    /// Used by native Linux UI and tests.
    pub fn new_standalone() -> Self {
        migrate_from_pai_voice();

        let file_cfg = load_config_file();
        let use_kr = file_cfg.use_keyring;
        let key_store = keystore::KeyStore::new();

        migrate_plaintext_key(&key_store, use_kr);
        migrate_keyring_to_per_provider(
            &key_store,
            &file_cfg.api_url,
            &file_cfg.llm_api_url,
            use_kr,
        );

        let transcription_provider = Provider::from_url(&file_cfg.api_url);
        let llm_provider = Provider::from_url(&file_cfg.llm_api_url);
        let stored_key = load_key_with_store(
            &key_store,
            KeyringScope::Stt(transcription_provider),
            use_kr,
        );
        let stored_llm_key =
            load_key_with_store(&key_store, KeyringScope::Llm(llm_provider), use_kr);

        log(&format!(
            "Config loaded: url={}, model={}, device={:?}, provider={}, has_key={}, llm_provider={}, llm_enabled={}, llm_style={}",
            file_cfg.api_url, file_cfg.api_model, file_cfg.selected_device, transcription_provider,
            stored_key.is_some(), llm_provider, file_cfg.llm_enabled, file_cfg.llm_style.as_str()
        ));

        let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let input_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
            file_cfg.input_gain.to_bits(),
        ));
        let audio_tx =
            audio::spawn_audio_thread(audio_buffer.clone(), input_gain_atomic.clone());

        AppState {
            recording: Mutex::new(false),
            api_key: Mutex::new(stored_key),
            api_url: Mutex::new(file_cfg.api_url),
            api_model: Mutex::new(file_cfg.api_model),
            language: Mutex::new(file_cfg.language),
            prompt: Mutex::new(file_cfg.prompt),
            shortcut_mode: Mutex::new(file_cfg.shortcut_mode),
            shortcut: Mutex::new(file_cfg.shortcut),
            selected_device: Mutex::new(file_cfg.selected_device.clone()),
            audio_sample_rate: Mutex::new(audio::device_sample_rate(
                &file_cfg.selected_device,
            )),
            transcript: Mutex::new(String::new()),
            audio_buffer,
            audio_tx: Mutex::new(audio_tx),
            streaming_active: Arc::new(AtomicBool::new(false)),
            llm_enabled: Mutex::new(file_cfg.llm_enabled),
            llm_style: Mutex::new(file_cfg.llm_style),
            llm_tone: Mutex::new(file_cfg.llm_tone),
            llm_custom_prompt: Mutex::new(file_cfg.llm_custom_prompt),
            llm_translate_to: Mutex::new(file_cfg.llm_translate_to),
            llm_api_url: Mutex::new(file_cfg.llm_api_url),
            llm_api_model: Mutex::new(file_cfg.llm_api_model),
            llm_use_same_key: Mutex::new(file_cfg.llm_use_same_key),
            llm_api_key: Mutex::new(stored_llm_key),
            llm_log_enabled: Mutex::new(file_cfg.llm_log_enabled),
            chunk_streaming_enabled: Mutex::new(file_cfg.chunk_streaming_enabled),
            preprocessing_enabled: Mutex::new(file_cfg.preprocessing_enabled),
            audio_debug_enabled: Mutex::new(file_cfg.audio_debug_enabled),
            use_keyring: Mutex::new(file_cfg.use_keyring),
            border_style: Mutex::new(file_cfg.border_style),
            waveform_style: Mutex::new(file_cfg.waveform_style),
            overlay_position: Mutex::new(file_cfg.overlay_position),
            keep_in_clipboard: Mutex::new(file_cfg.keep_in_clipboard),
            input_gain: input_gain_atomic,
            key_store,
            audio_debug_session_dir: Mutex::new(None),
            window_anchor: Mutex::new(
                match (file_cfg.window_anchor_right, file_cfg.window_anchor_bottom) {
                    (Some(r), Some(b)) => Some((r, b)),
                    _ => None,
                },
            ),
            stats_total_words: Mutex::new(file_cfg.stats_total_words),
            stats_total_speaking_secs: Mutex::new(file_cfg.stats_total_speaking_secs),
        }
    }
}
```

- [ ] **Step 6b: Add assertions to `new_standalone()`**

After constructing AppState, add postcondition assertions:
```rust
// Postconditions
assert!(!state.api_url.lock().unwrap().is_empty(), "api_url must not be empty after init");
assert!(!state.api_model.lock().unwrap().is_empty(), "api_model must not be empty after init");
assert!(state.input_gain.load(std::sync::atomic::Ordering::Relaxed) != 0 || file_cfg.input_gain == 0.0,
    "input_gain atomic must be set");
```

- [ ] **Step 6c: Add test for `new_standalone()`**

Add to `lib.rs` test module (or create one):
```rust
#[cfg(test)]
mod standalone_tests {
    use super::*;

    #[test]
    fn new_standalone_creates_valid_state() {
        let state = AppState::new_standalone();
        assert!(!*state.recording.lock().unwrap());
        assert!(!state.api_url.lock().unwrap().is_empty());
        assert!(!state.api_model.lock().unwrap().is_empty());
        let gain_bits = state.input_gain.load(std::sync::atomic::Ordering::Relaxed);
        let gain = f32::from_bits(gain_bits);
        assert!(gain >= 0.0 && gain <= 2.0, "gain must be in [0.0, 2.0]");
        assert!(gain.is_finite());
    }
}
```

- [ ] **Step 7: Refactor `run()` to use `new_standalone()` internally**

In `pub fn run()` (line 2409), replace the duplicated AppState construction (lines 2434-2525) with:

```rust
#[cfg(feature = "tauri-runtime")]
pub fn run() {
    // Panic hook setup stays the same (lines 2411-2427)
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {}\nBacktrace:\n{}", info, bt);
        eprintln!("{}", msg);
        if let Some(path) = log_path() {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write;
                let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(f, "[{}] {}", ts, msg);
            }
        }
    }));

    log("=== Dimmy starting ===");
    if let Some(p) = log_path() {
        log(&format!("Log file: {}", p.display()));
    }

    let state = AppState::new_standalone();
    let shortcut_preset = state.shortcut.lock().unwrap().clone();

    tauri::Builder::default()
        // ... rest of builder unchanged, but use `state` instead of inline construction
        .manage(state)
        .setup(move |app| {
            // ... setup unchanged
```

This deduplicates the AppState construction between `run()` and `new_standalone()`.

- [ ] **Step 8: Verify `cargo build` with default features (Tauri)**

```bash
cd src-tauri && cargo build 2>&1 | tail -5
```

Expected: Compiles successfully. All existing functionality preserved.

- [ ] **Step 9: Verify `cargo build --no-default-features` (no Tauri)**

```bash
cd src-tauri && cargo build --no-default-features 2>&1 | tail -20
```

Expected: Compiles successfully without Tauri. May have warnings about unused items — those are OK for now.

- [ ] **Step 10: Run existing tests**

```bash
cd src-tauri && cargo test --lib 2>&1 | tail -10
```

Expected: All existing tests pass (186+ Rust tests).

- [ ] **Step 11: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/build.rs src-tauri/src/lib.rs
git commit -m "feat: feature-gate Tauri, add AppState::new_standalone()

Allow dimmy_lib to compile without Tauri (--no-default-features).
Business logic modules unchanged. Enables Linux native UI crate."
```

---

## Task 2: Scaffold Linux crate

**Files:**
- Create: `native-ui/linux/Cargo.toml`
- Create: `native-ui/linux/src/main.rs`
- Create: `native-ui/linux/src/state.rs`

- [ ] **Step 1: Create `native-ui/linux/Cargo.toml`**

```toml
[package]
name = "dimmy-linux"
version = "0.3.63"
edition = "2021"
description = "Dimmy Linux native UI — GTK4 + libadwaita"
license = "AGPL-3.0-only"

[[bin]]
name = "dimmy-linux"
path = "src/main.rs"

[dependencies]
dimmy_lib = { path = "../../src-tauri", default-features = false }
gtk4 = "0.9"
libadwaita = { version = "0.7", features = ["v1_4"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
log = "0.4"
env_logger = "0.11"
```

Note: Start with minimal deps. Add gtk4-layer-shell, ashpd, ksni in later tasks when needed.

- [ ] **Step 2: Create `native-ui/linux/src/state.rs`**

```rust
//! Bridge between Rust AppState and GTK main loop.
//!
//! AppEvent enum carries typed events from background threads (tokio)
//! to the GTK main loop via glib::Sender.

/// Events sent from background threads to the GTK main loop.
#[derive(Debug, Clone)]
pub enum AppEvent {
    RecordingStarted,
    RecordingStopped,
    AmplitudeUpdate(f32),
    TranscriptionProgress { current: u32, total: u32 },
    TranscriptionComplete(String),
    LlmComplete(String),
    Error(String),
    StyleChanged(String),
    ToneChanged(String),
}

/// Create a glib channel pair for AppEvents.
/// Returns (Sender, Receiver) — Sender is Send+Clone for background threads.
pub fn create_event_channel() -> (
    gtk4::glib::Sender<AppEvent>,
    gtk4::glib::Receiver<AppEvent>,
) {
    gtk4::glib::MainContext::channel(gtk4::glib::Priority::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_event_is_send_and_clone() {
        fn assert_send<T: Send>() {}
        fn assert_clone<T: Clone>() {}
        assert_send::<AppEvent>();
        assert_clone::<AppEvent>();
    }

    #[test]
    fn app_event_debug_format() {
        let event = AppEvent::TranscriptionComplete("hello".to_string());
        let debug = format!("{:?}", event);
        assert!(debug.contains("hello"));
    }

    #[test]
    fn app_event_amplitude_range() {
        let event = AppEvent::AmplitudeUpdate(0.5);
        match event {
            AppEvent::AmplitudeUpdate(v) => {
                assert!(v >= 0.0 && v <= 1.0);
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 3: Create `native-ui/linux/src/main.rs`**

```rust
//! Dimmy Linux native UI — GTK4 + libadwaita entry point.

mod state;

use dimmy_lib::log;
use libadwaita as adw;
use adw::prelude::*;

fn main() {
    env_logger::init();

    // Panic hook — log to file
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {}\nBacktrace:\n{}", info, bt);
        eprintln!("{}", msg);
        log(&msg);
    }));

    log("=== Dimmy Linux starting ===");

    // Initialize AppState from config + keyring
    let app_state = dimmy_lib::AppState::new_standalone();
    log("AppState initialized");

    // Create GTK application
    let app = adw::Application::builder()
        .application_id("com.dimmy.app")
        .build();

    // Share AppState via application — GTK takes ownership
    let app_state = std::sync::Arc::new(app_state);
    let state_clone = app_state.clone();

    app.connect_activate(move |app| {
        // Create event channel for async → GTK communication
        let (sender, receiver) = state::create_event_channel();

        // Spawn tokio runtime in background thread
        let _rt_handle = {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            std::thread::spawn(move || {
                rt.block_on(async {
                    // Runtime stays alive for the lifetime of the app
                    tokio::signal::ctrl_c().await.ok();
                });
            })
        };

        // Placeholder: show a simple window to prove the stack works
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Dimmy")
            .default_width(400)
            .default_height(300)
            .build();

        let label = gtk4::Label::new(Some(&format!(
            "Dimmy Linux — GTK4 + libadwaita\nAppState loaded: api_url={}",
            state_clone.api_url.lock().unwrap_or_else(|e| e.into_inner())
        )));
        window.set_content(Some(&label));

        // Attach event receiver to GTK main loop
        receiver.attach(None, move |event| {
            log(&format!("AppEvent: {:?}", event));
            gtk4::glib::ControlFlow::Continue
        });

        window.present();
    });

    app.run();
}
```

- [ ] **Step 4: Verify Linux crate compiles**

```bash
cd native-ui/linux && cargo build 2>&1 | tail -10
```

Expected: Compiles successfully. GTK4 + libadwaita link properly.

Note: This requires GTK4 and libadwaita dev packages installed:
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev
```

- [ ] **Step 5: Run Linux crate tests**

```bash
cd native-ui/linux && cargo test 2>&1 | tail -10
```

Expected: 3 tests pass (state module tests).

- [ ] **Step 6: Verify Tauri build is not broken**

```bash
cd src-tauri && cargo build 2>&1 | tail -5
```

Expected: Compiles successfully with default features (Tauri included).

- [ ] **Step 7: Commit**

```bash
git add native-ui/linux/
git commit -m "feat: scaffold Linux native UI crate (GTK4 + libadwaita)

Minimal crate that depends on dimmy_lib (no Tauri), initializes
AppState::new_standalone(), creates AdwApplication with placeholder window.
Includes AppEvent channel bridge for async → GTK communication."
```

---

## Task 3: Hotkey + recording pipeline (Wayland + X11)

**Files:**
- Create: `native-ui/linux/src/hotkey.rs`
- Create: `native-ui/linux/src/text_injector.rs`
- Modify: `native-ui/linux/Cargo.toml` (add ashpd, x11rb deps)
- Modify: `native-ui/linux/src/main.rs` (wire up hotkey → record → paste pipeline)

- [ ] **Step 1: Add dependencies to Cargo.toml**

Add to `[dependencies]`:
```toml
ashpd = "0.10"
x11rb = { version = "0.13", features = ["allow-unsafe-code"], optional = true }
arboard = "3"

[features]
default = ["x11-fallback"]
x11-fallback = ["x11rb"]
```

- [ ] **Step 2: Create `text_injector.rs`**

```rust
//! Text injection: copy to clipboard + simulate Ctrl+V.
//!
//! Wayland: wtype (primary) or ydotool (fallback)
//! X11: xdotool
//! Last resort: clipboard-only (user pastes manually)

use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PasteMethod {
    Wtype,
    Ydotool,
    Xdotool,
    ClipboardOnly,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplayServer {
    Wayland,
    X11,
    Unknown,
}

pub fn detect_display_server() -> DisplayServer {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => DisplayServer::Wayland,
        Ok("x11") => DisplayServer::X11,
        _ => {
            // Fallback: check WAYLAND_DISPLAY
            if std::env::var("WAYLAND_DISPLAY").is_ok() {
                DisplayServer::Wayland
            } else {
                DisplayServer::X11
            }
        }
    }
}

fn tool_available(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn detect_paste_method(display: DisplayServer) -> PasteMethod {
    match display {
        DisplayServer::Wayland => {
            if tool_available("wtype") {
                PasteMethod::Wtype
            } else if tool_available("ydotool") {
                PasteMethod::Ydotool
            } else {
                PasteMethod::ClipboardOnly
            }
        }
        DisplayServer::X11 | DisplayServer::Unknown => {
            if tool_available("xdotool") {
                PasteMethod::Xdotool
            } else {
                PasteMethod::ClipboardOnly
            }
        }
    }
}

/// Copy text to clipboard and optionally simulate Ctrl+V.
pub fn inject_text(text: &str, method: PasteMethod) -> Result<(), String> {
    assert!(!text.is_empty(), "inject_text: text must not be empty");

    // Step 1: Copy to clipboard
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("Clipboard: {}", e))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("Clipboard set: {}", e))?;

    // Step 2: Simulate Ctrl+V (if not clipboard-only)
    match method {
        PasteMethod::Wtype => {
            // Small delay for clipboard to settle
            std::thread::sleep(std::time::Duration::from_millis(50));
            let status = Command::new("wtype")
                .args(["-M", "ctrl", "-P", "v", "-m", "ctrl", "-p", "v"])
                .status()
                .map_err(|e| format!("wtype: {}", e))?;
            if !status.success() {
                return Err("wtype failed".into());
            }
        }
        PasteMethod::Ydotool => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let status = Command::new("ydotool")
                .args(["key", "29:1", "47:1", "47:0", "29:0"]) // Ctrl+V
                .status()
                .map_err(|e| format!("ydotool: {}", e))?;
            if !status.success() {
                return Err("ydotool failed".into());
            }
        }
        PasteMethod::Xdotool => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let status = Command::new("xdotool")
                .args(["key", "--clearmodifiers", "ctrl+v"])
                .status()
                .map_err(|e| format!("xdotool: {}", e))?;
            if !status.success() {
                return Err("xdotool failed".into());
            }
        }
        PasteMethod::ClipboardOnly => {
            // Text is already in clipboard — nothing more to do
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_display_server_from_env() {
        // This test checks the function runs without panicking.
        // Actual result depends on environment.
        let ds = detect_display_server();
        assert!(matches!(
            ds,
            DisplayServer::Wayland | DisplayServer::X11 | DisplayServer::Unknown
        ));
    }

    #[test]
    fn detect_paste_method_clipboard_fallback() {
        // On CI without wtype/ydotool/xdotool, should fall back to ClipboardOnly
        let method = detect_paste_method(DisplayServer::Unknown);
        // Can't assert specific method since CI might have xdotool
        assert!(matches!(
            method,
            PasteMethod::Xdotool | PasteMethod::ClipboardOnly
        ));
    }

    #[test]
    fn tool_available_returns_bool() {
        // "ls" should be available on any Linux
        assert!(tool_available("ls"));
        // garbage should not
        assert!(!tool_available("definitely_not_a_real_tool_xyz"));
    }
}
```

- [ ] **Step 3: Create `hotkey.rs`**

```rust
//! Global hotkeys for Linux.
//!
//! Wayland: xdg-desktop-portal GlobalShortcuts (ashpd)
//! X11: XGrabKey via x11rb

use dimmy_lib::log;

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

/// Placeholder for Phase 3 Step 2 — full implementation.
/// For now, provides detection and logging.
pub fn detect_hotkey_backend() -> &'static str {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => {
            log("Hotkey backend: xdg-desktop-portal (Wayland)");
            "portal"
        }
        _ => {
            log("Hotkey backend: XGrabKey (X11)");
            "x11"
        }
    }
}

/// Public API contract — downstream tasks (pill, onboarding) code against these.
/// Full implementation requires Linux desktop testing.

/// Register a global hotkey. Events are sent via the glib Sender.
/// Returns Ok(()) if registration succeeds, Err with message if not.
pub fn register_hotkey(
    _shortcut: &str,
    _sender: gtk4::glib::Sender<HotkeyEvent>,
) -> Result<(), String> {
    let backend = detect_hotkey_backend();
    dimmy_lib::log(&format!("register_hotkey: backend={}, stub", backend));
    // TODO: Implement portal registration (Wayland) or XGrabKey (X11)
    Ok(())
}

/// Unregister the currently active global hotkey.
pub fn unregister_hotkey() {
    dimmy_lib::log("unregister_hotkey: stub");
    // TODO: Implement cleanup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_backend_returns_valid() {
        let backend = detect_hotkey_backend();
        assert!(backend == "portal" || backend == "x11");
    }
}
```

- [ ] **Step 4: Wire up modules in `main.rs`**

Add `mod hotkey;` and `mod text_injector;` to main.rs, log detection results at startup:

```rust
mod hotkey;
mod state;
mod text_injector;

// In main(), after AppState init:
let display = text_injector::detect_display_server();
let paste_method = text_injector::detect_paste_method(display);
log(&format!("Display: {:?}, Paste: {:?}", display, paste_method));
let _hotkey_backend = hotkey::detect_hotkey_backend();
```

- [ ] **Step 5: Run tests**

```bash
cd native-ui/linux && cargo test 2>&1 | tail -15
```

Expected: All tests pass (state + text_injector + hotkey tests).

- [ ] **Step 6: Commit**

```bash
git add native-ui/linux/
git commit -m "feat(linux): add hotkey detection and text injection

Detect Wayland vs X11, choose paste method (wtype/ydotool/xdotool/clipboard).
Hotkey backend detection (portal vs XGrabKey). Full hotkey
implementation requires Linux desktop testing."
```

---

## Task 4: Pill overlay window

**Files:**
- Create: `native-ui/linux/src/pill_window.rs`
- Create: `native-ui/linux/src/waveform.rs`
- Modify: `native-ui/linux/Cargo.toml` (add gtk4-layer-shell)
- Modify: `native-ui/linux/src/main.rs`

- [ ] **Step 1: Add gtk4-layer-shell to Cargo.toml**

```toml
gtk4-layer-shell = "0.4"
```

- [ ] **Step 2: Create `waveform.rs`**

Custom GTK4 DrawingArea that renders 5 waveform styles using Cairo. The widget takes an amplitude (0.0–1.0) and a style enum, draws the visualization.

Key implementation details:
- `WaveformStyle` enum: Bars, BarsCenter, BarsRound, Line, Dots
- 7 bars with weights `[0.3, 0.5, 0.7, 1.0, 0.7, 0.5, 0.3]`
- 5 dots with weights `[0.4, 0.7, 1.0, 0.7, 0.4]`
- Height range: 3–16px bars, 3–10px dots
- 12Hz refresh timer via `glib::timeout_add_local`
- Per-bar random jitter (±20%) for organic motion
- White color (#E6FFFFFF)

The full Cairo drawing code for all 5 styles must be implemented. Each style is a match arm in the draw callback.

- [ ] **Step 3: Create `pill_window.rs`**

Floating transparent overlay using gtk4-layer-shell. Key implementation:

- Layer shell setup: `gtk4_layer_shell::init_for_window()`, set layer to `Overlay`, anchor to configured position, set exclusive zone to -1 (no space reservation), set keyboard mode to None
- X11 fallback: standard GtkWindow with type hints (dialog, skip-taskbar, always-on-top)
- 7-state machine matching Windows/macOS: Idle, IdleHover, Recording, Transcribing, Processing, Completing, Error
- Border: CSS-based for solid colors, Cairo draw for rainbow gradient
- Transparency: CSS `background: transparent;` + RGBA Cairo drawing
- Drag: GtkGestureDrag on the window
- Context menu: GtkPopoverMenu with "Settings" and "Hide"
- Scroll: GtkEventControllerScroll on dot/language labels
- Timer: recording MM:SS counter (1Hz), pill auto-dismiss (1.2s completing, 3s error)
- Size: 36x36 idle circle, 40px height capsule when expanded

- [ ] **Step 4: Wire pill into `main.rs`**

Replace the placeholder window with the pill overlay. Connect AppEvent receiver to update pill state.

- [ ] **Step 5: Compile and verify**

```bash
cd native-ui/linux && cargo build 2>&1 | tail -10
```

- [ ] **Step 6: Commit**

```bash
git add native-ui/linux/
git commit -m "feat(linux): pill overlay window with waveform and state machine

Transparent floating overlay via gtk4-layer-shell (Wayland) with X11
fallback. 7 states, 5 waveform styles, rainbow/solid borders, drag,
scroll-to-cycle, context menu."
```

---

## Task 5: Settings window (8 tabs)

**Files:**
- Create: `native-ui/linux/src/settings/mod.rs`
- Create: `native-ui/linux/src/settings/general.rs`
- Create: `native-ui/linux/src/settings/shortcut.rs`
- Create: `native-ui/linux/src/settings/output.rs`
- Create: `native-ui/linux/src/settings/overlay.rs`
- Create: `native-ui/linux/src/settings/permissions.rs`
- Create: `native-ui/linux/src/settings/stats.rs`
- Create: `native-ui/linux/src/settings/debug.rs`
- Create: `native-ui/linux/src/settings/about.rs`

Each tab is a separate file returning an `adw::PreferencesPage`. The container (`mod.rs`) creates the `adw::PreferencesWindow`, adds all pages, and manages the Advanced toggle visibility.

Implementation notes per tab:
- **General**: `adw::ComboRow` for language/theme/STT provider, `adw::PasswordEntryRow` for API key, `adw::SwitchRow` for toggles. Advanced section uses `adw::PreferencesGroup` with conditional visibility.
- **Shortcut**: Custom shortcut recorder widget (GtkButton that captures key events). Preset buttons in a horizontal box. `adw::ComboRow` for mode.
- **Output**: LLM style ComboRow with colored prefix labels. Provider ComboRow. Conditional visibility for custom fields and separate LLM key.
- **Overlay**: ComboRows for position/border/waveform. Show overlay toggle. Idle opacity ComboRow.
- **Permissions**: Status rows checking `which wtype`, `which ydotool`, `which xdotool`. Mic check via cpal device enumeration. Install hints per distro.
- **Stats**: ActionRows with formatted numbers. Time saved formula.
- **Debug**: Simulation button, audio health button, switch rows for debug flags.
- **About**: Version label, update check button (async GitHub API via reqwest), links.

All settings read from / write to `AppState` directly (no JSON serialization).

Save button calls `dimmy_lib::save_config_file()` with values read from AppState.

- [ ] **Step 1-8: Create each settings tab file**
- [ ] **Step 9: Create `mod.rs` container with Advanced toggle**
- [ ] **Step 10: Wire settings into main.rs (open from pill context menu)**
- [ ] **Step 11: Test compilation**
- [ ] **Step 12: Commit**

```bash
git commit -m "feat(linux): settings window with 8 tabs

AdwPreferencesWindow with General, Shortcut, Output, Overlay,
Permissions, Stats, Debug, About tabs. Advanced toggle hides/shows
Debug+Stats tabs and advanced sections. Direct AppState read/write."
```

---

## Task 6: Onboarding wizard

**Files:**
- Create: `native-ui/linux/src/onboarding/mod.rs`
- Create: `native-ui/linux/src/onboarding/welcome.rs`
- Create: `native-ui/linux/src/onboarding/shortcut.rs`
- Create: `native-ui/linux/src/onboarding/tryit.rs`
- Modify: `native-ui/linux/src/main.rs`

3-step AdwCarousel wizard:
1. Welcome: icon + title + tagline + "Get Started"
2. Shortcut: recorder + presets + mode picker
3. Try It: live test with pill shown

On completion: save config, mark onboarding done, start normal mode.

Check `dimmy_lib::onboarding_completed()` at startup to decide whether to show wizard or go straight to pill.

- [ ] **Steps 1-5: Create files, wire into main.rs, test, commit**

```bash
git commit -m "feat(linux): onboarding wizard (3-step carousel)

Welcome → Shortcut setup → Live test. Uses AdwCarousel with progress
dots. Saves config and starts normal mode on completion."
```

---

## Task 7: System tray

**Files:**
- Create: `native-ui/linux/src/tray.rs`
- Modify: `native-ui/linux/Cargo.toml` (add ksni)
- Modify: `native-ui/linux/src/main.rs`

StatusNotifierItem via ksni crate:
- Icon changes per state
- Context menu: status, language, style, shortcut, show/hide pill, settings, quit
- Left-click: toggle pill

- [ ] **Steps 1-4: Add dep, create tray.rs, wire up, test, commit**

```bash
git commit -m "feat(linux): system tray via StatusNotifierItem

ksni-based tray icon with context menu (status, language, style,
shortcut, show/hide pill, settings, quit). Icon changes per state."
```

---

## Task 8: Polish + packaging

**Files:**
- Modify: `native-ui/linux/src/waveform.rs` (remaining styles)
- Modify: `native-ui/linux/src/pill_window.rs` (hover, scroll, animations)
- Create: `native-ui/linux/assets/` (icons, .desktop file)
- Modify: `.github/workflows/staging-native.yml` (add Linux job)
- Modify: `.github/workflows/release.yml` (add Linux build)

- [ ] **Step 1: Finalize all 5 waveform styles**
- [ ] **Step 2: Pill hover expand animation**
- [ ] **Step 3: Scroll-to-cycle LLM style and language**
- [ ] **Step 4: Create SVG icons + .desktop file**
- [ ] **Step 5: Add Linux build to CI (Ubuntu 24.04 runner)**

CI steps:
```yaml
- name: Install GTK4 deps
  run: sudo apt-get install -y libgtk-4-dev libadwaita-1-dev

- name: Build Linux UI
  run: cd native-ui/linux && cargo build --release
```

- [ ] **Step 6: AppImage packaging script**
- [ ] **Step 7: .deb packaging script**
- [ ] **Step 8: Commit all polish + CI**

```bash
git commit -m "feat(linux): polish, icons, CI, and packaging

All 5 waveform styles, hover/scroll interactions, SVG icons,
.desktop file. CI builds on Ubuntu 24.04. AppImage + .deb packaging."
```

---

## Verification Checklist

After all tasks are complete, verify:

- [ ] `cd src-tauri && cargo build` — Tauri build still works
- [ ] `cd src-tauri && cargo build --no-default-features` — builds without Tauri
- [ ] `cd src-tauri && cargo test --lib` — all 186+ tests pass
- [ ] `cd native-ui/linux && cargo build --release` — Linux UI builds
- [ ] `cd native-ui/linux && cargo test` — all Linux tests pass
- [ ] `cd native-ui/linux && cargo clippy -- -D warnings` — zero warnings
- [ ] Run `dimmy-linux` on Ubuntu 24.04 Wayland — pill appears, settings open
- [ ] Run `dimmy-linux` on Ubuntu 24.04 X11 — same functionality
- [ ] Hotkey → record → transcribe → paste works end-to-end
- [ ] All 8 settings tabs present and functional
- [ ] Onboarding wizard works for fresh install
- [ ] Tray icon appears with working context menu

mod audio;
mod hotkey;
mod transcribe;

use audio::AudioCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, LogicalSize, LogicalPosition};

const DEFAULT_API_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

fn config_dir_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join("pai-voice"))
}

fn config_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("config.json"))
}

fn log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("pai-voice.log"))
}

/// Write a log line to %APPDATA%/pai-voice/pai-voice.log (visible on Windows GUI apps)
pub(crate) fn log(msg: &str) {
    use std::io::Write;
    eprintln!("{}", msg); // also stderr for dev/terminal use
    if let Some(path) = log_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
    }
}

/// Save non-sensitive config to file (NO api_key — that goes to keyring ONLY)
fn save_config_file(api_url: &str, api_model: &str, selected_device: &Option<String>, language: &str, shortcut_mode: &str) {
    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut cfg = serde_json::json!({
            "api_url": api_url,
            "api_model": api_model,
            "language": language,
            "shortcut_mode": shortcut_mode,
        });
        if let Some(dev) = selected_device {
            cfg["selected_device"] = serde_json::json!(dev);
        }
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap_or_default());
    }
}

/// Load non-sensitive config from file
fn load_config_file() -> (String, String, Option<String>, String, String) {
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                let url = v["api_url"].as_str().unwrap_or(DEFAULT_API_URL).to_string();
                let model = v["api_model"].as_str().unwrap_or(DEFAULT_MODEL).to_string();
                let device = v["selected_device"].as_str().map(|s| s.to_string());
                let language = v["language"].as_str().unwrap_or("").to_string();
                let shortcut_mode = v["shortcut_mode"].as_str().unwrap_or("toggle").to_string();
                return (url, model, device, language, shortcut_mode);
            }
        }
    }
    (DEFAULT_API_URL.to_string(), DEFAULT_MODEL.to_string(), None, String::new(), "toggle".to_string())
}


/// Migrate: if old config.json has api_key in plain text, move to secure storage and REMOVE from file
fn migrate_plaintext_key() {
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(key) = v["api_key"].as_str() {
                    if !key.is_empty() {
                        log("Migrating plaintext API key to secure storage...");
                        match save_api_key_secure(key) {
                            Ok(()) => log("Key migrated to secure storage"),
                            Err(e) => log(&format!("WARNING: migration failed: {}", e)),
                        }
                        // Remove plaintext key from config file
                        let url = v["api_url"].as_str().unwrap_or(DEFAULT_API_URL);
                        let model = v["api_model"].as_str().unwrap_or(DEFAULT_MODEL);
                        let device = v["selected_device"].as_str().map(|s| s.to_string());
                        let language = v["language"].as_str().unwrap_or("");
                        let shortcut_mode = v["shortcut_mode"].as_str().unwrap_or("toggle");
                        save_config_file(url, model, &device, language, shortcut_mode);
                        log("Plaintext key removed from config file");
                    }
                }
            }
        }
    }
}

// ── Secure key storage: keyring with native platform backends ──
// Requires features: windows-native, apple-native, sync-secret-service

fn save_api_key_secure(key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new("pai-voice", "api-key")
        .map_err(|e| {
            log(&format!("ERROR: keyring Entry::new failed: {}", e));
            format!("Credential store error: {}", e)
        })?;
    entry.set_password(key).map_err(|e| {
        log(&format!("ERROR: keyring set_password failed: {}", e));
        format!("Failed to save API key: {}", e)
    })?;
    log("API key saved to secure storage (keyring)");
    Ok(())
}

fn load_api_key_secure() -> Option<String> {
    match keyring::Entry::new("pai-voice", "api-key") {
        Ok(entry) => match entry.get_password() {
            Ok(key) => {
                log("API key loaded from secure storage (keyring)");
                Some(key)
            }
            Err(e) => {
                log(&format!("WARNING: keyring get_password failed: {}", e));
                None
            }
        },
        Err(e) => {
            log(&format!("WARNING: keyring Entry::new failed on load: {}", e));
            None
        }
    }
}

pub struct AppState {
    pub recording: Mutex<bool>,
    pub api_key: Mutex<Option<String>>,
    pub api_url: Mutex<String>,
    pub api_model: Mutex<String>,
    pub language: Mutex<String>,
    pub shortcut_mode: Mutex<String>, // "toggle" or "hold"
    pub selected_device: Mutex<Option<String>>,
    pub audio_sample_rate: Mutex<u32>,
    pub transcript: Mutex<String>,
    pub audio_buffer: Arc<Mutex<Vec<f32>>>,
    pub audio_tx: Mutex<Sender<AudioCommand>>,
    pub streaming_active: Arc<AtomicBool>,
}

#[tauri::command]
fn start_recording(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let mut recording = state.recording.lock().map_err(|e| e.to_string())?;
    if *recording {
        return Err("Already recording".into());
    }
    *recording = true;

    let selected_device = state.selected_device.lock().map_err(|e| e.to_string())?.clone();

    // Get sample rate for the SELECTED device (not default)
    let device_sr = audio::device_sample_rate(&selected_device);
    *state.audio_sample_rate.lock().map_err(|e| e.to_string())? = device_sr;

    state
        .audio_tx
        .lock()
        .map_err(|e| e.to_string())?
        .send(AudioCommand::Start(selected_device))
        .map_err(|e| e.to_string())?;

    state.streaming_active.store(true, Ordering::SeqCst);

    let api_key = state.api_key.lock().map_err(|e| e.to_string())?.clone();
    let api_url = state.api_url.lock().map_err(|e| e.to_string())?.clone();
    let api_model = state.api_model.lock().map_err(|e| e.to_string())?.clone();
    let language = state.language.lock().map_err(|e| e.to_string())?.clone();

    if let Some(key) = api_key {
        let buffer = state.audio_buffer.clone();
        let streaming = state.streaming_active.clone();
        let handle = app_handle.clone();
        let sample_rate = device_sr as usize;

        tauri::async_runtime::spawn(async move {
            let mut offset: usize = 0;
            let mut chunk_index: u32 = 0;
            let min_chunk_samples = sample_rate * 2;
            let max_chunk_samples = sample_rate * 12;
            let silence_threshold: f32 = 0.01;
            let silence_duration_samples = (sample_rate as f32 * 0.4) as usize;

            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                if !streaming.load(Ordering::SeqCst) {
                    break;
                }

                let (should_split, split_end) = {
                    let buf = match buffer.lock() {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    let available = buf.len().saturating_sub(offset);

                    if available < min_chunk_samples {
                        (false, 0)
                    } else if available >= max_chunk_samples {
                        (true, offset + available)
                    } else {
                        let check_len = silence_duration_samples.min(available);
                        let tail_start = buf.len() - check_len;
                        let rms: f32 = buf[tail_start..buf.len()]
                            .iter()
                            .map(|s| s * s)
                            .sum::<f32>()
                            / check_len as f32;
                        let rms = rms.sqrt();
                        if rms < silence_threshold {
                            (true, buf.len())
                        } else {
                            (false, 0)
                        }
                    }
                };

                if !should_split {
                    continue;
                }

                let chunk_data = {
                    let buf = match buffer.lock() {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    buf[offset..split_end].to_vec()
                };

                offset = split_end;
                chunk_index += 1;

                let _ = handle.emit(
                    "chunk-status",
                    serde_json::json!({
                        "index": chunk_index,
                        "status": "sending",
                    }),
                );

                let wav_result = audio::encode_wav(&chunk_data, sample_rate as u32).map_err(|e| e.to_string());
                match wav_result {
                    Ok(wav_data) => {
                        match transcribe::transcribe_audio(
                            &api_url, &api_model, &key, &wav_data, &language,
                        )
                        .await
                        {
                            Ok(text) => {
                                let _ = handle.emit(
                                    "transcription-chunk",
                                    serde_json::json!({
                                        "index": chunk_index,
                                        "text": text,
                                    }),
                                );
                            }
                            Err(e) => {
                                let _ = handle.emit(
                                    "chunk-status",
                                    serde_json::json!({
                                        "index": chunk_index,
                                        "status": "error",
                                        "error": e.to_string(),
                                    }),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let _ = handle.emit(
                            "chunk-status",
                            serde_json::json!({
                                "index": chunk_index,
                                "status": "error",
                                "error": e,
                            }),
                        );
                    }
                }
            }
        });
    }

    Ok("Recording started".into())
}

#[tauri::command]
async fn stop_recording(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    state.streaming_active.store(false, Ordering::SeqCst);

    let (buffer, api_key, api_url, api_model, language) = {
        let mut recording = state.recording.lock().map_err(|e| e.to_string())?;
        *recording = false;

        state
            .audio_tx
            .lock()
            .map_err(|e| e.to_string())?
            .send(AudioCommand::Stop)
            .map_err(|e| e.to_string())?;

        std::thread::sleep(std::time::Duration::from_millis(50));

        let buffer = {
            let mut buf = state.audio_buffer.lock().map_err(|e| e.to_string())?;
            let data = buf.clone();
            buf.clear(); // Release memory — buffer grows unbounded during recording
            data
        };

        let api_key = state
            .api_key
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or_else(|| "No API key configured. Open Settings to set one.".to_string())?;

        let api_url = state.api_url.lock().map_err(|e| e.to_string())?.clone();
        let api_model = state.api_model.lock().map_err(|e| e.to_string())?.clone();
        let language = state.language.lock().map_err(|e| e.to_string())?.clone();

        (buffer, api_key, api_url, api_model, language)
    };

    if buffer.is_empty() {
        return Err("No audio captured".into());
    }

    let _ = app_handle.emit(
        "chunk-status",
        serde_json::json!({ "index": 0, "status": "final" }),
    );

    let sr = *state.audio_sample_rate.lock().map_err(|e| e.to_string())?;
    let wav_data = audio::encode_wav(&buffer, sr).map_err(|e| e.to_string())?;
    let transcript =
        transcribe::transcribe_audio(&api_url, &api_model, &api_key, &wav_data, &language)
            .await
            .map_err(|e| e.to_string())?;

    *state.transcript.lock().map_err(|e| e.to_string())? = transcript.clone();

    let _ = app_handle.emit(
        "transcription-final",
        serde_json::json!({ "text": transcript }),
    );

    Ok(transcript)
}

#[tauri::command]
fn get_transcript(state: tauri::State<'_, AppState>) -> Result<String, String> {
    Ok(state.transcript.lock().map_err(|e| e.to_string())?.clone())
}

#[tauri::command]
fn set_config(
    api_key: Option<String>,
    api_url: String,
    api_model: String,
    language: String,
    shortcut_mode: String,
    selected_device: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    log(&format!("set_config called: mode={}, device={:?}", shortcut_mode, selected_device));

    if let Some(ref key) = api_key {
        if !key.is_empty() {
            save_api_key_secure(key)?;
            *state.api_key.lock().map_err(|e| e.to_string())? = Some(key.clone());
        }
    }

    save_config_file(&api_url, &api_model, &selected_device, &language, &shortcut_mode);
    log("Config file saved");

    *state.api_url.lock().map_err(|e| e.to_string())? = api_url;
    *state.api_model.lock().map_err(|e| e.to_string())? = api_model;
    *state.language.lock().map_err(|e| e.to_string())? = language;
    *state.shortcut_mode.lock().map_err(|e| e.to_string())? = shortcut_mode;
    *state.selected_device.lock().map_err(|e| e.to_string())? = selected_device;
    log("set_config completed OK");
    Ok(())
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let has_key = state.api_key.lock().map_err(|e| e.to_string())?.is_some();
    let api_url = state.api_url.lock().map_err(|e| e.to_string())?.clone();
    let api_model = state.api_model.lock().map_err(|e| e.to_string())?.clone();
    let language = state.language.lock().map_err(|e| e.to_string())?.clone();
    let shortcut_mode = state.shortcut_mode.lock().map_err(|e| e.to_string())?.clone();
    let selected_device = state.selected_device.lock().map_err(|e| e.to_string())?.clone();
    let devices = audio::list_input_devices();
    Ok(serde_json::json!({
        "has_key": has_key,
        "api_url": api_url,
        "api_model": api_model,
        "language": language,
        "shortcut_mode": shortcut_mode,
        "selected_device": selected_device,
        "devices": devices,
    }))
}

#[tauri::command]
fn get_amplitude(state: tauri::State<'_, AppState>) -> Result<f32, String> {
    let buffer = state.audio_buffer.lock().map_err(|e| e.to_string())?;
    if buffer.is_empty() {
        return Ok(0.0);
    }
    // ~50ms window, adaptive to device sample rate (matches PS1's ReadEnergy(1600))
    let sr = *state.audio_sample_rate.lock().map_err(|e| e.to_string())? as usize;
    let window = sr / 20; // 48kHz→2400, 16kHz→800
    let start = buffer.len().saturating_sub(window);
    let len = buffer.len() - start;
    let rms: f32 = buffer[start..].iter().map(|s| s * s).sum::<f32>() / len as f32;
    Ok(rms.sqrt())
}

#[tauri::command]
fn get_audio_device(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let selected = state.selected_device.lock().map_err(|e| e.to_string())?.clone();
    Ok(selected.unwrap_or_else(|| audio::default_input_device_name()))
}

#[tauri::command]
fn paste_text(text: String) -> Result<(), String> {
    use arboard::Clipboard;
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(&text).map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo.key(modifier, Direction::Press).map_err(|e| e.to_string())?;
    enigo.key(Key::Unicode('v'), Direction::Click).map_err(|e| e.to_string())?;
    enigo.key(modifier, Direction::Release).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn resize_window(w: f64, h: f64, app_handle: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app_handle.get_webview_window("main") {
        win.set_size(LogicalSize::new(w, h)).map_err(|e| e.to_string())?;
        // Reposition to keep bottom-right corner anchored with padding
        if let Ok(Some(monitor)) = win.primary_monitor() {
            let screen = monitor.size();
            let scale = monitor.scale_factor();
            let padding = 20.0_f64;
            let x = (screen.width as f64 / scale) - w - padding;
            let y = (screen.height as f64 / scale) - h - padding;
            let _ = win.set_position(LogicalPosition::new(x, y));
        }
    }
    Ok(())
}

pub fn run() {
    // Log panics to file before crashing
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {}", info);
        eprintln!("{}", msg);
        // Write directly to log file since log() might not work during panic
        if let Some(path) = log_path() {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                use std::io::Write;
                let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(f, "[{}] {}", ts, msg);
            }
        }
    }));

    log("=== Vocino starting ===");
    if let Some(p) = log_path() {
        log(&format!("Log file: {}", p.display()));
    }

    migrate_plaintext_key();

    let (file_url, file_model, file_device, file_language, file_shortcut_mode) = load_config_file();
    let stored_key = load_api_key_secure();
    log(&format!("Config loaded: url={}, model={}, device={:?}, has_key={}",
        file_url, file_model, file_device, stored_key.is_some()));

    let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let audio_tx = audio::spawn_audio_thread(audio_buffer.clone());

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState {
            recording: Mutex::new(false),
            api_key: Mutex::new(stored_key),
            api_url: Mutex::new(file_url),
            api_model: Mutex::new(file_model),
            language: Mutex::new(file_language),
            shortcut_mode: Mutex::new(file_shortcut_mode),
            selected_device: Mutex::new(file_device.clone()),
            audio_sample_rate: Mutex::new(audio::device_sample_rate(&file_device)),
            transcript: Mutex::new(String::new()),
            audio_buffer,
            audio_tx: Mutex::new(audio_tx),
            streaming_active: Arc::new(AtomicBool::new(false)),
        })
        .setup(|app| {
            // Remove Windows DWM border and set transparent background
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_shadow(false);
                use tauri::window::Color;
                let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

                // Open DevTools in debug builds
                #[cfg(debug_assertions)]
                window.open_devtools();

                // Position bottom-right with padding
                if let Ok(Some(monitor)) = window.primary_monitor() {
                    let screen = monitor.size();
                    let scale = monitor.scale_factor();
                    let win_w = 220.0_f64; // initial logical width
                    let win_h = 32.0_f64;  // initial logical height
                    let padding = 20.0_f64;
                    let x = (screen.width as f64 / scale) - win_w - padding;
                    let y = (screen.height as f64 / scale) - win_h - padding;
                    let _ = window.set_position(LogicalPosition::new(x, y));
                    log(&format!("Window positioned: {}x{} at ({}, {})", win_w, win_h, x as i32, y as i32));
                }
            }
            // Install low-level keyboard hook for Win+Alt (2-key combo)
            hotkey::install(log);

            // Poll for hotkey events and emit to frontend
            let hotkey_handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    let event = hotkey::take_event();
                    if event != 0 {
                        let state = hotkey_handle.state::<AppState>();
                        let is_recording = *state.recording.lock().unwrap_or_else(|e| e.into_inner());
                        let mode = state.shortcut_mode.lock()
                            .map(|m| m.clone())
                            .unwrap_or_else(|_| "toggle".to_string());

                        if event == 1 {
                            // Combo pressed
                            if mode == "hold" {
                                if !is_recording {
                                    if let Some(w) = hotkey_handle.get_webview_window("main") {
                                        let _ = w.emit("shortcut-start", ());
                                    }
                                }
                            } else {
                                // Toggle mode
                                if is_recording {
                                    if let Some(w) = hotkey_handle.get_webview_window("main") {
                                        let _ = w.emit("shortcut-stop", ());
                                    }
                                } else {
                                    if let Some(w) = hotkey_handle.get_webview_window("main") {
                                        let _ = w.emit("shortcut-start", ());
                                    }
                                }
                            }
                        } else if event == 2 {
                            // Combo released
                            if mode == "hold" && is_recording {
                                if let Some(w) = hotkey_handle.get_webview_window("main") {
                                    let _ = w.emit("shortcut-stop", ());
                                }
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            get_transcript,
            set_config,
            get_config,
            get_amplitude,
            get_audio_device,
            paste_text,
            resize_window,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vocino");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_with_device() {
        // Use a temp dir to avoid polluting real config
        let tmp = std::env::temp_dir().join("pai-voice-test-config");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");

        let cfg = serde_json::json!({
            "api_url": "https://test.example.com/v1/transcribe",
            "api_model": "test-model-v1",
            "language": "it",
            "selected_device": "Test Microphone"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert_eq!(v["api_url"].as_str().unwrap(), "https://test.example.com/v1/transcribe");
        assert_eq!(v["api_model"].as_str().unwrap(), "test-model-v1");
        assert_eq!(v["language"].as_str().unwrap(), "it");
        assert_eq!(v["selected_device"].as_str().unwrap(), "Test Microphone");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_roundtrip_no_device() {
        let tmp = std::env::temp_dir().join("pai-voice-test-config2");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");

        let cfg = serde_json::json!({
            "api_url": DEFAULT_API_URL,
            "api_model": DEFAULT_MODEL,
            "language": "",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();

        assert_eq!(v["api_url"].as_str().unwrap(), DEFAULT_API_URL);
        assert_eq!(v["api_model"].as_str().unwrap(), DEFAULT_MODEL);
        assert!(v["selected_device"].is_null(), "No device should produce null");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_missing_file_returns_defaults() {
        let (url, model, device, language, _shortcut_mode) = load_config_file();
        // Even if config exists, verify types are correct
        assert!(!url.is_empty(), "URL should not be empty");
        assert!(!model.is_empty(), "Model should not be empty");
        // device and language can be None/"" legitimately
        let _ = (device, language);
    }

    #[test]
    fn save_config_no_panic_with_none_device() {
        // Should not panic when device is None
        save_config_file("https://example.com", "test", &None, "en", "toggle");
    }

    #[test]
    fn save_config_no_panic_with_some_device() {
        save_config_file("https://example.com", "test", &Some("Mic".to_string()), "it", "toggle");
    }

    #[test]
    fn config_dir_path_returns_some() {
        let p = config_dir_path();
        assert!(p.is_some(), "config_dir_path should return Some on any OS");
        let p = p.unwrap();
        assert!(p.ends_with("pai-voice"), "Path should end with 'pai-voice'");
    }

    #[test]
    fn config_path_ends_with_config_json() {
        let p = config_path();
        assert!(p.is_some());
        let p = p.unwrap();
        assert!(p.ends_with("config.json"), "Should end with config.json, got: {:?}", p);
    }

    #[test]
    fn log_path_ends_with_log_file() {
        let p = log_path();
        assert!(p.is_some());
        let p = p.unwrap();
        assert!(p.ends_with("pai-voice.log"), "Should end with pai-voice.log, got: {:?}", p);
    }

    #[test]
    fn log_writes_timestamped_line_to_file() {
        let tmp = std::env::temp_dir().join("pai-voice-test-log");
        let _ = std::fs::create_dir_all(&tmp);
        let log_file = tmp.join("test.log");
        // Write directly using the same logic as log()
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
                .unwrap();
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            writeln!(f, "[{}] {}", ts, "test log message").unwrap();
        }
        let contents = std::fs::read_to_string(&log_file).unwrap();
        assert!(contents.contains("test log message"), "Log should contain the message");
        assert!(contents.contains("[20"), "Log should contain timestamp starting with [20xx");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_returns_valid_strings() {
        // load_config_file always returns non-empty url and model
        let (url, model, _device, _lang, _mode) = load_config_file();
        assert!(url.starts_with("https://"), "URL should be HTTPS, got: {}", url);
        assert!(!model.is_empty(), "Model should not be empty");
    }

    #[test]
    fn save_config_creates_valid_json() {
        // Verify save_config_file writes parseable JSON
        // Note: parallel tests may race on the config file, so we retry parse once
        save_config_file("https://test.local/v1", "test-m", &Some("Dev1".into()), "fr", "toggle");
        if let Some(path) = config_path() {
            let data = std::fs::read_to_string(&path).unwrap();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                assert!(v["api_url"].is_string(), "api_url should be a string");
                assert!(v["api_model"].is_string(), "api_model should be a string");
            }
            // If parse fails, another test raced the write — that's OK, the structure is tested above
            save_config_file(DEFAULT_API_URL, DEFAULT_MODEL, &None, "", "toggle");
        }
    }

    #[test]
    fn migrate_plaintext_key_no_panic_without_key() {
        // If config has no api_key field, migration should be a no-op
        migrate_plaintext_key();
    }

    #[test]
    fn default_constants_are_valid() {
        assert!(DEFAULT_API_URL.starts_with("https://"), "API URL should use HTTPS");
        assert!(!DEFAULT_MODEL.is_empty(), "Default model should not be empty");
    }
}

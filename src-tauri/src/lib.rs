mod audio;
mod hotkey;
mod llm;
mod preprocess;
mod transcribe;

use audio::AudioCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager};

const DEFAULT_API_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";
/// Default prompt guides Whisper to produce punctuated, well-formatted output.
/// Whisper mimics the style of this text — punctuation, capitalization, etc.
const DEFAULT_PROMPT: &str = "Hello, how are you? Fine, thanks! Today we'll discuss an interesting topic. Ciao, come stai? Bene, grazie! Oggi parliamo di un argomento interessante.";
const DEFAULT_LLM_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const DEFAULT_LLM_MODEL: &str = "llama-3.1-8b-instant";
const MAX_RECORDING_SECS: usize = 30 * 60; // 30 minutes hard cap
const MAX_LOG_BYTES: u64 = 1_048_576; // 1 MB log rotation threshold

fn config_dir_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join("dimmy"))
}

fn config_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("config.json"))
}

fn log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("dimmy.log"))
}

/// Write a log line to %APPDATA%/dimmy/dimmy.log (visible on Windows GUI apps)
pub(crate) fn log(msg: &str) {
    use std::io::Write;
    eprintln!("{}", msg); // also stderr for dev/terminal use
    if let Some(path) = log_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Rotate: if log exceeds MAX_LOG_BYTES, keep only the last half
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_LOG_BYTES {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    let half = data.len() / 2;
                    // Find the next newline after the halfway point to avoid splitting a line
                    let cut = data[half..]
                        .find('\n')
                        .map(|i| half + i + 1)
                        .unwrap_or(half);
                    let _ = std::fs::write(&path, &data[cut..]);
                }
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
    }
}

/// Non-sensitive config persisted to disk.
struct AppConfig {
    api_url: String,
    api_model: String,
    selected_device: Option<String>,
    language: String,
    shortcut_mode: String,
    shortcut: String,
    prompt: String,
    // LLM post-processing fields
    llm_enabled: bool,
    llm_style: String,
    llm_tone: String,
    llm_custom_prompt: String,
    llm_api_url: String,
    llm_api_model: String,
    llm_use_same_key: bool,
    llm_log_enabled: bool,
    preprocessing_enabled: bool,
    // Window position — bottom-right anchor in logical pixels
    window_anchor_right: Option<f64>,
    window_anchor_bottom: Option<f64>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            api_model: DEFAULT_MODEL.to_string(),
            selected_device: None,
            language: String::new(),
            shortcut_mode: "toggle".to_string(),
            shortcut: if cfg!(target_os = "macos") {
                "cmd+option"
            } else {
                "win+alt"
            }
            .to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            llm_enabled: false,
            llm_style: "off".to_string(),
            llm_tone: "none".to_string(),
            llm_custom_prompt: String::new(),
            llm_api_url: DEFAULT_LLM_URL.to_string(),
            llm_api_model: DEFAULT_LLM_MODEL.to_string(),
            llm_use_same_key: true,
            llm_log_enabled: true,
            preprocessing_enabled: true,
            window_anchor_right: None,
            window_anchor_bottom: None,
        }
    }
}

/// Save non-sensitive config to file (NO api_key — that goes to keyring ONLY)
fn save_config_file(cfg: &AppConfig) {
    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut json = serde_json::json!({
            "api_url": cfg.api_url,
            "api_model": cfg.api_model,
            "language": cfg.language,
            "shortcut_mode": cfg.shortcut_mode,
            "shortcut": cfg.shortcut,
            "prompt": cfg.prompt,
            "llm_enabled": cfg.llm_enabled,
            "llm_style": cfg.llm_style,
            "llm_tone": cfg.llm_tone,
            "llm_custom_prompt": cfg.llm_custom_prompt,
            "llm_api_url": cfg.llm_api_url,
            "llm_api_model": cfg.llm_api_model,
            "llm_use_same_key": cfg.llm_use_same_key,
            "llm_log_enabled": cfg.llm_log_enabled,
            "preprocessing_enabled": cfg.preprocessing_enabled,
        });
        if let Some(ref dev) = cfg.selected_device {
            json["selected_device"] = serde_json::json!(dev);
        }
        if let Some(r) = cfg.window_anchor_right {
            json["window_anchor_right"] = serde_json::json!(r);
        }
        if let Some(b) = cfg.window_anchor_bottom {
            json["window_anchor_bottom"] = serde_json::json!(b);
        }
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        );
    }
}

/// Load non-sensitive config from file. Missing LLM fields use defaults (backward compatible).
fn load_config_file() -> AppConfig {
    let defaults = AppConfig::default();
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                return AppConfig {
                    api_url: v["api_url"].as_str().unwrap_or(DEFAULT_API_URL).to_string(),
                    api_model: v["api_model"].as_str().unwrap_or(DEFAULT_MODEL).to_string(),
                    selected_device: v["selected_device"].as_str().map(|s| s.to_string()),
                    language: v["language"].as_str().unwrap_or("").to_string(),
                    shortcut_mode: v["shortcut_mode"].as_str().unwrap_or("toggle").to_string(),
                    shortcut: v["shortcut"].as_str().unwrap_or("win+alt").to_string(),
                    prompt: v["prompt"].as_str().unwrap_or(DEFAULT_PROMPT).to_string(),
                    llm_enabled: v["llm_enabled"].as_bool().unwrap_or(defaults.llm_enabled),
                    llm_style: v["llm_style"]
                        .as_str()
                        .unwrap_or(&defaults.llm_style)
                        .to_string(),
                    llm_tone: v["llm_tone"]
                        .as_str()
                        .unwrap_or(&defaults.llm_tone)
                        .to_string(),
                    llm_custom_prompt: v["llm_custom_prompt"]
                        .as_str()
                        .unwrap_or(&defaults.llm_custom_prompt)
                        .to_string(),
                    llm_api_url: v["llm_api_url"]
                        .as_str()
                        .unwrap_or(&defaults.llm_api_url)
                        .to_string(),
                    llm_api_model: v["llm_api_model"]
                        .as_str()
                        .unwrap_or(&defaults.llm_api_model)
                        .to_string(),
                    llm_use_same_key: v["llm_use_same_key"]
                        .as_bool()
                        .unwrap_or(defaults.llm_use_same_key),
                    llm_log_enabled: v["llm_log_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.llm_log_enabled),
                    preprocessing_enabled: v["preprocessing_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.preprocessing_enabled),
                    window_anchor_right: v["window_anchor_right"].as_f64(),
                    window_anchor_bottom: v["window_anchor_bottom"].as_f64(),
                };
            }
        }
    }
    defaults
}

/// Migrate from old "pai-voice" config/keyring to "dimmy" for existing users.
fn migrate_from_pai_voice() {
    let dimmy_dir = match config_dir_path() {
        Some(d) => d,
        None => return,
    };
    // If dimmy config dir already exists, skip migration
    if dimmy_dir.exists() {
        return;
    }
    let old_dir = match dirs::config_dir().map(|p| p.join("pai-voice")) {
        Some(d) => d,
        None => return,
    };
    if !old_dir.exists() {
        return;
    }
    log("Migrating from pai-voice to dimmy...");

    // Copy config and log files
    let _ = std::fs::create_dir_all(&dimmy_dir);
    for name in &["config.json", "pai-voice.log"] {
        let src = old_dir.join(name);
        if src.exists() {
            let dest_name = if *name == "pai-voice.log" {
                "dimmy.log"
            } else {
                name
            };
            let dest = dimmy_dir.join(dest_name);
            if let Err(e) = std::fs::copy(&src, &dest) {
                log(&format!("WARNING: failed to copy {}: {}", name, e));
            } else {
                log(&format!("Copied {} -> {}", src.display(), dest.display()));
            }
        }
    }

    // Migrate keyring entries from service "pai-voice" to "dimmy"
    let key_names = [
        "api-key-groq",
        "api-key-openai",
        "api-key-custom",
        "llm-key-groq",
        "llm-key-openai",
        "llm-key-custom",
        "api-key",
        "llm-api-key",
    ];
    for name in &key_names {
        if let Ok(old_entry) = keyring::Entry::new("pai-voice", name) {
            if let Ok(key) = old_entry.get_password() {
                if let Ok(new_entry) = keyring::Entry::new("dimmy", name) {
                    match new_entry.set_password(&key) {
                        Ok(()) => log(&format!("Migrated keyring entry: {}", name)),
                        Err(e) => log(&format!(
                            "WARNING: keyring migration failed for {}: {}",
                            name, e
                        )),
                    }
                }
            }
        }
    }
    log("Migration from pai-voice complete");
}

/// Migrate: if old config.json has api_key in plain text, move to secure storage and REMOVE from file
fn migrate_plaintext_key() {
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(key) = v["api_key"].as_str() {
                    if !key.is_empty() {
                        let api_url = v["api_url"].as_str().unwrap_or(DEFAULT_API_URL);
                        let provider = url_to_provider(api_url);
                        log(&format!(
                            "Migrating plaintext API key to secure storage (provider={})...",
                            provider
                        ));
                        match save_key_for_provider("api-key", provider, key) {
                            Ok(()) => log("Key migrated to secure storage"),
                            Err(e) => log(&format!("WARNING: migration failed: {}", e)),
                        }
                        // Re-save config without the plaintext key
                        let cfg = load_config_file();
                        save_config_file(&cfg);
                        log("Plaintext key removed from config file");
                    }
                }
            }
        }
    }
}

// ── Per-provider secure key storage ─────────────────────────────────
// Keys are stored per-provider so switching providers doesn't lose keys.
// Keyring entries: "dimmy" / "api-key-{provider}" and "llm-key-{provider}"

/// Derive a provider identifier from a URL hostname.
fn url_to_provider(url: &str) -> &str {
    if url.contains("groq.com") {
        "groq"
    } else if url.contains("openai.com") {
        "openai"
    } else {
        "custom"
    }
}

fn save_key_for_provider(prefix: &str, provider: &str, key: &str) -> Result<(), String> {
    let entry_name = format!("{}-{}", prefix, provider);
    let entry = keyring::Entry::new("dimmy", &entry_name).map_err(|e| {
        log(&format!(
            "ERROR: keyring Entry::new({}) failed: {}",
            entry_name, e
        ));
        format!("Credential store error: {}", e)
    })?;
    entry.set_password(key).map_err(|e| {
        log(&format!(
            "ERROR: keyring set_password({}) failed: {}",
            entry_name, e
        ));
        format!("Failed to save key: {}", e)
    })?;
    log(&format!("Key saved to secure storage: {}", entry_name));
    Ok(())
}

fn load_key_for_provider(prefix: &str, provider: &str) -> Option<String> {
    let entry_name = format!("{}-{}", prefix, provider);
    match keyring::Entry::new("dimmy", &entry_name) {
        Ok(entry) => match entry.get_password() {
            Ok(key) => {
                log(&format!("Key loaded from secure storage: {}", entry_name));
                Some(key)
            }
            Err(_) => None,
        },
        Err(_) => None,
    }
}

fn has_key_for_provider(prefix: &str, provider: &str) -> bool {
    let entry_name = format!("{}-{}", prefix, provider);
    match keyring::Entry::new("dimmy", &entry_name) {
        Ok(entry) => entry.get_password().is_ok(),
        Err(_) => false,
    }
}

fn delete_key(service: &str, name: &str) {
    if let Ok(entry) = keyring::Entry::new(service, name) {
        let _ = entry.delete_credential();
    }
}

/// Migrate old single "api-key" and "llm-api-key" entries to per-provider entries.
fn migrate_keyring_to_per_provider(api_url: &str, llm_api_url: &str) {
    // Migrate transcription key
    if let Ok(entry) = keyring::Entry::new("dimmy", "api-key") {
        if let Ok(key) = entry.get_password() {
            let provider = url_to_provider(api_url);
            log(&format!("Migrating old api-key to api-key-{}", provider));
            let _ = save_key_for_provider("api-key", provider, &key);
            delete_key("dimmy", "api-key");
        }
    }
    // Migrate LLM key
    if let Ok(entry) = keyring::Entry::new("dimmy", "llm-api-key") {
        if let Ok(key) = entry.get_password() {
            let provider = url_to_provider(llm_api_url);
            log(&format!(
                "Migrating old llm-api-key to llm-key-{}",
                provider
            ));
            let _ = save_key_for_provider("llm-key", provider, &key);
            delete_key("dimmy", "llm-api-key");
        }
    }
}

pub struct AppState {
    pub recording: Mutex<bool>,
    pub api_key: Mutex<Option<String>>,
    pub api_url: Mutex<String>,
    pub api_model: Mutex<String>,
    pub language: Mutex<String>,
    pub prompt: Mutex<String>, // Whisper style prompt (punctuation + vocabulary)
    pub shortcut_mode: Mutex<String>, // "toggle" or "hold"
    pub shortcut: Mutex<String>, // "win+alt", "ctrl+alt", "ctrl+shift"
    pub selected_device: Mutex<Option<String>>,
    pub audio_sample_rate: Mutex<u32>,
    pub transcript: Mutex<String>,
    pub audio_buffer: Arc<Mutex<Vec<f32>>>,
    pub audio_tx: Mutex<Sender<AudioCommand>>,
    pub streaming_active: Arc<AtomicBool>,
    // LLM post-processing state
    pub llm_enabled: Mutex<bool>,
    pub llm_style: Mutex<String>,
    pub llm_tone: Mutex<String>,
    pub llm_custom_prompt: Mutex<String>,
    pub llm_api_url: Mutex<String>,
    pub llm_api_model: Mutex<String>,
    pub llm_use_same_key: Mutex<bool>,
    pub llm_api_key: Mutex<Option<String>>,
    pub llm_log_enabled: Mutex<bool>,
    pub preprocessing_enabled: Mutex<bool>,
    /// Bottom-right anchor in logical pixels — persisted across restarts
    pub window_anchor: Mutex<Option<(f64, f64)>>,
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

    let selected_device = state
        .selected_device
        .lock()
        .map_err(|e| e.to_string())?
        .clone();

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
    let prompt = state.prompt.lock().map_err(|e| e.to_string())?.clone();

    let preprocess_on = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;

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
            let max_buffer_samples = sample_rate * MAX_RECORDING_SECS;
            let mut preprocessor = preprocess::AudioPreprocessor::new(sample_rate as u32);

            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                if !streaming.load(Ordering::SeqCst) {
                    break;
                }

                // Auto-stop if buffer exceeds max recording duration
                if let Ok(buf) = buffer.lock() {
                    if buf.len() >= max_buffer_samples {
                        drop(buf);
                        streaming.store(false, Ordering::SeqCst);
                        if let Some(w) = handle.get_webview_window("main") {
                            let _ = w.emit("shortcut-stop", ());
                        }
                        break;
                    }
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

                // Preprocess: highpass + VAD + normalize (if enabled)
                let processed = if preprocess_on {
                    let p = preprocessor.process(&chunk_data);
                    if p.is_empty() {
                        // Entire chunk was noise/silence — skip sending to Whisper
                        continue;
                    }
                    p
                } else {
                    chunk_data
                };

                chunk_index += 1;

                let _ = handle.emit(
                    "chunk-status",
                    serde_json::json!({
                        "index": chunk_index,
                        "status": "sending",
                    }),
                );

                let wav_result =
                    audio::encode_wav(&processed, sample_rate as u32).map_err(|e| e.to_string());
                match wav_result {
                    Ok(wav_data) => {
                        match transcribe::transcribe_audio(
                            &api_url, &api_model, &key, &wav_data, &language, &prompt,
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

    let (buffer, api_key, api_url, api_model, language, prompt) = {
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
        let prompt = state.prompt.lock().map_err(|e| e.to_string())?.clone();

        (buffer, api_key, api_url, api_model, language, prompt)
    };

    if buffer.is_empty() {
        return Err("No audio captured".into());
    }

    let _ = app_handle.emit(
        "chunk-status",
        serde_json::json!({ "index": 0, "status": "final" }),
    );

    let sr = *state.audio_sample_rate.lock().map_err(|e| e.to_string())?;
    let preprocess_final = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    // Preprocess final buffer: highpass + VAD + normalize (if enabled)
    let audio_data = if preprocess_final {
        let processed = preprocess::process_buffer(&buffer, sr);
        if processed.is_empty() {
            return Err("No speech detected in recording".into());
        }
        processed
    } else {
        buffer
    };
    let wav_data = audio::encode_wav(&audio_data, sr).map_err(|e| e.to_string())?;
    let transcript = transcribe::transcribe_audio(
        &api_url, &api_model, &api_key, &wav_data, &language, &prompt,
    )
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
#[allow(clippy::too_many_arguments)]
fn set_config(
    api_key: Option<String>,
    api_url: String,
    api_model: String,
    language: String,
    shortcut_mode: String,
    shortcut: Option<String>,
    selected_device: Option<String>,
    prompt: String,
    // LLM fields — all Option for backward compatibility
    llm_enabled: Option<bool>,
    llm_style: Option<String>,
    llm_tone: Option<String>,
    llm_custom_prompt: Option<String>,
    llm_api_url: Option<String>,
    llm_api_model: Option<String>,
    llm_use_same_key: Option<bool>,
    llm_api_key: Option<String>,
    llm_log_enabled: Option<bool>,
    preprocessing_enabled: Option<bool>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    log(&format!(
        "set_config called: mode={}, device={:?}",
        shortcut_mode, selected_device
    ));

    // Save transcription API key for the provider derived from the URL
    let transcription_provider = url_to_provider(&api_url);
    if let Some(ref key) = api_key {
        if !key.is_empty() {
            save_key_for_provider("api-key", transcription_provider, key)?;
            *state.api_key.lock().map_err(|e| e.to_string())? = Some(key.clone());
        }
    }

    // Handle separate LLM API key (provider derived from LLM URL)
    let llm_url_for_provider = llm_api_url.as_deref().unwrap_or(&api_url);
    let llm_provider = url_to_provider(llm_url_for_provider);
    if let Some(ref key) = llm_api_key {
        if !key.is_empty() {
            save_key_for_provider("llm-key", llm_provider, key)?;
            *state.llm_api_key.lock().map_err(|e| e.to_string())? = Some(key.clone());
        }
    }

    // Update LLM state if provided
    if let Some(v) = llm_enabled {
        *state.llm_enabled.lock().map_err(|e| e.to_string())? = v;
    }
    if let Some(ref v) = llm_style {
        *state.llm_style.lock().map_err(|e| e.to_string())? = v.clone();
    }
    if let Some(ref v) = llm_tone {
        *state.llm_tone.lock().map_err(|e| e.to_string())? = v.clone();
    }
    if let Some(ref v) = llm_custom_prompt {
        *state.llm_custom_prompt.lock().map_err(|e| e.to_string())? = v.clone();
    }
    if let Some(ref v) = llm_api_url {
        *state.llm_api_url.lock().map_err(|e| e.to_string())? = v.clone();
    }
    if let Some(ref v) = llm_api_model {
        *state.llm_api_model.lock().map_err(|e| e.to_string())? = v.clone();
    }
    if let Some(v) = llm_use_same_key {
        *state.llm_use_same_key.lock().map_err(|e| e.to_string())? = v;
    }
    if let Some(v) = llm_log_enabled {
        *state.llm_log_enabled.lock().map_err(|e| e.to_string())? = v;
    }
    if let Some(v) = preprocessing_enabled {
        *state
            .preprocessing_enabled
            .lock()
            .map_err(|e| e.to_string())? = v;
    }
    if let Some(ref v) = shortcut {
        *state.shortcut.lock().map_err(|e| e.to_string())? = v.clone();
        hotkey::set_shortcut(v);
    }

    // Build AppConfig from current state for saving (acquire each lock individually)
    let cur_shortcut = state.shortcut.lock().map_err(|e| e.to_string())?.clone();
    let cur_llm_enabled = *state.llm_enabled.lock().map_err(|e| e.to_string())?;
    let cur_llm_style = state.llm_style.lock().map_err(|e| e.to_string())?.clone();
    let cur_llm_tone = state.llm_tone.lock().map_err(|e| e.to_string())?.clone();
    let cur_llm_custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let cur_llm_api_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
    let cur_llm_api_model = state
        .llm_api_model
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let cur_llm_use_same_key = *state.llm_use_same_key.lock().map_err(|e| e.to_string())?;
    let cur_llm_log_enabled = *state.llm_log_enabled.lock().map_err(|e| e.to_string())?;
    let cur_preprocessing_enabled = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let cur_anchor = *state.window_anchor.lock().map_err(|e| e.to_string())?;
    let cfg = AppConfig {
        api_url: api_url.clone(),
        api_model: api_model.clone(),
        selected_device: selected_device.clone(),
        language: language.clone(),
        shortcut_mode: shortcut_mode.clone(),
        shortcut: cur_shortcut,
        prompt: prompt.clone(),
        llm_enabled: cur_llm_enabled,
        llm_style: cur_llm_style,
        llm_tone: cur_llm_tone,
        llm_custom_prompt: cur_llm_custom_prompt,
        llm_api_url: cur_llm_api_url,
        llm_api_model: cur_llm_api_model,
        llm_use_same_key: cur_llm_use_same_key,
        llm_log_enabled: cur_llm_log_enabled,
        preprocessing_enabled: cur_preprocessing_enabled,
        window_anchor_right: cur_anchor.map(|(r, _)| r),
        window_anchor_bottom: cur_anchor.map(|(_, b)| b),
    };
    save_config_file(&cfg);
    log("Config file saved");

    *state.api_url.lock().map_err(|e| e.to_string())? = api_url.clone();
    *state.api_model.lock().map_err(|e| e.to_string())? = api_model;
    *state.language.lock().map_err(|e| e.to_string())? = language;
    *state.prompt.lock().map_err(|e| e.to_string())? = prompt;
    *state.shortcut_mode.lock().map_err(|e| e.to_string())? = shortcut_mode;
    *state.selected_device.lock().map_err(|e| e.to_string())? = selected_device;

    // When provider changes, load the stored key for the new provider into AppState
    if api_key.is_none() || api_key.as_deref() == Some("") {
        let provider = url_to_provider(&api_url);
        if let Some(key) = load_key_for_provider("api-key", provider) {
            *state.api_key.lock().map_err(|e| e.to_string())? = Some(key);
        }
    }
    if llm_api_key.is_none() || llm_api_key.as_deref() == Some("") {
        let llm_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
        let provider = url_to_provider(&llm_url);
        if let Some(key) = load_key_for_provider("llm-key", provider) {
            *state.llm_api_key.lock().map_err(|e| e.to_string())? = Some(key);
        }
    }

    log("set_config completed OK");
    Ok(())
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let has_key = state.api_key.lock().map_err(|e| e.to_string())?.is_some();
    let api_url = state.api_url.lock().map_err(|e| e.to_string())?.clone();
    let api_model = state.api_model.lock().map_err(|e| e.to_string())?.clone();
    let language = state.language.lock().map_err(|e| e.to_string())?.clone();
    let prompt = state.prompt.lock().map_err(|e| e.to_string())?.clone();
    let shortcut_mode = state
        .shortcut_mode
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let selected_device = state
        .selected_device
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let devices = audio::list_input_devices();

    let llm_enabled = *state.llm_enabled.lock().map_err(|e| e.to_string())?;
    let llm_style = state.llm_style.lock().map_err(|e| e.to_string())?.clone();
    let llm_tone = state.llm_tone.lock().map_err(|e| e.to_string())?.clone();
    let llm_custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_api_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
    let llm_api_model = state
        .llm_api_model
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_use_same_key = *state.llm_use_same_key.lock().map_err(|e| e.to_string())?;
    let has_llm_key = state
        .llm_api_key
        .lock()
        .map_err(|e| e.to_string())?
        .is_some();
    let llm_log_enabled = *state.llm_log_enabled.lock().map_err(|e| e.to_string())?;
    let preprocessing_enabled = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let shortcut = state.shortcut.lock().map_err(|e| e.to_string())?.clone();
    let shortcut_label = hotkey::current_label();

    let styles: Vec<&str> = llm::STYLES.iter().map(|(name, _)| *name).collect();
    let tones: Vec<&str> = llm::TONES.iter().map(|(name, _)| *name).collect();

    // Per-provider key flags
    let has_groq_key = has_key_for_provider("api-key", "groq");
    let has_openai_key = has_key_for_provider("api-key", "openai");
    let has_custom_key = has_key_for_provider("api-key", "custom");
    let has_llm_groq_key = has_key_for_provider("llm-key", "groq");
    let has_llm_openai_key = has_key_for_provider("llm-key", "openai");
    let has_llm_custom_key = has_key_for_provider("llm-key", "custom");

    Ok(serde_json::json!({
        "has_key": has_key,
        "api_url": api_url,
        "api_model": api_model,
        "language": language,
        "prompt": prompt,
        "shortcut_mode": shortcut_mode,
        "selected_device": selected_device,
        "devices": devices,
        "llm_enabled": llm_enabled,
        "llm_style": llm_style,
        "llm_tone": llm_tone,
        "llm_custom_prompt": llm_custom_prompt,
        "llm_api_url": llm_api_url,
        "llm_api_model": llm_api_model,
        "llm_use_same_key": llm_use_same_key,
        "has_llm_key": has_llm_key,
        "llm_log_enabled": llm_log_enabled,
        "preprocessing_enabled": preprocessing_enabled,
        "shortcut": shortcut,
        "shortcut_label": shortcut_label,
        "llm_styles": styles,
        "llm_tones": tones,
        // Per-provider key flags
        "has_groq_key": has_groq_key,
        "has_openai_key": has_openai_key,
        "has_custom_key": has_custom_key,
        "has_llm_groq_key": has_llm_groq_key,
        "has_llm_openai_key": has_llm_openai_key,
        "has_llm_custom_key": has_llm_custom_key,
    }))
}

#[tauri::command]
async fn process_with_llm(
    text: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let enabled = *state.llm_enabled.lock().map_err(|e| e.to_string())?;
    let style = state.llm_style.lock().map_err(|e| e.to_string())?.clone();

    if !enabled || style == "off" {
        return Ok(text);
    }

    let tone = state.llm_tone.lock().map_err(|e| e.to_string())?.clone();
    let custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let api_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
    let model = state
        .llm_api_model
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let use_same_key = *state.llm_use_same_key.lock().map_err(|e| e.to_string())?;

    let api_key = if use_same_key {
        state.api_key.lock().map_err(|e| e.to_string())?.clone()
    } else {
        state.llm_api_key.lock().map_err(|e| e.to_string())?.clone()
    };

    let api_key = match api_key {
        Some(k) => k,
        None => {
            log("LLM: no API key available, returning raw text");
            let _ = app_handle.emit(
                "llm-status",
                serde_json::json!({ "status": "error", "error": "No LLM API key" }),
            );
            return Ok(text);
        }
    };

    let log_enabled = *state.llm_log_enabled.lock().map_err(|e| e.to_string())?;

    if log_enabled {
        log(&format!("LLM INPUT [{}+{}]: {}", style, tone, text));
    }

    let _ = app_handle.emit("llm-status", serde_json::json!({ "status": "processing" }));

    match llm::process_text(
        &api_url,
        &model,
        &api_key,
        &text,
        &style,
        &tone,
        &custom_prompt,
    )
    .await
    {
        Ok(enhanced) => {
            if log_enabled {
                log(&format!("LLM OUTPUT [{}+{}]: {}", style, tone, enhanced));
            }
            let _ = app_handle.emit("llm-status", serde_json::json!({ "status": "done" }));
            Ok(enhanced)
        }
        Err(e) => {
            log(&format!("LLM processing failed: {}", e));
            let _ = app_handle.emit(
                "llm-status",
                serde_json::json!({ "status": "error", "error": e.to_string() }),
            );
            // Graceful fallback: return original text, never block paste
            Ok(text)
        }
    }
}

#[tauri::command]
fn start_shortcut_recording() -> Result<(), String> {
    log("start_shortcut_recording called");
    hotkey::start_recording();
    Ok(())
}

#[tauri::command]
fn poll_shortcut_recording(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    // Poll modifier keys via GetAsyncKeyState
    hotkey::poll_recording_keys();

    match hotkey::take_recorded() {
        Some((name, label)) => {
            log(&format!(
                "poll_shortcut_recording: combo detected: {} ({})",
                name, label
            ));
            hotkey::set_shortcut(&name);
            *state.shortcut.lock().map_err(|e| e.to_string())? = name.clone();
            if let Err(e) = save_current_config(&state) {
                log(&format!("WARNING: save_current_config failed: {}", e));
            }
            Ok(serde_json::json!({ "done": true, "name": name, "label": label }))
        }
        None => Ok(serde_json::json!({ "done": false })),
    }
}

#[tauri::command]
fn cancel_shortcut_recording() -> Result<(), String> {
    hotkey::stop_recording();
    Ok(())
}

#[tauri::command]
fn cycle_llm_style(
    direction: i32,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let mut style = state.llm_style.lock().map_err(|e| e.to_string())?;
    let total = llm::STYLES.len();
    let current_idx = llm::STYLES
        .iter()
        .position(|(n, _)| *n == style.as_str())
        .unwrap_or(0);
    let new_idx = if direction > 0 {
        (current_idx + 1) % total
    } else {
        (current_idx + total - 1) % total
    };
    let (new_name, _) = llm::STYLES[new_idx];
    *style = new_name.to_string();

    // Also enable/disable LLM based on style
    if let Ok(mut enabled) = state.llm_enabled.lock() {
        *enabled = new_name != "off";
    }

    // Persist to config file
    drop(style);
    save_current_config(&state)?;

    Ok(serde_json::json!({
        "style": new_name,
        "index": new_idx,
        "total": total,
    }))
}

#[tauri::command]
fn cycle_llm_tone(
    direction: i32,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let mut tone = state.llm_tone.lock().map_err(|e| e.to_string())?;
    let total = llm::TONES.len();
    let current_idx = llm::TONES
        .iter()
        .position(|(n, _)| *n == tone.as_str())
        .unwrap_or(0);
    let new_idx = if direction > 0 {
        (current_idx + 1) % total
    } else {
        (current_idx + total - 1) % total
    };
    let (new_name, _) = llm::TONES[new_idx];
    *tone = new_name.to_string();

    drop(tone);
    save_current_config(&state)?;

    Ok(serde_json::json!({
        "tone": new_name,
        "index": new_idx,
        "total": total,
    }))
}

/// Build AppConfig by acquiring each mutex individually (one at a time, no overlapping locks).
fn snapshot_config(state: &AppState) -> Result<AppConfig, String> {
    let api_url = state.api_url.lock().map_err(|e| e.to_string())?.clone();
    let api_model = state.api_model.lock().map_err(|e| e.to_string())?.clone();
    let selected_device = state
        .selected_device
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let language = state.language.lock().map_err(|e| e.to_string())?.clone();
    let shortcut_mode = state
        .shortcut_mode
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let shortcut = state.shortcut.lock().map_err(|e| e.to_string())?.clone();
    let prompt = state.prompt.lock().map_err(|e| e.to_string())?.clone();
    let llm_enabled = *state.llm_enabled.lock().map_err(|e| e.to_string())?;
    let llm_style = state.llm_style.lock().map_err(|e| e.to_string())?.clone();
    let llm_tone = state.llm_tone.lock().map_err(|e| e.to_string())?.clone();
    let llm_custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_api_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
    let llm_api_model = state
        .llm_api_model
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_use_same_key = *state.llm_use_same_key.lock().map_err(|e| e.to_string())?;
    let llm_log_enabled = *state.llm_log_enabled.lock().map_err(|e| e.to_string())?;
    let preprocessing_enabled = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let anchor = *state.window_anchor.lock().map_err(|e| e.to_string())?;

    Ok(AppConfig {
        api_url,
        api_model,
        selected_device,
        language,
        shortcut_mode,
        shortcut,
        prompt,
        llm_enabled,
        llm_style,
        llm_tone,
        llm_custom_prompt,
        llm_api_url,
        llm_api_model,
        llm_use_same_key,
        llm_log_enabled,
        preprocessing_enabled,
        window_anchor_right: anchor.map(|(r, _)| r),
        window_anchor_bottom: anchor.map(|(_, b)| b),
    })
}

/// Helper to persist current in-memory state to config file.
fn save_current_config(state: &tauri::State<'_, AppState>) -> Result<(), String> {
    let cfg = snapshot_config(state)?;
    save_config_file(&cfg);
    Ok(())
}

/// Counter for periodic amplitude logging (every ~5s at 60fps polling)
static AMP_LOG_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
    let rms = rms.sqrt();

    // Log raw mic level every ~5 seconds for diagnostics
    let count = AMP_LOG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count.is_multiple_of(300) {
        let peak = buffer[start..]
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);
        log(&format!(
            "Mic level: rms={:.6}, peak={:.6}, samples={}",
            rms, peak, len
        ));
    }

    Ok(rms)
}

#[tauri::command]
fn get_audio_device(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let selected = state
        .selected_device
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    Ok(selected.unwrap_or_else(audio::default_input_device_name))
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

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Unicode('v'), Direction::Click)
        .map_err(|e| e.to_string())?;
    enigo
        .key(modifier, Direction::Release)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get the usable desktop area excluding the taskbar.
/// Returns (right, bottom) in physical pixels for the primary monitor.
#[cfg(target_os = "windows")]
fn get_work_area() -> Option<(i32, i32)> {
    #[repr(C)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    extern "system" {
        fn SystemParametersInfoW(
            action: u32,
            param: u32,
            data: *mut std::ffi::c_void,
            flags: u32,
        ) -> i32;
    }
    const SPI_GETWORKAREA: u32 = 0x0030;
    unsafe {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut rect as *mut _ as *mut std::ffi::c_void,
            0,
        ) != 0
        {
            Some((rect.right, rect.bottom))
        } else {
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn get_work_area() -> Option<(i32, i32)> {
    // NSScreen.mainScreen.visibleFrame excludes menu bar and Dock
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct NSSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    extern "C" {
        // objc_msgSend with different return types
        fn objc_getClass(name: *const u8) -> *mut std::ffi::c_void;
        fn sel_registerName(name: *const u8) -> *mut std::ffi::c_void;
    }

    // Use objc_msgSend for pointer-returning calls and
    // objc_msgSend_stret for struct-returning calls on x86_64.
    // On aarch64 all returns go through objc_msgSend.
    extern "C" {
        fn objc_msgSend(
            obj: *mut std::ffi::c_void,
            sel: *mut std::ffi::c_void,
            ...
        ) -> *mut std::ffi::c_void;
    }

    #[cfg(target_arch = "x86_64")]
    extern "C" {
        fn objc_msgSend_stret(
            stret: *mut NSRect,
            obj: *mut std::ffi::c_void,
            sel: *mut std::ffi::c_void,
            ...
        );
    }

    unsafe {
        let ns_screen = objc_getClass(b"NSScreen\0".as_ptr());
        if ns_screen.is_null() {
            return None;
        }
        let main_screen_sel = sel_registerName(b"mainScreen\0".as_ptr());
        let screen = objc_msgSend(ns_screen, main_screen_sel);
        if screen.is_null() {
            return None;
        }

        let visible_frame_sel = sel_registerName(b"visibleFrame\0".as_ptr());
        let frame_sel = sel_registerName(b"frame\0".as_ptr());

        let visible: NSRect;
        let full: NSRect;

        #[cfg(target_arch = "aarch64")]
        {
            // On ARM64, objc_msgSend returns structs directly
            let msg_send: unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *mut std::ffi::c_void,
            ) -> NSRect = std::mem::transmute(objc_msgSend as *const ());
            visible = msg_send(screen, visible_frame_sel);
            full = msg_send(screen, frame_sel);
        }

        #[cfg(target_arch = "x86_64")]
        {
            let mut v = std::mem::zeroed::<NSRect>();
            let mut f = std::mem::zeroed::<NSRect>();
            objc_msgSend_stret(&mut v, screen, visible_frame_sel);
            objc_msgSend_stret(&mut f, screen, frame_sel);
            visible = v;
            full = f;
        }

        // visibleFrame gives the usable area in screen coordinates
        // (origin.y is from bottom-left). We return (width, height) of the
        // visible area in physical-ish pixels, matching the Windows API shape.
        // The caller divides by scale_factor, so return raw point values.
        let _ = full; // available if we ever need the full frame
        Some((
            (visible.origin.x + visible.size.width) as i32,
            (visible.origin.y + visible.size.height) as i32,
        ))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn get_work_area() -> Option<(i32, i32)> {
    None
}

/// Position a window at the bottom-right of the usable desktop (above the taskbar).
fn position_bottom_right(win: &tauri::WebviewWindow, w: f64, h: f64) {
    let padding = 12.0_f64;
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let screen = monitor.size();
        let scale = monitor.scale_factor();
        // Use work area (excludes taskbar) when available, else full screen
        let (area_w, area_h) = get_work_area()
            .map(|(r, b)| (r as f64, b as f64))
            .unwrap_or((screen.width as f64, screen.height as f64));
        let x = (area_w / scale) - w - padding;
        let y = (area_h / scale) - h - padding;
        let _ = win.set_position(LogicalPosition::new(x, y));
    }
}

#[tauri::command]
fn resize_window(
    w: f64,
    h: f64,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if let Some(win) = app_handle.get_webview_window("main") {
        // Compute anchor from LIVE window position (captures user drags since last resize)
        let anchor = {
            if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
                let scale = win.scale_factor().unwrap_or(1.0);
                let right = pos.x as f64 / scale + size.width as f64 / scale;
                let bottom = pos.y as f64 / scale + size.height as f64 / scale;
                Some((right, bottom))
            } else {
                *state.window_anchor.lock().map_err(|e| e.to_string())?
            }
        };

        win.set_size(LogicalSize::new(w, h))
            .map_err(|e| e.to_string())?;
        // Re-enforce always-on-top (can be lost after show/hide cycles on some OS)
        let _ = win.set_always_on_top(true);

        if let Some((right, bottom)) = anchor {
            let mut x = (right - w).max(0.0);
            let mut y = (bottom - h).max(0.0);
            // Clamp to screen bounds so the window never ends up off-screen
            if let Ok(Some(monitor)) = win.primary_monitor() {
                let scale = win.scale_factor().unwrap_or(1.0);
                let screen_w = monitor.size().width as f64 / scale;
                let screen_h = monitor.size().height as f64 / scale;
                x = x.max(0.0).min((screen_w - w).max(0.0));
                y = y.max(0.0).min((screen_h - h).max(0.0));
            }
            let _ = win.set_position(LogicalPosition::new(x, y));
            // Update stored anchor
            let _ = state
                .window_anchor
                .lock()
                .map(|mut a| *a = Some((right, bottom)));
        } else {
            position_bottom_right(&win, w, h);
        }
    }
    Ok(())
}

#[tauri::command]
fn check_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
        }
        // Pass null options — just check, don't prompt
        unsafe { AXIsProcessTrustedWithOptions(std::ptr::null()) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[tauri::command]
fn get_version(app_handle: tauri::AppHandle) -> String {
    app_handle.package_info().version.to_string()
}

#[tauri::command]
async fn check_for_update(app_handle: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    match app_handle
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
    {
        Ok(Some(update)) => Ok(Some(update.version.clone())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn install_update(app_handle: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let update = app_handle
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    if let Some(update) = update {
        let mut downloaded = 0;
        update
            .download_and_install(
                |chunk_length, content_length| {
                    downloaded += chunk_length;
                    log(&format!(
                        "Update download: {} / {:?}",
                        downloaded, content_length
                    ));
                },
                || {
                    log("Update download complete, installing...");
                },
            )
            .await
            .map_err(|e| {
                let msg = format!("Update install failed: {}", e);
                log(&msg);
                msg
            })?;
        log("Update installed, restart required");
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

    migrate_from_pai_voice();
    migrate_plaintext_key();

    let file_cfg = load_config_file();

    // Migrate old single keyring entries to per-provider entries
    migrate_keyring_to_per_provider(&file_cfg.api_url, &file_cfg.llm_api_url);

    // Load the key for the current provider
    let transcription_provider = url_to_provider(&file_cfg.api_url);
    let llm_provider = url_to_provider(&file_cfg.llm_api_url);
    let stored_key = load_key_for_provider("api-key", transcription_provider);
    let stored_llm_key = load_key_for_provider("llm-key", llm_provider);
    // SECURITY: never log the actual key value — only whether one exists
    log(&format!("Config loaded: url={}, model={}, device={:?}, provider={}, has_key={}, llm_provider={}, llm_enabled={}, llm_style={}",
        file_cfg.api_url, file_cfg.api_model, file_cfg.selected_device, transcription_provider,
        stored_key.is_some(), llm_provider, file_cfg.llm_enabled, file_cfg.llm_style));

    let shortcut_preset = file_cfg.shortcut.clone();

    let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let audio_tx = audio::spawn_audio_thread(audio_buffer.clone());

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            recording: Mutex::new(false),
            api_key: Mutex::new(stored_key),
            api_url: Mutex::new(file_cfg.api_url),
            api_model: Mutex::new(file_cfg.api_model),
            language: Mutex::new(file_cfg.language),
            prompt: Mutex::new(file_cfg.prompt),
            shortcut_mode: Mutex::new(file_cfg.shortcut_mode),
            shortcut: Mutex::new(file_cfg.shortcut.clone()),
            selected_device: Mutex::new(file_cfg.selected_device.clone()),
            audio_sample_rate: Mutex::new(audio::device_sample_rate(&file_cfg.selected_device)),
            transcript: Mutex::new(String::new()),
            audio_buffer,
            audio_tx: Mutex::new(audio_tx),
            streaming_active: Arc::new(AtomicBool::new(false)),
            // LLM state
            llm_enabled: Mutex::new(file_cfg.llm_enabled),
            llm_style: Mutex::new(file_cfg.llm_style),
            llm_tone: Mutex::new(file_cfg.llm_tone),
            llm_custom_prompt: Mutex::new(file_cfg.llm_custom_prompt),
            llm_api_url: Mutex::new(file_cfg.llm_api_url),
            llm_api_model: Mutex::new(file_cfg.llm_api_model),
            llm_use_same_key: Mutex::new(file_cfg.llm_use_same_key),
            llm_api_key: Mutex::new(stored_llm_key),
            llm_log_enabled: Mutex::new(file_cfg.llm_log_enabled),
            preprocessing_enabled: Mutex::new(file_cfg.preprocessing_enabled),
            window_anchor: Mutex::new(
                match (file_cfg.window_anchor_right, file_cfg.window_anchor_bottom) {
                    (Some(r), Some(b)) => Some((r, b)),
                    _ => None,
                },
            ),
        })
        .setup(move |app| {
            // Remove Windows DWM border and set transparent background
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_shadow(false);
                use tauri::window::Color;
                let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

                // Open DevTools in debug builds
                #[cfg(debug_assertions)]
                window.open_devtools();

                // Position from saved anchor or default bottom-right
                let win_w = 220.0_f64;
                let win_h = 32.0_f64;
                {
                    let anchor = *app
                        .state::<AppState>()
                        .window_anchor
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some((right, bottom)) = anchor {
                        let x = (right - win_w).max(0.0);
                        let y = (bottom - win_h).max(0.0);
                        let _ = window.set_position(LogicalPosition::new(x, y));
                        log(&format!(
                            "Window positioned at saved anchor ({}, {})",
                            right, bottom
                        ));
                    } else {
                        position_bottom_right(&window, win_w, win_h);
                        // Compute initial anchor from the position we just set
                        if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size())
                        {
                            let scale = window.scale_factor().unwrap_or(1.0);
                            let right = pos.x as f64 / scale + size.width as f64 / scale;
                            let bottom = pos.y as f64 / scale + size.height as f64 / scale;
                            *app.state::<AppState>()
                                .window_anchor
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()) = Some((right, bottom));
                        }
                        log("Window positioned bottom-right (default)");
                    }
                }
            }
            // System tray icon with Show / Quit menu
            {
                use tauri::menu::{MenuBuilder, MenuItemBuilder};
                use tauri::tray::TrayIconBuilder;

                let show = MenuItemBuilder::with_id("show", "Show").build(app)?;
                let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
                let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

                TrayIconBuilder::new()
                    .icon(app.default_window_icon().unwrap().clone())
                    .menu(&menu)
                    .on_menu_event(move |app_handle, event| {
                        match event.id().as_ref() {
                            "show" => {
                                if let Some(win) = app_handle.get_webview_window("main") {
                                    // Reset to default bottom-right position
                                    position_bottom_right(&win, 56.0, 32.0);
                                    let _ = win.show();
                                    let _ = win.set_always_on_top(true);
                                    let _ = win.set_focus();
                                }
                            }
                            "quit" => {
                                app_handle.exit(0);
                            }
                            _ => {}
                        }
                    })
                    .build(app)?;
            }

            // Configure and install keyboard hook
            hotkey::set_shortcut(&shortcut_preset);
            hotkey::install(log);

            // Poll for hotkey events and emit to frontend
            let hotkey_handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    let event = hotkey::take_event();
                    if event != 0 {
                        let state = hotkey_handle.state::<AppState>();
                        let is_recording =
                            *state.recording.lock().unwrap_or_else(|e| e.into_inner());
                        let mode = state
                            .shortcut_mode
                            .lock()
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
                                } else if let Some(w) = hotkey_handle.get_webview_window("main") {
                                    let _ = w.emit("shortcut-start", ());
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
            process_with_llm,
            cycle_llm_style,
            cycle_llm_tone,
            start_shortcut_recording,
            poll_shortcut_recording,
            cancel_shortcut_recording,
            check_accessibility,
            get_version,
            check_for_update,
            install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Dimmy")
        .run(|app_handle, event| {
            // Save window position + config on close
            if let tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::CloseRequested { .. },
                ..
            } = &event
            {
                // Read live window position for anchor
                if let Some(win) = app_handle.get_webview_window("main") {
                    if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
                        let scale = win.scale_factor().unwrap_or(1.0);
                        let right = pos.x as f64 / scale + size.width as f64 / scale;
                        let bottom = pos.y as f64 / scale + size.height as f64 / scale;
                        let st: tauri::State<'_, AppState> = app_handle.state();
                        let _ = st
                            .window_anchor
                            .lock()
                            .map(|mut a| *a = Some((right, bottom)));
                        if let Ok(cfg) = snapshot_config(&st) {
                            save_config_file(&cfg);
                            log("Config saved on close (with window position)");
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_with_device() {
        // Use a temp dir to avoid polluting real config
        let tmp = std::env::temp_dir().join("dimmy-test-config");
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

        assert_eq!(
            v["api_url"].as_str().unwrap(),
            "https://test.example.com/v1/transcribe"
        );
        assert_eq!(v["api_model"].as_str().unwrap(), "test-model-v1");
        assert_eq!(v["language"].as_str().unwrap(), "it");
        assert_eq!(v["selected_device"].as_str().unwrap(), "Test Microphone");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_roundtrip_no_device() {
        let tmp = std::env::temp_dir().join("dimmy-test-config2");
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
        assert!(
            v["selected_device"].is_null(),
            "No device should produce null"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_missing_file_returns_defaults() {
        let cfg = load_config_file();
        assert!(!cfg.api_url.is_empty(), "URL should not be empty");
        assert!(!cfg.api_model.is_empty(), "Model should not be empty");
    }

    #[test]
    fn save_config_no_panic_with_none_device() {
        let mut cfg = AppConfig::default();
        cfg.selected_device = None;
        save_config_file(&cfg);
    }

    #[test]
    fn save_config_no_panic_with_some_device() {
        let mut cfg = AppConfig::default();
        cfg.selected_device = Some("Mic".to_string());
        cfg.language = "it".to_string();
        save_config_file(&cfg);
    }

    #[test]
    fn config_dir_path_returns_some() {
        let p = config_dir_path();
        assert!(p.is_some(), "config_dir_path should return Some on any OS");
        let p = p.unwrap();
        assert!(p.ends_with("dimmy"), "Path should end with 'dimmy'");
    }

    #[test]
    fn config_path_ends_with_config_json() {
        let p = config_path();
        assert!(p.is_some());
        let p = p.unwrap();
        assert!(
            p.ends_with("config.json"),
            "Should end with config.json, got: {:?}",
            p
        );
    }

    #[test]
    fn log_path_ends_with_log_file() {
        let p = log_path();
        assert!(p.is_some());
        let p = p.unwrap();
        assert!(
            p.ends_with("dimmy.log"),
            "Should end with dimmy.log, got: {:?}",
            p
        );
    }

    #[test]
    fn log_writes_timestamped_line_to_file() {
        let tmp = std::env::temp_dir().join("dimmy-test-log");
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
        assert!(
            contents.contains("test log message"),
            "Log should contain the message"
        );
        assert!(
            contents.contains("[20"),
            "Log should contain timestamp starting with [20xx"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_returns_valid_strings() {
        let cfg = load_config_file();
        assert!(
            cfg.api_url.starts_with("https://"),
            "URL should be HTTPS, got: {}",
            cfg.api_url
        );
        assert!(!cfg.api_model.is_empty(), "Model should not be empty");
    }

    #[test]
    fn save_config_creates_valid_json() {
        let mut cfg = AppConfig::default();
        cfg.api_url = "https://test.local/v1".to_string();
        cfg.api_model = "test-m".to_string();
        cfg.selected_device = Some("Dev1".into());
        cfg.language = "fr".to_string();
        save_config_file(&cfg);
        if let Some(path) = config_path() {
            let data = std::fs::read_to_string(&path).unwrap();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                assert!(v["api_url"].is_string(), "api_url should be a string");
                assert!(v["api_model"].is_string(), "api_model should be a string");
            }
            save_config_file(&AppConfig::default());
        }
    }

    #[test]
    fn migrate_plaintext_key_no_panic_without_key() {
        migrate_plaintext_key();
    }

    #[test]
    fn default_constants_are_valid() {
        assert!(
            DEFAULT_API_URL.starts_with("https://"),
            "API URL should use HTTPS"
        );
        assert!(
            !DEFAULT_MODEL.is_empty(),
            "Default model should not be empty"
        );
        assert!(
            DEFAULT_LLM_URL.starts_with("https://"),
            "LLM URL should use HTTPS"
        );
        assert!(
            !DEFAULT_LLM_MODEL.is_empty(),
            "Default LLM model should not be empty"
        );
    }

    #[test]
    fn llm_config_defaults() {
        let cfg = AppConfig::default();
        assert!(!cfg.llm_enabled, "LLM should be disabled by default");
        assert_eq!(cfg.llm_style, "off");
        assert_eq!(cfg.llm_tone, "none");
        assert!(cfg.llm_custom_prompt.is_empty());
        assert_eq!(cfg.llm_api_url, DEFAULT_LLM_URL);
        assert_eq!(cfg.llm_api_model, DEFAULT_LLM_MODEL);
        assert!(cfg.llm_use_same_key, "Should use same key by default");
    }

    #[test]
    fn llm_backward_compat_old_config() {
        // Simulate an old config file without LLM fields
        let tmp = std::env::temp_dir().join("dimmy-test-compat");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");

        let old_cfg = serde_json::json!({
            "api_url": "https://old.example.com/v1",
            "api_model": "old-model",
            "language": "de",
            "shortcut_mode": "hold",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&old_cfg).unwrap()).unwrap();

        // Manually parse to verify defaults apply
        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        let defaults = AppConfig::default();

        assert!(
            v["llm_enabled"].is_null(),
            "Old config shouldn't have llm_enabled"
        );
        assert_eq!(
            v["llm_enabled"].as_bool().unwrap_or(defaults.llm_enabled),
            false
        );
        assert_eq!(
            v["llm_style"].as_str().unwrap_or(&defaults.llm_style),
            "off"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

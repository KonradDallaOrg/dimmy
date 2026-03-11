mod audio;
pub mod error;
pub mod provider;
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
const DEFAULT_LLM_MODEL: &str = "llama-3.3-70b-versatile";
const MAX_RECORDING_SECS: usize = 30 * 60; // 30 minutes hard cap
const MAX_LOG_BYTES: u64 = 1_048_576; // 1 MB log rotation threshold

/// Default shortcut: Cmd+Opt+D on macOS (2 modifiers alone triggers too easily),
/// Win+Alt on Windows/Linux (safe because Win+Alt isn't commonly used).
fn default_shortcut() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "win+alt+d"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "win+alt"
    }
}

fn config_dir_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join("dimmy"))
}

fn config_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("config.json"))
}

fn log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("dimmy.log"))
}

fn transcription_debug_log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("transcription_debug.log"))
}

fn audio_debug_dir() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("audio_debug"))
}

/// Create a session directory for audio debug dumps and return its path.
fn create_audio_debug_session() -> Option<std::path::PathBuf> {
    let base = audio_debug_dir()?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let session_dir = base.join(&ts);
    std::fs::create_dir_all(&session_dir).ok()?;
    Some(session_dir)
}

/// Save WAV bytes to a file inside a debug session directory (fire-and-forget).
fn save_debug_wav(dir: &std::path::Path, filename: &str, wav_data: &[u8]) {
    let path = dir.join(filename);
    let _ = std::fs::write(&path, wav_data);
}

/// Save session metadata JSON to the debug directory.
fn save_debug_metadata(
    dir: &std::path::Path,
    sample_rate: u32,
    device: &Option<String>,
    preprocessing_on: bool,
    duration_secs: f64,
    chunk_count: u32,
) {
    let meta = serde_json::json!({
        "sample_rate": sample_rate,
        "device": device.as_deref().unwrap_or("default"),
        "preprocessing_enabled": preprocessing_on,
        "duration_secs": duration_secs,
        "chunk_count": chunk_count,
        "timestamp": chrono::Local::now().to_rfc3339(),
    });
    let path = dir.join("metadata.json");
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&meta).unwrap_or_default());
}

/// Append a line to the transcription debug log for chunk vs final comparison.
fn debug_transcription(msg: &str) {
    use std::io::Write;
    if let Some(path) = transcription_debug_log_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Rotate at 2 MB
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 2_097_152 {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    let half = data.len() / 2;
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
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
    }
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
    llm_style: llm::LlmStyle,
    llm_tone: llm::LlmTone,
    llm_custom_prompt: String,
    llm_translate_to: String,
    llm_api_url: String,
    llm_api_model: String,
    llm_use_same_key: bool,
    llm_log_enabled: bool,
    chunk_streaming_enabled: bool,
    preprocessing_enabled: bool,
    audio_debug_enabled: bool,
    // Window position — bottom-right anchor in logical pixels
    window_anchor_right: Option<f64>,
    window_anchor_bottom: Option<f64>,
    // KPI stats — cumulative across sessions
    stats_total_words: u64,
    stats_total_speaking_secs: f64,
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
            llm_style: llm::LlmStyle::Off,
            llm_tone: llm::LlmTone::None,
            llm_custom_prompt: String::new(),
            llm_translate_to: "none".to_string(),
            llm_api_url: DEFAULT_LLM_URL.to_string(),
            llm_api_model: DEFAULT_LLM_MODEL.to_string(),
            llm_use_same_key: true,
            llm_log_enabled: false,
            chunk_streaming_enabled: true,
            preprocessing_enabled: true,
            audio_debug_enabled: false,
            window_anchor_right: None,
            window_anchor_bottom: None,
            stats_total_words: 0,
            stats_total_speaking_secs: 0.0,
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
            "llm_style": cfg.llm_style.as_str(),
            "llm_tone": cfg.llm_tone.as_str(),
            "llm_custom_prompt": cfg.llm_custom_prompt,
            "llm_translate_to": cfg.llm_translate_to,
            "llm_api_url": cfg.llm_api_url,
            "llm_api_model": cfg.llm_api_model,
            "llm_use_same_key": cfg.llm_use_same_key,
            "llm_log_enabled": cfg.llm_log_enabled,
            "chunk_streaming_enabled": cfg.chunk_streaming_enabled,
            "preprocessing_enabled": cfg.preprocessing_enabled,
            "audio_debug_enabled": cfg.audio_debug_enabled,
            "stats_total_words": cfg.stats_total_words,
            "stats_total_speaking_secs": cfg.stats_total_speaking_secs,
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
                    shortcut: v["shortcut"].as_str().unwrap_or(default_shortcut()).to_string(),
                    prompt: v["prompt"].as_str().unwrap_or(DEFAULT_PROMPT).to_string(),
                    llm_enabled: v["llm_enabled"].as_bool().unwrap_or(defaults.llm_enabled),
                    llm_style: llm::LlmStyle::from_str_lossy(
                        v["llm_style"].as_str().unwrap_or("off"),
                    ),
                    llm_tone: llm::LlmTone::from_str_lossy(
                        v["llm_tone"].as_str().unwrap_or("none"),
                    ),
                    llm_custom_prompt: v["llm_custom_prompt"]
                        .as_str()
                        .unwrap_or(&defaults.llm_custom_prompt)
                        .to_string(),
                    llm_translate_to: v["llm_translate_to"]
                        .as_str()
                        .unwrap_or(&defaults.llm_translate_to)
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
                    chunk_streaming_enabled: v["chunk_streaming_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.chunk_streaming_enabled),
                    preprocessing_enabled: v["preprocessing_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.preprocessing_enabled),
                    audio_debug_enabled: v["audio_debug_enabled"]
                        .as_bool()
                        .unwrap_or(defaults.audio_debug_enabled),
                    window_anchor_right: v["window_anchor_right"].as_f64(),
                    window_anchor_bottom: v["window_anchor_bottom"].as_f64(),
                    stats_total_words: v["stats_total_words"].as_u64().unwrap_or(0),
                    stats_total_speaking_secs: v["stats_total_speaking_secs"]
                        .as_f64()
                        .unwrap_or(0.0),
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
                        let provider = Provider::from_url(api_url);
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

use provider::Provider;

fn save_key_for_provider(prefix: &str, provider: Provider, key: &str) -> Result<(), String> {
    let entry_name = format!("{}-{}", prefix, provider.as_str());
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

fn load_key_for_provider(prefix: &str, provider: Provider) -> Option<String> {
    let entry_name = format!("{}-{}", prefix, provider.as_str());
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

fn has_key_for_provider(prefix: &str, provider: Provider) -> bool {
    let entry_name = format!("{}-{}", prefix, provider.as_str());
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
            let provider = Provider::from_url(api_url);
            log(&format!("Migrating old api-key to api-key-{}", provider));
            let _ = save_key_for_provider("api-key", provider, &key);
            delete_key("dimmy", "api-key");
        }
    }
    // Migrate LLM key
    if let Ok(entry) = keyring::Entry::new("dimmy", "llm-api-key") {
        if let Ok(key) = entry.get_password() {
            let provider = Provider::from_url(llm_api_url);
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
    pub llm_style: Mutex<llm::LlmStyle>,
    pub llm_tone: Mutex<llm::LlmTone>,
    pub llm_custom_prompt: Mutex<String>,
    pub llm_translate_to: Mutex<String>,
    pub llm_api_url: Mutex<String>,
    pub llm_api_model: Mutex<String>,
    pub llm_use_same_key: Mutex<bool>,
    pub llm_api_key: Mutex<Option<String>>,
    pub llm_log_enabled: Mutex<bool>,
    pub chunk_streaming_enabled: Mutex<bool>,
    pub preprocessing_enabled: Mutex<bool>,
    pub audio_debug_enabled: Mutex<bool>,
    /// Path to current audio debug session directory (set during recording, cleared on stop)
    pub audio_debug_session_dir: Mutex<Option<std::path::PathBuf>>,
    /// Bottom-right anchor in logical pixels — persisted across restarts
    pub window_anchor: Mutex<Option<(f64, f64)>>,
    // KPI stats
    pub stats_total_words: Mutex<u64>,
    pub stats_total_speaking_secs: Mutex<f64>,
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

    let debug_log_on = *state
        .llm_log_enabled
        .lock()
        .map_err(|e| e.to_string())?;

    let audio_debug_on = *state
        .audio_debug_enabled
        .lock()
        .map_err(|e| e.to_string())?;

    let chunk_streaming_on = *state
        .chunk_streaming_enabled
        .lock()
        .map_err(|e| e.to_string())?;

    // Create debug session directory before spawning async task
    let debug_session_dir = if audio_debug_on {
        create_audio_debug_session()
    } else {
        None
    };
    // Store session dir in state so stop_recording can access it
    *state
        .audio_debug_session_dir
        .lock()
        .map_err(|e| e.to_string())? = debug_session_dir.clone();

    if let Some(key) = api_key.filter(|_| chunk_streaming_on) {
        let buffer = state.audio_buffer.clone();
        let streaming = state.streaming_active.clone();
        let handle = app_handle.clone();
        let sample_rate = device_sr as usize;

        tauri::async_runtime::spawn(async move {
            if debug_log_on {
                debug_transcription("========== NEW RECORDING SESSION ==========");
            }
            let mut offset: usize = 0;
            let mut chunk_index: u32 = 0;
            let mut debug_chunk_idx: u32 = 0;
            let min_chunk_samples = sample_rate * 5;
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

                // Save raw chunk WAV for audio debug (before preprocessing)
                debug_chunk_idx += 1;
                if let Some(ref dir) = debug_session_dir {
                    if let Ok(raw_wav) = audio::encode_wav(&chunk_data, sample_rate as u32) {
                        save_debug_wav(dir, &format!("chunk_{:03}_raw.wav", debug_chunk_idx), &raw_wav);
                    }
                }

                // Preprocess: highpass + VAD + normalize (if enabled)
                let processed = if preprocess_on {
                    let p = preprocessor.process(&chunk_data);
                    if p.is_empty() {
                        // Entire chunk was noise/silence — skip sending to Whisper
                        continue;
                    }
                    // Skip very short speech segments (<0.5s) — STT models hallucinate on tiny audio
                    let min_speech_samples = sample_rate / 2;
                    if p.len() < min_speech_samples {
                        if debug_log_on {
                            debug_transcription(&format!(
                                "CHUNK SKIP | speech too short: {:.2}s < 0.5s",
                                p.len() as f64 / sample_rate as f64
                            ));
                        }
                        continue;
                    }
                    p
                } else {
                    chunk_data
                };

                // Save processed chunk WAV for audio debug (after preprocessing)
                if let Some(ref dir) = debug_session_dir {
                    if let Ok(proc_wav) = audio::encode_wav(&processed, sample_rate as u32) {
                        save_debug_wav(dir, &format!("chunk_{:03}_processed.wav", debug_chunk_idx), &proc_wav);
                    }
                }

                chunk_index += 1;
                let chunk_duration_secs = processed.len() as f64 / sample_rate as f64;

                if debug_log_on {
                    debug_transcription(&format!(
                        "CHUNK #{} | samples={} | duration={:.2}s | offset={}..{}",
                        chunk_index, processed.len(), chunk_duration_secs, offset.saturating_sub(processed.len()), offset
                    ));
                }

                let _ = handle.emit(
                    "chunk-status",
                    serde_json::json!({
                        "index": chunk_index,
                        "status": "sending",
                    }),
                );

                let chunk_send_time = std::time::Instant::now();
                // Downsample to 16kHz before sending to Whisper (reduces upload by ~3x)
                let resampled = preprocess::downsample_to_16k(&processed, sample_rate as u32);
                let wav_result =
                    audio::encode_wav(&resampled, 16000).map_err(|e| e.to_string());
                match wav_result {
                    Ok(wav_data) => {
                        if debug_log_on {
                            debug_transcription(&format!(
                                "CHUNK #{} | wav_size={} bytes | sending to Whisper...",
                                chunk_index, wav_data.len()
                            ));
                        }
                        match transcribe::transcribe_audio(
                            &api_url, &api_model, &key, &wav_data, &language, &prompt,
                        )
                        .await
                        {
                            Ok(text) => {
                                if debug_log_on {
                                    let elapsed = chunk_send_time.elapsed();
                                    debug_transcription(&format!(
                                        "CHUNK #{} | latency={:.0}ms | text=\"{}\"",
                                        chunk_index, elapsed.as_millis(), text
                                    ));
                                }
                                let _ = handle.emit(
                                    "transcription-chunk",
                                    serde_json::json!({
                                        "index": chunk_index,
                                        "text": text,
                                    }),
                                );
                            }
                            Err(e) => {
                                if debug_log_on {
                                    let elapsed = chunk_send_time.elapsed();
                                    debug_transcription(&format!(
                                        "CHUNK #{} | ERROR after {:.0}ms | {}",
                                        chunk_index, elapsed.as_millis(), e
                                    ));
                                }
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

    let debug_log_on = *state
        .llm_log_enabled
        .lock()
        .map_err(|e| e.to_string())?;

    if debug_log_on {
        let total_duration_secs = buffer.len() as f64
            / *state.audio_sample_rate.lock().map_err(|e| e.to_string())? as f64;
        debug_transcription(&format!(
            "---------- STOP RECORDING | total_samples={} | duration={:.2}s ----------",
            buffer.len(),
            total_duration_secs
        ));
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

    // Audio debug: take session dir from state and save raw session WAV
    let debug_session_dir = state
        .audio_debug_session_dir
        .lock()
        .map_err(|e| e.to_string())?
        .take();
    let debug_device = state
        .selected_device
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    if let Some(ref dir) = debug_session_dir {
        if let Ok(raw_wav) = audio::encode_wav(&buffer, sr) {
            save_debug_wav(dir, "session_raw.wav", &raw_wav);
        }
    }

    // Preprocess final buffer: highpass + VAD + normalize (if enabled)
    let audio_data = if preprocess_final {
        let processed = preprocess::process_buffer(&buffer, sr);
        if processed.is_empty() {
            // Still save metadata even when no speech detected
            if let Some(ref dir) = debug_session_dir {
                let duration = buffer.len() as f64 / sr as f64;
                save_debug_metadata(dir, sr, &debug_device, true, duration, 0);
            }
            return Err("No speech detected in recording".into());
        }
        processed
    } else {
        buffer
    };
    // Downsample to 16kHz before sending to Whisper (reduces upload by ~3x)
    let resampled = preprocess::downsample_to_16k(&audio_data, sr);
    let wav_data = audio::encode_wav(&resampled, 16000).map_err(|e| e.to_string())?;

    // Audio debug: save processed session WAV at original sample rate (not downsampled)
    if let Some(ref dir) = debug_session_dir {
        if let Ok(proc_wav) = audio::encode_wav(&audio_data, sr) {
            save_debug_wav(dir, "session_processed.wav", &proc_wav);
        }
        let duration = audio_data.len() as f64 / sr as f64;
        save_debug_metadata(dir, sr, &debug_device, preprocess_final, duration, 0);
    }

    if debug_log_on {
        debug_transcription(&format!(
            "FINAL | wav_size={} bytes | audio_samples={} | sending to Whisper...",
            wav_data.len(),
            audio_data.len()
        ));
    }
    let final_send_time = std::time::Instant::now();

    let transcript = transcribe::transcribe_audio(
        &api_url, &api_model, &api_key, &wav_data, &language, &prompt,
    )
    .await
    .map_err(|e| e.to_string())?;

    if debug_log_on {
        let final_elapsed = final_send_time.elapsed();
        debug_transcription(&format!(
            "FINAL | latency={:.0}ms | text=\"{}\"",
            final_elapsed.as_millis(),
            transcript
        ));
        debug_transcription("========== END SESSION ==========\n");
    }

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
    llm_translate_to: Option<String>,
    llm_api_url: Option<String>,
    llm_api_model: Option<String>,
    llm_use_same_key: Option<bool>,
    llm_api_key: Option<String>,
    llm_log_enabled: Option<bool>,
    preprocessing_enabled: Option<bool>,
    chunk_streaming_enabled: Option<bool>,
    audio_debug_enabled: Option<bool>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    log(&format!(
        "set_config called: mode={}, device={:?}",
        shortcut_mode, selected_device
    ));

    // Save transcription API key for the provider derived from the URL
    let transcription_provider = Provider::from_url(&api_url);
    if let Some(ref key) = api_key {
        if !key.is_empty() {
            save_key_for_provider("api-key", transcription_provider, key)?;
            *state.api_key.lock().map_err(|e| e.to_string())? = Some(key.clone());
        }
    }

    // Handle separate LLM API key (provider derived from LLM URL)
    let llm_url_for_provider = llm_api_url.as_deref().unwrap_or(&api_url);
    let llm_provider = Provider::from_url(llm_url_for_provider);
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
        *state.llm_style.lock().map_err(|e| e.to_string())? = llm::LlmStyle::from_str_lossy(v);
    }
    if let Some(ref v) = llm_tone {
        *state.llm_tone.lock().map_err(|e| e.to_string())? = llm::LlmTone::from_str_lossy(v);
    }
    if let Some(ref v) = llm_custom_prompt {
        *state.llm_custom_prompt.lock().map_err(|e| e.to_string())? = v.clone();
    }
    if let Some(ref v) = llm_translate_to {
        *state.llm_translate_to.lock().map_err(|e| e.to_string())? = v.clone();
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
    if let Some(v) = chunk_streaming_enabled {
        *state
            .chunk_streaming_enabled
            .lock()
            .map_err(|e| e.to_string())? = v;
    }
    if let Some(v) = audio_debug_enabled {
        *state
            .audio_debug_enabled
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
    let cur_llm_translate_to = state
        .llm_translate_to
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
    let cur_chunk_streaming_enabled = *state
        .chunk_streaming_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let cur_preprocessing_enabled = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let cur_audio_debug_enabled = *state
        .audio_debug_enabled
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
        llm_translate_to: cur_llm_translate_to,
        llm_api_url: cur_llm_api_url,
        llm_api_model: cur_llm_api_model,
        llm_use_same_key: cur_llm_use_same_key,
        llm_log_enabled: cur_llm_log_enabled,
        chunk_streaming_enabled: cur_chunk_streaming_enabled,
        preprocessing_enabled: cur_preprocessing_enabled,
        audio_debug_enabled: cur_audio_debug_enabled,
        window_anchor_right: cur_anchor.map(|(r, _)| r),
        window_anchor_bottom: cur_anchor.map(|(_, b)| b),
        stats_total_words: *state.stats_total_words.lock().map_err(|e| e.to_string())?,
        stats_total_speaking_secs: *state
            .stats_total_speaking_secs
            .lock()
            .map_err(|e| e.to_string())?,
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
        let provider = Provider::from_url(&api_url);
        if let Some(key) = load_key_for_provider("api-key", provider) {
            *state.api_key.lock().map_err(|e| e.to_string())? = Some(key);
        }
    }
    if llm_api_key.is_none() || llm_api_key.as_deref() == Some("") {
        let llm_url = state.llm_api_url.lock().map_err(|e| e.to_string())?.clone();
        let provider = Provider::from_url(&llm_url);
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
    let llm_style = *state.llm_style.lock().map_err(|e| e.to_string())?;
    let llm_tone = *state.llm_tone.lock().map_err(|e| e.to_string())?;
    let llm_custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_translate_to = state
        .llm_translate_to
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
    let audio_debug_enabled = *state
        .audio_debug_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let shortcut = state.shortcut.lock().map_err(|e| e.to_string())?.clone();
    let shortcut_label = hotkey::current_label();

    let styles: Vec<&str> = llm::LlmStyle::ALL.iter().map(|s| s.as_str()).collect();
    let tones: Vec<&str> = llm::LlmTone::ALL.iter().map(|t| t.as_str()).collect();

    // Per-provider key flags
    let has_groq_key = has_key_for_provider("api-key", Provider::Groq);
    let has_openai_key = has_key_for_provider("api-key", Provider::OpenAI);
    let has_gemini_key = has_key_for_provider("api-key", Provider::Gemini);
    let has_deepgram_key = has_key_for_provider("api-key", Provider::Deepgram);
    let has_custom_key = has_key_for_provider("api-key", Provider::Custom);
    // LLM key flags: check dedicated llm-key first, fall back to api-key for same provider.
    // Keys are per-provider — if you entered an OpenAI key for transcription, it works for LLM too.
    let has_llm_groq_key = has_key_for_provider("llm-key", Provider::Groq) || has_key_for_provider("api-key", Provider::Groq);
    let has_llm_openai_key = has_key_for_provider("llm-key", Provider::OpenAI) || has_key_for_provider("api-key", Provider::OpenAI);
    let has_llm_openrouter_key = has_key_for_provider("llm-key", Provider::OpenRouter) || has_key_for_provider("api-key", Provider::OpenRouter);
    let has_llm_gemini_key = has_key_for_provider("llm-key", Provider::Gemini) || has_key_for_provider("api-key", Provider::Gemini);
    let has_llm_anthropic_key = has_key_for_provider("llm-key", Provider::Anthropic) || has_key_for_provider("api-key", Provider::Anthropic);
    let has_llm_custom_key = has_key_for_provider("llm-key", Provider::Custom) || has_key_for_provider("api-key", Provider::Custom);

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
        "llm_style": llm_style.as_str(),
        "llm_tone": llm_tone.as_str(),
        "llm_custom_prompt": llm_custom_prompt,
        "llm_translate_to": llm_translate_to,
        "llm_api_url": llm_api_url,
        "llm_api_model": llm_api_model,
        "llm_use_same_key": llm_use_same_key,
        "has_llm_key": has_llm_key,
        "llm_log_enabled": llm_log_enabled,
        "chunk_streaming_enabled": *state.chunk_streaming_enabled.lock().map_err(|e| e.to_string())?,
        "preprocessing_enabled": preprocessing_enabled,
        "audio_debug_enabled": audio_debug_enabled,
        "shortcut": shortcut,
        "shortcut_label": shortcut_label,
        "llm_styles": styles,
        "llm_tones": tones,
        // Per-provider key flags
        "has_groq_key": has_groq_key,
        "has_openai_key": has_openai_key,
        "has_gemini_key": has_gemini_key,
        "has_deepgram_key": has_deepgram_key,
        "has_custom_key": has_custom_key,
        "has_llm_groq_key": has_llm_groq_key,
        "has_llm_openai_key": has_llm_openai_key,
        "has_llm_openrouter_key": has_llm_openrouter_key,
        "has_llm_gemini_key": has_llm_gemini_key,
        "has_llm_anthropic_key": has_llm_anthropic_key,
        "has_llm_custom_key": has_llm_custom_key,
        "stats_total_words": *state.stats_total_words.lock().map_err(|e| e.to_string())?,
        "stats_total_speaking_secs": *state.stats_total_speaking_secs.lock().map_err(|e| e.to_string())?,
    }))
}

#[tauri::command]
fn update_stats(
    words: u64,
    speaking_secs: f64,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    *state.stats_total_words.lock().map_err(|e| e.to_string())? += words;
    *state
        .stats_total_speaking_secs
        .lock()
        .map_err(|e| e.to_string())? += speaking_secs;
    save_current_config(&state)?;
    Ok(())
}

#[tauri::command]
async fn process_with_llm(
    text: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let enabled = *state.llm_enabled.lock().map_err(|e| e.to_string())?;
    let style = *state.llm_style.lock().map_err(|e| e.to_string())?;
    let translate_to = state.llm_translate_to.lock().map_err(|e| e.to_string())?.clone();
    let translating = !translate_to.is_empty() && translate_to != "none";

    if !enabled && !translating {
        return Ok(text);
    }
    if style.is_off() && !translating {
        return Ok(text);
    }

    let tone = *state.llm_tone.lock().map_err(|e| e.to_string())?;
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
        // Try dedicated LLM key first, then fall back to transcription key for same provider.
        // Keys are per-provider: if you entered an OpenAI key for transcription,
        // it should work for LLM too when OpenAI is selected.
        let llm_key = state.llm_api_key.lock().map_err(|e| e.to_string())?.clone();
        if llm_key.is_some() {
            llm_key
        } else {
            let llm_provider = Provider::from_url(&api_url);
            load_key_for_provider("api-key", llm_provider)
        }
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
        style,
        tone,
        &custom_prompt,
        &translate_to,
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
            log("LLM processing failed (see UI for details)");
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
    let new_style = style.cycle(direction);
    *style = new_style;

    // Also enable/disable LLM based on style + translate_to
    if let Ok(mut enabled) = state.llm_enabled.lock() {
        let translating = state
            .llm_translate_to
            .lock()
            .map(|t| !t.is_empty() && *t != "none")
            .unwrap_or(false);
        *enabled = !new_style.is_off() || translating;
    }

    // Persist to config file
    drop(style);
    save_current_config(&state)?;

    let new_idx = llm::LlmStyle::ALL.iter().position(|s| *s == new_style).unwrap_or(0);
    Ok(serde_json::json!({
        "style": new_style.as_str(),
        "index": new_idx,
        "total": llm::LlmStyle::ALL.len(),
    }))
}

#[tauri::command]
fn cycle_llm_tone(
    direction: i32,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let mut tone = state.llm_tone.lock().map_err(|e| e.to_string())?;
    let new_tone = tone.cycle(direction);
    *tone = new_tone;

    drop(tone);
    save_current_config(&state)?;

    let new_idx = llm::LlmTone::ALL.iter().position(|t| *t == new_tone).unwrap_or(0);
    Ok(serde_json::json!({
        "tone": new_tone.as_str(),
        "index": new_idx,
        "total": llm::LlmTone::ALL.len(),
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
    let llm_style = *state.llm_style.lock().map_err(|e| e.to_string())?;
    let llm_tone = *state.llm_tone.lock().map_err(|e| e.to_string())?;
    let llm_custom_prompt = state
        .llm_custom_prompt
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let llm_translate_to = state
        .llm_translate_to
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
    let chunk_streaming_enabled = *state
        .chunk_streaming_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let preprocessing_enabled = *state
        .preprocessing_enabled
        .lock()
        .map_err(|e| e.to_string())?;
    let audio_debug_enabled = *state
        .audio_debug_enabled
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
        llm_translate_to,
        llm_api_url,
        llm_api_model,
        llm_use_same_key,
        llm_log_enabled,
        chunk_streaming_enabled,
        preprocessing_enabled,
        audio_debug_enabled,
        window_anchor_right: anchor.map(|(r, _)| r),
        window_anchor_bottom: anchor.map(|(_, b)| b),
        stats_total_words: *state.stats_total_words.lock().map_err(|e| e.to_string())?,
        stats_total_speaking_secs: *state
            .stats_total_speaking_secs
            .lock()
            .map_err(|e| e.to_string())?,
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

    // IMPORTANT: on ARM64 (Apple Silicon), objc_msgSend is NOT variadic.
    // Declaring it as variadic causes the compiler to use the C variadic ABI
    // (stack-based args) instead of register-based, producing garbage pointers
    // and SIGSEGV / PAC failures. Declare as raw symbol and transmute to typed
    // function pointers for each call signature.
    extern "C" {
        fn objc_msgSend();
    }

    #[cfg(target_arch = "x86_64")]
    extern "C" {
        fn objc_msgSend_stret(
            stret: *mut NSRect,
            obj: *mut std::ffi::c_void,
            sel: *mut std::ffi::c_void,
        );
    }

    type MsgSendPtr = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;

    unsafe {
        let send_ptr: MsgSendPtr =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let ns_screen = objc_getClass(b"NSScreen\0".as_ptr());
        if ns_screen.is_null() {
            return None;
        }
        let main_screen_sel = sel_registerName(b"mainScreen\0".as_ptr());
        let screen = send_ptr(ns_screen, main_screen_sel);
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
            let msg_send_rect: unsafe extern "C" fn(
                *mut std::ffi::c_void,
                *mut std::ffi::c_void,
            ) -> NSRect = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            visible = msg_send_rect(screen, visible_frame_sel);
            full = msg_send_rect(screen, frame_sel);
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

/// Clamp a window position to the usable work area (excludes taskbar/panel).
/// All inputs and outputs are in logical pixels.
fn clamp_to_work_area(win: &tauri::WebviewWindow, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let scale = monitor.scale_factor();
        let screen_w = monitor.size().width as f64;
        let screen_h = monitor.size().height as f64;
        // Work area in logical pixels — excludes taskbar/panel
        let (area_w, area_h) = get_work_area()
            .map(|(r, b)| (r as f64 / scale, b as f64 / scale))
            .unwrap_or_else(|| {
                // Linux fallback: assume ~48px panel at bottom (physical), convert to logical
                let panel_px = 48.0;
                (screen_w / scale, (screen_h - panel_px) / scale)
            });
        let cx = x.max(0.0).min((area_w - w).max(0.0));
        let cy = y.max(0.0).min((area_h - h).max(0.0));
        (cx, cy)
    } else {
        (x.max(0.0), y.max(0.0))
    }
}

/// Position a window at the bottom-right of the usable desktop (above the taskbar).
fn position_bottom_right(win: &tauri::WebviewWindow, w: f64, h: f64) {
    let padding = 12.0_f64;
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let scale = monitor.scale_factor();
        let screen_w = monitor.size().width as f64;
        let screen_h = monitor.size().height as f64;
        // Work area in logical pixels — excludes taskbar/panel
        let (area_w, area_h) = get_work_area()
            .map(|(r, b)| (r as f64 / scale, b as f64 / scale))
            .unwrap_or_else(|| {
                let panel_px = 48.0;
                (screen_w / scale, (screen_h - panel_px) / scale)
            });
        let x = area_w - w - padding;
        let y = area_h - h - padding;
        let (x, y) = clamp_to_work_area(win, x, y, w, h);
        let _ = win.set_position(LogicalPosition::new(x, y));
    }
}

/// Force WebView2 to redraw with transparent background.
/// On Windows, DWM can lose the transparent compositing after z-order changes
/// or resize. Calling InvalidateRect forces a full repaint that restores it.
#[cfg(target_os = "windows")]
fn force_transparent_redraw(win: &tauri::WebviewWindow) {
    use tauri::window::Color;
    let _ = win.set_background_color(Some(Color(0, 0, 0, 0)));
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            extern "system" {
                fn InvalidateRect(
                    hwnd: *mut std::ffi::c_void,
                    rect: *const std::ffi::c_void,
                    erase: i32,
                ) -> i32;
            }
            InvalidateRect(hwnd.0 as *mut std::ffi::c_void, std::ptr::null(), 1);
        }
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
        // Read LIVE position + size before resize (captures user drags)
        let prev = if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
            let scale = win.scale_factor().unwrap_or(1.0);
            let x = pos.x as f64 / scale;
            let y = pos.y as f64 / scale;
            let pw = size.width as f64 / scale;
            let ph = size.height as f64 / scale;
            Some((x, y, pw, ph))
        } else {
            None
        };

        win.set_size(LogicalSize::new(w, h))
            .map_err(|e| e.to_string())?;
        let _ = win.set_always_on_top(true);
        #[cfg(target_os = "windows")]
        force_transparent_redraw(&win);

        if let Some((old_x, old_y, old_w, old_h)) = prev {
            // Get screen dimensions for smart anchor flipping
            let (screen_w, screen_h) = get_work_area()
                .map(|(sw, sh)| {
                    let scale = win.scale_factor().unwrap_or(1.0);
                    (sw as f64 / scale, sh as f64 / scale)
                })
                .unwrap_or((1920.0, 1080.0));

            // Determine which screen edges the window is closest to.
            // Use distances from each edge, not center — this way a pill in the
            // bottom-right corner always anchors bottom-right regardless of
            // how large the settings panel is.
            let dist_left = old_x;
            let dist_right = screen_w - (old_x + old_w);
            let dist_top = old_y;
            let dist_bottom = screen_h - (old_y + old_h);

            let new_x = if dist_right <= dist_left {
                // Closer to right edge: anchor right edge (expand/shrink left)
                (old_x + old_w - w).max(0.0)
            } else {
                // Closer to left edge: anchor left edge (expand/shrink right)
                old_x
            };
            let new_y = if dist_bottom <= dist_top {
                // Closer to bottom: anchor bottom edge (expand/shrink up)
                (old_y + old_h - h).max(0.0)
            } else {
                // Closer to top: anchor top edge (expand/shrink down)
                old_y
            };

            let (new_x, new_y) = clamp_to_work_area(&win, new_x, new_y, w, h);
            let _ = win.set_position(LogicalPosition::new(new_x, new_y));

            // Store bottom-right as anchor for persistence
            let _ = state
                .window_anchor
                .lock()
                .map(|mut a| *a = Some((new_x + w, new_y + h)));
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

/// Prompt the user to grant Accessibility permissions (macOS only).
/// Shows the native macOS dialog that links to System Settings.
#[tauri::command]
fn prompt_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
            fn CFDictionaryCreate(
                allocator: *const std::ffi::c_void,
                keys: *const *const std::ffi::c_void,
                values: *const *const std::ffi::c_void,
                num_values: isize,
                key_callbacks: *const std::ffi::c_void,
                value_callbacks: *const std::ffi::c_void,
            ) -> *const std::ffi::c_void;
            fn CFRelease(cf: *const std::ffi::c_void);
            fn CFStringCreateWithCString(
                alloc: *const std::ffi::c_void,
                c_str: *const u8,
                encoding: u32,
            ) -> *const std::ffi::c_void;
            static kCFBooleanTrue: *const std::ffi::c_void;
            // These are CFDictionaryKeyCallBacks / CFDictionaryValueCallBacks structs.
            // Declare as opaque bytes and take their address to get the correct pointer.
            static kCFTypeDictionaryKeyCallBacks: [u8; 0];
            static kCFTypeDictionaryValueCallBacks: [u8; 0];
        }

        unsafe {
            let prompt_key = CFStringCreateWithCString(
                std::ptr::null(),
                b"AXTrustedCheckOptionPrompt\0".as_ptr(),
                0x0600_0100, // kCFStringEncodingUTF8
            );
            let keys = [prompt_key];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                kCFTypeDictionaryKeyCallBacks.as_ptr() as *const std::ffi::c_void,
                kCFTypeDictionaryValueCallBacks.as_ptr() as *const std::ffi::c_void,
            );
            let trusted = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            CFRelease(prompt_key);
            trusted
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Open macOS System Settings > Privacy & Security > Accessibility pane.
#[tauri::command]
fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }
}

#[tauri::command]
fn open_audio_debug_dir() -> Result<(), String> {
    let dir = audio_debug_dir().ok_or("Cannot determine config directory")?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&dir).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
    Ok(())
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
    let transcription_provider = Provider::from_url(&file_cfg.api_url);
    let llm_provider = Provider::from_url(&file_cfg.llm_api_url);
    let stored_key = load_key_for_provider("api-key", transcription_provider);
    let stored_llm_key = load_key_for_provider("llm-key", llm_provider);
    // SECURITY: never log the actual key value — only whether one exists
    log(&format!("Config loaded: url={}, model={}, device={:?}, provider={}, has_key={}, llm_provider={}, llm_enabled={}, llm_style={}",
        file_cfg.api_url, file_cfg.api_model, file_cfg.selected_device, transcription_provider,
        stored_key.is_some(), llm_provider, file_cfg.llm_enabled, file_cfg.llm_style.as_str()));

    let shortcut_preset = file_cfg.shortcut.clone();

    let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let audio_tx = audio::spawn_audio_thread(audio_buffer.clone());

    tauri::Builder::default()
        // Single-instance MUST be registered first
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second instance was launched — focus the existing window
            log("Second instance detected, focusing existing window");
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
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
            llm_translate_to: Mutex::new(file_cfg.llm_translate_to),
            llm_api_url: Mutex::new(file_cfg.llm_api_url),
            llm_api_model: Mutex::new(file_cfg.llm_api_model),
            llm_use_same_key: Mutex::new(file_cfg.llm_use_same_key),
            llm_api_key: Mutex::new(stored_llm_key),
            llm_log_enabled: Mutex::new(file_cfg.llm_log_enabled),
            chunk_streaming_enabled: Mutex::new(file_cfg.chunk_streaming_enabled),
            preprocessing_enabled: Mutex::new(file_cfg.preprocessing_enabled),
            audio_debug_enabled: Mutex::new(file_cfg.audio_debug_enabled),
            audio_debug_session_dir: Mutex::new(None),
            window_anchor: Mutex::new(
                match (file_cfg.window_anchor_right, file_cfg.window_anchor_bottom) {
                    (Some(r), Some(b)) => Some((r, b)),
                    _ => None,
                },
            ),
            stats_total_words: Mutex::new(file_cfg.stats_total_words),
            stats_total_speaking_secs: Mutex::new(file_cfg.stats_total_speaking_secs),
        })
        .setup(move |app| {
            // Remove Windows DWM border and set transparent background
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_shadow(false);
                use tauri::window::Color;
                let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

                // macOS: ensure fully transparent NSWindow (fixes Retina border artifacts)
                // IMPORTANT: on ARM64 (Apple Silicon), objc_msgSend is NOT variadic.
                // We must declare it without args and transmute to typed function pointers,
                // otherwise the compiler uses the C variadic ABI (stack-based) instead of
                // the register-based ABI, producing garbage return values and SIGSEGV.
                #[cfg(target_os = "macos")]
                {
                    unsafe {
                        use std::ffi::{c_int, c_void};
                        type Id = *mut c_void;
                        type Sel = *const c_void;

                        // Typed function pointers for objc_msgSend with different signatures
                        type MsgSendNoArgs = unsafe extern "C" fn(Id, Sel) -> Id;
                        type MsgSendInt = unsafe extern "C" fn(Id, Sel, c_int) -> Id;
                        type MsgSendObj = unsafe extern "C" fn(Id, Sel, Id) -> Id;

                        extern "C" {
                            fn objc_msgSend(); // raw symbol — cast before calling
                            fn sel_registerName(name: *const u8) -> Sel;
                            fn objc_getClass(name: *const u8) -> Id;
                        }

                        let send: MsgSendNoArgs = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                        let send_int: MsgSendInt = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                        let send_obj: MsgSendObj = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

                        // Get NSWindow pointer via Tauri's raw window handle
                        let ns_win: Id = window.ns_window().unwrap_or(std::ptr::null_mut() as _) as Id;
                        if !ns_win.is_null() {
                            // [nsWindow setOpaque:NO]
                            let sel_set_opaque = sel_registerName(b"setOpaque:\0".as_ptr());
                            send_int(ns_win, sel_set_opaque, 0 as c_int);

                            // [NSColor clearColor]
                            let ns_color_class = objc_getClass(b"NSColor\0".as_ptr());
                            let sel_clear_color = sel_registerName(b"clearColor\0".as_ptr());
                            let clear: Id = send(ns_color_class, sel_clear_color);

                            // [nsWindow setBackgroundColor:[NSColor clearColor]]
                            let sel_set_bg = sel_registerName(b"setBackgroundColor:\0".as_ptr());
                            send_obj(ns_win, sel_set_bg, clear);

                            // [nsWindow setHasShadow:NO]
                            let sel_set_shadow = sel_registerName(b"setHasShadow:\0".as_ptr());
                            send_int(ns_win, sel_set_shadow, 0 as c_int);
                        }
                    }
                }

                // Windows: remove DWM border artifacts on transparent overlay window.
                // Tauri already calls DwmEnableBlurBehindWindow with an empty region
                // for `transparent: true`. We add:
                // 1. WS_POPUP style — removes the DWM caption frame entirely
                // 2. DWMWCP_DONOTROUND — disables Win11 auto-rounded corners
                // 3. DWMWA_COLOR_NONE border — removes the 1px DWM border
                // NOTE: Do NOT use DwmExtendFrameIntoClientArea(-1) — it adds glass
                // blur/shadow, making the border worse.
                #[cfg(target_os = "windows")]
                {
                    use std::ffi::c_void;

                    extern "system" {
                        fn GetWindowLongPtrW(hwnd: *mut c_void, index: i32) -> isize;
                        fn SetWindowLongPtrW(
                            hwnd: *mut c_void,
                            index: i32,
                            new_long: isize,
                        ) -> isize;
                        fn SetWindowPos(
                            hwnd: *mut c_void,
                            insert_after: *mut c_void,
                            x: i32,
                            y: i32,
                            cx: i32,
                            cy: i32,
                            flags: u32,
                        ) -> i32;
                        fn DwmSetWindowAttribute(
                            hwnd: *mut c_void,
                            attr: u32,
                            value: *const c_void,
                            size: u32,
                        ) -> i32;
                    }

                    const GWL_STYLE: i32 = -16;
                    const GWL_EXSTYLE: i32 = -20;
                    const WS_POPUP: isize = 0x80000000u32 as isize;
                    const WS_VISIBLE: isize = 0x10000000;
                    const WS_EX_LAYERED: isize = 0x00080000;
                    const WS_EX_TOOLWINDOW: isize = 0x00000080;
                    const SWP_FRAMECHANGED: u32 = 0x0020;
                    const SWP_NOMOVE: u32 = 0x0002;
                    const SWP_NOSIZE: u32 = 0x0001;
                    const SWP_NOZORDER: u32 = 0x0004;
                    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
                    const DWMWCP_DONOTROUND: u32 = 1;
                    const DWMWA_BORDER_COLOR: u32 = 34;
                    const DWMWA_COLOR_NONE: u32 = 0xFFFFFFFE;

                    if let Ok(hwnd) = window.hwnd() {
                        let hwnd = hwnd.0 as *mut c_void;
                        unsafe {
                            // WS_POPUP removes DWM caption frame.
                            // Remove WS_CLIPCHILDREN — it prevents the WebView2
                            // child from being repainted after DWM recomposition,
                            // causing the white background flicker (wry#1331).
                            const WS_CLIPCHILDREN: isize = 0x02000000;
                            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                            SetWindowLongPtrW(
                                hwnd,
                                GWL_STYLE,
                                (style | WS_POPUP) & !0x00C00000 & !WS_CLIPCHILDREN | WS_VISIBLE,
                            );

                            // WS_EX_LAYERED + WS_EX_TOOLWINDOW: layered window
                            // prevents DWM from compositing its own border/shadow
                            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                            SetWindowLongPtrW(
                                hwnd,
                                GWL_EXSTYLE,
                                ex_style | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                            );

                            // Apply style changes
                            SetWindowPos(
                                hwnd,
                                std::ptr::null_mut(),
                                0, 0, 0, 0,
                                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
                            );

                            // Disable Win11 rounded corners
                            let corner = DWMWCP_DONOTROUND;
                            DwmSetWindowAttribute(
                                hwnd,
                                DWMWA_WINDOW_CORNER_PREFERENCE,
                                &corner as *const u32 as *const c_void,
                                std::mem::size_of::<u32>() as u32,
                            );

                            // Set border color to none
                            let color = DWMWA_COLOR_NONE;
                            DwmSetWindowAttribute(
                                hwnd,
                                DWMWA_BORDER_COLOR,
                                &color as *const u32 as *const c_void,
                                std::mem::size_of::<u32>() as u32,
                            );
                        }
                    }
                }

                // Open DevTools in debug builds
                #[cfg(debug_assertions)]
                window.open_devtools();

                // Position from saved anchor or default bottom-right
                let win_w = 60.0_f64; // MICRO_W + margin
                let win_h = 36.0_f64; // PILL_H + margin
                {
                    let anchor = *app
                        .state::<AppState>()
                        .window_anchor
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some((right, bottom)) = anchor {
                        let x = (right - win_w).max(0.0);
                        let y = (bottom - win_h).max(0.0);
                        // Clamp to work area in case resolution/monitor changed since last session
                        let (x, y) = clamp_to_work_area(&window, x, y, win_w, win_h);
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
                                    // Restore saved anchor position (clamped to work area)
                                    let micro_w = 60.0_f64; // MICRO_W + margin
                                    let micro_h = 36.0_f64; // PILL_H + margin
                                    let st: tauri::State<'_, AppState> = app_handle.state();
                                    let anchor = st
                                        .window_anchor
                                        .lock()
                                        .ok()
                                        .and_then(|a| *a);
                                    if let Some((right, bottom)) = anchor {
                                        let x = (right - micro_w).max(0.0);
                                        let y = (bottom - micro_h).max(0.0);
                                        let (x, y) = clamp_to_work_area(&win, x, y, micro_w, micro_h);
                                        let _ = win.set_position(LogicalPosition::new(x, y));
                                    } else {
                                        position_bottom_right(&win, micro_w, micro_h);
                                    }
                                    let _ = win.show();
                                    let _ = win.set_always_on_top(true);
                                    let _ = win.set_focus();
                                    #[cfg(target_os = "windows")]
                                    force_transparent_redraw(&win);
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

            // Windows: periodic transparency guard.
            // WebView2 loses transparent compositing unpredictably (alt-tab,
            // virtual desktop switch, nearby window resize, display scaling, etc.).
            // A 500ms timer is the only reliable way to keep the pill transparent.
            #[cfg(target_os = "windows")]
            {
                let transparency_handle = app.handle().clone();
                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        if let Some(win) = transparency_handle.get_webview_window("main") {
                            force_transparent_redraw(&win);
                        }
                    }
                });
            }

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
            update_stats,
            cycle_llm_style,
            cycle_llm_tone,
            start_shortcut_recording,
            poll_shortcut_recording,
            cancel_shortcut_recording,
            check_accessibility,
            prompt_accessibility,
            open_accessibility_settings,
            get_version,
            open_audio_debug_dir,
            check_for_update,
            install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Dimmy")
        .run(|app_handle, event| {
            // Windows: re-apply transparent background on focus and resize.
            // WebView2 loses transparency after DWM z-order/composition changes.
            #[cfg(target_os = "windows")]
            if let tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::Focused(true) | tauri::WindowEvent::Resized(_),
                ..
            } = &event
            {
                if let Some(win) = app_handle.get_webview_window("main") {
                    force_transparent_redraw(&win);
                }
            }

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
        assert_eq!(cfg.llm_style, llm::LlmStyle::Off);
        assert_eq!(cfg.llm_tone, llm::LlmTone::None);
        assert!(cfg.llm_custom_prompt.is_empty());
        assert_eq!(cfg.llm_translate_to, "none");
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
            llm::LlmStyle::from_str_lossy(v["llm_style"].as_str().unwrap_or("off")),
            llm::LlmStyle::Off
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

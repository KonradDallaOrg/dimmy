pub mod audio;
pub mod error;
pub mod ffi;
pub mod filler;
pub mod history;
mod hotkey;
pub mod keystore;
pub mod llm;
pub mod local_stt;
pub mod preprocess;
pub mod provider;
pub mod transcribe;

use audio::AudioCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub const DEFAULT_API_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
pub const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";
/// Default prompt guides Whisper to produce punctuated, well-formatted output.
/// Whisper mimics the style of this text — punctuation, capitalization, etc.
pub const DEFAULT_PROMPT: &str = "Hello, how are you? Fine, thanks! Today we'll discuss an interesting topic. Ciao, come stai? Bene, grazie! Oggi parliamo di un argomento interessante.";
pub const DEFAULT_LLM_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const DEFAULT_LLM_MODEL: &str = "llama-3.3-70b-versatile";
#[allow(dead_code)] // Used by native UI via FFI recording logic
const MAX_RECORDING_SECS: usize = 30 * 60; // 30 minutes hard cap
const MAX_LOG_BYTES: u64 = 1_048_576; // 1 MB log rotation threshold
/// Tail buffer: keep recording for this long after the user releases the hotkey.
/// Catches trailing audio when the user's finger lifts slightly before finishing
/// the last syllable. Same approach used by Discord (~200ms), TeamSpeak, Mumble.
/// 300ms is generous enough for dictation without feeling laggy.
#[allow(dead_code)] // Used by native UI via FFI stop logic
const STOP_TAIL_MS: u64 = 300;

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

pub fn config_dir_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join("dimmy"))
}

/// Marker file path for onboarding completion.
/// Separate from config.json so deleting/resetting config doesn't re-trigger onboarding.
pub fn onboarding_marker_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join(".onboarding_done"))
}

/// Check if onboarding has been completed.
/// Returns true if: marker file exists, OR config.json exists (existing user upgrading).
/// This prevents the onboarding from appearing for existing users who upgrade to a
/// version that introduces onboarding — they already have a working config.
pub fn onboarding_completed() -> bool {
    // If marker exists, done
    if onboarding_marker_path().map(|p| p.exists()).unwrap_or(true) {
        return true;
    }
    // If config.json exists, this is an existing user — auto-mark as done
    if config_path().map(|p| p.exists()).unwrap_or(false) {
        log("Existing config.json found — skipping onboarding for upgrade user");
        let _ = mark_onboarding_done();
        return true;
    }
    false
}

/// Mark onboarding as completed by creating the marker file.
pub fn mark_onboarding_done() -> Result<(), String> {
    let path = onboarding_marker_path().ok_or("Cannot determine config directory")?;
    // Ensure config dir exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, "1").map_err(|e| e.to_string())?;
    assert!(
        path.exists(),
        "Onboarding marker file must exist after write"
    );
    Ok(())
}

pub fn config_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("config.json"))
}

pub fn log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("dimmy.log"))
}

#[allow(dead_code)]
fn transcription_debug_log_path() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("transcription_debug.log"))
}

fn audio_debug_dir() -> Option<std::path::PathBuf> {
    config_dir_path().map(|p| p.join("audio_debug"))
}

/// Create a session directory for audio debug dumps and return its path.
pub fn create_debug_session_dir() -> Option<std::path::PathBuf> {
    let base = audio_debug_dir()?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let session_dir = base.join(&ts);
    std::fs::create_dir_all(&session_dir).ok()?;
    Some(session_dir)
}

/// Save WAV bytes to a file inside a debug session directory (fire-and-forget).
pub fn save_debug_wav(dir: &std::path::Path, filename: &str, wav_data: &[u8]) {
    let path = dir.join(filename);
    let _ = std::fs::write(&path, wav_data);
}

/// Save session metadata JSON to the debug directory.
#[allow(dead_code)]
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
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );
}

/// Append a line to the transcription debug log for chunk vs final comparison.
#[allow(dead_code)]
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
pub fn log(msg: &str) {
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
pub struct AppConfig {
    pub api_url: String,
    pub api_model: String,
    pub selected_device: Option<String>,
    pub language: String,
    pub shortcut_mode: String,
    pub shortcut: String,
    pub prompt: String,
    // LLM post-processing fields
    pub llm_enabled: bool,
    pub llm_style: llm::LlmStyle,
    pub llm_tone: llm::LlmTone,
    pub llm_custom_prompt: String,
    pub llm_translate_to: String,
    pub llm_api_url: String,
    pub llm_api_model: String,
    pub llm_use_same_key: bool,
    pub llm_log_enabled: bool,
    pub chunk_streaming_enabled: bool,
    pub preprocessing_enabled: bool,
    pub audio_debug_enabled: bool,
    pub use_keyring: bool,
    // UI appearance fields (used by native frontends, opaque to Rust core)
    pub border_style: String,
    pub waveform_style: String,
    pub overlay_position: String,
    pub keep_in_clipboard: bool,
    /// Input gain (0.0-2.0, default 1.0). Attenuate hot mics (e.g. BT headsets).
    pub input_gain: f32,
    // Window position — bottom-right anchor in logical pixels
    pub window_anchor_right: Option<f64>,
    pub window_anchor_bottom: Option<f64>,
    // KPI stats — cumulative across sessions
    pub stats_total_words: u64,
    pub stats_total_speaking_secs: f64,
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
            chunk_streaming_enabled: false,
            preprocessing_enabled: true,
            audio_debug_enabled: false,
            use_keyring: false,
            border_style: "Rainbow".to_string(),
            waveform_style: "Bars".to_string(),
            overlay_position: "Bottom Right".to_string(),
            keep_in_clipboard: false,
            input_gain: 1.0,
            window_anchor_right: None,
            window_anchor_bottom: None,
            stats_total_words: 0,
            stats_total_speaking_secs: 0.0,
        }
    }
}

/// Save non-sensitive config to file (NO api_key — that goes to keyring ONLY)
pub fn save_config_file(cfg: &AppConfig) {
    // Preconditions: validate new config fields
    assert!(
        cfg.input_gain >= 0.0 && cfg.input_gain <= 2.0,
        "save_config_file: input_gain must be in [0.0, 2.0], got {}",
        cfg.input_gain
    );
    assert!(
        cfg.input_gain.is_finite(),
        "save_config_file: input_gain must be finite, got {}",
        cfg.input_gain
    );
    assert!(
        !cfg.border_style.is_empty(),
        "save_config_file: border_style must be non-empty"
    );
    assert!(
        !cfg.waveform_style.is_empty(),
        "save_config_file: waveform_style must be non-empty"
    );
    assert!(
        !cfg.overlay_position.is_empty(),
        "save_config_file: overlay_position must be non-empty"
    );

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
            "use_keyring": cfg.use_keyring,
            "border_style": cfg.border_style,
            "waveform_style": cfg.waveform_style,
            "overlay_position": cfg.overlay_position,
            "keep_in_clipboard": cfg.keep_in_clipboard,
            "input_gain": cfg.input_gain,
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
pub fn load_config_file() -> AppConfig {
    let defaults = AppConfig::default();
    if let Some(path) = config_path() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                return AppConfig {
                    api_url: v["api_url"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(DEFAULT_API_URL)
                        .to_string(),
                    api_model: v["api_model"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(DEFAULT_MODEL)
                        .to_string(),
                    selected_device: v["selected_device"].as_str().map(|s| s.to_string()),
                    language: v["language"].as_str().unwrap_or("").to_string(),
                    shortcut_mode: v["shortcut_mode"].as_str().unwrap_or("toggle").to_string(),
                    shortcut: v["shortcut"]
                        .as_str()
                        .unwrap_or(default_shortcut())
                        .to_string(),
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
                    use_keyring: v["use_keyring"].as_bool().unwrap_or(defaults.use_keyring),
                    border_style: v["border_style"].as_str().unwrap_or("Rainbow").to_string(),
                    waveform_style: v["waveform_style"].as_str().unwrap_or("Bars").to_string(),
                    overlay_position: v["overlay_position"]
                        .as_str()
                        .unwrap_or("Bottom Right")
                        .to_string(),
                    keep_in_clipboard: v["keep_in_clipboard"].as_bool().unwrap_or(false),
                    input_gain: v["input_gain"].as_f64().unwrap_or(1.0) as f32,
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
pub fn migrate_from_pai_voice() {
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
pub fn migrate_plaintext_key(store: &keystore::KeyStore, use_keyring: bool) {
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
                        match save_key_with_store(
                            store,
                            KeyringScope::Stt(provider),
                            key,
                            use_keyring,
                        ) {
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
// Key storage: routes through KeyStore (local encrypted file or OS keyring)

use provider::{KeyringScope, Provider};

/// Convenience wrappers that read use_keyring from AppState.
/// These maintain the same call signatures used throughout lib.rs.
pub fn save_key_with_store(
    store: &keystore::KeyStore,
    scope: KeyringScope,
    key: &str,
    use_keyring: bool,
) -> Result<(), String> {
    store.save_key(scope, key, use_keyring)
}

pub fn load_key_with_store(
    store: &keystore::KeyStore,
    scope: KeyringScope,
    use_keyring: bool,
) -> Option<String> {
    store.load_key(scope, use_keyring)
}

/// Migrate old single "api-key" and "llm-api-key" keyring entries to per-provider entries.
/// This handles the legacy format from before per-provider key storage.
pub fn migrate_keyring_to_per_provider(
    store: &keystore::KeyStore,
    api_url: &str,
    llm_api_url: &str,
    use_keyring: bool,
) {
    // Migrate transcription key
    if let Ok(entry) = keyring::Entry::new("dimmy", "api-key") {
        if let Ok(key) = entry.get_password() {
            let provider = Provider::from_url(api_url);
            log(&format!("Migrating old api-key to api-key-{}", provider));
            let _ = save_key_with_store(store, KeyringScope::Stt(provider), &key, use_keyring);
            store.delete_legacy_key("dimmy", "api-key");
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
            let _ = save_key_with_store(store, KeyringScope::Llm(provider), &key, use_keyring);
            store.delete_legacy_key("dimmy", "llm-api-key");
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
    pub use_keyring: Mutex<bool>,
    // UI appearance (opaque to Rust, round-tripped for native frontends)
    pub border_style: Mutex<String>,
    pub waveform_style: Mutex<String>,
    pub overlay_position: Mutex<String>,
    pub keep_in_clipboard: Mutex<bool>,
    /// Input gain as AtomicU32 (f32 bits) — shared with audio capture thread
    pub input_gain: Arc<std::sync::atomic::AtomicU32>,
    pub key_store: keystore::KeyStore,
    /// Path to current audio debug session directory (set during recording, cleared on stop)
    pub audio_debug_session_dir: Mutex<Option<std::path::PathBuf>>,
    /// Bottom-right anchor in logical pixels — persisted across restarts
    pub window_anchor: Mutex<Option<(f64, f64)>>,
    // KPI stats
    pub stats_total_words: Mutex<u64>,
    pub stats_total_speaking_secs: Mutex<f64>,
}

impl AppState {
    /// Create AppState from config file + keyring, without any UI framework.
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

        let transcription_provider = provider::Provider::from_url(&file_cfg.api_url);
        let llm_provider = provider::Provider::from_url(&file_cfg.llm_api_url);
        let stored_key = load_key_with_store(
            &key_store,
            provider::KeyringScope::Stt(transcription_provider),
            use_kr,
        );
        let stored_llm_key = load_key_with_store(
            &key_store,
            provider::KeyringScope::Llm(llm_provider),
            use_kr,
        );

        log(&format!(
            "Config loaded: url={}, model={}, device={:?}, provider={}, has_key={}, llm_provider={}, llm_enabled={}, llm_style={}",
            file_cfg.api_url, file_cfg.api_model, file_cfg.selected_device, transcription_provider,
            stored_key.is_some(), llm_provider, file_cfg.llm_enabled, file_cfg.llm_style.as_str()
        ));

        let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let input_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
            file_cfg.input_gain.to_bits(),
        ));
        let audio_tx = audio::spawn_audio_thread(audio_buffer.clone(), input_gain_atomic.clone());

        let state = AppState {
            recording: Mutex::new(false),
            api_key: Mutex::new(stored_key),
            api_url: Mutex::new(file_cfg.api_url),
            api_model: Mutex::new(file_cfg.api_model),
            language: Mutex::new(file_cfg.language),
            prompt: Mutex::new(file_cfg.prompt),
            shortcut_mode: Mutex::new(file_cfg.shortcut_mode),
            shortcut: Mutex::new(file_cfg.shortcut),
            selected_device: Mutex::new(file_cfg.selected_device.clone()),
            audio_sample_rate: Mutex::new(audio::device_sample_rate(&file_cfg.selected_device)),
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
        };

        // Postconditions
        assert!(
            !state.api_url.lock().unwrap().is_empty(),
            "api_url must not be empty after init"
        );
        assert!(
            !state.api_model.lock().unwrap().is_empty(),
            "api_model must not be empty after init"
        );

        state
    }
}

/// Build AppConfig by acquiring each mutex individually (one at a time, no overlapping locks).
/// Shared by all native UI consumers via FFI.
pub fn snapshot_config(state: &AppState) -> Result<AppConfig, String> {
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
        use_keyring: *state.use_keyring.lock().map_err(|e| e.to_string())?,
        border_style: state
            .border_style
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        waveform_style: state
            .waveform_style
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        overlay_position: state
            .overlay_position
            .lock()
            .map_err(|e| e.to_string())?
            .clone(),
        keep_in_clipboard: *state.keep_in_clipboard.lock().map_err(|e| e.to_string())?,
        input_gain: f32::from_bits(state.input_gain.load(Ordering::Relaxed)),
        window_anchor_right: anchor.map(|(r, _)| r),
        window_anchor_bottom: anchor.map(|(_, b)| b),
        stats_total_words: *state.stats_total_words.lock().map_err(|e| e.to_string())?,
        stats_total_speaking_secs: *state
            .stats_total_speaking_secs
            .lock()
            .map_err(|e| e.to_string())?,
    })
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
        let store = keystore::KeyStore::new();
        migrate_plaintext_key(&store, false);
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

    #[test]
    fn onboarding_marker_path_returns_some() {
        let p = onboarding_marker_path();
        assert!(p.is_some(), "onboarding_marker_path should return Some");
        let p = p.unwrap();
        assert!(
            p.ends_with(".onboarding_done"),
            "Should end with .onboarding_done, got: {:?}",
            p
        );
        // Must be inside the dimmy config dir
        assert!(
            p.parent().unwrap().to_str().unwrap().contains("dimmy"),
            "Marker must be inside dimmy config dir"
        );
    }

    #[test]
    fn onboarding_marker_roundtrip() {
        // Use temp dir to avoid touching real marker
        let tmp = std::env::temp_dir().join("dimmy-test-onboarding");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::create_dir_all(&tmp);
        let marker = tmp.join(".onboarding_done");

        // Before marker exists: not completed
        assert!(!marker.exists(), "Marker should not exist yet");

        // Create marker
        std::fs::write(&marker, "1").unwrap();
        assert!(marker.exists(), "Marker must exist after write");

        // Read marker — contents don't matter, existence is the signal
        assert!(marker.is_file(), "Marker must be a file");

        // Deleting config.json should NOT affect marker
        let fake_config = tmp.join("config.json");
        std::fs::write(&fake_config, "{}").unwrap();
        std::fs::remove_file(&fake_config).unwrap();
        assert!(marker.exists(), "Marker must survive config.json deletion");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn mark_onboarding_done_creates_file() {
        // This test uses the real config dir but is idempotent
        // (marking done when already done is a no-op)
        let result = mark_onboarding_done();
        assert!(result.is_ok(), "mark_onboarding_done should succeed");
        assert!(
            onboarding_completed(),
            "onboarding_completed must return true after marking done"
        );
    }

    #[test]
    fn onboarding_completed_is_idempotent() {
        // Calling mark_onboarding_done multiple times must not fail
        let r1 = mark_onboarding_done();
        let r2 = mark_onboarding_done();
        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(onboarding_completed());
    }

    // ── input_gain / UI field tests ──────────────────────────────────

    #[test]
    fn default_config_input_gain_is_one() {
        let cfg = AppConfig::default();
        assert!(
            (cfg.input_gain - 1.0).abs() < f32::EPSILON,
            "Default input_gain should be 1.0, got {}",
            cfg.input_gain
        );
    }

    #[test]
    fn default_config_ui_fields_non_empty() {
        let cfg = AppConfig::default();
        assert!(
            !cfg.border_style.is_empty(),
            "border_style must not be empty"
        );
        assert!(
            !cfg.waveform_style.is_empty(),
            "waveform_style must not be empty"
        );
        assert!(
            !cfg.overlay_position.is_empty(),
            "overlay_position must not be empty"
        );
    }

    #[test]
    fn load_config_defaults_input_gain_when_missing() {
        // Write a config file without input_gain field
        let tmp = std::env::temp_dir().join("dimmy-test-gain-missing");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");
        let cfg_json = serde_json::json!({
            "api_url": DEFAULT_API_URL,
            "api_model": DEFAULT_MODEL,
            "language": "en",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg_json).unwrap()).unwrap();

        // Parse manually the same way load_config_file does
        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        let gain = v["input_gain"].as_f64().unwrap_or(1.0) as f32;
        assert!(
            (gain - 1.0).abs() < f32::EPSILON,
            "Missing input_gain should default to 1.0, got {}",
            gain
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_reads_input_gain() {
        let tmp = std::env::temp_dir().join("dimmy-test-gain-present");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("config.json");
        let cfg_json = serde_json::json!({
            "api_url": DEFAULT_API_URL,
            "api_model": DEFAULT_MODEL,
            "language": "en",
            "input_gain": 0.75,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg_json).unwrap()).unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        let gain = v["input_gain"].as_f64().unwrap_or(1.0) as f32;
        assert!(
            (gain - 0.75).abs() < 0.001,
            "input_gain should be 0.75, got {}",
            gain
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_load_roundtrip_preserves_new_fields() {
        // Save a config with custom input_gain and UI fields, then load and verify
        // Combined into one test to avoid race conditions with parallel tests
        // sharing the same config file
        let mut cfg = AppConfig::default();
        cfg.input_gain = 0.42;
        cfg.border_style = "Solid".to_string();
        cfg.waveform_style = "Line".to_string();
        cfg.overlay_position = "Top Left".to_string();
        cfg.keep_in_clipboard = true;
        save_config_file(&cfg);

        let loaded = load_config_file();
        assert!(
            (loaded.input_gain - 0.42).abs() < 0.001,
            "Roundtrip input_gain should be 0.42, got {}",
            loaded.input_gain
        );
        assert_eq!(loaded.border_style, "Solid");
        assert_eq!(loaded.waveform_style, "Line");
        assert_eq!(loaded.overlay_position, "Top Left");
        assert!(loaded.keep_in_clipboard);

        // Restore default
        save_config_file(&AppConfig::default());
    }

    #[test]
    #[should_panic(expected = "input_gain must be in [0.0, 2.0]")]
    fn save_config_rejects_input_gain_above_max() {
        let mut cfg = AppConfig::default();
        cfg.input_gain = 3.0;
        save_config_file(&cfg);
    }

    #[test]
    #[should_panic(expected = "input_gain must be in [0.0, 2.0]")]
    fn save_config_rejects_negative_input_gain() {
        let mut cfg = AppConfig::default();
        cfg.input_gain = -0.5;
        save_config_file(&cfg);
    }

    #[test]
    fn new_standalone_creates_valid_state() {
        let state = AppState::new_standalone();
        assert!(!*state.recording.lock().unwrap());
        assert!(!state.api_url.lock().unwrap().is_empty());
        assert!(!state.api_model.lock().unwrap().is_empty());
        let gain_bits = state.input_gain.load(std::sync::atomic::Ordering::Relaxed);
        let gain = f32::from_bits(gain_bits);
        assert!(
            gain >= 0.0 && gain <= 2.0,
            "gain must be in [0.0, 2.0], got {}",
            gain
        );
        assert!(gain.is_finite(), "gain must be finite, got {}", gain);
    }
}

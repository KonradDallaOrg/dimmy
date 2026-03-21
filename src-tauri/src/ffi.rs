//! C-compatible FFI layer for native UI frontends.
//!
//! Exposes the Dimmy Rust core as a shared library (cdylib) that can be called
//! from Swift (macOS), C# (Windows), or C/Vala (Linux) without Tauri.
//!
//! All functions use C-compatible types: `*const c_char`, `*mut c_char`, `c_int`, `c_float`.
//! JSON strings are used for complex data exchange (config, device lists, events).

use std::ffi::{c_char, c_float, c_int, CStr, CString};
use std::sync::{Arc, Mutex, OnceLock};

use crate::audio::AudioCommand;
use crate::keystore::KeyStore;
use crate::provider::{KeyringScope, Provider};
use crate::{load_config_file, log, save_config_file, save_key_with_store, AppState};

// ── Global state ────────────────────────────────────────────────────

static GLOBAL_STATE: OnceLock<AppState> = OnceLock::new();
static EVENT_CALLBACK: Mutex<Option<extern "C" fn(*const c_char)>> = Mutex::new(None);

fn state() -> &'static AppState {
    GLOBAL_STATE
        .get()
        .expect("dimmy_init() must be called before any other function")
}

/// Emit an event to the native UI via the registered callback.
/// Called from within Rust core instead of `app_handle.emit()`.
pub fn emit_event(event_name: &str, payload_json: &str) {
    if let Ok(guard) = EVENT_CALLBACK.lock() {
        if let Some(cb) = *guard {
            let json = format!(r#"{{"event":"{}","payload":{}}}"#, event_name, payload_json);
            if let Ok(cstr) = CString::new(json) {
                cb(cstr.as_ptr());
            }
        }
    }
}

// ── Helper: write string into caller-provided buffer ────────────────

fn write_to_buf(s: &str, buf: *mut c_char, buf_len: c_int) -> c_int {
    if buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let bytes = s.as_bytes();
    let max = (buf_len - 1) as usize; // leave room for null terminator
    let copy_len = bytes.len().min(max);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, copy_len);
        *buf.add(copy_len) = 0; // null terminator
    }
    copy_len as c_int
}

// ── Lifecycle ───────────────────────────────────────────────────────

/// Initialize the Dimmy core. Must be called once before any other function.
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_init() -> c_int {
    // Set up panic hook with backtrace
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {}\nBacktrace:\n{}", info, bt);
        eprintln!("{}", msg);
        if let Some(path) = crate::log_path() {
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

    log("=== Dimmy FFI starting ===");

    // Load config
    let file_cfg = load_config_file();
    let use_kr = file_cfg.use_keyring;
    let key_store = KeyStore::new();

    // Migrate legacy keys
    crate::migrate_plaintext_key(&key_store, use_kr);
    crate::migrate_keyring_to_per_provider(
        &key_store,
        &file_cfg.api_url,
        &file_cfg.llm_api_url,
        use_kr,
    );

    // Load API keys
    let transcription_provider = Provider::from_url(&file_cfg.api_url);
    let llm_provider = Provider::from_url(&file_cfg.llm_api_url);
    let stored_key = crate::load_key_with_store(
        &key_store,
        KeyringScope::Stt(transcription_provider),
        use_kr,
    );
    let stored_llm_key =
        crate::load_key_with_store(&key_store, KeyringScope::Llm(llm_provider), use_kr);

    log(&format!(
        "FFI init: provider={}, has_key={}, llm_provider={}, llm_enabled={}",
        transcription_provider,
        stored_key.is_some(),
        llm_provider,
        file_cfg.llm_enabled
    ));

    // Audio thread
    let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let audio_tx = crate::audio::spawn_audio_thread(audio_buffer.clone());

    let app_state = AppState {
        recording: Mutex::new(false),
        api_key: Mutex::new(stored_key),
        api_url: Mutex::new(file_cfg.api_url),
        api_model: Mutex::new(file_cfg.api_model),
        language: Mutex::new(file_cfg.language),
        prompt: Mutex::new(file_cfg.prompt),
        shortcut_mode: Mutex::new(file_cfg.shortcut_mode),
        shortcut: Mutex::new(file_cfg.shortcut),
        selected_device: Mutex::new(file_cfg.selected_device.clone()),
        audio_sample_rate: Mutex::new(crate::audio::device_sample_rate(&file_cfg.selected_device)),
        transcript: Mutex::new(String::new()),
        audio_buffer,
        audio_tx: Mutex::new(audio_tx),
        streaming_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        key_store,
        audio_debug_session_dir: Mutex::new(None),
        window_anchor: Mutex::new(None),
        stats_total_words: Mutex::new(file_cfg.stats_total_words),
        stats_total_speaking_secs: Mutex::new(file_cfg.stats_total_speaking_secs),
    };

    match GLOBAL_STATE.set(app_state) {
        Ok(()) => {
            log("FFI init complete");
            0
        }
        Err(_) => {
            log("ERROR: dimmy_init() called twice");
            -1
        }
    }
}

/// Shut down: save config and clean up.
#[no_mangle]
pub extern "C" fn dimmy_shutdown() {
    if let Some(st) = GLOBAL_STATE.get() {
        if let Ok(cfg) = crate::snapshot_config(st) {
            save_config_file(&cfg);
            log("Config saved on shutdown");
        }
    }
    log("=== Dimmy FFI shutdown ===");
}

/// Register event callback. The native UI provides a function pointer that
/// receives JSON strings for events (recording_progress, chunk_status, etc.).
#[no_mangle]
pub extern "C" fn dimmy_set_event_callback(cb: extern "C" fn(*const c_char)) {
    if let Ok(mut guard) = EVENT_CALLBACK.lock() {
        *guard = Some(cb);
    }
}

// ── Recording ───────────────────────────────────────────────────────

/// Start recording. Returns 0=OK, -1=no API key, -2=already recording.
#[no_mangle]
pub extern "C" fn dimmy_start_recording() -> c_int {
    let st = state();

    let mut recording = match st.recording.lock() {
        Ok(r) => r,
        Err(_) => return -3,
    };
    if *recording {
        return -2;
    }

    // Fail fast: no API key
    let has_key = st.api_key.lock().map(|k| k.is_some()).unwrap_or(false);
    if !has_key {
        return -1;
    }

    *recording = true;

    let selected_device = st.selected_device.lock().ok().and_then(|d| d.clone());
    let device_sr = crate::audio::device_sample_rate(&selected_device);
    if let Ok(mut sr) = st.audio_sample_rate.lock() {
        *sr = device_sr;
    }

    let _ = st
        .audio_tx
        .lock()
        .map(|tx| tx.send(AudioCommand::Start(selected_device)));

    emit_event("recording_started", "{}");
    0
}

/// Stop recording and get transcript. Returns transcript length, or negative on error.
/// Transcript is written to `out_buf` (null-terminated).
#[no_mangle]
pub extern "C" fn dimmy_stop_recording(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let st = state();

    // Stop audio capture
    let _ = st.audio_tx.lock().map(|tx| tx.send(AudioCommand::Stop));
    if let Ok(mut r) = st.recording.lock() {
        *r = false;
    }

    // Get audio buffer
    let buffer = match st.audio_buffer.lock() {
        Ok(mut b) => {
            let data = b.clone();
            b.clear();
            data
        }
        Err(_) => return -1,
    };

    if buffer.is_empty() {
        return write_to_buf("", out_buf, buf_len);
    }

    emit_event("status", r#"{"state":"transcribing"}"#);

    // Process audio and transcribe (blocking)
    let sample_rate = st.audio_sample_rate.lock().map(|s| *s).unwrap_or(16000);
    let api_url = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let api_model = st.api_model.lock().map(|m| m.clone()).unwrap_or_default();
    let api_key = match st.api_key.lock().ok().and_then(|k| k.clone()) {
        Some(k) => k,
        None => return write_to_buf("", out_buf, buf_len),
    };
    let language = st.language.lock().map(|l| l.clone()).unwrap_or_default();
    let prompt = st.prompt.lock().map(|p| p.clone()).unwrap_or_default();
    let preprocessing = st.preprocessing_enabled.lock().map(|p| *p).unwrap_or(true);
    // Build typed audio pipeline: RawAudio → ProcessedAudio
    let raw = crate::audio::RawAudio {
        samples: buffer,
        sample_rate,
    };
    let processed = raw.preprocess(preprocessing);
    if processed.is_empty() {
        emit_event("error", r#"{"message":"No speech detected"}"#);
        return write_to_buf("", out_buf, buf_len);
    }

    // Determine provider file size limit for chunking
    let provider = Provider::from_url(&api_url);
    let max_bytes = provider.max_file_bytes();

    // Transcribe (using the runtime)
    let rt = tokio::runtime::Runtime::new().unwrap();
    let transcript = rt.block_on(async {
        crate::transcribe::transcribe_chunked(
            &api_url,
            &api_model,
            &api_key,
            processed,
            &language,
            &prompt,
            max_bytes,
            Some(&|current, total| {
                emit_event(
                    "chunk_progress",
                    &format!(r#"{{"current":{},"total":{}}}"#, current, total),
                );
            }),
        )
        .await
    });

    match transcript {
        Ok(text) => {
            emit_event(
                "transcript_ready",
                &format!(r#"{{"text":"{}"}}"#, text.replace('"', "\\\"")),
            );
            write_to_buf(&text, out_buf, buf_len)
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            emit_event(
                "error",
                &format!(r#"{{"message":"{}"}}"#, err_msg.replace('"', "\\\"")),
            );
            write_to_buf("", out_buf, buf_len)
        }
    }
}

/// Cancel recording without transcribing.
#[no_mangle]
pub extern "C" fn dimmy_cancel_recording() {
    let st = state();
    let _ = st.audio_tx.lock().map(|tx| tx.send(AudioCommand::Stop));
    if let Ok(mut r) = st.recording.lock() {
        *r = false;
    }
    if let Ok(mut b) = st.audio_buffer.lock() {
        b.clear();
    }
    emit_event("recording_cancelled", "{}");
}

// ── Config ──────────────────────────────────────────────────────────

/// Get full config as JSON string. Returns length written, or -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_get_config_json(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let st = state();
    let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);

    // Build config JSON (same structure as get_config Tauri command)
    let has_stt_key = st.api_key.lock().map(|k| k.is_some()).unwrap_or(false);
    let has_llm_key = st.llm_api_key.lock().map(|k| k.is_some()).unwrap_or(false);

    let json = serde_json::json!({
        "has_key": has_stt_key,
        "api_url": *st.api_url.lock().unwrap_or_else(|e| e.into_inner()),
        "api_model": *st.api_model.lock().unwrap_or_else(|e| e.into_inner()),
        "language": *st.language.lock().unwrap_or_else(|e| e.into_inner()),
        "prompt": *st.prompt.lock().unwrap_or_else(|e| e.into_inner()),
        "shortcut_mode": *st.shortcut_mode.lock().unwrap_or_else(|e| e.into_inner()),
        "shortcut": *st.shortcut.lock().unwrap_or_else(|e| e.into_inner()),
        "selected_device": *st.selected_device.lock().unwrap_or_else(|e| e.into_inner()),
        "devices": crate::audio::list_input_devices(),
        "llm_enabled": *st.llm_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_style": st.llm_style.lock().map(|s| s.as_str().to_string()).unwrap_or_default(),
        "llm_tone": st.llm_tone.lock().map(|t| t.as_str().to_string()).unwrap_or_default(),
        "llm_custom_prompt": *st.llm_custom_prompt.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_translate_to": *st.llm_translate_to.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_api_url": *st.llm_api_url.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_api_model": *st.llm_api_model.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_use_same_key": *st.llm_use_same_key.lock().unwrap_or_else(|e| e.into_inner()),
        "has_llm_key": has_llm_key,
        "llm_log_enabled": *st.llm_log_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "chunk_streaming_enabled": *st.chunk_streaming_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "preprocessing_enabled": *st.preprocessing_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "audio_debug_enabled": *st.audio_debug_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "use_keyring": use_kr,
        "stats_total_words": *st.stats_total_words.lock().unwrap_or_else(|e| e.into_inner()),
        "stats_total_speaking_secs": *st.stats_total_speaking_secs.lock().unwrap_or_else(|e| e.into_inner()),
        // Per-provider key flags
        "has_groq_key": st.key_store.has_key(KeyringScope::Stt(Provider::Groq), use_kr),
        "has_openai_key": st.key_store.has_key(KeyringScope::Stt(Provider::OpenAI), use_kr),
        "has_gemini_key": st.key_store.has_key(KeyringScope::Stt(Provider::Gemini), use_kr),
        "has_deepgram_key": st.key_store.has_key(KeyringScope::Stt(Provider::Deepgram), use_kr),
        "has_custom_key": st.key_store.has_key(KeyringScope::Stt(Provider::Custom), use_kr),
    });

    let s = serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string());
    write_to_buf(&s, out_buf, buf_len)
}

/// Set config from JSON string. Returns 0=OK, -1=error.
/// # Safety
/// `json_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_set_config_json(json_ptr: *const c_char) -> c_int {
    if json_ptr.is_null() {
        return -1;
    }
    let json_str = CStr::from_ptr(json_ptr);
    let json_str = match json_str.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    let v: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return -1,
    };

    let st = state();
    let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);

    // Apply each field if present
    if let Some(s) = v["api_url"].as_str() {
        if let Ok(mut u) = st.api_url.lock() {
            *u = s.to_string();
        }
    }
    if let Some(s) = v["api_model"].as_str() {
        if let Ok(mut m) = st.api_model.lock() {
            *m = s.to_string();
        }
    }
    if let Some(s) = v["language"].as_str() {
        if let Ok(mut l) = st.language.lock() {
            *l = s.to_string();
        }
    }
    if let Some(s) = v["prompt"].as_str() {
        if let Ok(mut p) = st.prompt.lock() {
            *p = s.to_string();
        }
    }
    if let Some(s) = v["shortcut_mode"].as_str() {
        if let Ok(mut m) = st.shortcut_mode.lock() {
            *m = s.to_string();
        }
    }
    if let Some(s) = v["shortcut"].as_str() {
        if let Ok(mut sh) = st.shortcut.lock() {
            *sh = s.to_string();
        }
    }
    if let Some(s) = v["selected_device"].as_str() {
        if let Ok(mut d) = st.selected_device.lock() {
            *d = Some(s.to_string());
        }
    }

    // API key
    if let Some(key) = v["api_key"].as_str() {
        if !key.is_empty() {
            let url = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
            let provider = Provider::from_url(&url);
            let _ = save_key_with_store(&st.key_store, KeyringScope::Stt(provider), key, use_kr);
            if let Ok(mut k) = st.api_key.lock() {
                *k = Some(key.to_string());
            }
        }
    }

    // LLM fields
    if let Some(b) = v["llm_enabled"].as_bool() {
        if let Ok(mut e) = st.llm_enabled.lock() {
            *e = b;
        }
    }
    if let Some(s) = v["llm_style"].as_str() {
        if let Ok(mut style) = st.llm_style.lock() {
            *style = crate::llm::LlmStyle::from_str_lossy(s);
        }
    }
    if let Some(s) = v["llm_tone"].as_str() {
        if let Ok(mut tone) = st.llm_tone.lock() {
            *tone = crate::llm::LlmTone::from_str_lossy(s);
        }
    }
    if let Some(s) = v["llm_custom_prompt"].as_str() {
        if let Ok(mut p) = st.llm_custom_prompt.lock() {
            *p = s.to_string();
        }
    }
    if let Some(s) = v["llm_translate_to"].as_str() {
        if let Ok(mut t) = st.llm_translate_to.lock() {
            *t = s.to_string();
        }
    }
    if let Some(s) = v["llm_api_url"].as_str() {
        if let Ok(mut u) = st.llm_api_url.lock() {
            *u = s.to_string();
        }
    }
    if let Some(s) = v["llm_api_model"].as_str() {
        if let Ok(mut m) = st.llm_api_model.lock() {
            *m = s.to_string();
        }
    }
    if let Some(b) = v["llm_use_same_key"].as_bool() {
        if let Ok(mut k) = st.llm_use_same_key.lock() {
            *k = b;
        }
    }
    if let Some(key) = v["llm_api_key"].as_str() {
        if !key.is_empty() {
            let url = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
            let provider = Provider::from_url(&url);
            let _ = save_key_with_store(&st.key_store, KeyringScope::Llm(provider), key, use_kr);
            if let Ok(mut k) = st.llm_api_key.lock() {
                *k = Some(key.to_string());
            }
        }
    }
    if let Some(b) = v["llm_log_enabled"].as_bool() {
        if let Ok(mut l) = st.llm_log_enabled.lock() {
            *l = b;
        }
    }

    // Audio / appearance
    if let Some(b) = v["preprocessing_enabled"].as_bool() {
        if let Ok(mut p) = st.preprocessing_enabled.lock() {
            *p = b;
        }
    }
    if let Some(b) = v["chunk_streaming_enabled"].as_bool() {
        if let Ok(mut c) = st.chunk_streaming_enabled.lock() {
            *c = b;
        }
    }
    if let Some(b) = v["audio_debug_enabled"].as_bool() {
        if let Ok(mut a) = st.audio_debug_enabled.lock() {
            *a = b;
        }
    }
    if let Some(b) = v["use_keyring"].as_bool() {
        let old = st.use_keyring.lock().map(|k| *k).unwrap_or(false);
        if b != old {
            let _ = st.key_store.migrate_keys(b);
            if let Ok(mut k) = st.use_keyring.lock() {
                *k = b;
            }
        }
    }

    // Save to disk
    if let Ok(cfg) = crate::snapshot_config(st) {
        save_config_file(&cfg);
    }

    0
}

// ── Audio ───────────────────────────────────────────────────────────

/// Get current microphone amplitude (0.0 - 1.0).
#[no_mangle]
pub extern "C" fn dimmy_get_amplitude() -> c_float {
    let st = state();
    let buffer = match st.audio_buffer.lock() {
        Ok(b) => b,
        Err(_) => return 0.0,
    };
    if buffer.is_empty() {
        return 0.0;
    }
    // Peak amplitude of last ~800 samples (~50ms at 16kHz)
    let start = buffer.len().saturating_sub(800);
    let peak = buffer[start..]
        .iter()
        .fold(0.0f32, |max, &s| max.max(s.abs()));
    peak.min(1.0)
}

/// Get device list as JSON array. Caller must NOT free the returned pointer.
/// The string is valid until the next call to this function.
#[no_mangle]
pub extern "C" fn dimmy_list_devices_json(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let devices = crate::audio::list_input_devices();
    let json = serde_json::to_string(&devices).unwrap_or_else(|_| "[]".to_string());
    write_to_buf(&json, out_buf, buf_len)
}

// ── LLM ─────────────────────────────────────────────────────────────

/// Cycle LLM style. direction: +1 = next, -1 = previous.
#[no_mangle]
pub extern "C" fn dimmy_cycle_llm_style(direction: c_int) {
    let st = state();
    if let Ok(mut style) = st.llm_style.lock() {
        let styles = crate::llm::LlmStyle::ALL;
        let idx = styles.iter().position(|s| *s == *style).unwrap_or(0);
        let new_idx = if direction > 0 {
            (idx + 1) % styles.len()
        } else {
            (idx + styles.len() - 1) % styles.len()
        };
        *style = styles[new_idx];

        // Update llm_enabled based on style
        if let Ok(mut enabled) = st.llm_enabled.lock() {
            *enabled = styles[new_idx] != crate::llm::LlmStyle::Off;
        }

        emit_event(
            "style_changed",
            &format!(r#"{{"style":"{}"}}"#, styles[new_idx].as_str()),
        );
    }
}

/// Cycle LLM tone. direction: +1 = next, -1 = previous.
#[no_mangle]
pub extern "C" fn dimmy_cycle_llm_tone(direction: c_int) {
    let st = state();
    if let Ok(mut tone) = st.llm_tone.lock() {
        let tones = crate::llm::LlmTone::ALL;
        let idx = tones.iter().position(|t| *t == *tone).unwrap_or(0);
        let new_idx = if direction > 0 {
            (idx + 1) % tones.len()
        } else {
            (idx + tones.len() - 1) % tones.len()
        };
        *tone = tones[new_idx];

        emit_event(
            "tone_changed",
            &format!(r#"{{"tone":"{}"}}"#, tones[new_idx].as_str()),
        );
    }
}

// ── Stats ───────────────────────────────────────────────────────────

/// Update cumulative stats.
#[no_mangle]
pub extern "C" fn dimmy_update_stats(words: c_int, speaking_secs: f64) {
    let st = state();
    if let Ok(mut w) = st.stats_total_words.lock() {
        *w += words as u64;
    }
    if let Ok(mut s) = st.stats_total_speaking_secs.lock() {
        *s += speaking_secs;
    }
    // Persist
    if let Ok(cfg) = crate::snapshot_config(st) {
        save_config_file(&cfg);
    }
}

// ── Utility ─────────────────────────────────────────────────────────

/// Check if an API key is configured. Returns 1=yes, 0=no.
#[no_mangle]
pub extern "C" fn dimmy_has_api_key() -> c_int {
    let st = state();
    st.api_key.lock().map(|k| k.is_some() as c_int).unwrap_or(0)
}

/// Check if recording is active. Returns 1=yes, 0=no.
#[no_mangle]
pub extern "C" fn dimmy_is_recording() -> c_int {
    let st = state();
    st.recording.lock().map(|r| *r as c_int).unwrap_or(0)
}

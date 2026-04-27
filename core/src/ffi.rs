//! C-compatible FFI layer for native UI frontends.
//!
//! Exposes the Dimmy Rust core as a shared library (cdylib) that can be called
//! from Swift (macOS), C# (Windows), or Rust/GTK4 (Linux).
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

    // Negative space: copy_len must fit within buffer (excluding null terminator)
    assert!(
        copy_len < buf_len as usize,
        "copy_len {} must be < buf_len {}",
        copy_len,
        buf_len
    );

    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, copy_len);
        *buf.add(copy_len) = 0; // null terminator
    }

    // Postcondition: returned length matches what we wrote
    assert!(copy_len as c_int >= 0, "copy_len must be non-negative");
    copy_len as c_int
}

// ── Lifecycle ───────────────────────────────────────────────────────

/// Initialize the Dimmy core. Must be called once before any other function.
/// Returns 0 on success, -1 on error.
/// Session start timestamp, set on first dimmy_init call. Used to
/// compute the duration in app.session_ended.
static SESSION_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Counter of successful transcriptions in this session. Incremented
/// from the success branch of dimmy_stop_recording. Read on shutdown.
static TRANSCRIBE_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Best-effort early-init trace writer. Used to debug crashes that occur
/// before `crate::log` is reachable (e.g., panics during dimmy_init prior
/// to the first `log()` call). Writes to %TEMP%\dimmy_init_trace.log on
/// Windows, $TMPDIR/dimmy_init_trace.log elsewhere. Never panics — every
/// failure mode silently no-ops, so this can be safely invoked from
/// inside a panic hook or from a not-yet-initialised context.
fn write_init_trace(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("dimmy_init_trace.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

/// Inner body of dimmy_init. Split out so the public extern "C" wrapper
/// can run it inside `catch_unwind` and convert any panic into a clean
/// -1 return value. With `panic = unwind` (the default release profile)
/// a panic that escapes an `extern "C"` boundary is forced to abort via
/// __fastfail; catching it here keeps the host process alive and lets
/// the caller surface a normal error to the user.
fn dimmy_init_inner() -> c_int {
    let init_start = std::time::Instant::now();
    let _ = SESSION_START.set(init_start);
    write_init_trace("P1: SESSION_START set");

    // Set up panic hook with backtrace. NB: must NOT use eprintln!/println!
    // — see crate::log for why (Velopack-launched windowed app has no
    // stderr handle and eprintln panics, which inside a panic hook
    // recurses straight into __fastfail).
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {}\nBacktrace:\n{}", info, bt);
        // Always best-effort; never panic from the hook.
        write_init_trace(&msg);
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
    write_init_trace("P2: panic hook installed");

    log("=== Dimmy FFI starting ===");
    write_init_trace("P3: first log line written");

    // Initialise telemetry (Sentry crash + error pipeline). No-op when
    // the build did not embed a DSN, or when telemetry-sentry feature
    // is disabled. Must be after the panic hook above so Sentry's
    // own panic integration nests cleanly.
    crate::telemetry::init();
    write_init_trace("P4: telemetry init returned");

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
    let input_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
        file_cfg.input_gain.to_bits(),
    ));
    let audio_tx =
        crate::audio::spawn_audio_thread(audio_buffer.clone(), input_gain_atomic.clone());

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
        ggml_debug_logging: Mutex::new(file_cfg.ggml_debug_logging),
        use_keyring: Mutex::new(file_cfg.use_keyring),
        stt_mode: Mutex::new(file_cfg.stt_mode),
        local_model: Mutex::new(file_cfg.local_model),
        filler_removal_enabled: Mutex::new(file_cfg.filler_removal_enabled),
        llm_mode: Mutex::new(file_cfg.llm_mode),
        local_llm_model: Mutex::new(file_cfg.local_llm_model),
        border_style: Mutex::new(file_cfg.border_style),
        waveform_style: Mutex::new(file_cfg.waveform_style),
        overlay_position: Mutex::new(file_cfg.overlay_position),
        keep_in_clipboard: Mutex::new(file_cfg.keep_in_clipboard),
        input_gain: input_gain_atomic,
        key_store,
        audio_debug_session_dir: Mutex::new(None),
        window_anchor: Mutex::new(None),
        stats_total_words: Mutex::new(file_cfg.stats_total_words),
        stats_total_speaking_secs: Mutex::new(file_cfg.stats_total_speaking_secs),
        history_store: Mutex::new({
            let history_db = crate::config_dir_path()
                .map(|p| p.join("history.db"))
                .unwrap_or_else(|| std::path::PathBuf::from("history.db"));
            crate::history::HistoryStore::new(&history_db).ok()
        }),
    };

    // Apply ggml debug toggle to the gpu_diag trampoline before any model
    // load. set_ggml_debug_enabled is the lock-free side of the AtomicBool
    // the C-ABI log callback reads on every line.
    crate::gpu_diag::set_ggml_debug_enabled(file_cfg.ggml_debug_logging);

    // GPU stability telemetry: read the sticky known-bad marker the
    // previous run may have left on disk. We do NOT eagerly probe the
    // GPU at init time (the probe has side effects: env-var mutation,
    // ggml backend init, library loads); instead we report the
    // *intended* backend (compile-time feature) and whether last run
    // crashed during GPU init. perf.gpu_status fires every launch;
    // error.gpu_crash fires only on the launch immediately after a
    // crash, so that we can compute "GPU crashes per N launches"
    // independently of whether the user actually triggered a
    // local-STT call this session.
    let known_bad_record = crate::gpu_health::read_known_bad();
    let known_bad_found = known_bad_record.is_some();
    let compiled_backend = compiled_gpu_backend();
    crate::telemetry::track(crate::telemetry::Event::PerfGpuStatus {
        backend: compiled_backend,
        fell_back_to_cpu: known_bad_found,
        known_bad: known_bad_found,
    });
    if let Some(rec) = known_bad_record {
        crate::telemetry::track(crate::telemetry::Event::ErrorGpuCrash {
            backend: compiled_backend,
            // `context` is a free-form Rust string written by us at
            // crash recovery time (e.g. "whisper_load: <path>"); the
            // sanitize::scrub_path filter applied by `looks_like_secret`
            // before send guards against any path leakage.
            context: rec.context,
        });
    }

    match GLOBAL_STATE.set(app_state) {
        Ok(()) => {
            log("FFI init complete");
            write_init_trace("P5: GLOBAL_STATE set, init complete");

            // Emit app.started — once per process. The cold-start figure
            // includes everything between dimmy_init entry and now
            // (panic-hook setup, config load, key migration, etc).
            let cold_start_ms = init_start.elapsed().as_millis() as u64;
            crate::telemetry::track(crate::telemetry::Event::AppStarted {
                version: env!("CARGO_PKG_VERSION"),
                os: crate::telemetry::events::os_name(),
                arch: crate::telemetry::events::arch_name(),
                cold_start_ms,
            });

            0
        }
        Err(_) => {
            log("ERROR: dimmy_init() called twice");
            -1
        }
    }
}

/// Public FFI entry. Wraps the body in `catch_unwind` so any panic
/// inside the inner init (config load, audio thread spawn, telemetry
/// init, …) is converted to a -1 return value rather than aborting the
/// host process via __fastfail. Without this, a panic crossing the
/// `extern "C"` boundary would force-abort because the default ABI is
/// no-unwind. The C# host can then surface a normal error to the user.
#[no_mangle]
pub extern "C" fn dimmy_init() -> c_int {
    write_init_trace("P0: dimmy_init entered");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(dimmy_init_inner));
    match result {
        Ok(rc) => {
            write_init_trace(&format!("P_OK: dimmy_init returning {}", rc));
            rc
        }
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else {
                "(unknown panic payload)".to_string()
            };
            write_init_trace(&format!("P_PANIC: dimmy_init body panicked: {}", msg));
            -1
        }
    }
}

/// Shut down: stop audio, save config and clean up.
#[no_mangle]
pub extern "C" fn dimmy_shutdown() {
    if let Some(st) = GLOBAL_STATE.get() {
        // Stop audio stream first — release microphone handle before process exits
        if let Ok(tx) = st.audio_tx.lock() {
            let _ = tx.send(AudioCommand::Stop);
            log("Audio stream stopped on shutdown");
        }
        // Clear recording flag
        if let Ok(mut r) = st.recording.lock() {
            *r = false;
        }
        // Postcondition: recording must be false after shutdown
        if let Ok(r) = st.recording.lock() {
            assert!(
                !*r,
                "dimmy_shutdown: recording flag must be false after shutdown"
            );
        }

        // Small delay to let cpal release the device
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Release whisper and LLM models from VRAM
        crate::local_stt::clear_model_cache();
        crate::local_llm::clear_llm_cache();

        if let Ok(cfg) = crate::snapshot_config(st) {
            save_config_file(&cfg);
            log("Config saved on shutdown");
        }
    }

    // Telemetry: app.session_ended. Best-effort; if SESSION_START was
    // never set (init failed), we skip rather than guess a duration.
    if let Some(start) = SESSION_START.get() {
        let duration_secs = start.elapsed().as_secs();
        let transcribe_count = TRANSCRIBE_COUNT.load(std::sync::atomic::Ordering::Relaxed);
        crate::telemetry::track(crate::telemetry::Event::AppSessionEnded {
            duration_secs,
            transcribe_count,
        });
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

    // Fail fast: no API key (only required for cloud STT mode)
    let is_local = st
        .stt_mode
        .lock()
        .map(|m| m.as_str() == "local")
        .unwrap_or(false);
    if !is_local {
        let has_key = st.api_key.lock().map(|k| k.is_some()).unwrap_or(false);
        if !has_key {
            return -1;
        }
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
    // Preconditions: caller must provide a valid buffer
    if out_buf.is_null() || buf_len <= 0 {
        log("ERROR: dimmy_stop_recording called with null buffer or invalid length");
        return -1;
    }

    let st = state();

    // Wait briefly for audio samples to arrive if the stream just started.
    // The cpal stream can take 100-300ms to produce first samples after Start.
    // Without this, rapid Start→Stop yields an empty buffer.
    {
        let max_wait_ms = 500;
        let poll_ms = 20;
        let mut waited = 0;
        while waited < max_wait_ms {
            if let Ok(b) = st.audio_buffer.lock() {
                if !b.is_empty() {
                    log(&format!(
                        "[StopRec] buffer ready after {}ms ({} samples)",
                        waited,
                        b.len()
                    ));
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(poll_ms));
            waited += poll_ms;
        }
        // Assert: buffer wait did not exceed a reasonable maximum
        assert!(
            waited <= max_wait_ms + poll_ms,
            "dimmy_stop_recording: buffer wait {}ms exceeded max {}ms",
            waited,
            max_wait_ms
        );
        if waited >= max_wait_ms {
            log("[StopRec] WARNING: timed out waiting for audio samples");
        }
    }

    // Stop audio capture
    let _ = st.audio_tx.lock().map(|tx| tx.send(AudioCommand::Stop));
    if let Ok(mut r) = st.recording.lock() {
        *r = false;
    }

    // Small delay to let in-flight audio callbacks flush
    std::thread::sleep(std::time::Duration::from_millis(30));

    // Get audio buffer
    let buffer = match st.audio_buffer.lock() {
        Ok(mut b) => {
            let data = b.clone();
            b.clear();
            data
        }
        Err(_) => return -1,
    };

    // Diagnostic: log buffer stats
    let buf_len_samples = buffer.len();
    let peak = buffer.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(
        peak.is_finite(),
        "dimmy_stop_recording: peak amplitude must be finite, got {}",
        peak
    );
    log(&format!(
        "[StopRec] buffer: {} samples, peak amplitude: {:.6}",
        buf_len_samples, peak
    ));

    if buffer.is_empty() {
        log("[StopRec] buffer empty — returning empty");
        return write_to_buf("", out_buf, buf_len);
    }

    // Detect completely silent input (muted mic / privacy blocked)
    if peak < 1e-7 && buf_len_samples > 4800 {
        log("[StopRec] Microphone appears muted (all zeros) — check system settings");
        emit_event(
            "error",
            r#"{"message":"Microphone is muted — check system sound settings"}"#,
        );
        return write_to_buf("", out_buf, buf_len);
    }

    // Detect clipping (>5% of samples at ±1.0) — common with BT headsets
    if peak >= 0.999 && buf_len_samples > 4800 {
        let clipped = buffer.iter().filter(|&&s| s.abs() >= 0.999).count();
        let clip_pct = clipped as f64 / buf_len_samples as f64 * 100.0;
        assert!(
            (0.0..=100.0).contains(&clip_pct),
            "dimmy_stop_recording: clipping percentage must be in [0, 100], got {}",
            clip_pct
        );
        if clip_pct > 5.0 {
            log(&format!("[StopRec] WARNING: {:.1}% of samples clipped — audio may be distorted. Lower mic volume or set input_gain < 1.0", clip_pct));
            emit_event(
                "error",
                r#"{"message":"Microphone input is clipping — lower mic volume in Settings"}"#,
            );
        }
    }

    emit_event("status", r#"{"state":"transcribing"}"#);

    // Process audio and transcribe (blocking)
    let sample_rate = st.audio_sample_rate.lock().map(|s| *s).unwrap_or(16000);
    let stt_mode = st
        .stt_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "cloud".to_string());
    let local_model_filename = st
        .local_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "ggml-base-q8_0.bin".to_string());
    let api_url = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let api_model = st.api_model.lock().map(|m| m.clone()).unwrap_or_default();
    // API key is only required for cloud mode
    let api_key = st.api_key.lock().ok().and_then(|k| k.clone());
    if stt_mode == "cloud" && api_key.is_none() {
        return write_to_buf("", out_buf, buf_len);
    }
    let language = st.language.lock().map(|l| l.clone()).unwrap_or_default();
    let prompt = st.prompt.lock().map(|p| p.clone()).unwrap_or_default();
    let preprocessing = st.preprocessing_enabled.lock().map(|p| *p).unwrap_or(true);
    // Audio debug: create session directory if enabled
    let audio_debug = st.audio_debug_enabled.lock().map(|d| *d).unwrap_or(false);
    let debug_dir = if audio_debug {
        crate::create_debug_session_dir()
    } else {
        None
    };

    // Build typed audio pipeline: RawAudio → ProcessedAudio
    let raw = crate::audio::RawAudio {
        samples: buffer,
        sample_rate,
    };

    // Save raw audio before preprocessing
    if let Some(ref dir) = debug_dir {
        if let Ok(raw_wav) = raw.to_wav() {
            crate::save_debug_wav(dir, "raw.wav", &raw_wav);
            log(&format!(
                "[Debug] Saved raw.wav ({} bytes) to {:?}",
                raw_wav.len(),
                dir
            ));
        }
    }

    let processed = raw.preprocess(preprocessing);
    log(&format!(
        "[StopRec] after preprocess: {} samples (preprocessing={})",
        processed.samples.len(),
        preprocessing
    ));

    // Save processed audio
    if let Some(ref dir) = debug_dir {
        if let Ok(proc_wav) = processed.to_wav() {
            crate::save_debug_wav(dir, "processed.wav", &proc_wav);
            log(&format!(
                "[Debug] Saved processed.wav ({} bytes)",
                proc_wav.len()
            ));
        }
    }

    if processed.is_empty() {
        log("[StopRec] VAD removed all audio — no speech detected");
        emit_event("error", r#"{"message":"No speech detected"}"#);
        return write_to_buf("", out_buf, buf_len);
    }

    // Route transcription based on stt_mode: "local" or "cloud"
    let transcribe_start = std::time::Instant::now();
    let transcript = if stt_mode == "local" {
        log(&format!(
            "[StopRec] Local STT mode — model: {}",
            local_model_filename
        ));
        crate::transcribe::transcribe_audio_local(&processed, &language, &local_model_filename)
    } else {
        // Cloud mode: existing flow — WAV encode + chunked cloud API
        let cloud_key = api_key.unwrap_or_default();
        let provider = Provider::from_url(&api_url);
        let max_bytes = provider.max_file_bytes();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            crate::transcribe::transcribe_chunked(
                &api_url,
                &api_model,
                &cloud_key,
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
        })
    };

    // Save debug metadata + transcript
    let (transcript_text, transcript_err) = match &transcript {
        Ok(text) => (Some(text.as_str()), None),
        Err(e) => (None, Some(format!("{}", e))),
    };

    if let Some(ref dir) = debug_dir {
        let device_name = st
            .selected_device
            .lock()
            .ok()
            .and_then(|d| d.clone())
            .unwrap_or_default();
        let llm_style = st
            .llm_style
            .lock()
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
        let llm_tone = st
            .llm_tone
            .lock()
            .map(|t| t.as_str().to_string())
            .unwrap_or_default();
        let input_gain = f32::from_bits(st.input_gain.load(std::sync::atomic::Ordering::Relaxed));

        let metadata = serde_json::json!({
            "timestamp": chrono::Local::now().to_rfc3339(),
            "device": device_name,
            "sample_rate": sample_rate,
            "raw_samples": buf_len_samples,
            "peak_amplitude": peak,
            "preprocessing": preprocessing,
            "input_gain": input_gain,
            "stt_mode": stt_mode,
            "provider": if stt_mode == "local" { &local_model_filename } else { &api_url },
            "model": if stt_mode == "local" { &local_model_filename } else { &api_model },
            "language": language,
            "llm_style": llm_style,
            "llm_tone": llm_tone,
            "transcript": transcript_text.unwrap_or(""),
            "error": transcript_err.as_deref().unwrap_or(""),
            "duration_secs": buf_len_samples as f64 / sample_rate as f64,
        });
        let meta_json = serde_json::to_string_pretty(&metadata).unwrap_or_default();
        let _ = std::fs::write(dir.join("metadata.json"), &meta_json);
        log("[Debug] Saved metadata.json");
    }

    match transcript {
        Ok(text) => {
            // Apply filler removal if enabled
            let filler_enabled = st
                .filler_removal_enabled
                .lock()
                .map(|f| *f)
                .unwrap_or(false);
            let text = if filler_enabled {
                let cleaned = crate::filler::remove_fillers(&text, &language);
                if cleaned != text {
                    log(&format!(
                        "[StopRec] Filler removal: {} chars → {} chars",
                        text.len(),
                        cleaned.len()
                    ));
                }
                cleaned
            } else {
                text
            };

            let preview: &str = if text.len() > 200 {
                match text.char_indices().nth(200) {
                    Some((idx, _)) => &text[..idx],
                    None => &text,
                }
            } else {
                &text
            };
            log(&format!(
                "[StopRec] Final transcript ({} chars, mode={}): {:?}",
                text.len(),
                stt_mode,
                preview
            ));

            // Update stats: word count from transcript, duration from audio samples
            let speaking_secs = buf_len_samples as f64 / sample_rate as f64;
            let words = text.split_whitespace().count() as c_int;
            dimmy_update_stats(words, speaking_secs);

            // Telemetry: transcription.completed (success path).
            let processing_ms = transcribe_start.elapsed().as_millis() as u64;
            let mode_static: &'static str = if stt_mode == "local" {
                "local"
            } else {
                "cloud"
            };
            let provider_static: &'static str = if stt_mode == "local" {
                "local_whisper"
            } else {
                crate::telemetry::sanitize::provider_from_url(&api_url)
            };
            let llm_enabled_now = st.llm_enabled.lock().map(|e| *e).unwrap_or(false);
            crate::telemetry::track(crate::telemetry::Event::TranscriptionCompleted {
                mode: mode_static,
                provider: provider_static,
                audio_secs: speaking_secs,
                processing_ms,
                word_count: words.max(0) as u32,
                language: language.clone(),
                success: true,
                had_filler_removal: filler_enabled,
                had_llm: llm_enabled_now,
            });
            TRANSCRIBE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // Auto-save to history
            if !text.trim().is_empty() {
                if let Ok(guard) = st.history_store.lock() {
                    if let Some(ref store) = *guard {
                        let lang = st.language.lock().map(|l| l.clone()).unwrap_or_default();
                        let lang = if lang.is_empty() {
                            "en".to_string()
                        } else {
                            lang
                        };
                        let _ = store.save(&text, &lang, speaking_secs);
                    }
                }
            }

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

            // Telemetry: transcription.failed + Sentry mirror.
            let mode_static: &'static str = if stt_mode == "local" {
                "local"
            } else {
                "cloud"
            };
            let provider_static: &'static str = if stt_mode == "local" {
                "local_whisper"
            } else {
                crate::telemetry::sanitize::provider_from_url(&api_url)
            };
            let category = crate::telemetry::sanitize::error_category(&err_msg, None);
            crate::telemetry::track(crate::telemetry::Event::TranscriptionFailed {
                mode: mode_static,
                provider: provider_static,
                error_category: category,
            });
            crate::telemetry::capture_error(category, &err_msg);

            write_to_buf("", out_buf, buf_len)
        }
    }
}

/// Cancel recording without transcribing.
#[no_mangle]
pub extern "C" fn dimmy_cancel_recording() {
    let st = state();

    // Compute audio_secs BEFORE clearing, for the telemetry event.
    let sample_rate = st.audio_sample_rate.lock().map(|s| *s).unwrap_or(16_000);
    let audio_secs = st
        .audio_buffer
        .lock()
        .map(|b| b.len() as f64 / sample_rate.max(1) as f64)
        .unwrap_or(0.0);

    let _ = st.audio_tx.lock().map(|tx| tx.send(AudioCommand::Stop));
    if let Ok(mut r) = st.recording.lock() {
        *r = false;
    }
    if let Ok(mut b) = st.audio_buffer.lock() {
        b.clear();
    }
    emit_event("recording_cancelled", "{}");

    crate::telemetry::track(crate::telemetry::Event::TranscriptionCancelled { audio_secs });
}

// ── Config ──────────────────────────────────────────────────────────

/// Get full config as JSON string. Returns length written, or -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_get_config_json(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let st = state();
    let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);

    // Build config JSON for native UI consumers
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
        "ggml_debug_logging": *st.ggml_debug_logging.lock().unwrap_or_else(|e| e.into_inner()),
        "use_keyring": use_kr,
        "stt_mode": *st.stt_mode.lock().unwrap_or_else(|e| e.into_inner()),
        "local_model": *st.local_model.lock().unwrap_or_else(|e| e.into_inner()),
        "filler_removal_enabled": *st.filler_removal_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_mode": *st.llm_mode.lock().unwrap_or_else(|e| e.into_inner()),
        "local_llm_model": *st.local_llm_model.lock().unwrap_or_else(|e| e.into_inner()),
        "border_style": *st.border_style.lock().unwrap_or_else(|e| e.into_inner()),
        "waveform_style": *st.waveform_style.lock().unwrap_or_else(|e| e.into_inner()),
        "overlay_position": *st.overlay_position.lock().unwrap_or_else(|e| e.into_inner()),
        "keep_in_clipboard": *st.keep_in_clipboard.lock().unwrap_or_else(|e| e.into_inner()),
        "input_gain": f32::from_bits(st.input_gain.load(std::sync::atomic::Ordering::Relaxed)),
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

    // ─── Snapshot config-trackable fields BEFORE apply ──────────────
    // Used after the apply block to diff and emit `config.*_changed`
    // PostHog events. Only the fields explicitly listed below feed
    // analytics; others (prompt text, custom shortcut string, …) are
    // intentionally NOT tracked because their content can include user
    // language and we want to keep PostHog payloads PII-free.
    let prev_stt_mode: String = st.stt_mode.lock().map(|m| m.clone()).unwrap_or_default();
    let prev_api_url: String = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let prev_llm_api_url: String = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let prev_llm_enabled: bool = st.llm_enabled.lock().map(|e| *e).unwrap_or(false);
    let prev_llm_style: &'static str = st.llm_style.lock().map(|s| s.as_str()).unwrap_or("default");
    let prev_preprocessing: bool = st.preprocessing_enabled.lock().map(|p| *p).unwrap_or(false);
    let prev_input_gain = f32::from_bits(st.input_gain.load(std::sync::atomic::Ordering::Relaxed));

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
            // Telemetry: provider-only, never the key value. Activation
            // signal — distinguishes "configured something" from "left
            // defaults".
            crate::telemetry::track(crate::telemetry::Event::FeatureApiKeySet {
                scope: "stt",
                provider: provider.as_str(),
            });
        }
    }

    // If api_url changed but no new key was provided, reload key from keystore
    if v["api_url"].as_str().is_some() && v["api_key"].as_str().is_none() {
        let url = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
        let provider = Provider::from_url(&url);
        let reloaded =
            crate::load_key_with_store(&st.key_store, KeyringScope::Stt(provider), use_kr);
        if let Ok(mut k) = st.api_key.lock() {
            *k = reloaded;
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
            crate::telemetry::track(crate::telemetry::Event::FeatureApiKeySet {
                scope: "llm",
                provider: provider.as_str(),
            });
        }
    }
    // If llm_api_url changed but no new key was provided, reload key from keystore
    // for the new provider (fixes key loss when switching LLM providers)
    if v["llm_api_url"].as_str().is_some() && v["llm_api_key"].as_str().is_none() {
        let url = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
        let provider = Provider::from_url(&url);
        let reloaded =
            crate::load_key_with_store(&st.key_store, KeyringScope::Llm(provider), use_kr);
        if let Ok(mut k) = st.llm_api_key.lock() {
            *k = reloaded;
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
    if let Some(b) = v["ggml_debug_logging"].as_bool() {
        if let Ok(mut g) = st.ggml_debug_logging.lock() {
            *g = b;
        }
        // Mirror to the lock-free atomic the gpu_diag trampoline reads.
        crate::gpu_diag::set_ggml_debug_enabled(b);
    }
    // Local STT fields
    if let Some(s) = v["stt_mode"].as_str() {
        if let Ok(mut m) = st.stt_mode.lock() {
            *m = s.to_string();
        }
    }
    if let Some(s) = v["local_model"].as_str() {
        if let Ok(mut m) = st.local_model.lock() {
            if *m != s {
                log(&format!(
                    "[LocalSTT] Model changed: {} → {}, clearing cache",
                    *m, s
                ));
                crate::local_stt::clear_model_cache();
            }
            *m = s.to_string();
        }
    }
    if let Some(b) = v["filler_removal_enabled"].as_bool() {
        if let Ok(mut f) = st.filler_removal_enabled.lock() {
            *f = b;
        }
    }
    // Local LLM fields
    if let Some(s) = v["llm_mode"].as_str() {
        if let Ok(mut m) = st.llm_mode.lock() {
            *m = s.to_string();
        }
    }
    if let Some(s) = v["local_llm_model"].as_str() {
        if let Ok(mut m) = st.local_llm_model.lock() {
            if *m != s {
                log(&format!(
                    "[LocalLLM] Model changed: {} → {}, clearing cache",
                    *m, s
                ));
                crate::local_llm::clear_llm_cache();
            }
            *m = s.to_string();
        }
    }
    // UI appearance fields (round-tripped for native frontends)
    if let Some(s) = v["border_style"].as_str() {
        if let Ok(mut bs) = st.border_style.lock() {
            *bs = s.to_string();
        }
    }
    if let Some(s) = v["waveform_style"].as_str() {
        if let Ok(mut ws) = st.waveform_style.lock() {
            *ws = s.to_string();
        }
    }
    if let Some(s) = v["overlay_position"].as_str() {
        if let Ok(mut op) = st.overlay_position.lock() {
            *op = s.to_string();
        }
    }
    if let Some(b) = v["keep_in_clipboard"].as_bool() {
        if let Ok(mut kc) = st.keep_in_clipboard.lock() {
            *kc = b;
        }
    }
    if let Some(g) = v["input_gain"].as_f64() {
        let gain = (g as f32).clamp(0.0, 2.0);
        st.input_gain
            .store(gain.to_bits(), std::sync::atomic::Ordering::Relaxed);
        log(&format!("[Config] input_gain set to {:.2}", gain));
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

    // ─── Diff against pre-apply snapshot and emit telemetry ─────────
    // Best-effort: every track() is fire-and-forget and silently drops
    // when the user has disabled analytics or no API key was compiled
    // in. Provider changes are derived from URL deltas via Provider::
    // from_url so that we report a stable "groq" / "openai" / "anthropic"
    // tag rather than the raw URL (which may include user-custom paths
    // or self-hosted endpoints).
    {
        let new_stt_mode: String = st.stt_mode.lock().map(|m| m.clone()).unwrap_or_default();
        if new_stt_mode != prev_stt_mode {
            crate::telemetry::track(crate::telemetry::Event::ConfigSttModeChanged {
                mode: stt_mode_to_static(&new_stt_mode),
            });
        }

        let new_api_url: String = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
        let prev_provider = Provider::from_url(&prev_api_url);
        let new_provider = Provider::from_url(&new_api_url);
        if prev_provider != new_provider {
            crate::telemetry::track(crate::telemetry::Event::ConfigCloudProviderChanged {
                provider: new_provider.as_str(),
            });
        }

        // LLM provider change tracked under the same event with a
        // marker, since adding a new variant would churn the schema.
        let new_llm_api_url: String = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
        let prev_llm_provider = Provider::from_url(&prev_llm_api_url);
        let new_llm_provider = Provider::from_url(&new_llm_api_url);
        if prev_llm_provider != new_llm_provider {
            crate::telemetry::track(crate::telemetry::Event::ConfigCloudProviderChanged {
                provider: new_llm_provider.as_str(),
            });
        }

        let new_llm_enabled: bool = st.llm_enabled.lock().map(|e| *e).unwrap_or(false);
        if new_llm_enabled != prev_llm_enabled {
            crate::telemetry::track(crate::telemetry::Event::ConfigLlmEnabledChanged {
                enabled: new_llm_enabled,
            });
        }

        let new_llm_style: &'static str =
            st.llm_style.lock().map(|s| s.as_str()).unwrap_or("default");
        if new_llm_style != prev_llm_style {
            crate::telemetry::track(crate::telemetry::Event::ConfigLlmStyleChanged {
                style: new_llm_style.to_string(),
            });
        }

        let new_preprocessing: bool = st.preprocessing_enabled.lock().map(|p| *p).unwrap_or(false);
        if new_preprocessing != prev_preprocessing {
            crate::telemetry::track(crate::telemetry::Event::ConfigPreprocessingChanged {
                enabled: new_preprocessing,
            });
        }

        let new_input_gain =
            f32::from_bits(st.input_gain.load(std::sync::atomic::Ordering::Relaxed));
        // Only emit if delta exceeds a perceptible threshold; the slider
        // ticks in 0.05 increments so 0.001 catches every real movement
        // while filtering Mutex/atomic round-trip noise.
        if (new_input_gain - prev_input_gain).abs() > 0.001 {
            crate::telemetry::track(crate::telemetry::Event::ConfigInputGainChanged {
                gain: new_input_gain,
            });
        }
    }

    // Save to disk
    if let Ok(cfg) = crate::snapshot_config(st) {
        save_config_file(&cfg);
    }

    0
}

/// Map a free-form `stt_mode` string (read from config / FFI) to the
/// stable analytics enum tag. Anything we don't recognise becomes
/// `"unknown"` so the dashboard can flag UI regressions cleanly.
fn stt_mode_to_static(s: &str) -> &'static str {
    match s {
        "local" => "local",
        "cloud" => "cloud",
        _ => "unknown",
    }
}

/// Returns the GPU backend that this binary was compiled with — the
/// *intended* path, not the runtime success/fallback. Used as a
/// stable property for `perf.gpu_status` and `error.gpu_crash` so
/// dashboards segment by build flavour. CUDA/Metal/Vulkan flags are
/// mutually exclusive in practice; the precedence below matches the
/// dominant real-world build targets per platform.
fn compiled_gpu_backend() -> &'static str {
    if cfg!(feature = "local-stt-cuda") || cfg!(feature = "local-llm-cuda") {
        "cuda"
    } else if cfg!(feature = "local-stt-metal") || cfg!(feature = "local-llm-metal") {
        "metal"
    } else if cfg!(feature = "local-stt-vulkan") || cfg!(feature = "local-llm-vulkan") {
        "vulkan"
    } else {
        "cpu"
    }
}

// ── GPU diagnostics ─────────────────────────────────────────────────

/// Read the current GPU known-bad marker. Returns a JSON document via the
/// caller-provided buffer. Fields:
/// - `known_bad` (bool): a recovered crash record exists on disk.
/// - `timestamp` (string|null): ISO-ish timestamp from the saved record.
/// - `context` (string|null): which call aborted (e.g. "whisper_load: …").
/// - `fingerprint_matches` (bool|null): true → driver looks unchanged since
///   the crash → GPU stays disabled. false → driver appears to have changed
///   → next launch will retry GPU. null when `known_bad` is false.
///
/// Writes the JSON body and returns its byte length, or -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_gpu_get_status(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let json = match crate::gpu_health::read_known_bad() {
        None => serde_json::json!({
            "known_bad": false,
            "timestamp": null,
            "context": null,
            "fingerprint_matches": null,
        }),
        Some(record) => {
            let current = crate::gpu_diag::compute_driver_fingerprint();
            let matches = current == record.fingerprint;
            serde_json::json!({
                "known_bad": true,
                "timestamp": record.timestamp,
                "context": record.context,
                "fingerprint_matches": matches,
            })
        }
    };
    let s = serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string());
    write_to_buf(&s, out_buf, buf_len)
}

/// Remove the sticky GPU known-bad marker. After this call, the next process
/// launch will re-probe the GPU instead of forcing CPU. Within the current
/// process the GPU backend status is already cached in a `OnceLock`, so the
/// effect only takes hold after a restart — the UI must surface that.
///
/// Returns 0 on success.
#[no_mangle]
pub extern "C" fn dimmy_gpu_clear_known_bad() -> c_int {
    crate::gpu_health::clear_known_bad();
    log("[GPU] Known-bad marker cleared by user — GPU will be retried on next launch");
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
    // Use fold that skips NaN/Inf samples defensively
    let start = buffer.len().saturating_sub(800);
    let peak = buffer[start..].iter().fold(0.0f32, |max, &s| {
        let abs = s.abs();
        if abs.is_finite() {
            max.max(abs)
        } else {
            max
        }
    });
    // clamp guarantees [0.0, 1.0]; NaN filtered in fold above
    peak.clamp(0.0, 1.0)
}

/// Get device list as JSON array. Caller must NOT free the returned pointer.
/// The string is valid until the next call to this function.
#[no_mangle]
pub extern "C" fn dimmy_list_devices_json(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let devices = crate::audio::list_input_devices();
    let json = serde_json::to_string(&devices).unwrap_or_else(|_| "[]".to_string());
    write_to_buf(&json, out_buf, buf_len)
}

/// Check audio device health. Returns JSON with diagnostic info.
/// Fields: has_devices (bool), device_count (int), selected_available (bool),
/// default_device (string|null), selected_device (string|null),
/// can_open_stream (bool), error (string|null)
#[no_mangle]
pub extern "C" fn dimmy_check_audio_health(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let devices = crate::audio::list_input_devices();
    let device_count = devices.len();
    let has_devices = device_count > 0;

    let default_device = host.default_input_device();
    let default_name = default_device
        .as_ref()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    // Check if selected device exists
    let selected = GLOBAL_STATE
        .get()
        .and_then(|st| st.selected_device.lock().ok().and_then(|d| d.clone()));
    let selected_available = match &selected {
        Some(name) => devices.iter().any(|d| d == name),
        None => has_devices, // no selection = will use default
    };

    // Try to actually open a stream on the target device (quick probe)
    let mut can_open = false;
    let mut error: Option<String> = None;

    let probe_device = if let Some(ref name) = selected {
        host.input_devices()
            .ok()
            .and_then(|mut devs| devs.find(|d| d.name().ok().as_deref() == Some(name.as_str())))
            .or(default_device)
    } else {
        default_device
    };

    if let Some(dev) = probe_device {
        match dev.default_input_config() {
            Ok(config) => {
                // Try building a stream briefly to verify device access
                let result = dev.build_input_stream(
                    &config.into(),
                    |_data: &[f32], _: &cpal::InputCallbackInfo| {},
                    |err| {
                        let _ = err;
                    },
                    None,
                );
                match result {
                    Ok(stream) => {
                        match stream.play() {
                            Ok(()) => {
                                can_open = true;
                                // Drop stream immediately — we just needed to verify
                                drop(stream);
                            }
                            Err(e) => error = Some(format!("Stream play failed: {}", e)),
                        }
                    }
                    Err(e) => error = Some(format!("Cannot open audio stream: {}", e)),
                }
            }
            Err(e) => error = Some(format!("Cannot get device config: {}", e)),
        }
    } else {
        error = Some("No audio input device found".to_string());
    }

    let selected_json = match &selected {
        Some(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        None => "null".to_string(),
    };
    let error_json = match &error {
        Some(e) => format!("\"{}\"", e.replace('"', "\\\"")),
        None => "null".to_string(),
    };

    let json = format!(
        r#"{{"has_devices":{},"device_count":{},"selected_available":{},"default_device":"{}","selected_device":{},"can_open_stream":{},"error":{}}}"#,
        has_devices,
        device_count,
        selected_available,
        default_name.replace('"', "\\\""),
        selected_json,
        can_open,
        error_json
    );

    // Postcondition: JSON must be valid
    assert!(
        serde_json::from_str::<serde_json::Value>(&json).is_ok(),
        "dimmy_check_audio_health: produced invalid JSON: {}",
        json
    );

    log(&format!("[AudioHealth] {}", json));
    write_to_buf(&json, out_buf, buf_len)
}

// ── LLM ─────────────────────────────────────────────────────────────

/// Cycle LLM style. direction: +1 = next, -1 = previous.
/// Invalid direction is silently ignored (logged).
#[no_mangle]
pub extern "C" fn dimmy_cycle_llm_style(direction: c_int) {
    if direction != 1 && direction != -1 {
        log(&format!(
            "ERROR: dimmy_cycle_llm_style called with invalid direction: {}",
            direction
        ));
        return;
    }
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
/// Invalid direction is silently ignored (logged).
#[no_mangle]
pub extern "C" fn dimmy_cycle_llm_tone(direction: c_int) {
    if direction != 1 && direction != -1 {
        log(&format!(
            "ERROR: dimmy_cycle_llm_tone called with invalid direction: {}",
            direction
        ));
        return;
    }
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

/// Process text through LLM enhancement. Reads style/tone/config from global state.
/// Returns length written to buffer, or -1 on error, 0 if LLM disabled/style=Off.
///
/// # Safety
/// `text_ptr` must be a valid null-terminated UTF-8 C string.
/// `out_buf` must point to a buffer of at least `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_process_with_llm(
    text_ptr: *const c_char,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    // Preconditions
    if text_ptr.is_null() {
        log("ERROR: dimmy_process_with_llm called with null text_ptr");
        return -1;
    }
    if out_buf.is_null() || buf_len <= 0 {
        log("ERROR: dimmy_process_with_llm called with null/invalid buffer");
        return -1;
    }

    let text = match CStr::from_ptr(text_ptr).to_str() {
        Ok(s) => s,
        Err(_) => {
            log("ERROR: dimmy_process_with_llm: invalid UTF-8 in text_ptr");
            return -1;
        }
    };

    // Empty text → return empty (not an error)
    if text.is_empty() {
        return write_to_buf("", out_buf, buf_len);
    }

    let st = state();

    // Check if LLM is enabled
    let enabled = st.llm_enabled.lock().map(|e| *e).unwrap_or(false);
    if !enabled {
        return write_to_buf(text, out_buf, buf_len);
    }

    let style = st
        .llm_style
        .lock()
        .map(|s| *s)
        .unwrap_or(crate::llm::LlmStyle::Off);
    if style == crate::llm::LlmStyle::Off {
        return write_to_buf(text, out_buf, buf_len);
    }

    let tone = st
        .llm_tone
        .lock()
        .map(|t| *t)
        .unwrap_or(crate::llm::LlmTone::None);
    let custom_prompt = st
        .llm_custom_prompt
        .lock()
        .map(|p| p.clone())
        .unwrap_or_default();
    let translate_to = st
        .llm_translate_to
        .lock()
        .map(|t| t.clone())
        .unwrap_or_default();

    // ── Local LLM mode: bypass cloud entirely ─────────────────────
    let llm_mode = st
        .llm_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "cloud".to_string());

    if llm_mode == "local" {
        let local_model_filename = st
            .local_llm_model
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|_| crate::local_llm::DEFAULT_LLM_MODEL.to_string());
        let model_path = crate::local_llm::model_path(&local_model_filename);

        emit_event("status", r#"{"state":"processing"}"#);

        match crate::local_llm::process_text_local(
            &model_path,
            text,
            style,
            tone,
            &custom_prompt,
            &translate_to,
        ) {
            Ok(enhanced) => {
                let preview = if enhanced.len() > 120 {
                    format!("{}...", &enhanced[..120])
                } else {
                    enhanced.clone()
                };
                log(&format!(
                    "Local LLM complete: {} chars → {} chars: {:?}",
                    text.len(),
                    enhanced.len(),
                    preview
                ));
                return write_to_buf(&enhanced, out_buf, buf_len);
            }
            Err(e) => {
                let err_msg = format!("{}", e);
                log(&format!("ERROR: Local LLM failed: {}", err_msg));
                emit_event(
                    "error",
                    &format!(
                        r#"{{"message":"Local LLM: {}"}}"#,
                        err_msg.replace('"', "\\\"")
                    ),
                );
                return write_to_buf(text, out_buf, buf_len); // graceful degradation
            }
        }
    }

    // ── Cloud LLM mode ──────────────────────────────────────────
    let use_same_key = st.llm_use_same_key.lock().map(|k| *k).unwrap_or(true);
    let llm_url = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let llm_model = st
        .llm_api_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default();

    let api_key = if use_same_key {
        st.api_key.lock().ok().and_then(|k| k.clone())
    } else {
        st.llm_api_key.lock().ok().and_then(|k| k.clone())
    };

    let api_key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => {
            log("ERROR: dimmy_process_with_llm: no LLM API key available");
            emit_event("error", r#"{"message":"No LLM API key configured"}"#);
            // Return original text on key error (graceful degradation)
            return write_to_buf(text, out_buf, buf_len);
        }
    };

    // Resolve URL: use LLM-specific URL or default
    let api_url = if !llm_url.is_empty() {
        llm_url
    } else {
        crate::DEFAULT_LLM_URL.to_string()
    };

    let api_model = if !llm_model.is_empty() {
        llm_model
    } else {
        crate::DEFAULT_LLM_MODEL.to_string()
    };

    emit_event("status", r#"{"state":"processing"}"#);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log(&format!(
                "ERROR: dimmy_process_with_llm: failed to create runtime: {}",
                e
            ));
            return write_to_buf(text, out_buf, buf_len);
        }
    };

    let llm_start = std::time::Instant::now();
    let result = rt.block_on(crate::llm::process_text(
        &api_url,
        &api_model,
        &api_key,
        text,
        style,
        tone,
        &custom_prompt,
        &translate_to,
    ));
    let llm_processing_ms = llm_start.elapsed().as_millis() as u64;
    let llm_provider_static: &'static str = crate::telemetry::sanitize::provider_from_url(&api_url);

    match result {
        Ok(enhanced) => {
            log(&format!(
                "LLM processing complete: {} chars → {} chars",
                text.len(),
                enhanced.len()
            ));
            crate::telemetry::track(crate::telemetry::Event::LlmApplied {
                mode: "cloud",
                provider: llm_provider_static,
                style: style.as_str().to_string(),
                tone: tone.as_str().to_string(),
                processing_ms: llm_processing_ms,
                success: true,
            });
            write_to_buf(&enhanced, out_buf, buf_len)
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            log(&format!("ERROR: LLM processing failed: {}", err_msg));
            let category = crate::telemetry::sanitize::error_category(&err_msg, None);
            crate::telemetry::track(crate::telemetry::Event::LlmFailed {
                mode: "cloud",
                provider: llm_provider_static,
                error_category: category,
            });
            crate::telemetry::capture_error(category, &err_msg);
            emit_event(
                "error",
                &format!(r#"{{"message":"LLM: {}"}}"#, err_msg.replace('"', "\\\"")),
            );
            // Return original text on LLM failure (graceful degradation)
            write_to_buf(text, out_buf, buf_len)
        }
    }
}

// ── Stats ───────────────────────────────────────────────────────────

/// Update cumulative stats. Returns 0=OK, -1=invalid input.
#[no_mangle]
pub extern "C" fn dimmy_update_stats(words: c_int, speaking_secs: f64) -> c_int {
    // Preconditions: stats must be non-negative and finite
    if words < 0 {
        log(&format!(
            "ERROR: dimmy_update_stats called with negative words: {}",
            words
        ));
        return -1;
    }
    if speaking_secs < 0.0 || !speaking_secs.is_finite() {
        log(&format!(
            "ERROR: dimmy_update_stats called with invalid speaking_secs: {}",
            speaking_secs
        ));
        return -1;
    }

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
    0
}

// ── Utility ─────────────────────────────────────────────────────────

/// Check if an API key is configured. Returns 1=yes, 0=no.
#[no_mangle]
pub extern "C" fn dimmy_has_api_key() -> c_int {
    let st = state();
    st.api_key.lock().map(|k| k.is_some() as c_int).unwrap_or(0)
}

/// Return the version string from Cargo.toml into caller-provided buffer.
/// Returns number of bytes written, or -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_get_version(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    write_to_buf(env!("CARGO_PKG_VERSION"), out_buf, buf_len)
}

/// Check if recording is active. Returns 1=yes, 0=no.
#[no_mangle]
pub extern "C" fn dimmy_is_recording() -> c_int {
    let st = state();
    st.recording.lock().map(|r| *r as c_int).unwrap_or(0)
}

// ── Local model management ──────────────────────────────────────────

/// Get JSON array of available local models with download status.
/// Returns bytes written or -1.
#[no_mangle]
pub extern "C" fn dimmy_list_local_models(buf: *mut c_char, buf_len: c_int) -> c_int {
    let models: Vec<serde_json::Value> = crate::local_stt::AVAILABLE_MODELS
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "filename": m.filename,
                "size_mb": m.size_mb,
                "description": m.description,
                "downloaded": crate::local_stt::model_exists(m.filename),
            })
        })
        .collect();
    let json = serde_json::to_string(&models).unwrap_or_default();
    write_to_buf(&json, buf, buf_len)
}

/// Download a model. BLOCKING — run on background thread from native UI.
/// Emits "model_download_progress" events with {"filename":"...","downloaded":N,"total":N}.
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// `filename_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_download_model(filename_ptr: *const c_char) -> c_int {
    let filename = {
        if filename_ptr.is_null() {
            return -1;
        }
        match CStr::from_ptr(filename_ptr).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return -1,
        }
    };

    let fname_clone = filename.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(crate::local_stt::download_model(
        &filename,
        move |downloaded, total| {
            let payload = format!(
                r#"{{"filename":"{}","downloaded":{},"total":{}}}"#,
                fname_clone, downloaded, total
            );
            emit_event("model_download_progress", &payload);
        },
    ));

    match result {
        Ok(_) => 0,
        Err(e) => {
            let msg: String = format!("{}", e).chars().take(200).collect();
            emit_event("error", &format!(r#"{{"message":"{}"}}"#, msg));
            -1
        }
    }
}

/// Check if a specific model is downloaded. Returns 1 if yes, 0 if no.
///
/// # Safety
/// `filename_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_model_exists(filename_ptr: *const c_char) -> c_int {
    let filename = {
        if filename_ptr.is_null() {
            return 0;
        }
        match CStr::from_ptr(filename_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return 0,
        }
    };
    if crate::local_stt::model_exists(filename) {
        1
    } else {
        0
    }
}

// ── Local LLM model management ─────────────────────────────────────

/// List available local LLM models as JSON array.
#[no_mangle]
pub extern "C" fn dimmy_list_llm_models(buf: *mut c_char, buf_len: c_int) -> c_int {
    if buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let models: Vec<serde_json::Value> = crate::local_llm::AVAILABLE_LLM_MODELS
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "filename": m.filename,
                "size_mb": m.size_mb,
                "description": m.description,
                "downloaded": crate::local_llm::model_exists(m.filename),
            })
        })
        .collect();
    let json = serde_json::to_string(&models).unwrap_or_default();
    write_to_buf(&json, buf, buf_len)
}

/// Download an LLM model. BLOCKING — run on background thread from native UI.
/// Emits "llm_model_download_progress" events.
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// `filename_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_download_llm_model(filename_ptr: *const c_char) -> c_int {
    let filename = {
        if filename_ptr.is_null() {
            return -1;
        }
        match CStr::from_ptr(filename_ptr).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return -1,
        }
    };

    let fname_clone = filename.clone();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(crate::local_llm::download_model(
        &filename,
        move |downloaded, total| {
            let payload = format!(
                r#"{{"filename":"{}","downloaded":{},"total":{}}}"#,
                fname_clone, downloaded, total
            );
            emit_event("llm_model_download_progress", &payload);
        },
    ));

    match result {
        Ok(_) => 0,
        Err(e) => {
            let msg: String = format!("{}", e).chars().take(200).collect();
            emit_event("error", &format!(r#"{{"message":"{}"}}"#, msg));
            -1
        }
    }
}

/// Check if a specific LLM model is downloaded. Returns 1 if yes, 0 if no.
///
/// # Safety
/// `filename_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_llm_model_exists(filename_ptr: *const c_char) -> c_int {
    let filename = {
        if filename_ptr.is_null() {
            return 0;
        }
        match CStr::from_ptr(filename_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return 0,
        }
    };
    if crate::local_llm::model_exists(filename) {
        1
    } else {
        0
    }
}

// ── History ─────────────────────────────────────────────────────────

/// Helper: serialize a slice of Transcripts to a JSON array string.
fn transcripts_to_json(transcripts: &[crate::history::Transcript]) -> String {
    let arr: Vec<serde_json::Value> = transcripts
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "text": t.text,
                "language": t.language,
                "timestamp": t.timestamp,
                "duration": t.duration,
                "word_count": t.word_count,
            })
        })
        .collect();
    serde_json::to_string(&arr).unwrap_or_default()
}

/// Save a transcript to history. Returns transcript ID or -1 on error.
///
/// # Safety
/// `text_ptr` and `language_ptr` must be valid null-terminated UTF-8 C strings
/// (language_ptr may be null, defaults to "en").
#[no_mangle]
pub unsafe extern "C" fn dimmy_history_save(
    text_ptr: *const c_char,
    language_ptr: *const c_char,
    duration: f64,
) -> c_int {
    let text = {
        if text_ptr.is_null() {
            return -1;
        }
        match CStr::from_ptr(text_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        }
    };
    let language = {
        if language_ptr.is_null() {
            "en"
        } else {
            CStr::from_ptr(language_ptr).to_str().unwrap_or("en")
        }
    };

    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            match store.save(text, language, duration) {
                Ok(id) => id as c_int,
                Err(_) => -1,
            }
        } else {
            -1
        }
    } else {
        -1
    }
}

/// Get recent transcripts as JSON array. Returns bytes written or -1.
#[no_mangle]
pub extern "C" fn dimmy_history_recent(limit: c_int, buf: *mut c_char, buf_len: c_int) -> c_int {
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            match store.recent(limit) {
                Ok(transcripts) => {
                    let json = transcripts_to_json(&transcripts);
                    write_to_buf(&json, buf, buf_len)
                }
                Err(_) => -1,
            }
        } else {
            -1
        }
    } else {
        -1
    }
}

/// Search transcripts via FTS5. Returns JSON array or -1.
///
/// # Safety
/// `query_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_history_search(
    query_ptr: *const c_char,
    limit: c_int,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    let query = {
        if query_ptr.is_null() {
            return -1;
        }
        match CStr::from_ptr(query_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        }
    };
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            match store.search(query, limit) {
                Ok(transcripts) => {
                    let json = transcripts_to_json(&transcripts);
                    write_to_buf(&json, buf, buf_len)
                }
                Err(_) => -1,
            }
        } else {
            -1
        }
    } else {
        -1
    }
}

/// Delete a transcript by ID. Returns 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_history_delete(id: c_int) -> c_int {
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            match store.delete(id as i64) {
                Ok(_) => 0,
                Err(_) => -1,
            }
        } else {
            -1
        }
    } else {
        -1
    }
}

// ── Hotkey (low-level keyboard hook) ─────────────────────────────

/// Install the global keyboard hook. Call once at startup.
/// The hook runs on a background thread with its own message pump.
#[no_mangle]
pub extern "C" fn dimmy_hotkey_install() {
    crate::hotkey::install(|msg| crate::log(&format!("[Hotkey] {}", msg)));
}

/// Set the shortcut combo, e.g. "Win+Alt", "Ctrl+Shift+X".
///
/// # Safety
/// `combo_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_hotkey_set(combo_ptr: *const c_char) {
    if combo_ptr.is_null() {
        return;
    }
    if let Ok(combo) = CStr::from_ptr(combo_ptr).to_str() {
        crate::hotkey::set_shortcut(combo);
        crate::log(&format!("[Hotkey] set_shortcut(\"{}\")", combo));
    }
}

/// Poll for hotkey events. Returns:
/// - 0 = no event
/// - 1 = pressed (all keys in combo are down)
/// - 2 = released (any key in combo released after press)
#[no_mangle]
pub extern "C" fn dimmy_hotkey_take_event() -> c_int {
    let ev = crate::hotkey::take_event();
    // Emit telemetry on EVENT_PRESSED (=1). The C# host polls this
    // every ~50ms; a non-zero return at the press edge means the
    // user actually triggered a recording via the global hotkey
    // (vs the in-app button, which calls `dimmy_start_recording`
    // directly and never goes through the hotkey path). Combined
    // with `transcription.completed` totals, dashboards derive the
    // hotkey-vs-button ratio.
    if ev == 1 {
        crate::telemetry::track(crate::telemetry::Event::FeatureHotkeyTriggered);
    }
    ev as c_int
}

/// Start recording mode for shortcut capture.
#[no_mangle]
pub extern "C" fn dimmy_hotkey_start_recording() {
    crate::hotkey::start_recording();
}

/// Poll recording: scans for pressed key combo via GetAsyncKeyState.
/// Call this every ~100ms while recording a new shortcut.
#[no_mangle]
pub extern "C" fn dimmy_hotkey_poll_recording() {
    crate::hotkey::poll_recording_keys();
}

/// Take the recorded shortcut. Returns bytes written to buf, or 0 if not ready.
/// Format: JSON `{"label":"Ctrl+Shift+X","combo":"Ctrl+Shift+X"}`.
#[no_mangle]
pub extern "C" fn dimmy_hotkey_take_recorded(buf: *mut c_char, buf_len: c_int) -> c_int {
    if let Some((label, combo)) = crate::hotkey::take_recorded() {
        let json = serde_json::json!({"label": label, "combo": combo});
        write_to_buf(&json.to_string(), buf, buf_len)
    } else {
        0
    }
}

/// Stop recording mode.
#[no_mangle]
pub extern "C" fn dimmy_hotkey_stop_recording() {
    crate::hotkey::stop_recording();
}

// ── Telemetry ─────────────────────────────────────────────────────────
//
// Five FFI entries for the native UIs to drive the analytics opt-out
// toggle and read the anonymous ID for display in Settings → Privacy.
// Event submission itself is internal to core/ — UIs never construct
// or name events directly. See docs/dev/telemetry-plan.md §4.3.

/// Set the runtime telemetry-enabled flag. Returns 0 on success.
/// `enabled` is treated as a C-style bool: 0 = off, anything else = on.
#[no_mangle]
pub extern "C" fn dimmy_telemetry_set_enabled(enabled: c_int) -> c_int {
    crate::telemetry::set_enabled(enabled != 0);
    0
}

/// Read the current runtime telemetry-enabled flag. 0 = off, 1 = on.
#[no_mangle]
pub extern "C" fn dimmy_telemetry_is_enabled() -> c_int {
    if crate::telemetry::is_enabled() {
        1
    } else {
        0
    }
}

/// Write the anonymous ID into the caller-provided buffer.
/// Returns the number of bytes written, or -1 on error.
/// The ID is a 36-char UUIDv4 + null terminator (37 bytes total).
#[no_mangle]
pub extern "C" fn dimmy_telemetry_anonymous_id(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let id = crate::telemetry::anonymous_id();
    write_to_buf(id, out_buf, buf_len)
}

/// Forget the persisted anonymous ID. The next process launch will
/// generate a fresh one. Returns 0.
#[no_mangle]
pub extern "C" fn dimmy_telemetry_reset_anonymous_id() -> c_int {
    crate::telemetry::reset_anonymous_id();
    0
}

/// Read the build-time status of the telemetry pipeline as JSON.
/// Used by Settings → Privacy to surface "telemetry not configured
/// in this build" when the API key wasn't injected (e.g. local dev
/// builds without secrets).
///
/// Schema: `{"has_compiled_key": bool, "enabled": bool,
///          "has_compiled_dsn": bool, "crash_enabled": bool}`
///
/// Returns the byte length written, or -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_telemetry_status(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let json = serde_json::json!({
        "has_compiled_key": crate::telemetry::has_compiled_key(),
        "enabled": crate::telemetry::is_enabled(),
        "has_compiled_dsn": crate::telemetry::has_compiled_dsn(),
        "crash_enabled": crate::telemetry::is_crash_enabled(),
    });
    let s = serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string());
    write_to_buf(&s, out_buf, buf_len)
}

/// Set the runtime crash-reporting enabled flag (Sentry pipeline).
/// Independent of the analytics toggle. Returns 0.
#[no_mangle]
pub extern "C" fn dimmy_telemetry_set_crash_enabled(enabled: c_int) -> c_int {
    crate::telemetry::set_crash_enabled(enabled != 0);
    0
}

/// Read the runtime crash-reporting enabled flag. 0 = off, 1 = on.
#[no_mangle]
pub extern "C" fn dimmy_telemetry_is_crash_enabled() -> c_int {
    if crate::telemetry::is_crash_enabled() {
        1
    } else {
        0
    }
}

/// Submit user-provided feedback to Sentry.
///
/// `kind_ptr`: `bug`, `feature`, or `general`. Defaults to `general`
///   if null or unknown.
/// `message_ptr`: required, the user's text. Up to 4 KB after
///   sanitisation. Null or empty → no-op (returns 0).
/// `email_ptr`: optional. May be null. Empty/whitespace → not included.
///
/// Returns 0 on success (best-effort: queued for send, no confirmation
/// of delivery), or -1 on a precondition failure (e.g. invalid UTF-8).
///
/// # Safety
/// All non-null string pointers must be valid null-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn dimmy_telemetry_capture_feedback(
    kind_ptr: *const c_char,
    message_ptr: *const c_char,
    email_ptr: *const c_char,
) -> c_int {
    if message_ptr.is_null() {
        return 0; // empty feedback is a no-op, not an error
    }
    let message = match CStr::from_ptr(message_ptr).to_str() {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return 0,
    };
    let kind = if kind_ptr.is_null() {
        "general"
    } else {
        CStr::from_ptr(kind_ptr).to_str().unwrap_or("general")
    };
    let email_owned;
    let email = if email_ptr.is_null() {
        None
    } else {
        match CStr::from_ptr(email_ptr).to_str() {
            Ok(s) if !s.trim().is_empty() => {
                email_owned = s.to_string();
                Some(email_owned.as_str())
            }
            _ => None,
        }
    };
    crate::telemetry::capture_feedback(kind, message, email);
    0
}

// ── Autostart ─────────────────────────────────────────────────────

/// Enable or disable launch-at-login. Returns 0 on success, -1 on
/// any OS-level failure (registry write denied, plist directory
/// missing, exe path unresolvable, …). On success, also emits a
/// `config.autostart_changed` PostHog event so dashboards see the
/// flip rate.
///
/// The C# UI is expected to bind this as a real toggle — on
/// non-zero return, the UI should NOT flip its `IsOn` state and
/// should surface an error, otherwise the user sees "the switch
/// went on but autostart did nothing" and quietly loses trust.
#[no_mangle]
pub extern "C" fn dimmy_autostart_set_enabled(enabled: c_int) -> c_int {
    let want = enabled != 0;
    match crate::autostart::set_enabled(want) {
        Ok(()) => {
            log(&format!("[autostart] set enabled={}", want));
            crate::telemetry::track(crate::telemetry::Event::ConfigAutostartChanged {
                enabled: want,
            });
            0
        }
        Err(e) => {
            log(&format!("[autostart] set failed: {}", e));
            -1
        }
    }
}

/// Read the current autostart state. Returns 1 if the autostart
/// entry is present, 0 if not (or if we couldn't tell — see
/// `crate::autostart::is_enabled` for the swallowed-error rationale).
#[no_mangle]
pub extern "C" fn dimmy_autostart_is_enabled() -> c_int {
    if crate::autostart::is_enabled() {
        1
    } else {
        0
    }
}

/// Get history stats as JSON. Returns bytes written or -1.
#[no_mangle]
pub extern "C" fn dimmy_history_stats(buf: *mut c_char, buf_len: c_int) -> c_int {
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            match store.stats() {
                Ok(stats) => {
                    let json = serde_json::json!({
                        "total_words": stats.total_words,
                        "total_sessions": stats.total_sessions,
                        "total_duration": stats.total_duration,
                    });
                    write_to_buf(&json.to_string(), buf, buf_len)
                }
                Err(_) => -1,
            }
        } else {
            -1
        }
    } else {
        -1
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test-only FFI — compiled ONLY when `--features test-ffi` is set.
// Never reaches release binaries. Used by integration tests in core/tests/.
// ═══════════════════════════════════════════════════════════════════════

/// Inject pre-recorded PCM samples directly into the audio buffer, bypassing
/// cpal entirely. After calling this, `dimmy_stop_recording` will process the
/// injected samples through the exact same pipeline (preprocess → STT → LLM)
/// as a real recording.
///
/// This is the Tier-1 hook for integration testing: deterministic audio in,
/// assertable transcript out, no microphone required.
///
/// # Safety
/// `samples_ptr` must be a valid pointer to `samples_len` contiguous `f32`
/// values. Callers guarantee this lifetime covers the duration of the call.
///
/// Returns 0 on success, -1 on null/empty input, -2 on mutex poisoning.
#[cfg(feature = "test-ffi")]
#[no_mangle]
pub unsafe extern "C" fn dimmy_inject_pcm_for_test(
    samples_ptr: *const f32,
    samples_len: c_int,
    sample_rate: u32,
) -> c_int {
    if samples_ptr.is_null() || samples_len <= 0 {
        return -1;
    }
    assert!(sample_rate > 0, "sample_rate must be positive");

    let slice = std::slice::from_raw_parts(samples_ptr, samples_len as usize);

    // Validate: all samples finite and in [-1.0, 1.0]. Reject bad test input
    // early — catches malformed fixtures before they poison the pipeline.
    for (i, &s) in slice.iter().enumerate() {
        assert!(s.is_finite(), "injected sample {} is not finite: {}", i, s);
        assert!(
            (-1.0..=1.0).contains(&s),
            "injected sample {} out of range: {}",
            i,
            s
        );
    }

    let st = state();

    if let Ok(mut sr) = st.audio_sample_rate.lock() {
        *sr = sample_rate;
    } else {
        return -2;
    }

    if let Ok(mut r) = st.recording.lock() {
        *r = true;
    } else {
        return -2;
    }

    if let Ok(mut b) = st.audio_buffer.lock() {
        b.clear();
        b.extend_from_slice(slice);
    } else {
        return -2;
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_char;

    // ── write_to_buf tests ──────────────────────────────────────────

    #[test]
    fn write_to_buf_normal_string() {
        let mut buf = vec![0u8; 64];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("hello", ptr, 64);
        assert_eq!(result, 5, "should return number of bytes written");
        let written = unsafe { CStr::from_ptr(ptr).to_str().unwrap() };
        assert_eq!(written, "hello");
    }

    #[test]
    fn write_to_buf_null_pointer_returns_neg1() {
        let result = write_to_buf("hello", std::ptr::null_mut(), 64);
        assert_eq!(result, -1, "null buffer must return -1");
    }

    #[test]
    fn write_to_buf_zero_length_returns_neg1() {
        let mut buf = vec![0u8; 64];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("hello", ptr, 0);
        assert_eq!(result, -1, "zero buf_len must return -1");
    }

    #[test]
    fn write_to_buf_negative_length_returns_neg1() {
        let mut buf = vec![0u8; 64];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("hello", ptr, -5);
        assert_eq!(result, -1, "negative buf_len must return -1");
    }

    #[test]
    fn write_to_buf_truncates_long_string() {
        let mut buf = vec![0u8; 4]; // room for 3 chars + null
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("hello", ptr, 4);
        assert_eq!(result, 3, "should truncate to buf_len - 1");
        let written = unsafe { CStr::from_ptr(ptr).to_str().unwrap() };
        assert_eq!(written, "hel");
    }

    #[test]
    fn write_to_buf_empty_string() {
        let mut buf = vec![0xFFu8; 8];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("", ptr, 8);
        assert_eq!(result, 0, "empty string writes 0 bytes");
        assert_eq!(buf[0], 0, "null terminator at position 0");
    }

    #[test]
    fn write_to_buf_exact_fit() {
        // "ab" needs 3 bytes (2 chars + null)
        let mut buf = vec![0u8; 3];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("ab", ptr, 3);
        assert_eq!(result, 2);
        let written = unsafe { CStr::from_ptr(ptr).to_str().unwrap() };
        assert_eq!(written, "ab");
    }

    #[test]
    fn write_to_buf_one_byte_buffer_only_null() {
        let mut buf = vec![0xFFu8; 1];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let result = write_to_buf("hello", ptr, 1);
        assert_eq!(result, 0, "1-byte buffer can only hold null terminator");
        assert_eq!(buf[0], 0);
    }

    // ── emit_event tests ────────────────────────────────────────────

    use std::sync::atomic::{AtomicBool, Ordering};

    static TEST_CB_CALLED: AtomicBool = AtomicBool::new(false);

    // Use UnsafeCell for test callback data to avoid static_mut_refs warning
    use std::cell::UnsafeCell;
    struct TestBuf(UnsafeCell<[u8; 512]>);
    unsafe impl Sync for TestBuf {}
    static TEST_CB_DATA: TestBuf = TestBuf(UnsafeCell::new([0; 512]));

    extern "C" fn test_callback(ptr: *const c_char) {
        TEST_CB_CALLED.store(true, Ordering::SeqCst);
        if !ptr.is_null() {
            let s = unsafe { CStr::from_ptr(ptr) };
            let bytes = s.to_bytes();
            unsafe {
                let buf = &mut *TEST_CB_DATA.0.get();
                let len = bytes.len().min(511);
                buf[..len].copy_from_slice(&bytes[..len]);
                buf[len] = 0;
            }
        }
    }

    #[test]
    fn emit_event_with_no_callback_does_not_panic() {
        // Reset callback to None
        if let Ok(mut guard) = EVENT_CALLBACK.lock() {
            *guard = None;
        }
        // Should not panic
        emit_event("test", "{}");
    }

    #[test]
    fn emit_event_calls_registered_callback() {
        TEST_CB_CALLED.store(false, Ordering::SeqCst);
        unsafe {
            *TEST_CB_DATA.0.get() = [0; 512];
        }

        if let Ok(mut guard) = EVENT_CALLBACK.lock() {
            *guard = Some(test_callback);
        }

        emit_event("recording_started", r#"{"foo":"bar"}"#);

        assert!(
            TEST_CB_CALLED.load(Ordering::SeqCst),
            "callback must be called"
        );
        let received = unsafe { CStr::from_ptr((*TEST_CB_DATA.0.get()).as_ptr() as *const c_char) };
        let json_str = received.to_str().unwrap();
        assert!(json_str.contains(r#""event":"recording_started""#));
        assert!(json_str.contains(r#""payload":{"foo":"bar"}"#));

        // Cleanup
        if let Ok(mut guard) = EVENT_CALLBACK.lock() {
            *guard = None;
        }
    }

    // ── Test helper: minimal AppState for unit tests ────────────────

    use std::sync::Once;

    static INIT_TEST_STATE: Once = Once::new();

    /// Initialize GLOBAL_STATE with a minimal AppState for testing.
    /// Safe to call multiple times — OnceLock + Once ensure single init.
    fn ensure_test_state() {
        INIT_TEST_STATE.call_once(|| {
            let (tx, _rx) = std::sync::mpsc::channel();
            let test_state = AppState {
                recording: Mutex::new(false),
                api_key: Mutex::new(Some("test-key-123".to_string())),
                api_url: Mutex::new(
                    "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                ),
                api_model: Mutex::new("whisper-large-v3-turbo".to_string()),
                language: Mutex::new("en".to_string()),
                prompt: Mutex::new(String::new()),
                shortcut_mode: Mutex::new("toggle".to_string()),
                shortcut: Mutex::new("ctrl+shift".to_string()),
                selected_device: Mutex::new(None),
                audio_sample_rate: Mutex::new(16000),
                transcript: Mutex::new(String::new()),
                audio_buffer: Arc::new(Mutex::new(Vec::new())),
                audio_tx: Mutex::new(tx),
                streaming_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                llm_enabled: Mutex::new(false),
                llm_style: Mutex::new(crate::llm::LlmStyle::Off),
                llm_tone: Mutex::new(crate::llm::LlmTone::None),
                llm_custom_prompt: Mutex::new(String::new()),
                llm_translate_to: Mutex::new(String::new()),
                llm_api_url: Mutex::new(String::new()),
                llm_api_model: Mutex::new(String::new()),
                llm_use_same_key: Mutex::new(true),
                llm_api_key: Mutex::new(None),
                llm_log_enabled: Mutex::new(false),
                chunk_streaming_enabled: Mutex::new(false),
                preprocessing_enabled: Mutex::new(true),
                audio_debug_enabled: Mutex::new(false),
                ggml_debug_logging: Mutex::new(false),
                use_keyring: Mutex::new(false),
                stt_mode: Mutex::new("local".to_string()),
                local_model: Mutex::new("ggml-base-q8_0.bin".to_string()),
                filler_removal_enabled: Mutex::new(true),
                llm_mode: Mutex::new("cloud".to_string()),
                local_llm_model: Mutex::new(crate::local_llm::DEFAULT_LLM_MODEL.to_string()),
                border_style: Mutex::new("Rainbow".to_string()),
                waveform_style: Mutex::new("Bars".to_string()),
                overlay_position: Mutex::new("Bottom Right".to_string()),
                keep_in_clipboard: Mutex::new(false),
                input_gain: Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
                key_store: crate::keystore::KeyStore::new(),
                audio_debug_session_dir: Mutex::new(None),
                window_anchor: Mutex::new(None),
                stats_total_words: Mutex::new(100),
                stats_total_speaking_secs: Mutex::new(60.0),
                history_store: Mutex::new(
                    crate::history::HistoryStore::new(std::path::Path::new(":memory:")).ok(),
                ),
            };
            let _ = GLOBAL_STATE.set(test_state);
        });
    }

    // ── dimmy_has_api_key tests ─────────────────────────────────────

    #[test]
    fn has_api_key_returns_1_when_key_set() {
        ensure_test_state();
        let result = dimmy_has_api_key();
        assert_eq!(result, 1, "test state has api key");
    }

    // ── dimmy_is_recording tests ────────────────────────────────────

    #[test]
    fn is_recording_returns_0_when_not_recording() {
        ensure_test_state();
        // Ensure not recording
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        assert_eq!(dimmy_is_recording(), 0);
    }

    #[test]
    fn is_recording_returns_1_when_recording() {
        ensure_test_state();
        if let Ok(mut r) = state().recording.lock() {
            *r = true;
        }
        let result = dimmy_is_recording();
        // Reset
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        assert_eq!(result, 1);
    }

    // ── dimmy_get_amplitude tests ───────────────────────────────────

    #[test]
    fn get_amplitude_returns_zero_for_empty_buffer() {
        ensure_test_state();
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.clear();
        }
        let amp = dimmy_get_amplitude();
        assert_eq!(amp, 0.0);
    }

    #[test]
    fn get_amplitude_returns_peak_value() {
        ensure_test_state();
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.clear();
            // Put some samples in
            b.extend_from_slice(&[0.1, -0.5, 0.3, 0.7, -0.2]);
        }
        let amp = dimmy_get_amplitude();
        assert!((amp - 0.7).abs() < 0.001, "peak should be 0.7, got {}", amp);
    }

    #[test]
    fn get_amplitude_clamps_to_1() {
        ensure_test_state();
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.clear();
            b.extend_from_slice(&[0.5, 2.0, 0.3]); // 2.0 exceeds range
        }
        let amp = dimmy_get_amplitude();
        assert!(amp <= 1.0, "amplitude must be clamped to 1.0, got {}", amp);
    }

    // ── dimmy_start_recording tests ─────────────────────────────────

    #[test]
    fn start_recording_returns_neg2_if_already_recording() {
        ensure_test_state();
        if let Ok(mut r) = state().recording.lock() {
            *r = true;
        }
        let result = dimmy_start_recording();
        // Reset
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        assert_eq!(result, -2, "already recording should return -2");
    }

    #[test]
    fn start_recording_returns_neg1_if_no_key_cloud_mode() {
        ensure_test_state();
        // Set cloud mode — API key is required
        let old_mode = state().stt_mode.lock().unwrap().clone();
        *state().stt_mode.lock().unwrap() = "cloud".to_string();
        // Temporarily remove key
        let old_key = state().api_key.lock().unwrap().take();
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        let result = dimmy_start_recording();
        // Restore key and mode
        *state().api_key.lock().unwrap() = old_key;
        *state().stt_mode.lock().unwrap() = old_mode;
        assert_eq!(result, -1, "no API key in cloud mode should return -1");
    }

    // ── dimmy_cancel_recording tests ────────────────────────────────

    #[test]
    fn cancel_recording_clears_buffer_and_stops() {
        ensure_test_state();
        // Set up as if recording
        if let Ok(mut r) = state().recording.lock() {
            *r = true;
        }
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.extend_from_slice(&[0.1, 0.2, 0.3]);
        }

        dimmy_cancel_recording();

        let is_rec = state().recording.lock().map(|r| *r).unwrap_or(true);
        let buf_len = state().audio_buffer.lock().map(|b| b.len()).unwrap_or(999);
        assert!(!is_rec, "recording must be false after cancel");
        assert_eq!(buf_len, 0, "buffer must be cleared after cancel");
    }

    // ── dimmy_cycle_llm_style tests ─────────────────────────────────

    #[test]
    fn cycle_llm_style_forward_wraps_around() {
        ensure_test_state();
        // Set to last style
        let styles = crate::llm::LlmStyle::ALL;
        if let Ok(mut s) = state().llm_style.lock() {
            *s = styles[styles.len() - 1];
        }
        dimmy_cycle_llm_style(1);
        let current = *state().llm_style.lock().unwrap();
        assert_eq!(current, styles[0], "should wrap to first style");
    }

    #[test]
    fn cycle_llm_style_backward_wraps_around() {
        ensure_test_state();
        let styles = crate::llm::LlmStyle::ALL;
        if let Ok(mut s) = state().llm_style.lock() {
            *s = styles[0];
        }
        dimmy_cycle_llm_style(-1);
        let current = *state().llm_style.lock().unwrap();
        assert_eq!(
            current,
            styles[styles.len() - 1],
            "should wrap to last style"
        );
    }

    // ── dimmy_cycle_llm_tone tests ──────────────────────────────────

    #[test]
    fn cycle_llm_tone_forward_wraps_around() {
        ensure_test_state();
        let tones = crate::llm::LlmTone::ALL;
        if let Ok(mut t) = state().llm_tone.lock() {
            *t = tones[tones.len() - 1];
        }
        dimmy_cycle_llm_tone(1);
        let current = *state().llm_tone.lock().unwrap();
        assert_eq!(current, tones[0], "should wrap to first tone");
    }

    // ── dimmy_update_stats tests ────────────────────────────────────

    #[test]
    fn update_stats_accumulates() {
        ensure_test_state();
        let words_before = *state().stats_total_words.lock().unwrap();
        let secs_before = *state().stats_total_speaking_secs.lock().unwrap();

        dimmy_update_stats(42, 10.5);

        let words_after = *state().stats_total_words.lock().unwrap();
        let secs_after = *state().stats_total_speaking_secs.lock().unwrap();

        assert_eq!(words_after, words_before + 42);
        assert!((secs_after - (secs_before + 10.5)).abs() < 0.001);
    }

    // ── dimmy_get_config_json tests ─────────────────────────────────

    #[test]
    fn get_config_json_returns_valid_json() {
        ensure_test_state();
        let mut buf = vec![0u8; 8192];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let len = dimmy_get_config_json(ptr, 8192);
        assert!(len > 0, "should return positive length");

        let json_str = unsafe { CStr::from_ptr(ptr).to_str().unwrap() };
        let parsed: serde_json::Value =
            serde_json::from_str(json_str).expect("config JSON must be valid");
        assert!(parsed["has_key"].as_bool().unwrap(), "test state has key");
        assert_eq!(parsed["language"].as_str().unwrap(), "en");
    }

    #[test]
    fn get_config_json_null_buf_returns_neg1() {
        ensure_test_state();
        let result = dimmy_get_config_json(std::ptr::null_mut(), 100);
        assert_eq!(result, -1);
    }

    // ── dimmy_set_config_json tests ─────────────────────────────────

    #[test]
    fn set_config_json_applies_language() {
        ensure_test_state();
        let json = CString::new(r#"{"language":"it"}"#).unwrap();
        let result = unsafe { dimmy_set_config_json(json.as_ptr()) };
        assert_eq!(result, 0, "valid JSON should return 0");

        let lang = state().language.lock().unwrap().clone();
        assert_eq!(lang, "it");

        // Restore
        let restore = CString::new(r#"{"language":"en"}"#).unwrap();
        unsafe { dimmy_set_config_json(restore.as_ptr()) };
    }

    #[test]
    fn set_config_json_null_ptr_returns_neg1() {
        ensure_test_state();
        let result = unsafe { dimmy_set_config_json(std::ptr::null()) };
        assert_eq!(result, -1);
    }

    #[test]
    fn set_config_json_malformed_json_returns_neg1() {
        ensure_test_state();
        let bad = CString::new("not json at all {{{").unwrap();
        let result = unsafe { dimmy_set_config_json(bad.as_ptr()) };
        assert_eq!(result, -1, "malformed JSON must return -1");
    }

    // ── dimmy_list_devices_json tests ───────────────────────────────

    #[test]
    fn list_devices_json_returns_valid_json_array() {
        ensure_test_state();
        let mut buf = vec![0u8; 4096];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let len = dimmy_list_devices_json(ptr, 4096);
        assert!(len >= 2, "should return at least '[]'");

        let json_str = unsafe { CStr::from_ptr(ptr).to_str().unwrap() };
        let parsed: serde_json::Value =
            serde_json::from_str(json_str).expect("device list must be valid JSON");
        assert!(parsed.is_array(), "must be a JSON array");
    }

    // ── dimmy_set_event_callback tests ──────────────────────────────

    #[test]
    fn set_event_callback_replaces_previous() {
        extern "C" fn cb1(_: *const c_char) {}
        extern "C" fn cb2(_: *const c_char) {}

        dimmy_set_event_callback(cb1);
        let first = EVENT_CALLBACK.lock().unwrap().unwrap() as usize;

        dimmy_set_event_callback(cb2);
        let second = EVENT_CALLBACK.lock().unwrap().unwrap() as usize;

        assert_ne!(first, second, "callback should be replaced");

        // Cleanup
        if let Ok(mut guard) = EVENT_CALLBACK.lock() {
            *guard = None;
        }
    }

    // ── Negative space: invalid input returns error codes ──────────

    #[test]
    fn update_stats_rejects_negative_words() {
        ensure_test_state();
        let result = dimmy_update_stats(-1, 5.0);
        assert_eq!(result, -1, "negative words must return -1");
    }

    #[test]
    fn update_stats_rejects_negative_secs() {
        ensure_test_state();
        let result = dimmy_update_stats(0, -1.0);
        assert_eq!(result, -1, "negative secs must return -1");
    }

    #[test]
    fn update_stats_rejects_nan_secs() {
        ensure_test_state();
        let result = dimmy_update_stats(0, f64::NAN);
        assert_eq!(result, -1, "NaN secs must return -1");
    }

    #[test]
    fn update_stats_rejects_inf_secs() {
        ensure_test_state();
        let result = dimmy_update_stats(0, f64::INFINITY);
        assert_eq!(result, -1, "Inf secs must return -1");
    }

    #[test]
    fn cycle_style_ignores_zero_direction() {
        ensure_test_state();
        let before = *state().llm_style.lock().unwrap();
        dimmy_cycle_llm_style(0);
        let after = *state().llm_style.lock().unwrap();
        assert_eq!(
            before, after,
            "style should not change on invalid direction"
        );
    }

    #[test]
    fn cycle_tone_ignores_invalid_direction() {
        ensure_test_state();
        let before = *state().llm_tone.lock().unwrap();
        dimmy_cycle_llm_tone(5);
        let after = *state().llm_tone.lock().unwrap();
        assert_eq!(before, after, "tone should not change on invalid direction");
    }

    #[test]
    fn get_amplitude_handles_nan_in_buffer() {
        ensure_test_state();
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.clear();
            b.extend_from_slice(&[0.1, f32::NAN, 0.3]);
        }
        let amp = dimmy_get_amplitude();
        // NaN should be filtered out, peak = max(0.1, 0.3) = 0.3
        assert!(
            (amp - 0.3).abs() < 0.001,
            "NaN should be filtered, got {}",
            amp
        );
    }

    #[test]
    fn get_amplitude_handles_all_nan_buffer() {
        ensure_test_state();
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.clear();
            b.extend_from_slice(&[f32::NAN, f32::NAN, f32::NAN]);
        }
        let amp = dimmy_get_amplitude();
        assert_eq!(amp, 0.0, "all-NaN buffer should return 0.0");
    }

    #[test]
    fn stop_recording_rejects_null_buffer() {
        ensure_test_state();
        // Make sure not recording so it doesn't try to actually transcribe
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        let result = dimmy_stop_recording(std::ptr::null_mut(), 100);
        assert_eq!(result, -1, "null buffer must return -1");
    }

    #[test]
    fn stop_recording_rejects_zero_length() {
        ensure_test_state();
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        let mut buf = vec![0u8; 64];
        let result = dimmy_stop_recording(buf.as_mut_ptr() as *mut c_char, 0);
        assert_eq!(result, -1, "zero buf_len must return -1");
    }

    // ── dimmy_process_with_llm tests ────────────────────────────────

    #[test]
    fn process_with_llm_rejects_null_text() {
        ensure_test_state();
        let mut buf = vec![0u8; 1024];
        let result = unsafe {
            dimmy_process_with_llm(
                std::ptr::null(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        };
        assert_eq!(result, -1, "null text_ptr must return -1");
    }

    #[test]
    fn process_with_llm_rejects_null_buffer() {
        ensure_test_state();
        let text = CString::new("hello world").unwrap();
        let result = unsafe { dimmy_process_with_llm(text.as_ptr(), std::ptr::null_mut(), 1024) };
        assert_eq!(result, -1, "null out_buf must return -1");
    }

    #[test]
    fn process_with_llm_rejects_zero_buf_len() {
        ensure_test_state();
        let text = CString::new("hello world").unwrap();
        let mut buf = vec![0u8; 64];
        let result =
            unsafe { dimmy_process_with_llm(text.as_ptr(), buf.as_mut_ptr() as *mut c_char, 0) };
        assert_eq!(result, -1, "zero buf_len must return -1");
    }

    #[test]
    fn process_with_llm_empty_text_returns_empty() {
        ensure_test_state();
        let text = CString::new("").unwrap();
        let mut buf = vec![0u8; 1024];
        let result = unsafe {
            dimmy_process_with_llm(
                text.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        };
        assert_eq!(result, 0, "empty text should return 0 length");
    }

    #[test]
    fn process_with_llm_passthrough_when_disabled() {
        ensure_test_state();
        // Ensure LLM is disabled
        if let Ok(mut e) = state().llm_enabled.lock() {
            *e = false;
        }
        if let Ok(mut s) = state().llm_style.lock() {
            *s = crate::llm::LlmStyle::Off;
        }

        let input = "Hello world, this is a test.";
        let text = CString::new(input).unwrap();
        let mut buf = vec![0u8; 1024];
        let result = unsafe {
            dimmy_process_with_llm(
                text.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        };
        assert!(result > 0, "should return positive length");
        let output = unsafe {
            CStr::from_ptr(buf.as_ptr() as *const c_char)
                .to_str()
                .unwrap()
        };
        assert_eq!(output, input, "disabled LLM should pass through unchanged");
    }

    #[test]
    fn process_with_llm_passthrough_when_style_off() {
        ensure_test_state();
        // Enable LLM but set style to Off
        if let Ok(mut e) = state().llm_enabled.lock() {
            *e = true;
        }
        if let Ok(mut s) = state().llm_style.lock() {
            *s = crate::llm::LlmStyle::Off;
        }

        let input = "Test passthrough with style off";
        let text = CString::new(input).unwrap();
        let mut buf = vec![0u8; 1024];
        let result = unsafe {
            dimmy_process_with_llm(
                text.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        };
        assert!(result > 0);
        let output = unsafe {
            CStr::from_ptr(buf.as_ptr() as *const c_char)
                .to_str()
                .unwrap()
        };
        assert_eq!(output, input, "style=Off should pass through unchanged");

        // Cleanup
        if let Ok(mut e) = state().llm_enabled.lock() {
            *e = false;
        }
    }

    #[test]
    fn process_with_llm_graceful_no_key() {
        ensure_test_state();
        // Enable LLM with a real style but remove the key
        if let Ok(mut e) = state().llm_enabled.lock() {
            *e = true;
        }
        if let Ok(mut s) = state().llm_style.lock() {
            *s = crate::llm::LlmStyle::Professional;
        }
        if let Ok(mut k) = state().llm_use_same_key.lock() {
            *k = true;
        }
        let old_key = state().api_key.lock().unwrap().take();

        let input = "Graceful degradation test";
        let text = CString::new(input).unwrap();
        let mut buf = vec![0u8; 1024];
        let result = unsafe {
            dimmy_process_with_llm(
                text.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        };

        // Should return original text (graceful degradation)
        assert!(result > 0, "should still return text on no-key");
        let output = unsafe {
            CStr::from_ptr(buf.as_ptr() as *const c_char)
                .to_str()
                .unwrap()
        };
        assert_eq!(output, input, "no key → return original text");

        // Restore
        *state().api_key.lock().unwrap() = old_key;
        if let Ok(mut e) = state().llm_enabled.lock() {
            *e = false;
        }
        if let Ok(mut s) = state().llm_style.lock() {
            *s = crate::llm::LlmStyle::Off;
        }
    }

    // ── dimmy_check_audio_health tests ────────────────────────────────

    #[test]
    fn check_audio_health_rejects_null_buffer() {
        ensure_test_state();
        let result = dimmy_check_audio_health(std::ptr::null_mut(), 100);
        assert_eq!(result, -1, "null buffer must return -1");
    }

    #[test]
    fn check_audio_health_returns_valid_json() {
        ensure_test_state();
        let mut buf = vec![0u8; 4096];
        let result = dimmy_check_audio_health(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);
        assert!(
            result >= 0,
            "should return non-negative length, got {}",
            result
        );
        if result > 0 {
            let output = unsafe {
                CStr::from_ptr(buf.as_ptr() as *const c_char)
                    .to_str()
                    .unwrap()
            };
            let parsed: serde_json::Value = serde_json::from_str(output)
                .expect("dimmy_check_audio_health must return valid JSON");
            assert!(
                parsed["has_devices"].is_boolean(),
                "JSON must have has_devices boolean"
            );
            assert!(
                parsed["device_count"].is_number(),
                "JSON must have device_count number"
            );
            assert!(
                parsed["can_open_stream"].is_boolean(),
                "JSON must have can_open_stream boolean"
            );
        }
    }

    // ── dimmy_shutdown tests ──────────────────────────────────────────

    #[test]
    fn shutdown_clears_recording_flag() {
        ensure_test_state();
        // Set recording to true, then shutdown should clear it
        if let Ok(mut r) = state().recording.lock() {
            *r = true;
        }
        dimmy_shutdown();
        let recording = state().recording.lock().map(|r| *r).unwrap_or(true);
        assert!(!recording, "recording flag must be false after shutdown");
    }
}

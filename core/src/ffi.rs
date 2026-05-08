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

/// Holds the active realtime chunked transcriber while a recording is
/// in progress with `chunk_streaming_enabled && backend == parakeet`.
/// Taken out by `dimmy_stop_recording` to drain the final cumulative.
static CHUNKED: Mutex<Option<crate::chunked_stt::ChunkedTranscriber>> = Mutex::new(None);

/// Active meeting-mode session, if any. Independent of CHUNKED — the
/// meeting flow runs its own audio capture (started via
/// `dimmy_meeting_start`, NOT via the dictation hotkey).
static MEETING: Mutex<Option<crate::meeting::MeetingSession>> = Mutex::new(None);

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

/// Write 16 kHz mono int16 WAV. Used by the history-audio retention
/// path. Clamps + scales f32 [-1.0, 1.0] to i16 range with int16
/// saturation so peaking samples don't wrap around. Returns the
/// resulting file size on success.
fn write_pcm_as_wav_16k_mono_int16(
    path: &std::path::Path,
    samples_16k: &[f32],
) -> Result<i64, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("hound create {:?}: {}", path, e))?;
    for &s in samples_16k {
        // Clamp before scaling to avoid wraparound when the source
        // had >|1.0| amplitude (rare with AGC but possible without).
        let clamped = s.clamp(-1.0, 1.0);
        let i = (clamped * i16::MAX as f32) as i16;
        writer
            .write_sample(i)
            .map_err(|e| format!("hound write_sample: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("hound finalize: {e}"))?;
    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    Ok(size)
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
    let audio_buffer_secondary = Arc::new(Mutex::new(Vec::<f32>::new()));
    let input_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
        file_cfg.input_gain.to_bits(),
    ));
    let loopback_gain_atomic = Arc::new(std::sync::atomic::AtomicU32::new(
        file_cfg.loopback_gain.to_bits(),
    ));
    let audio_tx = crate::audio::spawn_audio_thread(
        audio_buffer.clone(),
        audio_buffer_secondary.clone(),
        input_gain_atomic.clone(),
        loopback_gain_atomic.clone(),
    );

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
        audio_buffer_secondary,
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
        local_stt_backend: Mutex::new(file_cfg.local_stt_backend),
        live_captions_enabled: Mutex::new(file_cfg.live_captions_enabled),
        save_audio_in_history: Mutex::new(file_cfg.save_audio_in_history),
        history_audio_keep_days: Mutex::new(file_cfg.history_audio_keep_days),
        history_audio_max_mb: Mutex::new(file_cfg.history_audio_max_mb),
        auto_recap_threshold_secs: Mutex::new(file_cfg.auto_recap_threshold_secs),
        filler_removal_enabled: Mutex::new(file_cfg.filler_removal_enabled),
        llm_mode: Mutex::new(file_cfg.llm_mode),
        local_llm_model: Mutex::new(file_cfg.local_llm_model),
        border_style: Mutex::new(file_cfg.border_style),
        waveform_style: Mutex::new(file_cfg.waveform_style),
        overlay_position: Mutex::new(file_cfg.overlay_position),
        keep_in_clipboard: Mutex::new(file_cfg.keep_in_clipboard),
        input_gain: input_gain_atomic,
        loopback_gain: loopback_gain_atomic,
        meeting_chunk_secs: Mutex::new(file_cfg.meeting_chunk_secs),
        audio_source: Mutex::new(file_cfg.audio_source),
        key_store,
        audio_debug_session_dir: Mutex::new(None),
        window_anchor: Mutex::new(None),
        stats_total_words: Mutex::new(file_cfg.stats_total_words),
        stats_total_speaking_secs: Mutex::new(file_cfg.stats_total_speaking_secs),
        app_rules: Mutex::new(file_cfg.app_rules.clone()),
        current_app_context: Mutex::new(crate::app_rules::AppContext::default()),
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

            // Background history-audio retention. Runs once 5 s after
            // init (lets the rest of the app settle) then once per
            // hour. Bounded I/O — only touches files in
            // <config>/history_audio/. Best-effort.
            std::thread::Builder::new()
                .name("history-audio-prune".into())
                .spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    loop {
                        let st = state();
                        let keep_days = st.history_audio_keep_days.lock().map(|n| *n).unwrap_or(30);
                        let max_mb = st.history_audio_max_mb.lock().map(|n| *n).unwrap_or(5_000);
                        if let Some(dir) = crate::history_audio_dir() {
                            match crate::history::prune_audio_dir(&dir, keep_days, max_mb) {
                                Ok((removed, bytes)) => {
                                    if removed > 0 {
                                        log(&format!(
                                            "[HistoryAudio] pruned {} files / {:.1} MB",
                                            removed,
                                            bytes as f64 / 1_048_576.0
                                        ));
                                    }
                                }
                                Err(e) => log(&format!("[HistoryAudio] prune err: {}", e)),
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_secs(3600));
                    }
                })
                .ok();

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

    // Resolve audio source: mic (default) | system (loopback) | mix.
    // Stored as a string in AppConfig so the JSON config round-trips
    // cleanly across UI / disk / FFI; AudioSource enum is the
    // internal representation.
    let source = st
        .audio_source
        .lock()
        .map(|s| crate::audio::AudioSource::from_str_lossy(&s))
        .unwrap_or(crate::audio::AudioSource::Mic);
    let _ = st.audio_tx.lock().map(|tx| {
        tx.send(AudioCommand::Start {
            device_name: selected_device,
            source,
        })
    });

    // Spawn the realtime chunked transcriber when the user has it
    // turned on AND the active local backend is Parakeet. Whisper.cpp
    // is too slow per-chunk to keep up; only Parakeet earns this path.
    let chunked_on = st
        .chunk_streaming_enabled
        .lock()
        .map(|b| *b)
        .unwrap_or(false);
    let backend_parakeet = st
        .local_stt_backend
        .lock()
        .map(|b| b.as_str() == "parakeet")
        .unwrap_or(false);
    if is_local && chunked_on && backend_parakeet {
        // Clear any zombie buffer from a previous run so the worker
        // doesn't transcribe stale audio. The audio thread will
        // refill from the new stream.
        if let Ok(mut b) = st.audio_buffer.lock() {
            b.clear();
        }
        let buffer_arc = st.audio_buffer.clone();
        let on_chunk: Arc<crate::chunked_stt::ChunkCallback> =
            Arc::new(|delta: &str, cumulative: &str, is_final: bool| {
                let payload = serde_json::json!({
                    "delta": delta,
                    "cumulative": cumulative,
                    "is_final": is_final,
                })
                .to_string();
                emit_event("stt_chunk", &payload);
            });
        let transcriber = crate::chunked_stt::ChunkedTranscriber::start(
            buffer_arc, device_sr,
            // 3 s chunks — chunked_smoke A/B 2026-05-06 showed first
            // chunk 8.7 s vs 12.6 s, cadence 3 s vs 5 s, real-time
            // margin 87 % vs 86 %. Interactive caption appears nearly
            // twice as often with no quality regression on jfk×6.
            3.0, 500, // overlap_ms — covers a word that straddles a chunk
            on_chunk,
        );
        if let Ok(mut slot) = CHUNKED.lock() {
            *slot = Some(transcriber);
        }
        log("[StartRec] chunked-stt worker spawned (5s+500ms+dedup)");
    }

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

    // Drain the chunked transcriber BEFORE the audio buffer is cleared.
    // The worker's stop() does one final pass on the trailing audio
    // (everything that arrived after the last 5 s window fired) — if
    // we cleared the buffer first, that final pass would find nothing
    // and the last few seconds of speech would silently disappear from
    // the cumulative transcript. Bug surfaced 2026-05-05 in user
    // testing: "non viene appeso l'ultimo pezzetto".
    let chunked_final = CHUNKED
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .map(|ct| {
            log("[StopRec] draining chunked-stt worker (pre-clear)");
            ct.stop()
        });

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
    let local_stt_backend = st
        .local_stt_backend
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "whisper".to_string());
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

    // Build typed audio pipeline: RawAudio → ProcessedAudio. Clone
    // the buffer so the history-audio retention path further down can
    // re-use the original raw samples (preprocess is destructive: VAD
    // trims silence, AGC scales, downsample to 16k discards data).
    // Saving the post-preprocess version would mean replaying back
    // strips out original mic level + breath cues.
    let raw_samples_for_history: Vec<f32> = buffer.clone();
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
    // The chunked transcriber was already drained above (before the
    // audio buffer was cleared) so we just consume its result here.
    let transcript = if let Some(cumulative) = chunked_final {
        if cumulative.trim().is_empty() {
            Err(crate::error::TranscribeError::Empty)
        } else {
            Ok(cumulative)
        }
    } else if stt_mode == "local" {
        if local_stt_backend == "parakeet" {
            log("[StopRec] Local STT mode — backend: parakeet (batch)");
            crate::transcribe::transcribe_audio_local_parakeet(&processed)
        } else {
            log(&format!(
                "[StopRec] Local STT mode — backend: whisper, model: {}",
                local_model_filename
            ));
            crate::transcribe::transcribe_audio_local(&processed, &language, &local_model_filename)
        }
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

            // Auto-save to history (v2 path: include app context +
            // current LLM style/translation so the History UI can
            // group by app and show what rewrite ran). The enhanced
            // text isn't known at this point — the LLM call happens
            // after `dimmy_stop_recording` returns. Caller (C# /
            // Swift) is expected to follow up with `dimmy_history_
            // update_enhanced(id, text)` once the LLM finishes.
            if !text.trim().is_empty() {
                if let Ok(guard) = st.history_store.lock() {
                    if let Some(ref store) = *guard {
                        let lang = st.language.lock().map(|l| l.clone()).unwrap_or_default();
                        let lang = if lang.is_empty() {
                            "en".to_string()
                        } else {
                            lang
                        };
                        let app_ctx = st
                            .current_app_context
                            .lock()
                            .map(|c| c.clone())
                            .unwrap_or_default();
                        let llm_style_str = st
                            .llm_style
                            .lock()
                            .map(|s| s.as_str().to_string())
                            .unwrap_or_default();
                        let llm_translate = st
                            .llm_translate_to
                            .lock()
                            .map(|t| t.clone())
                            .unwrap_or_default();
                        let meta = crate::history::SaveMetadata {
                            enhanced_text: None,
                            audio_path: None,
                            app_process_name: if app_ctx.process_name.is_empty() {
                                None
                            } else {
                                Some(app_ctx.process_name.as_str())
                            },
                            app_bundle_id: if app_ctx.bundle_id.is_empty() {
                                None
                            } else {
                                Some(app_ctx.bundle_id.as_str())
                            },
                            llm_style: if llm_style_str.is_empty() {
                                None
                            } else {
                                Some(llm_style_str.as_str())
                            },
                            llm_translate_to: if llm_translate.is_empty() {
                                None
                            } else {
                                Some(llm_translate.as_str())
                            },
                            size_bytes: 0,
                        };
                        if let Ok(id) = store.save_v2(&text, &lang, speaking_secs, meta) {
                            // Audio retention is opt-in. When on, save
                            // a 16 kHz mono int16 WAV next to the row
                            // and link it. Keep the WAV write off the
                            // critical path of the user's paste — it's
                            // best-effort and any failure is logged but
                            // doesn't fail the transcription.
                            let save_audio =
                                st.save_audio_in_history.lock().map(|b| *b).unwrap_or(false);
                            if save_audio {
                                if let Some(audio_dir) = crate::history_audio_dir() {
                                    let _ = std::fs::create_dir_all(&audio_dir);
                                    let wav_path = audio_dir.join(format!("{}.wav", id));
                                    let pcm_16k = if sample_rate == 16_000 {
                                        raw_samples_for_history.clone()
                                    } else {
                                        crate::preprocess::downsample_to_16k(
                                            &raw_samples_for_history,
                                            sample_rate,
                                        )
                                    };
                                    match write_pcm_as_wav_16k_mono_int16(&wav_path, &pcm_16k) {
                                        Ok(size) => {
                                            let _ = store.update_audio(
                                                id,
                                                &wav_path.to_string_lossy(),
                                                size,
                                            );
                                            log(&format!(
                                                "[History] saved audio for row #{} to {:?} ({} bytes)",
                                                id, wav_path, size
                                            ));
                                        }
                                        Err(e) => {
                                            log(&format!(
                                                "[History] WAV save failed for row #{}: {}",
                                                id, e
                                            ));
                                        }
                                    }
                                }
                            }
                        }
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
    // If a chunked transcriber was running, signal it and discard
    // its output — `stop()` joins the worker thread cleanly. Without
    // this drop the thread keeps running on a now-empty buffer.
    if let Some(ct) = CHUNKED.lock().ok().and_then(|mut slot| slot.take()) {
        let _ = ct.stop();
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

    let mut json = serde_json::json!({
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
        "local_stt_backend": *st.local_stt_backend.lock().unwrap_or_else(|e| e.into_inner()),
        "live_captions_enabled": *st.live_captions_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "filler_removal_enabled": *st.filler_removal_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "llm_mode": *st.llm_mode.lock().unwrap_or_else(|e| e.into_inner()),
        "local_llm_model": *st.local_llm_model.lock().unwrap_or_else(|e| e.into_inner()),
        "border_style": *st.border_style.lock().unwrap_or_else(|e| e.into_inner()),
        "waveform_style": *st.waveform_style.lock().unwrap_or_else(|e| e.into_inner()),
        "overlay_position": *st.overlay_position.lock().unwrap_or_else(|e| e.into_inner()),
        "keep_in_clipboard": *st.keep_in_clipboard.lock().unwrap_or_else(|e| e.into_inner()),
        "input_gain": f32::from_bits(st.input_gain.load(std::sync::atomic::Ordering::Relaxed)),
        "audio_source": st.audio_source.lock().map(|s| s.clone()).unwrap_or_else(|_| "mic".to_string()),
        "stats_total_words": *st.stats_total_words.lock().unwrap_or_else(|e| e.into_inner()),
        "stats_total_speaking_secs": *st.stats_total_speaking_secs.lock().unwrap_or_else(|e| e.into_inner()),
        // Per-provider key flags — STT
        "has_groq_key": st.key_store.has_key(KeyringScope::Stt(Provider::Groq), use_kr),
        "has_openai_key": st.key_store.has_key(KeyringScope::Stt(Provider::OpenAI), use_kr),
        "has_gemini_key": st.key_store.has_key(KeyringScope::Stt(Provider::Gemini), use_kr),
        "has_deepgram_key": st.key_store.has_key(KeyringScope::Stt(Provider::Deepgram), use_kr),
        "has_fireworks_key": st.key_store.has_key(KeyringScope::Stt(Provider::Fireworks), use_kr),
        "has_together_key": st.key_store.has_key(KeyringScope::Stt(Provider::Together), use_kr),
        "has_custom_key": st.key_store.has_key(KeyringScope::Stt(Provider::Custom), use_kr),
        // Per-LLM-provider key flags. Mirror of the STT block above so
        // the UI can refresh the green-check on a provider dropdown
        // change without first persisting config (which would force a
        // round-trip + redraw with stale state).
        "has_groq_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::Groq), use_kr),
        "has_openai_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::OpenAI), use_kr),
        "has_anthropic_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::Anthropic), use_kr),
        "has_gemini_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::Gemini), use_kr),
        "has_openrouter_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::OpenRouter), use_kr),
        "has_fireworks_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::Fireworks), use_kr),
        "has_together_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::Together), use_kr),
        "has_custom_llm_key": st.key_store.has_key(KeyringScope::Llm(Provider::Custom), use_kr),
    });

    // app_rules added outside the json! macro — including it inline pushes
    // the macro past its expansion-recursion limit (the json! macro is
    // recursive and we've accreted ~50 fields). Mutate the Value directly.
    if let Ok(rules) = st.app_rules.lock() {
        if let Ok(v) = serde_json::to_value(&*rules) {
            json["app_rules"] = v;
        }
    }

    // v2 retention + auto-recap fields. Same workaround as app_rules
    // above — the json! macro is already past its expansion budget,
    // so these are mutated onto the Value after the fact. Without
    // this block the Mac UI's Privacy → "Save audio" toggle and the
    // Advanced → auto-recap stepper round-trip to disk fine but
    // appear reverted on the next read because the getter omits them.
    if let Ok(b) = st.save_audio_in_history.lock() {
        json["save_audio_in_history"] = serde_json::Value::Bool(*b);
    }
    if let Ok(n) = st.history_audio_keep_days.lock() {
        json["history_audio_keep_days"] = serde_json::Value::from(*n);
    }
    if let Ok(n) = st.history_audio_max_mb.lock() {
        json["history_audio_max_mb"] = serde_json::Value::from(*n);
    }
    if let Ok(n) = st.auto_recap_threshold_secs.lock() {
        json["auto_recap_threshold_secs"] = serde_json::Value::from(*n);
    }

    // Per-provider LLM key flags — same recursion-limit reason as app_rules
    // above. These drive the green ✓ badge in Settings when the user picks
    // a provider in the dropdown, before they hit Save. Without them the
    // C# layer can't tell whether switching to Anthropic would surface a
    // stored key or require typing a fresh one.
    json["has_llm_groq_key"] = st
        .key_store
        .has_key(KeyringScope::Llm(Provider::Groq), use_kr)
        .into();
    json["has_llm_openai_key"] = st
        .key_store
        .has_key(KeyringScope::Llm(Provider::OpenAI), use_kr)
        .into();
    json["has_llm_anthropic_key"] = st
        .key_store
        .has_key(KeyringScope::Llm(Provider::Anthropic), use_kr)
        .into();
    json["has_llm_gemini_key"] = st
        .key_store
        .has_key(KeyringScope::Llm(Provider::Gemini), use_kr)
        .into();
    json["has_llm_openrouter_key"] = st
        .key_store
        .has_key(KeyringScope::Llm(Provider::OpenRouter), use_kr)
        .into();
    json["has_llm_custom_key"] = st
        .key_store
        .has_key(KeyringScope::Llm(Provider::Custom), use_kr)
        .into();

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
    if let Some(s) = v["local_stt_backend"].as_str() {
        if let Ok(mut m) = st.local_stt_backend.lock() {
            if *m != s {
                log(&format!("[LocalSTT] Backend changed: {} → {}", *m, s));
            }
            *m = s.to_string();
        }
    }
    if let Some(b) = v["live_captions_enabled"].as_bool() {
        if let Ok(mut f) = st.live_captions_enabled.lock() {
            *f = b;
        }
    }
    if let Some(b) = v["save_audio_in_history"].as_bool() {
        if let Ok(mut f) = st.save_audio_in_history.lock() {
            *f = b;
        }
    }
    if let Some(n) = v["history_audio_keep_days"].as_u64() {
        if let Ok(mut f) = st.history_audio_keep_days.lock() {
            *f = n as u32;
        }
    }
    if let Some(n) = v["history_audio_max_mb"].as_u64() {
        if let Ok(mut f) = st.history_audio_max_mb.lock() {
            *f = n as u32;
        }
    }
    if let Some(n) = v["auto_recap_threshold_secs"].as_u64() {
        if let Ok(mut f) = st.auto_recap_threshold_secs.lock() {
            *f = n as u32;
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
    if let Some(g) = v["loopback_gain"].as_f64() {
        let gain = (g as f32).clamp(0.5, 4.0);
        st.loopback_gain
            .store(gain.to_bits(), std::sync::atomic::Ordering::Relaxed);
        log(&format!("[Config] loopback_gain set to {:.2}", gain));
    }
    if let Some(s) = v["meeting_chunk_secs"].as_f64() {
        let secs = (s as f32).clamp(5.0, 60.0);
        if let Ok(mut slot) = st.meeting_chunk_secs.lock() {
            *slot = secs;
        }
        log(&format!("[Config] meeting_chunk_secs set to {:.1}", secs));
    }
    if let Some(s) = v["audio_source"].as_str() {
        let normalised = match s.to_ascii_lowercase().as_str() {
            "system" | "mix" | "mic" => s.to_ascii_lowercase(),
            _ => "mic".to_string(),
        };
        if let Ok(mut slot) = st.audio_source.lock() {
            *slot = normalised.clone();
        }
        log(&format!("[Config] audio_source set to {}", normalised));
    }
    if !v["app_rules"].is_null() {
        if let Ok(rules) =
            serde_json::from_value::<Vec<crate::app_rules::AppRule>>(v["app_rules"].clone())
        {
            if let Ok(mut slot) = st.app_rules.lock() {
                let count = rules.len();
                *slot = rules;
                log(&format!("[Config] app_rules set ({} rules)", count));
            }
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

/// Peak amplitude of the SECONDARY audio buffer (the loopback / system
/// audio stream populated in Mix mode). Returns 0.0 when no Mix
/// recording is active or the buffer hasn't been fed yet. Used by the
/// meeting-window dual-band waveform to draw mic + system as separate
/// bands so the user can see both streams at a glance.
#[no_mangle]
pub extern "C" fn dimmy_get_loopback_amplitude() -> c_float {
    let st = state();
    let buffer = match st.audio_buffer_secondary.lock() {
        Ok(b) => b,
        Err(_) => return 0.0,
    };
    if buffer.is_empty() {
        return 0.0;
    }
    let start = buffer.len().saturating_sub(800);
    let peak = buffer[start..].iter().fold(0.0f32, |max, &s| {
        let abs = s.abs();
        if abs.is_finite() {
            max.max(abs)
        } else {
            max
        }
    });
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

    let global_enabled = st.llm_enabled.lock().map(|e| *e).unwrap_or(false);

    let mut style = st
        .llm_style
        .lock()
        .map(|s| *s)
        .unwrap_or(crate::llm::LlmStyle::Off);

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
    let mut translate_to = st
        .llm_translate_to
        .lock()
        .map(|t| t.clone())
        .unwrap_or_default();

    // Apply app-rule overrides if the foreground app captured at hotkey-
    // down matches one of the user's configured rules. First-match wins.
    // An empty override leaves style/translate as the user's defaults.
    //
    // Important: rule resolution runs BEFORE the global llm_enabled gate
    // so a per-app rule can FORCE enhance for a specific app even when
    // the user has the global LLM toggle off (their default mode is
    // "no enhancement", but Notepad++ specifically gets Acronyms style).
    let mut rule_forced_enhance = false;
    {
        let rules = st.app_rules.lock().map(|r| r.clone()).unwrap_or_default();
        let ctx = st
            .current_app_context
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let ovr = crate::app_rules::resolve(&rules, &ctx);
        // Always log the resolution attempt so debugging "why didn't
        // my rule fire" requires only reading dimmy.log. We intentionally
        // log the rule count + the captured context so an empty rules
        // list is distinguishable from a no-match.
        log(&format!(
            "[AppRules] resolve ctx(process='{}', bundle='{}', wm='{}') against {} rule(s) → matched_idx={:?}",
            ctx.process_name,
            ctx.bundle_id,
            ctx.wm_class,
            rules.len(),
            ovr.matched_rule_index
        ));
        if let Some(s) = ovr.llm_style.as_deref() {
            let new_style = crate::llm::LlmStyle::from_str_lossy(s);
            log(&format!(
                "[AppRules] match #{} ctx={:?} style {:?} → {:?}",
                ovr.matched_rule_index.unwrap_or(usize::MAX),
                ctx.process_name,
                style,
                new_style
            ));
            style = new_style;
            // Rule fired with a non-off style → enhance even if the
            // global llm_enabled is false. The per-app rule is a
            // user-explicit override.
            if new_style != crate::llm::LlmStyle::Off {
                rule_forced_enhance = true;
            }
        }
        if let Some(t) = ovr.llm_translate_to {
            log(&format!(
                "[AppRules] match #{} translate_to '{}' → '{}'",
                ovr.matched_rule_index.unwrap_or(usize::MAX),
                translate_to,
                t
            ));
            translate_to = t;
        }
    }

    // Final gate: enhance only if either the global toggle is on, or
    // an app rule forced a non-off style for this specific app. If
    // style is still Off (no rule fired and global is off, OR rule
    // explicitly set style=off), pass the raw transcript through.
    if !global_enabled && !rule_forced_enhance {
        log("[LLM] global disabled and no rule forced enhance — pass-through");
        return write_to_buf(text, out_buf, buf_len);
    }
    if style == crate::llm::LlmStyle::Off {
        return write_to_buf(text, out_buf, buf_len);
    }

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

// ── Meeting mode ─────────────────────────────────────────────────

/// Start a long-form meeting session. Spins up a fresh audio capture
/// (independent of the dictation hotkey path) and a MeetingSession
/// worker that streams the recorded WAV to disk and transcribes
/// chunks every 15 s. Returns 0 on success and writes the meeting id
/// to `out_buf`. Returns -1 if a session is already active, -2 on
/// audio-capture start failure, -3 on filesystem error.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_meeting_start(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    {
        let guard = match MEETING.lock() {
            Ok(g) => g,
            Err(_) => return -1,
        };
        if guard.is_some() {
            return -1; // already active
        }
    }

    let st = state();
    let selected_device = st.selected_device.lock().ok().and_then(|d| d.clone());
    let mt_source = st
        .audio_source
        .lock()
        .map(|s| crate::audio::AudioSource::from_str_lossy(&s))
        .unwrap_or(crate::audio::AudioSource::Mic);
    // CRITICAL: when source = System, the primary capture stream is the
    // OUTPUT device in WASAPI loopback mode (~48 kHz on most systems),
    // NOT the mic. Querying the mic's rate here and feeding it to the
    // meeting writer makes audio.wav play back at 1/3 speed ("metallic,
    // very slow") because the WAV header advertises 16 kHz but the
    // shared buffer was actually filling at 48 kHz.
    let device_sr = crate::audio::primary_sample_rate(&selected_device, &mt_source);
    log(&format!(
        "[Meeting] primary_sample_rate={} source={:?}",
        device_sr, mt_source
    ));
    if let Ok(mut sr) = st.audio_sample_rate.lock() {
        *sr = device_sr;
    }
    // Clear any stale buffers from a previous recording so the meeting
    // worker starts at offset 0 on both primary and secondary streams.
    if let Ok(mut b) = st.audio_buffer.lock() {
        b.clear();
    }
    if let Ok(mut b) = st.audio_buffer_secondary.lock() {
        b.clear();
    }
    let _ = st.audio_tx.lock().map(|tx| {
        tx.send(crate::audio::AudioCommand::Start {
            device_name: selected_device,
            source: mt_source,
        })
    });

    // Snapshot the user's STT configuration so the meeting worker uses
    // the SAME backend (cloud or local) the dictation pipeline does.
    // Avoids the previous trap where meeting hardcoded a backend and
    // silently produced empty transcripts when the user's setup didn't
    // match (cloud user → meeting tried local; missing local model file
    // → meeting tried whisper anyway → "model not found" loop).
    let stt_snapshot = crate::meeting::SttSnapshot {
        mode: st
            .stt_mode
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_else(|| "local".to_string()),
        api_url: st
            .api_url
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default(),
        api_model: st
            .api_model
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default(),
        api_key: st.api_key.lock().ok().and_then(|k| k.clone()),
        prompt: st.prompt.lock().ok().map(|s| s.clone()).unwrap_or_default(),
        local_model: st
            .local_model
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default(),
        language: st
            .language
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_else(|| "auto".to_string()),
        chunk_secs: st.meeting_chunk_secs.lock().ok().map(|s| *s),
    };
    // Loopback device runs at its OWN native rate which may differ from
    // the mic (typical: BT mic 16k HFP + speakers 48k A2DP). meeting.rs
    // writes audio_system.wav with this rate so playback is correct
    // regardless of the mic/system mismatch.
    let system_sr = if matches!(mt_source, crate::audio::AudioSource::Mix) {
        crate::audio::secondary_sample_rate()
    } else {
        device_sr
    };
    log(&format!(
        "[Meeting] mic_sr={} system_sr={} source={:?}",
        device_sr, system_sr, mt_source
    ));
    let session = match crate::meeting::MeetingSession::start(
        st.audio_buffer.clone(),
        st.audio_buffer_secondary.clone(),
        device_sr,
        system_sr,
        mt_source,
        stt_snapshot,
    ) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("[Meeting] start failed: {}", e));
            // Best-effort: stop the audio we just started so the next
            // Start doesn't error on "already running".
            let _ = st
                .audio_tx
                .lock()
                .map(|tx| tx.send(crate::audio::AudioCommand::Stop));
            return -3;
        }
    };
    let id = session.id().to_string();
    if let Ok(mut g) = MEETING.lock() {
        *g = Some(session);
    }
    log(&format!("[Meeting] active id={}", id));
    write_to_buf(&id, out_buf, buf_len)
}

/// Stop the active meeting and return a JSON bundle with the id, dir,
/// transcript, duration, chunk_count, error. Caller is then expected
/// to optionally run dimmy_process_with_llm on the transcript and
/// call dimmy_meeting_save_post_process to persist the recap +
/// actions. Returns the byte length of the JSON, or -1 if no
/// meeting is active.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_meeting_stop(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let session = {
        let mut guard = match MEETING.lock() {
            Ok(g) => g,
            Err(_) => return -1,
        };
        match guard.take() {
            Some(s) => s,
            None => return -1,
        }
    };

    // Stop audio capture in parallel with the worker drain — the
    // worker's stop() blocks for up to one chunk's transcribe time
    // (a few hundred ms), so we issue the cpal Stop first.
    let st = state();
    let _ = st
        .audio_tx
        .lock()
        .map(|tx| tx.send(crate::audio::AudioCommand::Stop));
    if let Ok(mut r) = st.recording.lock() {
        *r = false;
    }

    let result = session.stop();
    let json = serde_json::json!({
        "id": result.id,
        "dir": result.dir.to_string_lossy(),
        "transcript": result.transcript,
        "duration_secs": result.duration_secs,
        "chunk_count": result.chunk_count,
        "error": result.error,
    })
    .to_string();
    write_to_buf(&json, out_buf, buf_len)
}

/// Persist the post-process LLM artefacts (recap, actions JSON,
/// optional translation) into the meeting directory. Each pointer
/// can be null/empty — empty fields are skipped, not written as
/// blank files. Returns 0 on success, -1 on any error.
///
/// # Safety
/// All non-null `*const c_char` pointers must be valid null-
/// terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn dimmy_meeting_save_post_process(
    dir_ptr: *const c_char,
    recap_ptr: *const c_char,
    actions_ptr: *const c_char,
    translated_ptr: *const c_char,
) -> c_int {
    if dir_ptr.is_null() {
        return -1;
    }
    let dir_str = match CStr::from_ptr(dir_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let dir = std::path::Path::new(dir_str);
    let recap = if recap_ptr.is_null() {
        ""
    } else {
        CStr::from_ptr(recap_ptr).to_str().unwrap_or("")
    };
    let actions = if actions_ptr.is_null() {
        ""
    } else {
        CStr::from_ptr(actions_ptr).to_str().unwrap_or("")
    };
    let translated = if translated_ptr.is_null() {
        None
    } else {
        match CStr::from_ptr(translated_ptr).to_str() {
            Ok(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    };
    match crate::meeting::save_post_process(dir, recap, actions, translated) {
        Ok(_) => 0,
        Err(e) => {
            log(&format!("[Meeting] save_post_process: {}", e));
            -1
        }
    }
}

/// Return JSON array of meeting sessions left with a `.recording`
/// marker (i.e. crashed before clean stop). UI surfaces this as a
/// "recover meeting?" prompt at startup. Returns the byte length of
/// the JSON, or -1 on buffer-too-small.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_meeting_list_orphans(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let arr = crate::meeting::list_orphans();
    let json = serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string());
    write_to_buf(&json, out_buf, buf_len)
}

/// Returns 1 if a meeting recording is currently active (between
/// dimmy_meeting_start and _stop), 0 otherwise. Used by the C#/Swift
/// host to gate the dictation hotkey: starting a parallel recording
/// while a meeting is in flight would corrupt both sessions because
/// they share the cpal audio buffer.
#[no_mangle]
pub extern "C" fn dimmy_meeting_is_active() -> c_int {
    MEETING.lock().map(|g| g.is_some() as c_int).unwrap_or(0)
}

/// Raw LLM call: send `prompt` to the configured LLM endpoint without
/// the dictation rewrite wrapper. Used by meeting-mode post-process
/// (recap + actions extraction) and any other caller that owns its
/// own prompt template.
///
/// Provider is auto-selected from the configured LLM API URL — same
/// routing the dictation enhance path uses. `model_override` lets the
/// caller request a stronger model than the user's dictation default
/// (e.g. claude-opus-4-7 or gemini-2.5-pro for recap quality);
/// pass an empty string to use the configured `llm_api_model`.
///
/// Returns the response byte length on success. Negative on error:
/// - -1 invalid args (null pointers, empty prompt)
/// - -2 no LLM API key / URL configured
/// - -3 HTTP / parsing error (truncated reason in dimmy.log)
///
/// # Safety
/// `prompt_ptr` and `model_override_ptr` (when non-null) must be
/// valid null-terminated UTF-8 C strings. `out_buf` must be a valid
/// writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_llm_call_raw(
    prompt_ptr: *const c_char,
    model_override_ptr: *const c_char,
    max_tokens: i32,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if prompt_ptr.is_null() || out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let prompt = match CStr::from_ptr(prompt_ptr).to_str() {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => return -1,
    };
    let model_override = if model_override_ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(model_override_ptr)
            .to_str()
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    let st = state();
    let api_url = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let api_model = st
        .llm_api_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default();
    let api_key = st
        .llm_api_key
        .lock()
        .ok()
        .and_then(|k| k.clone())
        .unwrap_or_default();

    if api_url.is_empty() || api_key.is_empty() {
        log("[LlmRaw] missing api_url or api_key — configure an LLM in Settings");
        return -2;
    }
    let model = if model_override.is_empty() {
        api_model
    } else {
        model_override
    };
    if model.is_empty() {
        log("[LlmRaw] no model — neither api_model nor override provided");
        return -2;
    }
    let max_tokens_u = if max_tokens <= 0 {
        4096
    } else {
        max_tokens as u64
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            log(&format!("[LlmRaw] tokio runtime: {}", e));
            return -3;
        }
    };
    let result = runtime.block_on(async {
        crate::llm::process_raw_prompt(&api_url, &model, &api_key, &prompt, max_tokens_u).await
    });
    match result {
        Ok(text) => {
            log(&format!(
                "[LlmRaw] ok — {} chars in (model={})",
                text.len(),
                model
            ));
            write_to_buf(&text, out_buf, buf_len)
        }
        Err(e) => {
            let msg = format!("{}", e);
            let mut truncated = msg;
            truncated.truncate(200);
            log(&format!("[LlmRaw] failed: {}", truncated));
            -3
        }
    }
}

/// Build flavor — "" (prod) or "staging". Embedded at compile time via
/// the `DIMMY_BUILD_FLAVOR` env var (build.rs). Native UIs read this on
/// launch and surface a "STAGING" watermark so a side-by-side tester
/// always knows which flavor they're looking at. Returns bytes written,
/// or -1 on null buffer.
#[no_mangle]
pub extern "C" fn dimmy_build_flavor(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    write_to_buf(crate::build_flavor(), out_buf, buf_len)
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

// ── Parakeet TDT v3 FP32 (local STT, alternative to whisper.cpp) ────
//
// Three exports: presence check, blocking download with progress
// events, and direct PCM → text. Mirrors the whisper.cpp shape so the
// UI can treat them as parallel local backends. All gated behind the
// `local-stt-parakeet` cargo feature; without it, transcribe returns
// a clear error and the others return false / no-op.

/// Returns 1 if the Parakeet model bundle is on disk and complete,
/// 0 otherwise. Used by Settings to decide whether to show a download
/// CTA or let the user pick Parakeet as the active local backend.
#[no_mangle]
pub extern "C" fn dimmy_parakeet_bundle_present() -> c_int {
    if crate::parakeet::active_bundle_present() {
        1
    } else {
        0
    }
}

/// Download the Parakeet TDT v3 FP32 bundle (~2.5 GB) into the
/// dimmy config dir. BLOCKING — call from a background thread.
/// Emits `parakeet_bundle_download_progress` events as
/// `{"downloaded":N,"total":N}`. Returns 0 on success, -1 on error;
/// on -1 also emits an `error` event with a short message.
#[no_mangle]
pub extern "C" fn dimmy_parakeet_download_bundle() -> c_int {
    let result = crate::parakeet::download_active_bundle(|downloaded, total| {
        let payload = format!(r#"{{"downloaded":{},"total":{}}}"#, downloaded, total);
        emit_event("parakeet_bundle_download_progress", &payload);
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            let msg: String = format!("{}", e).chars().take(200).collect();
            emit_event("error", &format!(r#"{{"message":"{}"}}"#, msg));
            -1
        }
    }
}

/// Pre-load the Parakeet sessions + run a tiny dummy inference so the
/// user's first real recording doesn't pay the ~6 s cold path. BLOCKING —
/// call from a background thread. Returns 0 on success, -1 on error
/// (most commonly "bundle not present" — caller should guard).
#[no_mangle]
pub extern "C" fn dimmy_parakeet_warmup() -> c_int {
    match crate::parakeet::warmup() {
        Ok(()) => 0,
        Err(e) => {
            log(&format!("[Parakeet warmup] {}", e));
            -1
        }
    }
}

/// Transcribe a 16 kHz mono f32 PCM buffer with Parakeet. Writes the
/// UTF-8 result into `buf` (null-terminated, truncated if buf_len is
/// too small). Returns the number of bytes written (excluding the
/// terminator), or -1 on error. The model is loaded lazily on the
/// first call and stays cached for the process lifetime.
///
/// # Safety
/// `pcm_ptr` must point to `pcm_len` valid `f32` samples (or be null
/// when pcm_len == 0). `buf` must be writable for `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_parakeet_transcribe(
    pcm_ptr: *const c_float,
    pcm_len: c_int,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if buf.is_null() || buf_len <= 0 {
        return -1;
    }
    if pcm_len < 0 {
        return -1;
    }
    let pcm: &[f32] = if pcm_len == 0 {
        &[]
    } else {
        if pcm_ptr.is_null() {
            return -1;
        }
        std::slice::from_raw_parts(pcm_ptr, pcm_len as usize)
    };

    match crate::parakeet::transcribe(pcm) {
        Ok(text) => write_to_buf(&text, buf, buf_len),
        Err(e) => {
            let msg: String = format!("{}", e).chars().take(200).collect();
            emit_event("error", &format!(r#"{{"message":"{}"}}"#, msg));
            -1
        }
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
                // v2 fields — null when the row predates the migration
                // (older transcripts) or the field wasn't applicable
                // (e.g. no LLM run, no audio retention).
                "enhanced_text": t.enhanced_text,
                "audio_path": t.audio_path,
                "app_process_name": t.app_process_name,
                "app_bundle_id": t.app_bundle_id,
                "llm_style": t.llm_style,
                "llm_translate_to": t.llm_translate_to,
                "size_bytes": t.size_bytes,
                "word_timestamps": t.word_timestamps,
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

/// Update the enhanced_text column for a row. Async follow-up after
/// the LLM rewrite finishes. Returns 0 on success, -1 on any error.
///
/// # Safety
/// `text_ptr` must be a valid null-terminated UTF-8 C string (or null
/// to clear the field).
#[no_mangle]
pub unsafe extern "C" fn dimmy_history_update_enhanced(
    id: c_int,
    text_ptr: *const c_char,
) -> c_int {
    let text = if text_ptr.is_null() {
        ""
    } else {
        match CStr::from_ptr(text_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        }
    };
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            return match store.update_enhanced(id as i64, text) {
                Ok(_) => 0,
                Err(_) => -1,
            };
        }
    }
    -1
}

/// Set the word_timestamps JSON column for a row. Caller serialises
/// the `[{"word":...,"start_ms":...,"end_ms":...}, ...]` array.
/// Empty/null clears the field. Returns 0 on success, -1 on error.
///
/// # Safety
/// `json_ptr` must be a valid null-terminated UTF-8 C string (or null).
#[no_mangle]
pub unsafe extern "C" fn dimmy_history_update_word_timestamps(
    id: c_int,
    json_ptr: *const c_char,
) -> c_int {
    let json = if json_ptr.is_null() {
        ""
    } else {
        match CStr::from_ptr(json_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        }
    };
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            return match store.update_word_timestamps(id as i64, json) {
                Ok(_) => 0,
                Err(_) => -1,
            };
        }
    }
    -1
}

/// Update the audio_path + size_bytes columns for a row. Called by
/// the audio retention layer once a recording's PCM is on disk.
///
/// # Safety
/// `path_ptr` must be a valid null-terminated UTF-8 C string (or null
/// to unlink).
#[no_mangle]
pub unsafe extern "C" fn dimmy_history_update_audio(
    id: c_int,
    path_ptr: *const c_char,
    size_bytes: i64,
) -> c_int {
    let path = if path_ptr.is_null() {
        ""
    } else {
        match CStr::from_ptr(path_ptr).to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        }
    };
    let st = state();
    if let Ok(guard) = st.history_store.lock() {
        if let Some(ref store) = *guard {
            return match store.update_audio(id as i64, path, size_bytes) {
                Ok(_) => 0,
                Err(_) => -1,
            };
        }
    }
    -1
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

/// Snapshot the foreground app at hotkey-down. Called by the platform layer
/// (C# / Swift / GTK) before recording begins; the LLM post-process step
/// reads this snapshot when applying `app_rules`. Pass empty/null strings
/// for fields that don't apply on the current OS.
///
/// JSON payload format:
///   `{"process_name": "slack.exe", "bundle_id": "", "wm_class": ""}`
///
/// Returns 0 on success, non-zero on parse error.
///
/// # Safety
/// `json_ptr` must be a valid null-terminated UTF-8 C string, or null.
#[no_mangle]
pub unsafe extern "C" fn dimmy_set_app_context(json_ptr: *const c_char) -> c_int {
    if json_ptr.is_null() {
        return 1;
    }
    let json_str = match CStr::from_ptr(json_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let v: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return 3,
    };
    let ctx = crate::app_rules::AppContext {
        process_name: v["process_name"].as_str().unwrap_or("").to_string(),
        bundle_id: v["bundle_id"].as_str().unwrap_or("").to_string(),
        wm_class: v["wm_class"].as_str().unwrap_or("").to_string(),
    };
    let st = match GLOBAL_STATE.get() {
        Some(s) => s,
        None => return 4,
    };
    if let Ok(mut slot) = st.current_app_context.lock() {
        *slot = ctx;
    }
    0
}

/// Clear the foreground-app snapshot. Called after transcription completes
/// so a stale snapshot can't bleed into the next recording.
///
/// # Safety
/// Safe to call from any thread once `dimmy_init` has run.
#[no_mangle]
pub unsafe extern "C" fn dimmy_clear_app_context() {
    if let Some(st) = GLOBAL_STATE.get() {
        if let Ok(mut slot) = st.current_app_context.lock() {
            *slot = crate::app_rules::AppContext::default();
        }
    }
}

/// Synchronously transcribe a WAV file using the active local STT
/// backend (whisper.cpp or Parakeet, per `local_stt_backend`). Cloud
/// transcription via this entry point is unimplemented for now —
/// callers that need cloud should use the recording flow.
///
/// `path_ptr` must point to a UTF-8 file path (any 16/24/32-bit int
/// or 32-bit float WAV). The file is decoded in-process via hound,
/// downmixed to mono, run through the standard preprocess pipeline
/// (highpass + VAD + AGC + downsample to 16 k), and routed to the
/// configured local backend. The resulting transcript is also
/// written to the history database with audio_path linking back to
/// the source file (so the user can replay it from the History UI).
///
/// Returns the transcript length on success (bytes written, excluding
/// the null terminator), or one of:
/// - -1 invalid args (null pointer / bad UTF-8 / null buffer)
/// - -2 file open / decode failure
/// - -3 VAD removed all audio (input was effectively silent)
/// - -4 cloud mode requested — not supported here
/// - -5 backend transcribe failed
///
/// # Safety
/// `path_ptr` must be a valid null-terminated UTF-8 C string and
/// `out_buf` a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_transcribe_file(
    path_ptr: *const c_char,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if path_ptr.is_null() || out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let path = match CStr::from_ptr(path_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    log(&format!("[FileLoad] decoding '{}'", path));

    // ── Decode WAV → mono f32 at the file's native sample rate ──
    let mut reader = match hound::WavReader::open(path) {
        Ok(r) => r,
        Err(e) => {
            log(&format!("[FileLoad] open failed: {}", e));
            return -2;
        }
    };
    let spec = reader.spec();
    if spec.sample_rate == 0 || spec.channels == 0 {
        log("[FileLoad] invalid WAV header (sample_rate or channels = 0)");
        return -2;
    }
    let raw_samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample as i32;
            if bits <= 0 {
                return -2;
            }
            let scale = (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / scale)
                .collect()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
    };
    if raw_samples.is_empty() {
        log("[FileLoad] WAV decoded to zero samples");
        return -2;
    }
    let mono: Vec<f32> = if spec.channels == 1 {
        raw_samples
    } else {
        let ch = spec.channels as usize;
        raw_samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    log(&format!(
        "[FileLoad] decoded: {} samples @ {} Hz mono ({:.1}s)",
        mono.len(),
        spec.sample_rate,
        mono.len() as f64 / spec.sample_rate as f64,
    ));

    // ── Preprocess (same pipeline the recording path uses) ──────
    let raw_samples_for_history = mono.clone();
    let raw = crate::audio::RawAudio {
        samples: mono,
        sample_rate: spec.sample_rate,
    };
    let processed = raw.preprocess(true);
    if processed.samples.is_empty() {
        log("[FileLoad] preprocess produced 0 samples (silent input?)");
        return -3;
    }

    // ── Route per active backend (local or cloud) ───────────────
    let st = state();
    let stt_mode = st
        .stt_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "local".to_string());
    let total_secs_pre = raw_samples_for_history.len() as f64 / spec.sample_rate as f64;

    // ── Cloud branch: hand off to transcribe_chunked ──────────────
    // Cloud STT routing reuses the same chunking machinery the live
    // dictation path uses. We block on a one-shot tokio runtime
    // because the FFI surface is sync and Velopack-installed apps
    // don't have an ambient runtime running. Empty cloud config
    // (missing key / URL) is treated as a configuration error so the
    // user gets actionable feedback in the Settings status row.
    if stt_mode != "local" {
        let api_url = st.api_url.lock().map(|u| u.clone()).unwrap_or_default();
        let api_key_opt = st.api_key.lock().ok().and_then(|g| g.clone());
        let api_key = api_key_opt.unwrap_or_default();
        let model = st.api_model.lock().map(|m| m.clone()).unwrap_or_default();
        let language_cloud = st.language.lock().map(|l| l.clone()).unwrap_or_default();
        let language_cloud = if language_cloud.is_empty() {
            "en".to_string()
        } else {
            language_cloud
        };
        if api_url.is_empty() || api_key.is_empty() || model.is_empty() {
            log("[FileLoad] cloud config incomplete (url/key/model)");
            return -6;
        }
        let provider = crate::provider::Provider::from_url(&api_url);
        let max_wav_bytes = provider.max_file_bytes();
        emit_event(
            "file_transcribe_progress",
            &serde_json::json!({
                "processed_secs": 0.0,
                "total_secs": total_secs_pre,
                "percent": 0.0,
            })
            .to_string(),
        );
        let total_secs_for_progress = total_secs_pre;
        let on_progress: Box<dyn Fn(usize, usize) + Send + Sync> =
            Box::new(move |idx: usize, total: usize| {
                let frac = idx as f64 / total.max(1) as f64;
                emit_event(
                    "file_transcribe_progress",
                    &serde_json::json!({
                        "processed_secs": frac * total_secs_for_progress,
                        "total_secs": total_secs_for_progress,
                        "percent": frac * 100.0,
                        "chunk_index": idx,
                        "chunk_total": total,
                    })
                    .to_string(),
                );
            });
        let rt = match tokio::runtime::Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                log(&format!("[FileLoad] tokio runtime: {}", e));
                return -7;
            }
        };
        let result = rt.block_on(crate::transcribe::transcribe_chunked(
            &api_url,
            &model,
            &api_key,
            processed,
            &language_cloud,
            "",
            max_wav_bytes,
            Some(on_progress.as_ref()),
        ));
        match result {
            Ok(text) => {
                if text.trim().is_empty() {
                    log("[FileLoad] cloud returned empty transcript");
                    return -5;
                }
                if let Ok(guard) = st.history_store.lock() {
                    if let Some(ref store) = *guard {
                        let _ = store.save(&text, &language_cloud, total_secs_pre);
                    }
                }
                return write_to_buf(&text, out_buf, buf_len);
            }
            Err(e) => {
                log(&format!("[FileLoad] cloud transcribe failed: {}", e));
                return -8;
            }
        }
    }

    let backend = st
        .local_stt_backend
        .lock()
        .map(|b| b.clone())
        .unwrap_or_else(|_| "whisper".to_string());
    let language = st.language.lock().map(|l| l.clone()).unwrap_or_default();
    let language = if language.is_empty() {
        "en".to_string()
    } else {
        language
    };
    let model = st
        .local_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "ggml-base-q8_0.bin".to_string());
    // Emit a starting event so the UI can flip its progress bar
    // from indeterminate to 0 % the moment we begin work.
    let total_secs = raw_samples_for_history.len() as f64 / spec.sample_rate as f64;
    emit_event(
        "file_transcribe_progress",
        &serde_json::json!({
            "processed_secs": 0.0,
            "total_secs": total_secs,
            "percent": 0.0,
        })
        .to_string(),
    );

    // Chunk the preprocessed audio so we can emit per-chunk progress
    // events. 30-second windows give a smooth percent indicator
    // without too many model wakeups; falling back to a single chunk
    // on short files (≤ 35 s) keeps the existing fast path.
    const CHUNK_MAX_SECS: usize = 30;
    let max_chunk_samples = CHUNK_MAX_SECS * processed.sample_rate as usize;
    let chunks = processed.split_at_silence(max_chunk_samples);
    let n_chunks = chunks.len();
    log(&format!(
        "[FileLoad] backend={} model={} → {} chunk(s)",
        backend, model, n_chunks
    ));

    let mut transcript_acc = String::new();
    // Accumulated word timestamps across all chunks, with each chunk's
    // local times offset by the chunk's start position in the file.
    // Only populated for the parakeet backend — whisper word-timestamp
    // extraction is a follow-up. Vec of `{"word","start","end"}` in JSON
    // value form so we don't have to round-trip strings.
    let mut word_ts_acc: Vec<serde_json::Value> = Vec::new();
    let chunk_secs = CHUNK_MAX_SECS as f64;
    for (idx, chunk) in chunks.into_iter().enumerate() {
        let chunk_offset_secs = (idx as f64) * chunk_secs;
        let result: Result<(String, Option<String>), _> = if backend == "parakeet" {
            crate::transcribe::transcribe_audio_local_parakeet_with_word_ts(&chunk)
                .map(|(t, j)| (t, Some(j)))
        } else {
            crate::transcribe::transcribe_audio_local(&chunk, &language, &model).map(|t| (t, None))
        };
        match result {
            Ok((text, ts_opt)) => {
                let delta = crate::chunked_stt::dedup_last_3_words(&transcript_acc, &text);
                if !delta.is_empty() {
                    if !transcript_acc.is_empty() && !transcript_acc.ends_with(' ') {
                        transcript_acc.push(' ');
                    }
                    transcript_acc.push_str(&delta);
                }
                if let Some(ts_json) = ts_opt {
                    if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(&ts_json) {
                        for mut entry in arr {
                            // Shift start/end into absolute file time.
                            if let Some(obj) = entry.as_object_mut() {
                                if let Some(s) = obj.get_mut("start") {
                                    if let Some(v) = s.as_f64() {
                                        *s = serde_json::json!(v + chunk_offset_secs);
                                    }
                                }
                                if let Some(e) = obj.get_mut("end") {
                                    if let Some(v) = e.as_f64() {
                                        *e = serde_json::json!(v + chunk_offset_secs);
                                    }
                                }
                            }
                            word_ts_acc.push(entry);
                        }
                    }
                }
            }
            Err(e) => {
                log(&format!(
                    "[FileLoad] chunk {} of {} failed: {}",
                    idx + 1,
                    n_chunks,
                    e
                ));
                // Continue — one bad chunk shouldn't kill the whole
                // file. Empty chunks are normal (long silence).
            }
        }
        // Best-effort progress: assume even-sized chunks. The user
        // sees a smoothly advancing bar even if the last chunk is
        // shorter than the rest.
        let processed_secs = ((idx + 1) as f64 * chunk_secs).min(total_secs);
        let percent = (processed_secs / total_secs.max(0.001) * 100.0).min(100.0);
        emit_event(
            "file_transcribe_progress",
            &serde_json::json!({
                "processed_secs": processed_secs,
                "total_secs": total_secs,
                "percent": percent,
                "chunk_index": idx + 1,
                "chunk_total": n_chunks,
            })
            .to_string(),
        );
    }

    if transcript_acc.trim().is_empty() {
        log("[FileLoad] all chunks produced empty transcripts");
        return -5;
    }
    let text = transcript_acc;

    // Auto-save to history (v1 schema on this branch). When the
    // history-v2 branch lands the merge will switch this to save_v2
    // with audio_path = source-file path (no copy) so the user can
    // jump from a History row to the original file.
    if !text.trim().is_empty() {
        if let Ok(guard) = st.history_store.lock() {
            if let Some(ref store) = *guard {
                let duration = raw_samples_for_history.len() as f64 / spec.sample_rate as f64;
                let saved_id = store.save(&text, &language, duration).ok();
                // Attach word timestamps when the parakeet path produced
                // them. Whisper backend leaves word_ts_acc empty → no-op.
                if let (Some(id), false) = (saved_id, word_ts_acc.is_empty()) {
                    let json = serde_json::Value::Array(word_ts_acc).to_string();
                    let _ = store.update_word_timestamps(id, &json);
                }
            }
        }
    }

    write_to_buf(&text, out_buf, buf_len)
}

/// Poll for hotkey events. Returns:
/// - 0 = no event
/// - 1 = pressed (all keys in combo are down)
/// - 2 = released (any key in combo released after price)
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

// ── Licensing ───────────────────────────────────────────────────────
//
// Native UI surface over license::*. The module is always compiled; with
// `license-client` off, status calls return `Unrestricted` and HTTP calls
// still work (server returns tokens we just can't verify) — UI should
// branch on the embedded pubkey emptiness if it cares about the difference.

use crate::license::{self, LicenseStatus, Tier};
use crate::telemetry::{events::Event, track};

/// Bucket a reqwest::Error into a small set of categorical strings so we
/// can emit telemetry without leaking the URL or full error chain. The
/// raw `reqwest::Error` Display impl can include the request URL which
/// in turn contains the activation code — that's PII. This helper keeps
/// the cardinality bounded.
fn license_error_category(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "network"
    } else if let Some(s) = e.status() {
        if s.is_client_error() {
            "server_4xx"
        } else if s.is_server_error() {
            "server_5xx"
        } else {
            "server_other"
        }
    } else {
        "unknown"
    }
}

fn tier_str_from_token() -> &'static str {
    // After save, the disk file holds the new token. Decode the tier
    // off the persisted state so the event tier matches what was written
    // (rather than what the caller passed in, which they didn't here).
    match license::check_status() {
        LicenseStatus::Active { tier, .. } => tier_str(tier),
        LicenseStatus::TrialActive { .. } => "trial",
        _ => "unknown",
    }
}

/// Server URL for /api/* calls. Mutable at runtime so the UI can point
/// at a dev server without recompiling. Defaults to the PoC localhost.
static LICENSING_SERVER_URL: OnceLock<Mutex<String>> = OnceLock::new();

fn licensing_server_url() -> String {
    LICENSING_SERVER_URL
        .get_or_init(|| Mutex::new(license::DEFAULT_SERVER_URL.to_string()))
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| license::DEFAULT_SERVER_URL.to_string())
}

fn write_license_err(buf: *mut c_char, buf_len: c_int, msg: &str) -> c_int {
    let json = serde_json::json!({"ok": false, "error": msg}).to_string();
    write_to_buf(&json, buf, buf_len)
}

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::Trial => "trial",
        Tier::Monthly => "monthly",
        Tier::Annual => "annual",
        Tier::Lifetime => "lifetime",
    }
}

#[derive(serde::Serialize)]
struct LicenseStatusWire {
    kind: &'static str,
    tier: Option<&'static str>,
    days_remaining: Option<i64>,
    days_offline: Option<u32>,
    error: Option<String>,
    cloud_enabled: bool,
    updates_enabled: bool,
    /// Active scope list — drives the per-feature ✅/❌ grid in UI.
    /// Empty for non-active states; full vocabulary for Unrestricted.
    scopes: Vec<String>,
    /// Unix epoch seconds when an Active subscription with cancel-at-
    /// period-end will lapse. UI renders "Cancels on YYYY-MM-DD" subtitle.
    /// `null` for everything except Active state with cancel scheduled.
    cancels_at: Option<i64>,
}

impl From<LicenseStatus> for LicenseStatusWire {
    fn from(s: LicenseStatus) -> Self {
        let cloud_enabled = s.cloud_enabled();
        let updates_enabled = s.updates_enabled();
        let scopes = s.scopes();
        let mut cancels_at: Option<i64> = None;
        let (kind, tier, days_remaining, days_offline, error) = match s {
            LicenseStatus::Unrestricted => ("Unrestricted", None, None, None, None),
            LicenseStatus::NotFound => ("NotFound", None, None, None, None),
            LicenseStatus::Invalid(e) => ("Invalid", None, None, None, Some(e)),
            LicenseStatus::TrialActive { days_remaining, .. } => (
                "TrialActive",
                Some("trial"),
                Some(days_remaining as i64),
                None,
                None,
            ),
            LicenseStatus::TrialExpired => ("TrialExpired", Some("trial"), None, None, None),
            LicenseStatus::Active {
                tier,
                days_remaining,
                cancels_at: ca,
                ..
            } => {
                cancels_at = ca;
                (
                    "Active",
                    Some(tier_str(tier)),
                    Some(days_remaining),
                    None,
                    None,
                )
            }
            LicenseStatus::Expired => ("Expired", None, None, None, None),
            LicenseStatus::Suspended { tier, days_offline } => (
                "Suspended",
                Some(tier_str(tier)),
                None,
                Some(days_offline),
                None,
            ),
        };
        Self {
            kind,
            tier,
            days_remaining,
            days_offline,
            error,
            cloud_enabled,
            updates_enabled,
            scopes,
            cancels_at,
        }
    }
}

/// Override the licensing server URL at runtime. **Debug-only** — gated
/// behind `cfg(debug_assertions)` so release binaries simply do not
/// export the symbol. Used only by scripted local tests that point a
/// debug build at an alternative endpoint (e.g. a `wrangler dev` mock
/// running on a different port). Release builds embed the URL via
/// `DIMMY_LICENSE_SERVER_URL` at compile time and refuse to be
/// re-pointed at runtime — the previous "Settings → Advanced → server
/// URL" UI is gone for the same reason: any production user able to
/// flip endpoint is one click from talking to a server we don't
/// control with a token they didn't earn.
/// Returns 0 on success, -1 on null/empty input, -2 on mutex poisoning.
///
/// # Safety
/// `url_ptr` must be a valid null-terminated UTF-8 C string.
#[cfg(debug_assertions)]
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_set_server_url(url_ptr: *const c_char) -> c_int {
    if url_ptr.is_null() {
        return -1;
    }
    let url = unsafe { CStr::from_ptr(url_ptr) }
        .to_string_lossy()
        .into_owned();
    if url.trim().is_empty() {
        return -1;
    }
    let cell =
        LICENSING_SERVER_URL.get_or_init(|| Mutex::new(license::DEFAULT_SERVER_URL.to_string()));
    match cell.lock() {
        Ok(mut g) => {
            *g = url;
            0
        }
        Err(_) => -2,
    }
}

/// Return the current license status as a JSON object. Schema:
///
/// ```json
/// {
///   "kind": "Unrestricted|NotFound|Invalid|TrialActive|TrialExpired|Active|Expired|Suspended",
///   "tier": "trial|monthly|annual|lifetime" | null,
///   "days_remaining": number | null,
///   "days_offline": number | null,
///   "error": string | null,
///   "cloud_enabled": bool,
///   "updates_enabled": bool
/// }
/// ```
///
/// Returns the number of bytes written to `buf` (excluding null), or -1 on bad args.
#[no_mangle]
pub extern "C" fn dimmy_license_status_json(buf: *mut c_char, buf_len: c_int) -> c_int {
    let status = license::check_status();
    let wire: LicenseStatusWire = status.into();
    let json = serde_json::to_string(&wire)
        .unwrap_or_else(|_| r#"{"kind":"Invalid","error":"serialize"}"#.to_string());
    write_to_buf(&json, buf, buf_len)
}

/// `POST /api/trial/start` via FFI. Writes JSON `{ok, magic_link?, error?}` to buf.
/// Sync-blocking on a fresh tokio runtime.
///
/// # Safety
/// `email_ptr` must be a valid null-terminated UTF-8 C string. `buf` must
/// point to at least `buf_len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_request_trial(
    email_ptr: *const c_char,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if email_ptr.is_null() {
        return write_license_err(buf, buf_len, "email required");
    }
    let email = unsafe { CStr::from_ptr(email_ptr) }
        .to_string_lossy()
        .into_owned();
    if email.trim().is_empty() {
        return write_license_err(buf, buf_len, "email required");
    }
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let result = rt.block_on(license::request_trial(&server, &email));
    let json = match result {
        Ok(r) => serde_json::json!({"ok": true, "magic_link": r.magic_link}),
        Err(e) => serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `GET /api/activate?code=…&device_label=…` via FFI. On success, persists the
/// returned token to `~/.config/dimmy/license.json` and stamps last_online_check.
/// Writes JSON `{ok, error?}` to buf.
///
/// # Safety
/// `code_ptr` and `label_ptr` must be valid null-terminated UTF-8 C strings
/// (`label_ptr` may be null — falls back to a generic device label). `buf`
/// must point to at least `buf_len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_redeem(
    code_ptr: *const c_char,
    label_ptr: *const c_char,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if code_ptr.is_null() {
        return write_license_err(buf, buf_len, "code required");
    }
    let code = unsafe { CStr::from_ptr(code_ptr) }
        .to_string_lossy()
        .into_owned();
    if code.trim().is_empty() {
        return write_license_err(buf, buf_len, "code required");
    }
    let label = if label_ptr.is_null() {
        "Unknown device".to_string()
    } else {
        let s = unsafe { CStr::from_ptr(label_ptr) }
            .to_string_lossy()
            .into_owned();
        if s.trim().is_empty() {
            "Unknown device".to_string()
        } else {
            s
        }
    };
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let result = rt.block_on(license::redeem_activation_code(&server, &code, &label));
    let json = match result {
        Ok(r) => match license::save_license_file(&r.token) {
            Ok(()) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let _ = license::set_last_online_check(now);
                track(Event::LicenseActivated {
                    tier: tier_str_from_token(),
                });
                serde_json::json!({"ok": true})
            }
            Err(e) => {
                track(Event::LicenseActivationFailed {
                    error_category: "disk",
                });
                serde_json::json!({"ok": false, "error": format!("save: {}", e)})
            }
        },
        Err(e) => {
            let cat = license_error_category(&e);
            track(Event::LicenseActivationFailed {
                error_category: cat,
            });
            serde_json::json!({"ok": false, "error": format!("{}", e)})
        }
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `POST /api/refresh` via FFI. Reads the current token from disk, posts it,
/// writes back the rotated one. Writes JSON `{ok, error?}` to buf.
#[no_mangle]
pub extern "C" fn dimmy_license_refresh(buf: *mut c_char, buf_len: c_int) -> c_int {
    let token = match license::load_license_file() {
        Ok(Some(t)) => t,
        Ok(None) => return write_license_err(buf, buf_len, "no license file"),
        Err(e) => return write_license_err(buf, buf_len, &format!("load: {}", e)),
    };
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let result = rt.block_on(license::refresh_token(&server, &token));
    let json = match result {
        Ok(r) => match license::save_license_file(&r.token) {
            Ok(()) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let _ = license::set_last_online_check(now);
                track(Event::LicenseRefreshed {
                    tier: tier_str_from_token(),
                });
                serde_json::json!({"ok": true})
            }
            Err(e) => {
                track(Event::LicenseRefreshFailed {
                    error_category: "disk",
                });
                serde_json::json!({"ok": false, "error": format!("save: {}", e)})
            }
        },
        Err(e) => {
            track(Event::LicenseRefreshFailed {
                error_category: license_error_category(&e),
            });
            serde_json::json!({"ok": false, "error": format!("{}", e)})
        }
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `POST /api/checkout/create { tier, token? }` via FFI. Token is read
/// from disk if present (carries email_hash for trial→paid linkage,
/// otherwise anonymous purchase from the NotFound state). Writes JSON
/// `{ok, url?, error?}` to buf — caller opens `url` in the system browser.
///
/// # Safety
/// `tier_ptr` must be a valid null-terminated UTF-8 C string.
/// `buf` must point to at least `buf_len` bytes of writable memory.
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_checkout_url(
    tier_ptr: *const c_char,
    email_ptr: *const c_char,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if tier_ptr.is_null() {
        return write_license_err(buf, buf_len, "tier required");
    }
    let tier = unsafe { CStr::from_ptr(tier_ptr) }
        .to_string_lossy()
        .into_owned();
    if !matches!(tier.as_str(), "monthly" | "annual" | "lifetime") {
        return write_license_err(buf, buf_len, "tier must be monthly, annual, or lifetime");
    }
    // Optional email — passed by the UI in post-sign-out flows so the
    // server can gate against an existing license + dedup the Stripe
    // customer object via customer_email. NULL is permitted (anonymous
    // first-purchase / token-authenticated path).
    let email = if email_ptr.is_null() {
        None
    } else {
        let s = unsafe { CStr::from_ptr(email_ptr) }
            .to_string_lossy()
            .into_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    let token = license::load_license_file().ok().flatten();
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    // create_checkout now returns a richer JSON value:
    //   { ok: true, url: "..." }                        → 2xx (caller opens browser)
    //   { ok: false, status, error, current_tier?, requested_tier? } → 4xx/5xx
    // We forward as-is so the UI can decide the fallback path (e.g.
    // 409 with current_tier=annual → "send magic link instead").
    let json = match rt.block_on(license::create_checkout(
        &server,
        &tier,
        token.as_deref(),
        email.as_deref(),
    )) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `POST /api/plan-change { token, new_tier }` via FFI. Plan-change is
/// for switching between sub tiers (monthly ⇄ annual) via Stripe's
/// subscription update API — proration is handled server-side. Use
/// `dimmy_license_checkout_url` for first purchase or for upgrading a
/// sub to lifetime; this fn rejects "lifetime" with an error.
///
/// Writes JSON `{ok, error?}` to buf. On success the next
/// `dimmy_license_refresh` call picks up the new tier (the server's
/// customer.subscription.updated webhook updates D1 first).
///
/// # Safety
/// `new_tier_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_plan_change(
    new_tier_ptr: *const c_char,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if new_tier_ptr.is_null() {
        return write_license_err(buf, buf_len, "new_tier required");
    }
    let new_tier = unsafe { CStr::from_ptr(new_tier_ptr) }
        .to_string_lossy()
        .into_owned();
    if !matches!(new_tier.as_str(), "monthly" | "annual") {
        return write_license_err(
            buf,
            buf_len,
            "new_tier must be 'monthly' or 'annual' (lifetime via checkout)",
        );
    }
    let token = match license::load_license_file() {
        Ok(Some(t)) => t,
        Ok(None) => return write_license_err(buf, buf_len, "no license file"),
        Err(e) => return write_license_err(buf, buf_len, &format!("load: {}", e)),
    };
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let json = match rt.block_on(license::change_plan(&server, &token, &new_tier)) {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `POST /api/billing-portal { token }` via FFI. Reads token from disk;
/// trials and source-builds get an error from the server (only paid
/// licenses with a `stripe_customer_id` can manage subscriptions).
/// Writes JSON `{ok, url?, error?}` to buf.
#[no_mangle]
pub extern "C" fn dimmy_license_billing_portal_url(buf: *mut c_char, buf_len: c_int) -> c_int {
    let token = match license::load_license_file() {
        Ok(Some(t)) => t,
        Ok(None) => return write_license_err(buf, buf_len, "no license file"),
        Err(e) => return write_license_err(buf, buf_len, &format!("load: {}", e)),
    };
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let json = match rt.block_on(license::billing_portal_url(&server, &token)) {
        Ok(url) if !url.is_empty() => serde_json::json!({"ok": true, "url": url}),
        Ok(_) => serde_json::json!({"ok": false, "error": "empty URL from server"}),
        Err(e) => serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `POST /api/devices/list { token }` via FFI. Reads token from disk,
/// returns JSON `{ok, license_id, tier, max_devices, devices: [...], error?}`.
#[no_mangle]
pub extern "C" fn dimmy_license_devices_list(buf: *mut c_char, buf_len: c_int) -> c_int {
    // Source / dev builds have no embedded pubkey AND no embedded server
    // URL; the URL falls back to http://localhost:8787 which obviously
    // isn't running for an end user, so the call surfaces a confusing
    // "connection refused localhost:8787" error in Settings → Devices.
    // Short-circuit here with a clear "licensing not configured" so the
    // UI can render an empty state instead of a network error.
    if license::EMBEDDED_PUBKEY_B64.is_empty() {
        return write_license_err(buf, buf_len, "licensing not configured in this build");
    }
    let token = match license::load_license_file() {
        Ok(Some(t)) => t,
        Ok(None) => return write_license_err(buf, buf_len, "no license file"),
        Err(e) => return write_license_err(buf, buf_len, &format!("load: {}", e)),
    };
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let json = match rt.block_on(license::list_devices(&server, &token)) {
        Ok(r) => serde_json::json!({
            "ok": true,
            "license_id": r.license_id,
            "tier": r.tier,
            "max_devices": r.max_devices,
            "devices": r.devices,
        }),
        Err(e) => serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// `POST /api/devices/deactivate { token, device_id? }` via FFI.
/// `device_id_ptr` may be null to self-deactivate (sign out the current device).
/// Writes JSON `{ok, error?}` to buf. On self-deactivate success, also clears
/// the local license file so the UI flips back to NotFound.
///
/// # Safety
/// `device_id_ptr` may be null. If non-null, must be a valid null-terminated
/// UTF-8 C string. `buf` must point to at least `buf_len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_device_deactivate(
    device_id_ptr: *const c_char,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    // Same source-build short-circuit as dimmy_license_devices_list —
    // skip the localhost HTTP attempt when there's no licensing server
    // configured for this build.
    if license::EMBEDDED_PUBKEY_B64.is_empty() {
        return write_license_err(buf, buf_len, "licensing not configured in this build");
    }
    let token = match license::load_license_file() {
        Ok(Some(t)) => t,
        Ok(None) => return write_license_err(buf, buf_len, "no license file"),
        Err(e) => return write_license_err(buf, buf_len, &format!("load: {}", e)),
    };
    let device_id = if device_id_ptr.is_null() {
        None
    } else {
        let s = unsafe { CStr::from_ptr(device_id_ptr) }
            .to_string_lossy()
            .into_owned();
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    };
    let is_self = device_id.is_none();
    let server = licensing_server_url();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return write_license_err(buf, buf_len, &format!("runtime: {}", e)),
    };
    let json = match rt.block_on(license::deactivate_device(
        &server,
        &token,
        device_id.as_deref(),
    )) {
        Ok(()) => {
            if is_self {
                if let Some(path) = license::license_path() {
                    let _ = std::fs::remove_file(&path);
                }
            }
            track(Event::LicenseDeviceDeactivated { is_self });
            serde_json::json!({"ok": true})
        }
        Err(e) => serde_json::json!({"ok": false, "error": format!("{}", e)}),
    };
    write_to_buf(&json.to_string(), buf, buf_len)
}

/// Capability check — does the active license carry the named scope?
/// Returns 1 = yes, 0 = no, -1 on null input. Source builds (no embedded
/// pubkey) report 1 for every scope.
///
/// # Safety
/// `scope_ptr` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_license_has_scope(scope_ptr: *const c_char) -> c_int {
    if scope_ptr.is_null() {
        return -1;
    }
    let scope = unsafe { CStr::from_ptr(scope_ptr) }
        .to_string_lossy()
        .into_owned();
    if scope.is_empty() {
        return -1;
    }
    let granted = license::has_scope(&scope);
    if !granted {
        // Categorical scope name only — never the user. Match against the
        // known vocab; unknown scopes don't get logged so we can't blow up
        // PostHog cardinality with attacker-controlled strings.
        let categorical: Option<&'static str> = match scope.as_str() {
            "managed_stt" => Some("managed_stt"),
            "managed_llm" => Some("managed_llm"),
            "auto_update" => Some("auto_update"),
            "history_sync" => Some("history_sync"),
            "premium_styles" => Some("premium_styles"),
            _ => None,
        };
        if let Some(s) = categorical {
            track(Event::LicenseScopeDenied { scope: s });
        }
    }
    if granted {
        1
    } else {
        0
    }
}

/// Delete the on-disk license file. Useful for "Sign out" / dev reset.
/// Returns 0 on success (or no-op if missing), -1 on error.
#[no_mangle]
pub extern "C" fn dimmy_license_clear() -> c_int {
    let path = match license::license_path() {
        Some(p) => p,
        None => return -1,
    };
    if path.exists() && std::fs::remove_file(&path).is_err() {
        return -1;
    }
    0
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
                local_stt_backend: Mutex::new("whisper".to_string()),
                live_captions_enabled: Mutex::new(true),
                save_audio_in_history: Mutex::new(false),
                history_audio_keep_days: Mutex::new(30),
                history_audio_max_mb: Mutex::new(5_000),
                auto_recap_threshold_secs: Mutex::new(60),
                filler_removal_enabled: Mutex::new(true),
                llm_mode: Mutex::new("cloud".to_string()),
                local_llm_model: Mutex::new(crate::local_llm::DEFAULT_LLM_MODEL.to_string()),
                border_style: Mutex::new("Rainbow".to_string()),
                waveform_style: Mutex::new("Bars".to_string()),
                overlay_position: Mutex::new("Bottom Right".to_string()),
                keep_in_clipboard: Mutex::new(false),
                input_gain: Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
                audio_source: Mutex::new("mic".to_string()),
                key_store: crate::keystore::KeyStore::new(),
                audio_debug_session_dir: Mutex::new(None),
                window_anchor: Mutex::new(None),
                stats_total_words: Mutex::new(100),
                stats_total_speaking_secs: Mutex::new(60.0),
                app_rules: Mutex::new(Vec::new()),
                current_app_context: Mutex::new(crate::app_rules::AppContext::default()),
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

    // ── LLM key reload + per-provider preservation tests ────────────
    //
    // These tests pin the contract the user complained about:
    //   1) Switching LLM provider in the UI must surface the key already
    //      stored for that provider, not "lose" it.
    //   2) Toggling `llm_use_same_key` must NEVER delete a per-provider
    //      key from the keystore — it only changes which key the dispatch
    //      uses at runtime.
    //   3) `get_config_json` must report `has_llm_<provider>_key` for each
    //      provider so the UI can update the green badge on dropdown
    //      change without saving first.
    //
    // The keystore is seeded via `replace_cache_for_testing` which
    // bypasses disk writes — running the test suite must not touch the
    // developer's real `~/.config/dimmy/keys.enc`.

    const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
    const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";

    #[test]
    fn set_config_json_reloads_llm_key_when_provider_url_changes() {
        ensure_test_state();
        // Seed: Anthropic LLM key in keystore, no OpenAI LLM key.
        state().key_store.replace_cache_for_testing(&[(
            KeyringScope::Llm(Provider::Anthropic),
            "sk-ant-seeded-test",
        )]);
        // Reset in-memory state so the reload is observable.
        if let Ok(mut k) = state().llm_api_key.lock() {
            *k = None;
        }
        if let Ok(mut u) = state().llm_api_url.lock() {
            *u = OPENAI_URL.to_string();
        }

        // UI sends the URL change without a key (PasswordBox empty).
        let json = format!(r#"{{"llm_api_url":"{}"}}"#, ANTHROPIC_URL);
        let c = CString::new(json).unwrap();
        let rc = unsafe { dimmy_set_config_json(c.as_ptr()) };
        assert_eq!(rc, 0, "valid JSON should return 0");

        let loaded = state().llm_api_key.lock().unwrap().clone();
        assert_eq!(
            loaded.as_deref(),
            Some("sk-ant-seeded-test"),
            "switching to Anthropic URL must reload the stored Anthropic key"
        );
    }

    #[test]
    fn set_config_json_use_same_key_toggle_does_not_delete_stored_llm_key() {
        ensure_test_state();
        // Seed Anthropic key + point state at Anthropic.
        state().key_store.replace_cache_for_testing(&[(
            KeyringScope::Llm(Provider::Anthropic),
            "sk-ant-must-survive",
        )]);
        if let Ok(mut u) = state().llm_api_url.lock() {
            *u = ANTHROPIC_URL.to_string();
        }

        // Toggle use_same_key=true (PasswordBox hidden in UI → no key sent).
        let json = format!(
            r#"{{"llm_use_same_key":true,"llm_api_url":"{}"}}"#,
            ANTHROPIC_URL
        );
        let c = CString::new(json).unwrap();
        let rc = unsafe { dimmy_set_config_json(c.as_ptr()) };
        assert_eq!(rc, 0);

        // Anthropic key must STILL be in keystore (not deleted by toggle).
        assert!(
            state()
                .key_store
                .has_key(KeyringScope::Llm(Provider::Anthropic), false),
            "use_same_key=true must NOT delete the Anthropic LLM key"
        );

        // Now toggle back off — key should still load when revisiting URL.
        let json2 = format!(
            r#"{{"llm_use_same_key":false,"llm_api_url":"{}"}}"#,
            ANTHROPIC_URL
        );
        let c2 = CString::new(json2).unwrap();
        let rc2 = unsafe { dimmy_set_config_json(c2.as_ptr()) };
        assert_eq!(rc2, 0);

        let loaded = state().llm_api_key.lock().unwrap().clone();
        assert_eq!(
            loaded.as_deref(),
            Some("sk-ant-must-survive"),
            "toggling use_same_key off and re-applying URL must restore the per-provider key"
        );
    }

    #[test]
    fn set_config_json_switching_providers_preserves_each_key() {
        ensure_test_state();
        state().key_store.replace_cache_for_testing(&[
            (KeyringScope::Llm(Provider::Anthropic), "sk-ant-aaa"),
            (KeyringScope::Llm(Provider::OpenAI), "sk-oai-bbb"),
        ]);

        // Anthropic → OpenAI → Anthropic round-trip; each step must
        // surface the right key, neither overwrite the other.
        for (url, expected) in &[
            (ANTHROPIC_URL, "sk-ant-aaa"),
            (OPENAI_URL, "sk-oai-bbb"),
            (ANTHROPIC_URL, "sk-ant-aaa"),
        ] {
            let json = format!(r#"{{"llm_api_url":"{}"}}"#, url);
            let c = CString::new(json).unwrap();
            let rc = unsafe { dimmy_set_config_json(c.as_ptr()) };
            assert_eq!(rc, 0, "set_config_json must succeed for {}", url);

            let loaded = state().llm_api_key.lock().unwrap().clone();
            assert_eq!(
                loaded.as_deref(),
                Some(*expected),
                "switching to {} should surface {}",
                url,
                expected
            );
        }

        // Both keystore entries still intact at the end.
        assert!(state()
            .key_store
            .has_key(KeyringScope::Llm(Provider::Anthropic), false));
        assert!(state()
            .key_store
            .has_key(KeyringScope::Llm(Provider::OpenAI), false));
    }

    #[test]
    fn get_config_json_reports_per_provider_has_llm_keys() {
        ensure_test_state();
        // Seed: only Anthropic LLM has a key.
        state()
            .key_store
            .replace_cache_for_testing(&[(KeyringScope::Llm(Provider::Anthropic), "sk-ant-only")]);

        let mut buf = vec![0u8; 8192];
        let ptr = buf.as_mut_ptr() as *mut c_char;
        let len = dimmy_get_config_json(ptr, 8192);
        assert!(len > 0, "get_config_json should produce output");

        let json_str = unsafe { CStr::from_ptr(ptr).to_str().unwrap() };
        let v: serde_json::Value = serde_json::from_str(json_str).expect("config JSON must parse");

        assert_eq!(
            v["has_llm_anthropic_key"], true,
            "Anthropic LLM seeded → should report true"
        );
        assert_eq!(
            v["has_llm_openai_key"], false,
            "OpenAI LLM not seeded → should report false"
        );
        assert_eq!(v["has_llm_groq_key"], false);
        assert_eq!(v["has_llm_gemini_key"], false);
        assert_eq!(v["has_llm_openrouter_key"], false);
        assert_eq!(v["has_llm_custom_key"], false);
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

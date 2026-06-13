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

/// Active realtime streaming dictation session (Deepgram WebSocket), if any.
/// Mutually exclusive with CHUNKED — when `streaming_dictation` is on and a
/// Deepgram key is present, this engine takes over the dictation capture
/// path. Taken out by `dimmy_stop_recording` to drain the final transcript,
/// same contract as CHUNKED.
static STREAMING: Mutex<Option<crate::deepgram_stream::DeepgramStreamer>> = Mutex::new(None);

/// Which dictation engine produced the current recording, for the
/// `transcription.completed` telemetry `engine` prop. Set in start_recording,
/// read at the stop emit (where the streaming/chunked locals are out of scope).
static DICTATION_ENGINE: Mutex<&'static str> = Mutex::new("batch");

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

/// Emit a `meeting_state` event with the current (active, paused) flags
/// so the native UI doesn't have to poll `dimmy_meeting_is_active` /
/// `dimmy_meeting_is_paused` to mirror state in the pill / taskbar.
///
/// Pattern: every meeting state transition (start / stop / pause /
/// resume) calls this exactly once. UIs subscribe via
/// `dimmy_set_event_callback` and update their view state on the
/// envelope, NOT on a `DispatcherTimer` / `NSTimer` polling loop. See
/// CLAUDE.md "No FFI-state polling rule".
fn emit_meeting_state_event(active: bool, paused: bool) {
    let payload = format!(r#"{{"active":{},"paused":{}}}"#, active, paused);
    emit_event("meeting_state", &payload);
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

/// Pick the canonical transcript from the live streaming/chunked workers.
/// A non-empty result from either is authoritative (its segments were
/// already typed live). An empty result means the live session FAILED
/// (WS dropped, auth error, mid-recording network blip) and the caller
/// must fall back to a batch pass on the still-intact buffered audio —
/// returning `None` here signals exactly that. Streaming wins over chunked
/// when both are present (they never both run, but the ordering is fixed
/// for determinism). Extracted so the work-loss guard has a regression
/// test: an empty live result must never beat the batch retry.
fn resolve_live_final(streaming: Option<String>, chunked: Option<String>) -> Option<String> {
    streaming
        .filter(|c| !c.trim().is_empty())
        .or_else(|| chunked.filter(|c| !c.trim().is_empty()))
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
    // Print the resolved config path explicitly. Two builds running
    // against different paths (e.g. Xcode debug build pointing at
    // `~/Library/Application Support/dimmy-staging/` while the
    // installed DMG points at `dimmy/`) is invisible without this
    // line — the colleague would see "my settings are empty in dev"
    // with no immediate hint why. One log line per init beats an
    // hour of `find ~/Library | grep dimmy`.
    let config_path_repr = crate::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unresolved>".to_string());
    let build_flavor = crate::build_flavor();
    log(&format!(
        "FFI init: config_path={} build_flavor={}",
        config_path_repr,
        if build_flavor.is_empty() {
            "<default>"
        } else {
            build_flavor
        }
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
        user_dict: Mutex::new(file_cfg.user_dict),
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
        llm_auth_method: Mutex::new(file_cfg.llm_auth_method.clone()),
        recap_auth_method: Mutex::new(file_cfg.recap_auth_method.clone()),
        recap_use_same_key: Mutex::new(file_cfg.recap_use_same_key),
        llm_api_key: Mutex::new(stored_llm_key),
        llm_log_enabled: Mutex::new(file_cfg.llm_log_enabled),
        recap_model_override: Mutex::new(file_cfg.recap_model_override),
        chunk_streaming_enabled: Mutex::new(file_cfg.chunk_streaming_enabled),
        streaming_dictation: Mutex::new(file_cfg.streaming_dictation),
        preprocessing_enabled: Mutex::new(file_cfg.preprocessing_enabled),
        audio_debug_enabled: Mutex::new(file_cfg.audio_debug_enabled),
        ggml_debug_logging: Mutex::new(file_cfg.ggml_debug_logging),
        use_keyring: Mutex::new(file_cfg.use_keyring),
        stt_mode: Mutex::new(file_cfg.stt_mode),
        local_model: Mutex::new(file_cfg.local_model),
        local_stt_backend: Mutex::new(file_cfg.local_stt_backend),
        live_captions_enabled: Mutex::new(file_cfg.live_captions_enabled),
        call_detect_enabled: Mutex::new(file_cfg.call_detect_enabled),
        call_detect_excluded_apps: Mutex::new(file_cfg.call_detect_excluded_apps),
        call_detect_cooldown_secs: Mutex::new(file_cfg.call_detect_cooldown_secs),
        call_detect_min_active_secs: Mutex::new(file_cfg.call_detect_min_active_secs),
        call_detect_timeout_cooldown_secs: Mutex::new(file_cfg.call_detect_timeout_cooldown_secs),
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
        meeting_storage_path: Mutex::new(file_cfg.meeting_storage_path),
        notion_target_id: Mutex::new(file_cfg.notion_target_id),
        notion_target_kind: Mutex::new(file_cfg.notion_target_kind),
        notion_target_title: Mutex::new(file_cfg.notion_target_title),
        notion_auto_send: Mutex::new(file_cfg.notion_auto_send),
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

            // Periodic-on-startup snapshot of the user-dict size. Lets
            // us see the distribution of dict-using vs not-using
            // installs + power-user tail. Bucketed via sanitize so
            // we can never fingerprint a specific dictionary count.
            {
                let st_for_dict = state();
                let dict_len = st_for_dict.user_dict.lock().map(|d| d.len()).unwrap_or(0);
                crate::telemetry::track(crate::telemetry::Event::UserDictSizeSnapshot {
                    size_bucket: crate::telemetry::sanitize::bucket_dict_size(dict_len),
                });
            }

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

/// Start recording. Returns 0=OK, -1=no API key, -2=already recording,
/// -3=lock poisoned, -7=meeting in progress (caller should suppress
/// the dictation hotkey — having two captures running concurrently
/// would clobber the meeting's audio_buffer / aec_mic_ring and corrupt
/// the active meeting recording).
#[no_mangle]
pub extern "C" fn dimmy_start_recording() -> c_int {
    let st = state();

    // Block dictation when a meeting is recording. The audio thread
    // can only host one capture configuration at a time — letting the
    // pill hijack it would silently truncate the meeting's WAV files
    // (synth_len would peg at 0 and the worker would write nothing
    // until the user clicks Stop). User-explicit fix request 2026-05-08.
    {
        let meeting_active = MEETING.lock().map(|g| g.is_some()).unwrap_or(false);
        if meeting_active {
            log("[StartRec] suppressed — meeting recording in progress (rc=-7)");
            return -7;
        }
    }

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
    // The mic cpal callback resamples to MEETING_CANONICAL_RATE (48 kHz)
    // before pushing to audio_buffer, so the dictation preprocess +
    // STT must use that rate, NOT the device-native rate. Previous code
    // here stored the device rate (e.g. 16 kHz on BT-HFP) which made
    // preprocess interpret a 48 kHz buffer at 16 kHz — 3× slowdown,
    // Whisper hallucinated "Grazie per la visione" on every dictation.
    // Reported 2026-05-20 right after the canonical-rate refactor.
    let _device_sr_diag = crate::audio::device_sample_rate(&selected_device);
    if let Ok(mut sr) = st.audio_sample_rate.lock() {
        *sr = crate::audio::MEETING_CANONICAL_RATE;
    }

    // Always-mix architecture (2026-05-08): the audio capture session
    // ALWAYS opens both mic + system loopback, AEC3 always processes
    // the mic with the loopback as far-end reference. Pill dictation
    // still consumes only the cleaned mic buffer (audio_buffer) — the
    // loopback samples flow into audio_buffer_secondary which the
    // dictation chunked-stt worker simply ignores. Net effect:
    //   • no more "Mic vs Mix vs System" decision at the call site
    //   • AEC3 cleans up speaker echo even during pill dictation
    //   • the pill amplitude visualizer can read MAX(mic, loopback) so
    //     the user sees activity from either source
    //   • robustness: aec.rs no longer blocks on missing ref, so a
    //     loopback that never delivers (no default output, no audio
    //     playing) gracefully falls back to mic-only behavior
    let source = crate::audio::AudioSource::Mix;
    let _ = st.audio_tx.lock().map(|tx| {
        tx.send(AudioCommand::Start {
            device_name: selected_device,
            source,
        })
    });

    // Realtime streaming dictation. When `streaming_dictation` is on the
    // host types each finished segment at the cursor as you speak. Engine
    // choice follows the user's STT mode:
    //   • cloud STT  → Deepgram WebSocket (lowest latency), if a Deepgram
    //                  key is saved (engine="deepgram", handled here).
    //   • local STT  → the chunked local path below drives the typing with
    //                  whisper/Parakeet (engine="local-stream").
    // Gating on `!is_local` is load-bearing: a leftover Deepgram key must
    // NOT hijack streaming when the user picked a LOCAL backend — otherwise
    // local typing never fires (the "non va" report, 2026-06-06).
    let streaming_on = st.streaming_dictation.lock().map(|b| *b).unwrap_or(false);
    let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);
    let dg_key = if streaming_on && !is_local {
        crate::load_key_with_store(&st.key_store, KeyringScope::Stt(Provider::Deepgram), use_kr)
    } else {
        None
    };
    let streaming_active = streaming_on && !is_local && dg_key.is_some();
    if streaming_active {
        if let Ok(mut b) = st.audio_buffer.lock() {
            b.clear();
        }
        let language = st.language.lock().map(|l| l.clone()).unwrap_or_default();
        let prompt_base = st.prompt.lock().map(|p| p.clone()).unwrap_or_default();
        let user_dict = st.user_dict.lock().map(|d| d.clone()).unwrap_or_default();
        let keyterms = crate::compose_stt_prompt(&prompt_base, &user_dict);
        let on_chunk: Arc<crate::deepgram_stream::StreamCallback> =
            Arc::new(|delta: &str, cumulative: &str, is_final: bool| {
                let payload = serde_json::json!({
                    "delta": delta,
                    "cumulative": cumulative,
                    "is_final": is_final,
                    "engine": "deepgram",
                })
                .to_string();
                emit_event("stt_chunk", &payload);
            });
        let streamer = crate::deepgram_stream::DeepgramStreamer::start(
            st.audio_buffer.clone(),
            crate::audio::MEETING_CANONICAL_RATE,
            dg_key.unwrap_or_default(),
            language,
            // Empty base => default wss://api.deepgram.com/v1/listen; the
            // streamer's compose_deepgram_ws_url appends model + params.
            String::new(),
            keyterms,
            on_chunk,
        );
        if let Ok(mut slot) = STREAMING.lock() {
            *slot = Some(streamer);
        }
        log("[StartRec] deepgram streaming dictation spawned");
    }

    // Realtime chunked transcriber — backend-agnostic. Two modes:
    //   • typing  : `streaming_dictation` ON, no Deepgram key, local STT →
    //               the chunked engine drives live cursor typing
    //               (engine="local-stream"); the host injects each delta
    //               and suppresses the final paste, same contract as the
    //               Deepgram streaming path. Works with whisper OR Parakeet.
    //   • caption : `chunk_streaming_enabled` ON → live subtitle overlay
    //               only (engine = backend name); final paste still happens
    //               (the stop path uses chunked_final as the transcript).
    // Whisper keeps up only on GPU; Parakeet is realtime on CPU. If a
    // backend can't keep pace the captions/typing simply lag — no crash.
    // Skipped entirely when Deepgram streaming already owns the capture.
    let chunked_on = st
        .chunk_streaming_enabled
        .lock()
        .map(|b| *b)
        .unwrap_or(false);
    let local_backend = st
        .local_stt_backend
        .lock()
        .map(|b| b.clone())
        .unwrap_or_default();
    let local_typing = streaming_on && !streaming_active && is_local;
    let chunked_captions = !streaming_active && !local_typing && is_local && chunked_on;
    if let Ok(mut e) = DICTATION_ENGINE.lock() {
        *e = if streaming_active {
            "deepgram_stream"
        } else if local_typing {
            "local_stream"
        } else if chunked_captions {
            "chunked_caption"
        } else {
            "batch"
        };
    }
    if local_typing || chunked_captions {
        // Clear any zombie buffer from a previous run so the worker
        // doesn't transcribe stale audio. The audio thread refills it.
        if let Ok(mut b) = st.audio_buffer.lock() {
            b.clear();
        }
        // Per-chunk transcriber for the active local backend.
        let transcribe_fn: Arc<crate::chunked_stt::TranscribeFn> = if local_backend == "parakeet" {
            Arc::new(|pcm: &[f32]| crate::parakeet::transcribe(pcm))
        } else {
            let model_filename = st.local_model.lock().map(|m| m.clone()).unwrap_or_default();
            let model_path = crate::local_stt::model_path(&model_filename);
            let language = st.language.lock().map(|l| l.clone()).unwrap_or_default();
            let prompt_base = st.prompt.lock().map(|p| p.clone()).unwrap_or_default();
            let user_dict = st.user_dict.lock().map(|d| d.clone()).unwrap_or_default();
            let prompt = crate::compose_stt_prompt(&prompt_base, &user_dict);
            Arc::new(move |pcm: &[f32]| {
                crate::local_stt::transcribe_local(&model_path, pcm, &language, &prompt)
            })
        };
        let engine = if local_typing {
            "local-stream".to_string()
        } else {
            local_backend.clone()
        };
        let typing = local_typing;
        let on_chunk: Arc<crate::chunked_stt::ChunkCallback> =
            Arc::new(move |delta: &str, cumulative: &str, is_final: bool| {
                // Typing mode: prefix a space so cursor-injected segments
                // don't run together — the chunked delta carries no leading
                // space (cumulative inserts one between segments).
                let out_delta = if typing && !delta.is_empty() && cumulative.len() > delta.len() {
                    format!(" {}", delta)
                } else {
                    delta.to_string()
                };
                let payload = serde_json::json!({
                    "delta": out_delta,
                    "cumulative": cumulative,
                    "is_final": is_final,
                    "engine": engine,
                })
                .to_string();
                emit_event("stt_chunk", &payload);
            });
        // 3 s chunks + 500 ms overlap (chunked_smoke A/B 2026-05-06).
        let transcriber = crate::chunked_stt::ChunkedTranscriber::start(
            st.audio_buffer.clone(),
            crate::audio::MEETING_CANONICAL_RATE,
            3.0,
            500,
            transcribe_fn,
            on_chunk,
        );
        if let Ok(mut slot) = CHUNKED.lock() {
            *slot = Some(transcriber);
        }
        log(&format!(
            "[StartRec] chunked-stt worker spawned (backend={}, mode={})",
            local_backend,
            if typing {
                "typing/local-stream"
            } else {
                "caption"
            }
        ));
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

    // Drain the Deepgram streaming session, if any. Same pre-clear timing
    // rationale as the chunked worker: stop() flushes the trailing audio
    // to the socket and waits for the final results before returning, so
    // the buffer must still be intact. The returned text is the canonical
    // transcript for history; segments were already injected at the cursor
    // live, so the host suppresses the final paste for streaming sessions.
    let streaming_final = STREAMING
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .map(|s| {
            log("[StopRec] draining deepgram streaming worker (pre-clear)");
            s.stop()
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
    // API key is only required for cloud mode. A streaming session carries
    // its own Deepgram key and already produced the transcript, so don't
    // short-circuit it on the selected-provider key being absent.
    let api_key = st.api_key.lock().ok().and_then(|k| k.clone());
    if stt_mode == "cloud" && api_key.is_none() && streaming_final.is_none() {
        return write_to_buf("", out_buf, buf_len);
    }
    let language = st.language.lock().map(|l| l.clone()).unwrap_or_default();
    let prompt_base = st.prompt.lock().map(|p| p.clone()).unwrap_or_default();
    let user_dict = st.user_dict.lock().map(|d| d.clone()).unwrap_or_default();
    // Compose the user's free-form prompt + curated vocabulary into a
    // single biasing string. Same path drives both cloud (multipart
    // `prompt` form field) and local Whisper (`initial_prompt`).
    let prompt = crate::compose_stt_prompt(&prompt_base, &user_dict);
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
    // The streaming (Deepgram) and chunked workers were drained above,
    // before the audio buffer was cleared. A NON-EMPTY result from either
    // is canonical — the segments were already typed live, so use it as-is.
    // An EMPTY result means the live session failed (WS dropped, auth
    // error, network blip mid-recording): fall through to a batch pass on
    // the still-intact `processed` audio rather than discarding the whole
    // recording. The host only suppresses the final paste once at least one
    // segment was injected live, so a zero-segment failure still gets pasted
    // from this batch transcript. Work-loss guard, 2026-06-13.
    let had_live = streaming_final.is_some() || chunked_final.is_some();
    let live_final = resolve_live_final(streaming_final, chunked_final);
    if had_live && live_final.is_none() {
        log("[StopRec] live streaming/chunked transcript was empty — falling back to batch STT on buffered audio (work-loss guard)");
    }
    let transcript = if let Some(cumulative) = live_final {
        Ok(cumulative)
    } else if stt_mode == "local" {
        if local_stt_backend == "parakeet" {
            log("[StopRec] Local STT mode — backend: parakeet (batch)");
            crate::transcribe::transcribe_audio_local_parakeet(&processed)
        } else {
            log(&format!(
                "[StopRec] Local STT mode — backend: whisper, model: {}",
                local_model_filename
            ));
            crate::transcribe::transcribe_audio_local(
                &processed,
                &language,
                &local_model_filename,
                &prompt,
            )
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
            // local_backend categorical: "whisper" | "parakeet" | "" when cloud.
            let local_backend_static: &'static str = if stt_mode == "local" {
                match st
                    .local_stt_backend
                    .lock()
                    .map(|b| b.clone())
                    .unwrap_or_default()
                    .as_str()
                {
                    "parakeet" => "parakeet",
                    _ => "whisper",
                }
            } else {
                ""
            };
            crate::telemetry::track(crate::telemetry::Event::TranscriptionCompleted {
                mode: mode_static,
                provider: provider_static,
                local_backend: local_backend_static,
                // Hotkey-driven dictation path — this is the only emit
                // site on the dictation flow. File-load + meeting emit
                // their own dedicated events.
                entry_point: "hotkey",
                audio_secs: speaking_secs,
                processing_ms,
                word_count: words.max(0) as u32,
                language: language.clone(),
                success: true,
                had_filler_removal: filler_enabled,
                had_llm: llm_enabled_now,
                engine: DICTATION_ENGINE.lock().map(|e| *e).unwrap_or("batch"),
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
            // Write the actual error to dimmy.log so failures don't go
            // silent. Truncate to 300 chars per CLAUDE.md "error bodies
            // ≤ 200 chars" rule (with slack for the prefix). The
            // upstream `TranscribeError::Display` already strips the
            // response body before this point, so no secret leak.
            // Burned 2026-05-15 — Groq decommissioned the
            // distil-whisper-large-v3-en model and Dimmy emitted only
            // `transcription.failed` telemetry without the body, so
            // the user had no way to find out why dictation broke.
            let truncated = if err_msg.len() > 300 {
                format!("{}…", crate::truncate_utf8(&err_msg, 300))
            } else {
                err_msg.clone()
            };
            log(&format!(
                "[STT] {} transcription failed: {}",
                if stt_mode == "local" {
                    "local"
                } else {
                    "cloud"
                },
                truncated
            ));
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
        "user_dict": st.user_dict.lock().map(|d| d.clone()).unwrap_or_default(),
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
        "llm_auth_method": st.llm_auth_method.lock().map(|s| s.clone()).unwrap_or_else(|_| "api_key".to_string()),
        "recap_auth_method": st.recap_auth_method.lock().map(|s| s.clone()).unwrap_or_default(),
        "recap_use_same_key": *st.recap_use_same_key.lock().unwrap_or_else(|e| e.into_inner()),
        "has_anthropic_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::Anthropic), use_kr),
        "has_openai_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::OpenAI), use_kr),
        "has_gemini_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::Gemini), use_kr),
        "has_groq_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::Groq), use_kr),
        "has_openrouter_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::OpenRouter), use_kr),
        "has_fireworks_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::Fireworks), use_kr),
        "has_together_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::Together), use_kr),
        "has_custom_recap_key": st.key_store.has_key(KeyringScope::Recap(Provider::Custom), use_kr),
        "has_llm_key": has_llm_key,
        "llm_log_enabled": *st.llm_log_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "recap_model_override": st.recap_model_override.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "chunk_streaming_enabled": *st.chunk_streaming_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "streaming_dictation": *st.streaming_dictation.lock().unwrap_or_else(|e| e.into_inner()),
        "preprocessing_enabled": *st.preprocessing_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "audio_debug_enabled": *st.audio_debug_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "ggml_debug_logging": *st.ggml_debug_logging.lock().unwrap_or_else(|e| e.into_inner()),
        "use_keyring": use_kr,
        "stt_mode": *st.stt_mode.lock().unwrap_or_else(|e| e.into_inner()),
        "local_model": *st.local_model.lock().unwrap_or_else(|e| e.into_inner()),
        "local_stt_backend": *st.local_stt_backend.lock().unwrap_or_else(|e| e.into_inner()),
        "live_captions_enabled": *st.live_captions_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "call_detect_enabled": *st.call_detect_enabled.lock().unwrap_or_else(|e| e.into_inner()),
        "call_detect_excluded_apps": st.call_detect_excluded_apps.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "call_detect_cooldown_secs": *st.call_detect_cooldown_secs.lock().unwrap_or_else(|e| e.into_inner()),
        "call_detect_min_active_secs": *st.call_detect_min_active_secs.lock().unwrap_or_else(|e| e.into_inner()),
        "call_detect_timeout_cooldown_secs": *st.call_detect_timeout_cooldown_secs.lock().unwrap_or_else(|e| e.into_inner()),
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
        // Per-provider key flags — STT. Covers every Provider enum variant
        // that can offer STT (Anthropic is skipped — it doesn't have a
        // dictation endpoint). Keep this list in sync with the Provider
        // enum in provider.rs; the UI relies on the green-check per-row.
        "has_groq_key": st.key_store.has_key(KeyringScope::Stt(Provider::Groq), use_kr),
        "has_openai_key": st.key_store.has_key(KeyringScope::Stt(Provider::OpenAI), use_kr),
        "has_openrouter_key": st.key_store.has_key(KeyringScope::Stt(Provider::OpenRouter), use_kr),
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
    // Notion integration token presence — drives the "Connected /
    // Not connected" status indicator on the Notion settings page.
    // Token value itself is NEVER round-tripped via config.
    json["has_notion_token"] = st
        .key_store
        .has_key(KeyringScope::NotionToken, use_kr)
        .into();
    // Notion target — surfaced for the picker to show "Recaps will
    // land in: <title>" status text, and so the C# VM can drive the
    // "you haven't picked a target yet" hint.
    json["notion_target_id"] = st
        .notion_target_id
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
        .into();
    json["notion_target_kind"] = st
        .notion_target_kind
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
        .into();
    json["notion_target_title"] = st
        .notion_target_title
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
        .into();
    json["notion_auto_send"] = st
        .notion_auto_send
        .lock()
        .map(|b| *b)
        .unwrap_or(false)
        .into();
    // Configured meeting storage override (raw value, empty = default).
    // The host UI reads the *effective* resolved dir via
    // `dimmy_meetings_dir`; this round-trips the user's setting so the
    // VM knows whether a custom dir is set.
    json["meeting_storage_path"] = st
        .meeting_storage_path
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
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
    if let Some(arr) = v["user_dict"].as_array() {
        // Accept replacement of the full list (settings-page edit
        // flow). Add / remove single-word mutations also have a
        // dedicated FFI (dimmy_user_dict_add / _remove) for the
        // hotkey path so a 500-word dict doesn't get re-serialised
        // for every keystroke.
        let new_dict: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .collect();
        if let Ok(mut d) = st.user_dict.lock() {
            *d = new_dict;
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
    if let Some(s) = v["llm_auth_method"].as_str() {
        // Allow-list of accepted values so a malformed UI write
        // can't put the dispatcher into an undefined state.
        let normalized = match s {
            "api_key" | "subscription" => s,
            _ => "api_key",
        };
        if let Ok(mut m) = st.llm_auth_method.lock() {
            *m = normalized.to_string();
        }
    }
    if let Some(s) = v["recap_auth_method"].as_str() {
        // Empty string is a legitimate "inherit from llm_auth_method"
        // signal. Anything else must match the same allow-list as
        // the dictation knob.
        let normalized = match s {
            "" | "api_key" | "subscription" => s,
            _ => "",
        };
        if let Ok(mut m) = st.recap_auth_method.lock() {
            *m = normalized.to_string();
        }
    }
    if let Some(b) = v["recap_use_same_key"].as_bool() {
        if let Ok(mut m) = st.recap_use_same_key.lock() {
            *m = b;
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
    if let Some(s) = v["recap_model_override"].as_str() {
        let changed = st
            .recap_model_override
            .lock()
            .map(|prev| prev.as_str() != s)
            .unwrap_or(false);
        if let Ok(mut slot) = st.recap_model_override.lock() {
            *slot = s.to_string();
        }
        log(&format!("[Config] recap_model_override set to {:?}", s));
        if changed {
            crate::telemetry::track(crate::telemetry::Event::ConfigRecapModelChanged {
                recap_model_bucket: crate::telemetry::sanitize::bucket_recap_model(s),
            });
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
    if let Some(b) = v["streaming_dictation"].as_bool() {
        if let Ok(mut c) = st.streaming_dictation.lock() {
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
    if let Some(b) = v["call_detect_enabled"].as_bool() {
        if let Ok(mut f) = st.call_detect_enabled.lock() {
            *f = b;
        }
    }
    if let Some(arr) = v["call_detect_excluded_apps"].as_array() {
        let parsed: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect();
        if let Ok(mut f) = st.call_detect_excluded_apps.lock() {
            *f = parsed;
        }
    }
    if let Some(n) = v["call_detect_cooldown_secs"].as_u64() {
        if let Ok(mut f) = st.call_detect_cooldown_secs.lock() {
            *f = n as u32;
        }
    }
    if let Some(n) = v["call_detect_min_active_secs"].as_u64() {
        if let Ok(mut f) = st.call_detect_min_active_secs.lock() {
            *f = n as u32;
        }
    }
    if let Some(n) = v["call_detect_timeout_cooldown_secs"].as_u64() {
        if let Ok(mut f) = st.call_detect_timeout_cooldown_secs.lock() {
            *f = n as u32;
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
    // Meeting storage override. Identity-class field: the host UI omits
    // it from a normal full-config save when empty (else a transient
    // empty VM wipes the saved dir). An explicit "" here is the
    // "reset to default" signal. Stored verbatim; `meetings_dir()`
    // validates writability + falls back at resolve time.
    if let Some(s) = v["meeting_storage_path"].as_str() {
        if let Ok(mut slot) = st.meeting_storage_path.lock() {
            *slot = s.trim().to_string();
        }
        log(&format!(
            "[Config] meeting_storage_path set ({})",
            if s.trim().is_empty() {
                "cleared → default"
            } else {
                "custom dir"
            }
        ));
    }
    // Notion target / auto-send. UI writes here on the picker
    // confirmation + auto-send toggle. Token is NEVER round-tripped
    // via config JSON — it lives in the keystore (encrypted) and is
    // touched only via the dedicated dimmy_notion_set_token FFI.
    if let Some(s) = v["notion_target_id"].as_str() {
        if let Ok(mut slot) = st.notion_target_id.lock() {
            *slot = s.to_string();
        }
        log(&format!(
            "[Config] notion_target_id set ({})",
            if s.is_empty() { "cleared" } else { "set" }
        ));
    }
    if let Some(s) = v["notion_target_kind"].as_str() {
        let normalised = match s.to_ascii_lowercase().as_str() {
            "page" | "database" => s.to_ascii_lowercase(),
            _ => "page".to_string(),
        };
        if let Ok(mut slot) = st.notion_target_kind.lock() {
            *slot = normalised;
        }
    }
    if let Some(s) = v["notion_target_title"].as_str() {
        if let Ok(mut slot) = st.notion_target_title.lock() {
            *slot = s.to_string();
        }
    }
    if let Some(b) = v["notion_auto_send"].as_bool() {
        if let Ok(mut slot) = st.notion_auto_send.lock() {
            *slot = b;
        }
        log(&format!("[Config] notion_auto_send set to {}", b));
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

/// Push externally-captured system-audio samples on platforms where cpal
/// cannot open a loopback device (macOS, Linux). Samples are forwarded via
/// the audio worker channel so they land in both audio_buffer_secondary
/// (meeting worker) and aec_ref_ring (AEC3 far-end reference) — identical
/// to what the Windows WASAPI loopback callback does.
///
/// Sample rate (Hz) of the currently active cpal mic stream. Returns
/// 0 when no recording is in flight. Used by the macOS
/// `SystemAudioCaptureService` (ScreenCaptureKit loopback) so it can
/// configure SCStream at the SAME rate the mic is running at —
/// hardcoding 48 kHz when the mic is at 16 kHz (BT A2DP) forces
/// the macOS audio HAL to renegotiate the global mixer, which
/// degrades headphone output during meetings.
#[no_mangle]
pub extern "C" fn dimmy_get_active_mic_sample_rate() -> c_int {
    crate::audio::active_mic_sample_rate() as c_int
}

/// Device-native sample rate of the live cpal mic (Hz), or 0 when no
/// recording is in flight. Distinct from
/// `dimmy_get_active_mic_sample_rate` which returns the canonical
/// post-resample rate (48 kHz). macOS SystemAudioCaptureService uses
/// this to size SCStream at the actual wire rate (e.g. 16 kHz on
/// BT-HFP), so SCKit doesn't deliver upsampled garbage tagged 48 kHz.
#[no_mangle]
pub extern "C" fn dimmy_get_active_mic_device_rate() -> c_int {
    crate::audio::active_mic_device_rate() as c_int
}

/// Probe the sample rate cpal WOULD open the configured mic at,
/// without actually opening a stream. Reads `default_input_config()`
/// on the user-selected device (or system default if none picked).
///
/// Useful for callers that need to know the rate BEFORE cpal has
/// activated the stream — e.g. macOS `SystemAudioCaptureService`
/// closes the race where `dimmy_get_active_mic_sample_rate` returns
/// 0 because the cpal Start command (sent by `dimmy_meeting_start`)
/// hasn't been serviced yet when SCStream starts. With this probe
/// the SCStream rate always matches what cpal will use, so the
/// secondary samples land in the audio_buffer_secondary at the same
/// rate as the audio.wav / audio_system.wav header.
///
/// Returns the probed rate (Hz), or 0 if no device is available.
#[no_mangle]
pub extern "C" fn dimmy_probe_primary_sample_rate() -> c_int {
    let st = state();
    let selected = st.selected_device.lock().ok().and_then(|d| d.clone());
    crate::audio::primary_sample_rate(&selected, &crate::audio::AudioSource::Mic) as c_int
}

/// `samples`     — interleaved f32 PCM, [-1.0, 1.0] expected (clamped).
/// `count`       — number of f32 values.
/// `sample_rate` — native rate of the SCStream / loopback source. When
///                 `> 0` it ALSO updates the loopback rate the meeting
///                 worker uses to size `audio_system.wav`'s WAV header
///                 (via `crate::audio::secondary_sample_rate`). The
///                 first non-zero push during a meeting is therefore
///                 enough to keep header and samples in sync — but
///                 prefer pre-setting via `dimmy_set_loopback_sample_rate`
///                 so the WAV is opened at the right rate from the
///                 first byte. `sample_rate <= 0` is treated as "no
///                 information"; the prior override (or platform
///                 fallback) stays in effect.
///
/// Returns 0 on success, -1 on null / bad args / channel send failure.
///
/// # Safety
///
/// `samples` must be a valid pointer to at least `count` f32 values.
#[no_mangle]
pub unsafe extern "C" fn dimmy_push_loopback_audio(
    samples: *const c_float,
    count: c_int,
    sample_rate: c_int,
) -> c_int {
    if samples.is_null() || count <= 0 {
        return -1;
    }
    if sample_rate > 0 {
        let rate = sample_rate as u32;
        if (8_000..=192_000).contains(&rate) {
            crate::audio::set_loopback_sample_rate_override(rate);
        }
    }
    let slice = std::slice::from_raw_parts(samples, count as usize);
    let vec: Vec<f32> = slice.iter().map(|&s| s.clamp(-1.0, 1.0)).collect();
    // Pass the validated source rate down to the worker so it can rebuild
    // its LinearResampler iff the rate changed (BT renegotiation / output
    // device swap mid-meeting). 0 means "no info" — the worker reads the
    // override stashed by set_loopback_sample_rate as a fallback.
    let src_rate_for_worker: u32 = if sample_rate > 0 {
        let r = sample_rate as u32;
        if (8_000..=192_000).contains(&r) {
            r
        } else {
            0
        }
    } else {
        0
    };
    let st = state();
    match st.audio_tx.lock() {
        Ok(tx) => match tx.send(crate::audio::AudioCommand::PushLoopback(
            vec,
            src_rate_for_worker,
        )) {
            Ok(_) => 0,
            Err(_) => -1,
        },
        Err(_) => -1,
    }
}

/// Tell the Rust core the sample rate at which the next
/// `dimmy_push_loopback_audio` stream will deliver samples. Called by
/// the macOS `SystemAudioCaptureService` BEFORE `dimmy_meeting_start`
/// so the meeting worker opens `audio_system.wav` with the correct
/// header (otherwise the WAV is sized at the legacy 48 kHz fallback
/// while SCStream pushes at the cpal mic rate, e.g. 16 kHz on BT-HFP,
/// and playback / STT are 3× off).
///
/// `rate == 0` clears the override (next read falls back to the
/// platform default). Out-of-range values (outside 8 kHz..=192 kHz)
/// are ignored — caller-side input range error, not worth aborting.
///
/// Returns 0 on success, -1 on out-of-range input (override not
/// changed).
#[no_mangle]
pub extern "C" fn dimmy_set_loopback_sample_rate(rate: c_int) -> c_int {
    if rate < 0 {
        return -1;
    }
    let r = rate as u32;
    if r != 0 && !(8_000..=192_000).contains(&r) {
        return -1;
    }
    crate::audio::set_loopback_sample_rate_override(r);
    0
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

    // Final gate: enhance / translate only when SOMETHING wants the
    // LLM to run. Three triggers can keep us alive past the gate:
    //   1. Global LLM toggle on (user's default mode is "enhance").
    //   2. An app rule fired with a non-off style for this app.
    //   3. The user picked a translate-target (pill scroll-wheel or
    //      pill menu → "Translate to: ..." — sets llm_translate_to
    //      independently of style).
    // The translate trigger was previously missing, so the dictation
    // flow swallowed the LLM call when style=Off even with a
    // translate target set — user-reported regression 2026-05-20.
    let translate_active = !translate_to.is_empty();
    if !global_enabled && !rule_forced_enhance && !translate_active {
        log("[LLM] global disabled, no rule, no translate target — pass-through");
        return write_to_buf(text, out_buf, buf_len);
    }
    if style == crate::llm::LlmStyle::Off && !translate_active {
        log("[LLM] style=off and no translate target — pass-through");
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
                    format!("{}...", crate::truncate_utf8(&enhanced, 120))
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
    let auth_method = st
        .llm_auth_method
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| "api_key".to_string());

    // Subscription mode bypasses the API-key check — the local `claude`
    // CLI carries auth via the user's saved Anthropic OAuth token in
    // ~/.claude/credentials.json. The legacy `claude-code://` URL is
    // also a "no API key" signal, kept for back-compat with configs
    // from the first iteration.
    let is_subscription = auth_method == "subscription"
        || crate::claude_code::is_claude_code_url(&llm_url)
        || crate::codex::is_codex_url(&llm_url);

    // Resolve the dispatch API key. The LLM vendor is derived from
    // `llm_url`; with `use_same_key=true` we try the per-vendor
    // upstream keystore (Llm-scope first, then Stt-scope as a
    // cross-layer fallback). With `use_same_key=false` we only use
    // the dedicated Llm-scope entry. This mirrors the recap
    // dispatcher's per-vendor lookup and decouples "reuse saved key"
    // from "STT and LLM must be the same provider" — a user with a
    // Groq STT key and Anthropic LLM can no longer accidentally
    // pull the Groq key into the Anthropic call.
    let api_key = if is_subscription {
        String::new()
    } else {
        let llm_vendor = Provider::from_url(&llm_url);
        let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);
        let raw = if use_same_key {
            let llm_scope =
                crate::load_key_with_store(&st.key_store, KeyringScope::Llm(llm_vendor), use_kr)
                    .unwrap_or_default();
            if !llm_scope.is_empty() {
                llm_scope
            } else {
                // Cross-layer skip — pull the STT-scope key for the
                // SAME vendor (user saved a Gemini STT key, now uses
                // Gemini for LLM too).
                crate::load_key_with_store(&st.key_store, KeyringScope::Stt(llm_vendor), use_kr)
                    .unwrap_or_default()
            }
        } else {
            crate::load_key_with_store(&st.key_store, KeyringScope::Llm(llm_vendor), use_kr)
                .unwrap_or_default()
        };
        if raw.is_empty() {
            log("ERROR: dimmy_process_with_llm: no LLM API key available");
            emit_event("error", r#"{"message":"No LLM API key configured"}"#);
            // Return original text on key error (graceful degradation)
            return write_to_buf(text, out_buf, buf_len);
        }
        raw
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
        &auth_method,
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

/// Command Mode: transform (or replace) the user's currently-selected
/// text using their spoken instruction. The host captures the selection
/// at hotkey-press and the transcript after STT, then calls this with
/// both. The selection is OPTIONAL: with one we build a transform/replace
/// prompt (`llm::build_command_transform_prompt`); without one we build a
/// generate-and-insert prompt (`llm::build_command_generate_prompt`) so the
/// spoken words become content inserted at the cursor. Either way the model
/// decides the sub-case in one call — no host-side heuristic, no extra
/// round-trip. Dispatches through the DICTATION LLM config (fast path),
/// NOT the recap config: command mode is a dictation-adjacent action.
///
/// Return-code contract:
/// ```text
/// >0  bytes written to out_buf (success)
/// -1  invalid input (null ptr, bad UTF-8, empty spoken — selection may be empty)
/// -2  LLM dispatch failed (network / API error)
/// -3  no LLM API key configured (cloud, non-subscription)
/// -4  local mode but the model file is not on disk
/// -5  failed to create the async runtime
/// ```
///
/// # Safety
/// `selection_ptr` and `spoken_ptr` must be valid NUL-terminated C
/// strings; `out_buf` must point to at least `buf_len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_command_transform(
    selection_ptr: *const c_char,
    spoken_ptr: *const c_char,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if selection_ptr.is_null() || spoken_ptr.is_null() || out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    // Selection is OPTIONAL. With a selection → transform/replace it. Without
    // one → the spoken words are a generation instruction whose result gets
    // inserted at the cursor. Empty/whitespace selection collapses to None.
    let selection = match CStr::from_ptr(selection_ptr).to_str() {
        Ok(s) if !s.trim().is_empty() => Some(s.to_string()),
        Ok(_) => None,
        Err(_) => return -1,
    };
    let spoken = match CStr::from_ptr(spoken_ptr).to_str() {
        Ok(s) if !s.trim().is_empty() => s.to_string(),
        _ => return -1,
    };

    let prompt = match selection {
        Some(ref sel) => crate::llm::build_command_transform_prompt(sel, &spoken),
        None => crate::llm::build_command_generate_prompt(&spoken),
    };

    let st = state();
    emit_event("status", r#"{"state":"processing"}"#);

    // ── Local LLM mode ───────────────────────────────────────────
    let llm_mode = st
        .llm_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "cloud".to_string());

    if llm_mode == "local" {
        let model_filename = st
            .local_llm_model
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|_| crate::local_llm::DEFAULT_LLM_MODEL.to_string());
        let model_path = crate::local_llm::model_path(&model_filename);
        if !model_path.is_file() {
            log(&format!(
                "[CmdMode] local mode but model not on disk: {}",
                model_path.display()
            ));
            return -4;
        }
        return match crate::local_llm::process_raw_prompt_local(&model_path, &prompt, 4096) {
            Ok(text) => write_to_buf(text.trim(), out_buf, buf_len),
            Err(e) => {
                log(&format!("[CmdMode] local transform failed: {}", e));
                -2
            }
        };
    }

    // ── Cloud / subscription mode ────────────────────────────────
    // Resolve the dictation LLM dispatch (mirrors dimmy_process_with_llm's
    // cloud branch but without the style/tone/app-rule machinery — command
    // mode carries its own instruction in the prompt). Subscription
    // bypasses the key check via the local `claude` CLI.
    let llm_url = st.llm_api_url.lock().map(|u| u.clone()).unwrap_or_default();
    let llm_model = st
        .llm_api_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_default();
    let auth_method = st
        .llm_auth_method
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| "api_key".to_string());
    let is_subscription = auth_method == "subscription"
        || crate::claude_code::is_claude_code_url(&llm_url)
        || crate::codex::is_codex_url(&llm_url);

    let api_key = if is_subscription {
        String::new()
    } else {
        let llm_vendor = Provider::from_url(&llm_url);
        let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);
        let use_same_key = st.llm_use_same_key.lock().map(|k| *k).unwrap_or(true);
        let raw = if use_same_key {
            let llm_scope =
                crate::load_key_with_store(&st.key_store, KeyringScope::Llm(llm_vendor), use_kr)
                    .unwrap_or_default();
            if !llm_scope.is_empty() {
                llm_scope
            } else {
                crate::load_key_with_store(&st.key_store, KeyringScope::Stt(llm_vendor), use_kr)
                    .unwrap_or_default()
            }
        } else {
            crate::load_key_with_store(&st.key_store, KeyringScope::Llm(llm_vendor), use_kr)
                .unwrap_or_default()
        };
        if raw.is_empty() {
            log("[CmdMode] no LLM API key configured");
            emit_event("error", r#"{"message":"No LLM API key configured"}"#);
            return -3;
        }
        raw
    };

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

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log(&format!("[CmdMode] runtime create failed: {}", e));
            return -5;
        }
    };
    let result = rt.block_on(crate::llm::process_raw_prompt(
        &api_url,
        &api_model,
        &api_key,
        &prompt,
        4096,
        &auth_method,
    ));
    match result {
        Ok(text) => write_to_buf(text.trim(), out_buf, buf_len),
        Err(e) => {
            let err_msg = format!("{}", e);
            log(&format!("[CmdMode] cloud transform failed: {}", err_msg));
            emit_event(
                "error",
                &format!(
                    r#"{{"message":"Command: {}"}}"#,
                    err_msg.replace('"', "\\\"")
                ),
            );
            -2
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
    // Always-mix (2026-05-08). The legacy `audio_source` config field
    // is honoured only for the rate-probe call below where some
    // backward-compat tests still set it explicitly. Capture itself
    // always opens both mic + loopback so the meeting worker has both
    // tracks regardless of what the user had configured pre-refactor.
    let mt_source = crate::audio::AudioSource::Mix;
    let _legacy_source_unused = st.audio_source.lock();
    // Meeting buffers always run at the canonical rate (48 kHz). The
    // cpal callbacks in audio.rs resample from the device-native rate
    // before pushing, so the WAV writer + STT both see a stable rate
    // regardless of the mic/loopback's actual hardware rate AND
    // regardless of any mid-meeting device swap (BT ↔ speakers ↔ USB
    // headset). Pre-resampling design (2026-05-19) used the mic's
    // native rate which broke when a device-swap mid-meeting changed
    // the effective rate without updating the WAV header — audio
    // played back "rallentato" because the writer thought it was
    // still at the original device rate.
    let device_sr = crate::audio::MEETING_CANONICAL_RATE;
    log(&format!(
        "[Meeting] canonical_rate={} source={:?}",
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
        // Compose user's free-form prompt + dict into one biasing
        // string so the meeting worker's STT path (cloud or local
        // Whisper) gets the same vocabulary advantage as live
        // dictation. user_dict isn't a field on SttSnapshot itself —
        // we squash it into `prompt` here at snapshot time.
        prompt: crate::compose_stt_prompt(
            &st.prompt.lock().ok().map(|s| s.clone()).unwrap_or_default(),
            &st.user_dict
                .lock()
                .ok()
                .map(|d| d.clone())
                .unwrap_or_default(),
        ),
        local_model: st
            .local_model
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default(),
        local_backend: st
            .local_stt_backend
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_else(|| "whisper".to_string()),
        language: st
            .language
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_else(|| "auto".to_string()),
        chunk_secs: st.meeting_chunk_secs.lock().ok().map(|s| *s),
    };
    // Both buffers always run at the canonical 48 kHz (see device_sr
    // comment above) — secondary cpal callback resamples too.
    let system_sr = crate::audio::MEETING_CANONICAL_RATE;
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
    emit_meeting_state_event(true, false);
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
    // Notify subscribers BEFORE the (potentially seconds-long) drain
    // happens — the pill should leave the meeting-active visual the
    // instant the user clicked Stop, not after the worker finishes
    // its last STT chunk.
    emit_meeting_state_event(false, false);
    // Re-arm the call-detector stop-suggestion path so a NEXT meeting
    // started from another detection isn't sitting on stale flags.
    {
        let mut g = call_detector_lock();
        g.meeting_stopped();
    }

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

/// Pause the in-flight meeting. While paused, the meeting worker
/// stops draining the audio buffers, stops writing to the WAV files,
/// and stops emitting STT chunks. cpal callbacks keep firing — we
/// don't bounce the streams (avoids a device-acquisition race on
/// resume). On resume, the worker advances its write/read cursors
/// past the paused window so the gap doesn't end up in the audio or
/// transcript timeline; a `[paused N ms]` marker lands in
/// transcripts.txt at the seam so the recap LLM sees the gap.
///
/// Returns:
///   1  state flipped (meeting was running, now paused)
///   0  no-op (meeting was already paused, or no meeting active)
///  -1  internal lock failure
#[no_mangle]
pub extern "C" fn dimmy_meeting_pause() -> c_int {
    match MEETING.lock() {
        Ok(g) => match g.as_ref() {
            Some(s) => {
                if s.pause() {
                    emit_meeting_state_event(true, true);
                    1
                } else {
                    0
                }
            }
            None => 0,
        },
        Err(_) => -1,
    }
}

/// Resume a paused meeting. Idempotent: returns 0 if the meeting
/// wasn't paused. See dimmy_meeting_pause for the gap-skip semantics.
#[no_mangle]
pub extern "C" fn dimmy_meeting_resume() -> c_int {
    match MEETING.lock() {
        Ok(g) => match g.as_ref() {
            Some(s) => {
                if s.resume() {
                    emit_meeting_state_event(true, false);
                    1
                } else {
                    0
                }
            }
            None => 0,
        },
        Err(_) => -1,
    }
}

/// Returns 1 if the active meeting is paused, 0 otherwise (including
/// no meeting active).
#[no_mangle]
pub extern "C" fn dimmy_meeting_is_paused() -> c_int {
    match MEETING.lock() {
        Ok(g) => match g.as_ref() {
            Some(s) => s.is_paused() as c_int,
            None => 0,
        },
        Err(_) => 0,
    }
}

// ── Custom dictionary (user vocabulary) ────────────────────────────
// User curates a list of words / short phrases that get injected as
// initial-prompt context into every STT call. Boosts recognition of
// names, jargon, brand terms the speech-to-text models would otherwise
// guess phonetically (Wispr Flow-equivalent feature). Lives on disk in
// config.json under `user_dict` so it's portable across machines via
// the same backup path users already have.
//
// Three FFI surfaces here:
//   - dimmy_user_dict_add(word)        — append + dedupe + persist
//   - dimmy_user_dict_remove(word)     — drop matches + persist
//   - dimmy_user_dict_list_json(...)   — read for the settings UI
//
// Bulk replacement (Settings page edit-everything-at-once) still goes
// through `dimmy_set_config_json` with a `user_dict: [...]` field —
// the dedicated add/remove paths exist for the global-hotkey flow so
// a 500-word dict doesn't get re-serialised on every keystroke.

/// Append `word` to the user dictionary if not already present (case-
/// insensitive de-dup). Persists immediately so a crash doesn't lose
/// the addition. Empty / whitespace-only words are rejected without
/// modifying state. Returns:
///   0  added (new word)
///   1  already present (no-op)
///  -1  invalid args (null / non-UTF-8) or persistence failure
///
/// # Safety
/// `word_ptr` must be a valid null-terminated UTF-8 C string. NULL → -1.
#[no_mangle]
pub unsafe extern "C" fn dimmy_user_dict_add(word_ptr: *const c_char) -> c_int {
    if word_ptr.is_null() {
        return -1;
    }
    let word = match unsafe { CStr::from_ptr(word_ptr) }.to_str() {
        Ok(s) => s.trim().to_string(),
        Err(_) => return -1,
    };
    if word.is_empty() {
        return -1;
    }
    let st = state();
    let mut already_present = false;
    if let Ok(mut d) = st.user_dict.lock() {
        if d.iter().any(|w| w.eq_ignore_ascii_case(&word)) {
            already_present = true;
        } else {
            d.push(word.clone());
        }
    } else {
        return -1;
    }
    if !already_present {
        if let Ok(cfg) = crate::snapshot_config(st) {
            save_config_file(&cfg);
        } else {
            return -1;
        }
        log(&format!(
            "[UserDict] added '{}' ({} chars)",
            word,
            word.len()
        ));
        0
    } else {
        log(&format!("[UserDict] '{}' already present — no-op", word));
        1
    }
}

/// Remove `word` from the user dictionary (case-insensitive match).
/// Persists immediately. Returns the count of entries dropped (0 = no
/// match), or -1 on invalid args / lock failure / persistence failure.
///
/// # Safety
/// `word_ptr` must be a valid null-terminated UTF-8 C string. NULL → -1.
#[no_mangle]
pub unsafe extern "C" fn dimmy_user_dict_remove(word_ptr: *const c_char) -> c_int {
    if word_ptr.is_null() {
        return -1;
    }
    let word = match unsafe { CStr::from_ptr(word_ptr) }.to_str() {
        Ok(s) => s.trim().to_string(),
        Err(_) => return -1,
    };
    if word.is_empty() {
        return -1;
    }
    let st = state();
    let removed_count = match st.user_dict.lock() {
        Ok(mut d) => {
            let before = d.len();
            d.retain(|w| !w.eq_ignore_ascii_case(&word));
            (before - d.len()) as c_int
        }
        Err(_) => return -1,
    };
    if removed_count > 0 {
        if let Ok(cfg) = crate::snapshot_config(st) {
            save_config_file(&cfg);
        } else {
            return -1;
        }
        log(&format!(
            "[UserDict] removed '{}' ({} entries)",
            word, removed_count
        ));
    }
    removed_count
}

/// Write the current dictionary to `out_buf` as a JSON array of
/// strings (e.g. `["foobar","Notion","ROM-O"]`). Returns the byte
/// length written, -1 on invalid args / lock failure, or -2 if the
/// JSON exceeds `buf_len` (caller should retry with a larger buffer).
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_user_dict_list_json(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let st = state();
    let json = match st.user_dict.lock() {
        Ok(d) => match serde_json::to_string(&*d) {
            Ok(j) => j,
            Err(_) => return -1,
        },
        Err(_) => return -1,
    };
    // Honour the documented contract: -2 when the buffer is too small,
    // so the C# / Swift wrappers can retry with a larger one instead of
    // silently receiving a truncated, invalid JSON array. `write_to_buf`
    // alone would truncate to `buf_len - 1` and return that length,
    // which the caller would then try to `JSON.parse` and crash on.
    if json.len() + 1 > buf_len as usize {
        return -2;
    }
    write_to_buf(&json, out_buf, buf_len)
}

// ── Claude Code (subscription login) ──────────────────────────────
// Use the user's Anthropic Pro / Team / Max subscription for LLM
// calls instead of consuming API-key credits. Delegates auth to
// Anthropic's official `claude` CLI (which handles browser-based
// OAuth) and dispatches every LLM call through a subprocess. See
// `core/src/claude_code.rs` for the full design.

/// Probe local Claude Code state. Returns:
///   0 = ready  (binary installed + credentials present)
///   1 = installed but not logged in
///   2 = not installed
///  -1 = lock/IO failure during probe
#[no_mangle]
pub extern "C" fn dimmy_claude_code_status() -> c_int {
    let s = crate::claude_code::status();
    crate::telemetry::track(crate::telemetry::Event::ClaudeCodeStatusProbed {
        status: crate::claude_code::status_label(&s),
    });
    s.as_code()
}

/// Diagnostic snapshot of the CLI-detection path search. JSON with
/// every candidate path tried, which existed, whether the login-
/// shell fallback resolved one, and whether credentials are
/// readable. Used by the Settings → About → "Diagnostics" button
/// to surface why detection failed (most common: GUI app PATH
/// doesn't include the user's Node-manager bin dir).
///
/// Returns bytes written, or -1 on null buf / -2 on too-small buf.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_claude_code_diagnostics(
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let json = crate::claude_code::diagnostics_json();
    if json.len() + 1 > buf_len as usize {
        return -2;
    }
    write_to_buf(&json, out_buf, buf_len)
}

/// Return the resolved Claude Code binary path into `out_buf` (as
/// a NUL-terminated UTF-8 string). Length of bytes written (without
/// NUL) on success, 0 if not installed, -1 on invalid args / too-
/// small buffer.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_claude_code_binary_path(
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    match crate::claude_code::detect_binary() {
        Some(p) => {
            let s = p.to_string_lossy().to_string();
            if s.len() + 1 > buf_len as usize {
                return -1;
            }
            write_to_buf(&s, out_buf, buf_len)
        }
        None => 0,
    }
}

/// Spawn `claude /login` so the user can authenticate via browser.
/// Detached subprocess — returns 0 once the spawn succeeded, -1 on
/// missing binary, -2 on spawn failure. The actual login completion
/// is detected by polling `dimmy_claude_code_status()` from the host.
#[no_mangle]
pub extern "C" fn dimmy_claude_code_spawn_login() -> c_int {
    match crate::claude_code::spawn_login() {
        Ok(()) => 0,
        Err(crate::claude_code::ClaudeCodeError::NotInstalled) => -1,
        Err(_) => -2,
    }
}

/// Run a small "ping" round-trip through the local `claude` CLI to
/// verify the subprocess can dispatch a real call. The host uses this
/// for the Settings "Test connection" button — gives the user a
/// concrete success signal that the binary works, the credentials
/// are alive, and a sample request completes within ~15 s.
///
/// The prompt is hard-coded to `"reply with the single word: pong"`
/// so the user's recent transcripts can NEVER end up on the wire
/// here — the test is observably content-free.
///
/// Returns the elapsed milliseconds on success (always > 0). Negative
/// values are categorical error codes:
///   -1 = not installed (binary missing)
///   -2 = not logged in (credentials missing)
///   -3 = subprocess spawn failure (OS-level)
///   -4 = invocation timeout (10 s ceiling)
///   -5 = non-zero exit from `claude`
///   -6 = stdout was not valid UTF-8
///
/// Side-effect: emits `claude_code.invocation` with kind="test" so
/// dashboards can distinguish ping invocations from real recap /
/// rewrite calls.
#[no_mangle]
pub extern "C" fn dimmy_claude_code_ping() -> c_int {
    use crate::claude_code::{error_category, ClaudeCodeError};
    let started = std::time::Instant::now();
    let result = crate::claude_code::run_blocking(
        "reply with the single word: pong",
        "",
        std::time::Duration::from_secs(15),
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (success, category) = match &result {
        Ok(_) => (true, "ok"),
        Err(e) => (false, error_category(e)),
    };
    crate::telemetry::track(crate::telemetry::Event::ClaudeCodeInvocation {
        kind: "test",
        processing_ms_bucket: crate::telemetry::sanitize::bucket_processing_ms(elapsed_ms),
        success,
        error_category: category,
    });
    match result {
        Ok(_) => {
            // Clamp to fit i32; a successful round-trip well under
            // 60 s never overflows but keep the contract explicit.
            let ms = elapsed_ms.min(i32::MAX as u64) as c_int;
            // Never return 0 from success — the host distinguishes
            // success-with-zero-elapsed from error states.
            ms.max(1)
        }
        Err(ClaudeCodeError::NotInstalled) => -1,
        Err(ClaudeCodeError::NotLoggedIn) => -2,
        Err(ClaudeCodeError::Spawn(_)) => -3,
        Err(ClaudeCodeError::Timeout) => -4,
        Err(ClaudeCodeError::NonZeroExit { .. }) => -5,
        Err(ClaudeCodeError::InvalidUtf8) => -6,
    }
}

/// Probe for a Node.js install. Returns a JSON envelope the setup
/// wizard binds to:
/// ```json
/// {
///   "found": true,
///   "path": "/opt/homebrew/bin/node",
///   "version": "22.5.1",
///   "major": 22,
///   "meets_minimum": true
/// }
/// ```
/// `meets_minimum` checks `major >= 18` — the minimum runtime version
/// claude-code requires; under it `npm install -g @anthropic-ai/claude-code`
/// fails with EBADENGINE and the wizard surfaces "please upgrade".
///
/// When Node isn't found: `{"found": false}` (other fields omitted).
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_claude_code_node_status(
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    const MIN_NODE_MAJOR: u32 = 18;
    let json = match crate::claude_code::detect_node_binary() {
        Some(path) => {
            let version = crate::claude_code::node_version();
            let major = version
                .as_deref()
                .and_then(crate::claude_code::parse_node_major);
            let meets = major.map(|m| m >= MIN_NODE_MAJOR).unwrap_or(false);
            serde_json::json!({
                "found": true,
                "path": path.to_string_lossy(),
                "version": version,
                "major": major,
                "meets_minimum": meets,
            })
        }
        None => serde_json::json!({ "found": false }),
    };
    write_to_buf(&json.to_string(), out_buf, buf_len)
}

/// Invalidate every cached binary lookup (claude + node) and return
/// the fresh `dimmy_claude_code_status()` code. Used by the setup
/// wizard's "I installed it, recheck" buttons after the user reports
/// an install completed — without this, Dimmy keeps the stale "not
/// found" answer until restart because `detect_binary()` caches its
/// first negative result for the process lifetime.
#[no_mangle]
pub extern "C" fn dimmy_claude_code_recheck() -> c_int {
    crate::claude_code::clear_cache();
    let s = crate::claude_code::status();
    s.as_code()
}

// ── Codex (OpenAI / ChatGPT subscription) CLI bridge ───────────────
// Direct mirror of the Claude Code FFI above. Same integer contracts so
// the C# / Swift hosts can share the status-card logic.

/// Probe local Codex state. 0 = ready, 1 = installed-not-logged-in,
/// 2 = not installed.
#[no_mangle]
pub extern "C" fn dimmy_codex_status() -> c_int {
    let s = crate::codex::status();
    crate::telemetry::track(crate::telemetry::Event::CodexStatusProbed {
        status: crate::codex::status_label(&s),
    });
    s.as_code()
}

/// Diagnostic snapshot of the Codex CLI-detection path search.
/// Returns bytes written, -1 on null buf, -2 on too-small buf.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_codex_diagnostics(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let json = crate::codex::diagnostics_json();
    if json.len() + 1 > buf_len as usize {
        return -2;
    }
    write_to_buf(&json, out_buf, buf_len)
}

/// Resolved Codex binary path into `out_buf`. Length on success, 0 if
/// not installed, -1 on invalid args / too-small buffer.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_codex_binary_path(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    match crate::codex::detect_binary() {
        Some(p) => {
            let s = p.to_string_lossy().to_string();
            if s.len() + 1 > buf_len as usize {
                return -1;
            }
            write_to_buf(&s, out_buf, buf_len)
        }
        None => 0,
    }
}

/// Spawn `codex login` so the user authenticates with ChatGPT via
/// browser. 0 = spawned, -1 = missing binary, -2 = spawn failure.
#[no_mangle]
pub extern "C" fn dimmy_codex_spawn_login() -> c_int {
    match crate::codex::spawn_login() {
        Ok(()) => 0,
        Err(crate::codex::CodexError::NotInstalled) => -1,
        Err(_) => -2,
    }
}

/// "Test connection" round-trip through `codex exec`. Returns elapsed
/// ms on success (> 0). Negative = categorical error:
///   -1 not installed, -2 not logged in, -3 spawn failure,
///   -4 timeout, -5 non-zero exit, -6 stdout not UTF-8.
/// The prompt is hard-coded ("reply with the single word: pong") so no
/// user content can reach the wire here.
#[no_mangle]
pub extern "C" fn dimmy_codex_ping() -> c_int {
    use crate::codex::{error_category, CodexError};
    let started = std::time::Instant::now();
    let result = crate::codex::run_blocking(
        "reply with the single word: pong",
        "",
        std::time::Duration::from_secs(30),
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (success, category) = match &result {
        Ok(_) => (true, "ok"),
        Err(e) => (false, error_category(e)),
    };
    crate::telemetry::track(crate::telemetry::Event::CodexInvocation {
        kind: "test",
        processing_ms_bucket: crate::telemetry::sanitize::bucket_processing_ms(elapsed_ms),
        success,
        error_category: category,
    });
    match result {
        Ok(_) => (elapsed_ms.min(i32::MAX as u64) as c_int).max(1),
        Err(CodexError::NotInstalled) => -1,
        Err(CodexError::NotLoggedIn) => -2,
        Err(CodexError::Spawn(_)) => -3,
        Err(CodexError::Timeout) => -4,
        Err(CodexError::NonZeroExit { .. }) => -5,
        Err(CodexError::InvalidUtf8) => -6,
    }
}

/// Invalidate the cached Codex binary lookup + return the fresh
/// `dimmy_codex_status()` code. Used by the Settings "recheck" button.
#[no_mangle]
pub extern "C" fn dimmy_codex_recheck() -> c_int {
    crate::codex::clear_cache();
    crate::codex::status().as_code()
}

// ── Claude Desktop MCP bridge ──────────────────────────────────────
// Counterpart to the standalone `dimmy-mcp` binary (mcp-server/). The
// wizard flow needs to detect the Claude Desktop install, patch the
// user's `claude_desktop_config.json` to register our binary, and
// expose heartbeat + recent-call telemetry so the Settings card can
// render a live status. See `core/src/claude_desktop.rs` for the
// detection + patch logic, and `mcp-server/` for the binary itself.

/// One-shot status snapshot for the Settings card + wizard. JSON:
/// ```json
/// {
///   "installed": true,
///   "install_path": "/Applications/Claude.app",
///   "config_path": "/Users/k/Library/Application Support/Claude/claude_desktop_config.json",
///   "config_patched": true,
///   "entry_command": "/Applications/Dimmy.app/.../dimmy-mcp",
///   "heartbeat_age_secs": 12,
///   "last_call": { "tool": "dimmy_get_recent_meetings", "ago_secs": 47, "ok": true }
/// }
/// ```
/// Missing pieces are omitted, never null — keeps the UI binding
/// simple ("field present" ⇔ "fact known").
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_claude_desktop_status(
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    // Compile-time namespace (see `_install`) — must match the dir the
    // extension was installed under, else status reports "not installed"
    // on staging even when it is.
    let namespace = crate::config_dir_name().to_string();

    let install = crate::claude_desktop::detect_claude_desktop();
    let ext_dir = crate::claude_desktop::extensions_root()
        .map(|r| r.join(crate::claude_desktop::extension_id(&namespace)));
    let manifest = crate::claude_desktop::read_installed_manifest(&namespace);
    let enabled = crate::claude_desktop::is_extension_enabled(&namespace);

    let mut obj = serde_json::Map::new();
    obj.insert("installed".into(), install.is_some().into());
    if let Some(p) = &install {
        obj.insert(
            "install_path".into(),
            p.to_string_lossy().into_owned().into(),
        );
    }
    if let Some(p) = &ext_dir {
        obj.insert(
            "extension_path".into(),
            p.to_string_lossy().into_owned().into(),
        );
    }
    // `config_patched` is the legacy field name from the previous
    // wizard era; we keep it as a host-side compat alias meaning
    // "Dimmy is registered with Claude" so the C#/Swift bindings
    // don't have to coordinate-rename. `extension_installed` is the
    // new canonical field.
    obj.insert("config_patched".into(), manifest.is_some().into());
    obj.insert("extension_installed".into(), manifest.is_some().into());
    obj.insert("extension_enabled".into(), enabled.into());
    if let Some(m) = &manifest {
        if let Some(v) = m.get("version").and_then(|v| v.as_str()) {
            obj.insert("extension_version".into(), v.to_string().into());
        }
        if let Some(cmd) = m
            .pointer("/server/mcp_config/command")
            .and_then(|v| v.as_str())
        {
            obj.insert("entry_command".into(), cmd.to_string().into());
        }
    }

    // Heartbeat + last-call live in our own config dir (where the
    // dimmy-mcp binary writes them), not Claude's. Resolve via the
    // same helper the rest of FFI uses.
    if let Some(dir) = crate::config_dir_path() {
        if let Some(hb) = crate::claude_desktop::read_heartbeat(&dir) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let age = now.saturating_sub(hb.timestamp);
            obj.insert("heartbeat_age_secs".into(), age.into());
        }
        let recent = crate::claude_desktop::read_recent_calls(&dir, 1);
        if let Some(c) = recent.first() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let ago = now.saturating_sub(c.ts);
            obj.insert(
                "last_call".into(),
                serde_json::json!({
                    "tool": c.tool,
                    "ago_secs": ago,
                    "ok": c.ok,
                    "elapsed_ms": c.elapsed_ms,
                }),
            );
        }
    }

    let json = serde_json::Value::Object(obj).to_string();
    write_to_buf(&json, out_buf, buf_len)
}

/// Install Dimmy as a Claude Desktop extension. `binary_path` must
/// be a NUL-terminated UTF-8 C string pointing at the dimmy-mcp
/// binary on disk; we copy it into the extension dir alongside the
/// manifest + icon. `version_ptr` is the host's version string
/// (env!("CARGO_PKG_VERSION") from the host's own build) — embedded
/// in the manifest so the Claude Connectors UI shows the version
/// next to the entry.
///
/// Returns 0 on success, negative on error:
///   -1 = null arg / invalid UTF-8 / extension root unresolved
///   -2 = binary missing at source path
///   -3 = copy failed (target locked? disk full? permissions?)
///   -4 = manifest/settings write failed
///   -5 = manifest serialization failed
///
/// Side-effect: also wipes any legacy `mcpServers.dimmy` entry
/// inside `claude_desktop_config.json` left over from the
/// patch_config era (in-development period only) so the user ends
/// up with a single source of truth.
///
/// # Safety
/// Both pointers must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn dimmy_claude_desktop_install(
    binary_path: *const c_char,
    version_ptr: *const c_char,
) -> c_int {
    if binary_path.is_null() || version_ptr.is_null() {
        return -1;
    }
    let path_str = match CStr::from_ptr(binary_path).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return -1,
    };
    let version_str = match CStr::from_ptr(version_ptr).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return -1,
    };
    // MUST be the compile-time `config_dir_name()`, NOT a runtime
    // `std::env::var` read. The .app process has no `DIMMY_CONFIG_NAMESPACE`
    // in its environment at runtime (it's baked at build via `option_env!`),
    // so reading the env here always defaulted to "dimmy" — even on a
    // staging-tester build that writes meetings under `dimmy-staging`. That
    // baked the wrong namespace into the Claude Desktop manifest, so the MCP
    // looked in `dimmy/meetings`, found nothing, and recaps written by Claude
    // (incl. the meeting title) landed in a phantom dir the app never reads.
    let namespace = crate::config_dir_name();
    match crate::claude_desktop::install_extension(
        std::path::Path::new(path_str),
        version_str,
        namespace,
    ) {
        Ok(_) => 0,
        Err(crate::claude_desktop::InstallError::NoExtensionsRoot) => -1,
        Err(crate::claude_desktop::InstallError::BinaryMissing) => -2,
        Err(crate::claude_desktop::InstallError::CopyFailed(_)) => -3,
        Err(crate::claude_desktop::InstallError::WriteFailed(_)) => -4,
        Err(crate::claude_desktop::InstallError::ParseFailed(_)) => -5,
    }
}

/// Remove the Dimmy Claude Desktop extension (the dir + its
/// per-extension settings file). Idempotent — returns 1 if a real
/// directory was removed, 0 if there was nothing to remove (no
/// install), negative on error. Same error codes as `_install`.
///
/// Side-effect: also wipes any leftover legacy `mcpServers.dimmy`
/// entry in `claude_desktop_config.json` so the user ends up with
/// a clean state regardless of which wizard era they installed in.
#[no_mangle]
pub extern "C" fn dimmy_claude_desktop_uninstall() -> c_int {
    // Same compile-time namespace as `_install` — see the note there.
    let namespace = crate::config_dir_name();
    match crate::claude_desktop::uninstall_extension(namespace) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(crate::claude_desktop::InstallError::NoExtensionsRoot) => -1,
        Err(crate::claude_desktop::InstallError::BinaryMissing) => -2,
        Err(crate::claude_desktop::InstallError::CopyFailed(_)) => -3,
        Err(crate::claude_desktop::InstallError::WriteFailed(_)) => -4,
        Err(crate::claude_desktop::InstallError::ParseFailed(_)) => -5,
    }
}

// ── Notion integration ─────────────────────────────────────────────
// Internal-integration-token model: user pastes a `ntn_...` token
// from notion.so/my-integrations into the Settings UI; we store it
// in the AES-256 keystore alongside STT/LLM keys, then use it to
// send meeting recaps via the Notion REST API. See `notion.rs`.

/// Set the Notion integration token. Empty token = clear (no upload
/// possible until re-set). Returns 0 on success, -1 on lock failure.
///
/// # Safety
/// `token_ptr` must be a valid null-terminated UTF-8 C string. NULL is
/// rejected (-1). Empty string clears the stored token.
#[no_mangle]
pub unsafe extern "C" fn dimmy_notion_set_token(token_ptr: *const c_char) -> c_int {
    if token_ptr.is_null() {
        return -1;
    }
    let token = match unsafe { CStr::from_ptr(token_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let st = state();
    let result = if token.is_empty() {
        save_key_with_store(&st.key_store, KeyringScope::NotionToken, "", false)
    } else {
        save_key_with_store(&st.key_store, KeyringScope::NotionToken, token, false)
    };
    match result {
        Ok(_) => {
            log(&format!(
                "[Notion] token {} ({} chars)",
                if token.is_empty() { "cleared" } else { "set" },
                token.len()
            ));
            0
        }
        Err(e) => {
            log(&format!("[Notion] save_key failed: {}", e));
            -1
        }
    }
}

/// Returns 1 if a Notion token is currently stored, 0 otherwise.
#[no_mangle]
pub extern "C" fn dimmy_notion_has_token() -> c_int {
    let st = state();
    let token = crate::load_key_with_store(
        &st.key_store,
        crate::provider::KeyringScope::NotionToken,
        false,
    );
    if token.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
        1
    } else {
        0
    }
}

/// Save an API key for a specific provider into a specific keystore
/// scope, WITHOUT changing the dictation `llm_api_url`. Used by the
/// Recap LLM Settings section to save a key for a vendor different
/// from the dictation provider, or a second key for the SAME vendor
/// (when the recap toggle "Use my saved <vendor> key" is OFF).
///
/// `scope_ptr` must be one of: "llm" (KeyringScope::Llm) or "recap"
/// (KeyringScope::Recap). Anything else is rejected.
/// `provider_ptr` must be a lowercase tag accepted by
/// `Provider::from_tag` AND have a `default_llm_url` mapping (so:
/// anthropic, openai, gemini, groq, openrouter, fireworks, together).
/// Local / Custom / Deepgram are rejected (no LLM completions
/// endpoint).
///
/// Returns:
///   0  = saved (or cleared)
///   -1 = invalid arg (NULL ptr, bad UTF-8, unknown scope, unknown / unmapped vendor)
///   -2 = keystore write failed
///
/// # Safety
/// All three pointers must be valid null-terminated UTF-8 C strings.
/// NULL is rejected (-1). Empty `key` clears the stored key for that
/// (scope, vendor) pair.
#[no_mangle]
pub unsafe extern "C" fn dimmy_save_llm_provider_key(
    scope_ptr: *const c_char,
    provider_ptr: *const c_char,
    key_ptr: *const c_char,
) -> c_int {
    if scope_ptr.is_null() || provider_ptr.is_null() || key_ptr.is_null() {
        return -1;
    }
    let scope_tag = match unsafe { CStr::from_ptr(scope_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let provider_tag = match unsafe { CStr::from_ptr(provider_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let key = match unsafe { CStr::from_ptr(key_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let provider = match Provider::from_tag(provider_tag) {
        Some(p) => p,
        None => {
            log(&format!(
                "[Keystore] rejecting save for unknown provider tag '{}'",
                provider_tag
            ));
            return -1;
        }
    };
    // Scope is validated against the provider's capabilities: "stt" only for
    // speech-capable vendors (Deepgram is STT-only and has no LLM URL, but is
    // valid here), "llm"/"recap" only for vendors with a completions endpoint.
    let scope = match scope_tag {
        "stt" if provider.supports_stt() => KeyringScope::Stt(provider),
        "llm" if provider.default_llm_url().is_some() => KeyringScope::Llm(provider),
        "recap" if provider.default_llm_url().is_some() => KeyringScope::Recap(provider),
        _ => {
            log(&format!(
                "[Keystore] rejecting scope '{}' for provider '{}' (capability mismatch)",
                scope_tag, provider_tag
            ));
            return -1;
        }
    };
    let st = state();
    let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);
    match save_key_with_store(&st.key_store, scope, key, use_kr) {
        Ok(_) => {
            log(&format!(
                "[Keystore] {} key {} for provider={}",
                scope_tag,
                if key.is_empty() { "cleared" } else { "saved" },
                provider.as_str()
            ));
            0
        }
        Err(e) => {
            log(&format!(
                "[Keystore] save failed for scope={} provider={}: {}",
                scope_tag,
                provider.as_str(),
                e
            ));
            -2
        }
    }
}

/// Verify the stored Notion token by calling /v1/users/me. Writes a
/// JSON envelope to `out_buf`:
///   `{"ok":true,"bot_name":"Dimmy","workspace_name":"Konrad's Workspace"}`
/// or
///   `{"ok":false,"error":"Notion API 401: ..."}`
///
/// Returns the JSON length on success, -1 on invalid args / no token.
///
/// # Safety
/// `out_buf` must be a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_notion_test_connection(
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let st = state();
    let token = match crate::load_key_with_store(
        &st.key_store,
        crate::provider::KeyringScope::NotionToken,
        false,
    ) {
        Some(t) if !t.is_empty() => t,
        _ => {
            let json = r#"{"ok":false,"error":"Notion token not set"}"#;
            return write_to_buf(json, out_buf, buf_len);
        }
    };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(_) => return -1,
    };
    let result = rt.block_on(crate::notion::ping(&token));
    let json = match result {
        Ok(info) => serde_json::json!({
            "ok": true,
            "bot_name": info.bot_name,
            "workspace_name": info.workspace_name,
        })
        .to_string(),
        Err(e) => serde_json::json!({
            "ok": false,
            "error": format!("{}", e),
        })
        .to_string(),
    };
    write_to_buf(&json, out_buf, buf_len)
}

/// Search Notion for pages + databases the integration has access to.
/// Used by the Settings picker. Writes JSON array:
///   `[{"id":"...","object":"page","title":"Notes","parent_label":"Workspace","url":"..."}, ...]`
/// Returns the JSON length on success, -1 on invalid args / no token,
/// or writes a `{"error":"..."}` envelope on API failure with length.
///
/// # Safety
/// `query_ptr` and `out_buf` must be valid C string / writable buffer.
#[no_mangle]
pub unsafe extern "C" fn dimmy_notion_search(
    query_ptr: *const c_char,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let query = if query_ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(query_ptr) }
            .to_str()
            .unwrap_or("")
            .to_string()
    };
    let st = state();
    let token = match crate::load_key_with_store(
        &st.key_store,
        crate::provider::KeyringScope::NotionToken,
        false,
    ) {
        Some(t) if !t.is_empty() => t,
        _ => {
            let json = r#"{"error":"Notion token not set"}"#;
            return write_to_buf(json, out_buf, buf_len);
        }
    };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(_) => return -1,
    };
    let result = rt.block_on(crate::notion::search(&token, &query));
    let json = match result {
        Ok(items) => serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string()),
        Err(e) => serde_json::json!({
            "error": format!("{}", e),
        })
        .to_string(),
    };
    write_to_buf(&json, out_buf, buf_len)
}

/// Send a meeting's recap to the configured Notion target. Reads
/// `recap.md` from the meeting dir; falls back to `transcripts.txt` +
/// regenerated recap if recap.md doesn't exist (caller's
/// responsibility to ensure recap was produced first).
///
/// Writes a JSON envelope to `out_buf`:
///   `{"ok":true,"page_id":"...","page_url":"https://www.notion.so/..."}`
/// or
///   `{"ok":false,"error":"..."}`
///
/// Returns the JSON length, or -1 on invalid args.
///
/// # Safety
/// `meeting_dir_ptr` and `out_buf` must be valid C string / writable
/// buffer.
#[no_mangle]
pub unsafe extern "C" fn dimmy_notion_send_recap(
    meeting_dir_ptr: *const c_char,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if meeting_dir_ptr.is_null() || out_buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let dir_str = match unsafe { CStr::from_ptr(meeting_dir_ptr) }.to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return -1,
    };
    let dir = std::path::PathBuf::from(dir_str);
    let st = state();

    let token = match crate::load_key_with_store(
        &st.key_store,
        crate::provider::KeyringScope::NotionToken,
        false,
    ) {
        Some(t) if !t.is_empty() => t,
        _ => {
            let json = r#"{"ok":false,"error":"Notion token not set"}"#;
            return write_to_buf(json, out_buf, buf_len);
        }
    };

    let target_id = st
        .notion_target_id
        .lock()
        .ok()
        .map(|s| s.clone())
        .unwrap_or_default();
    if target_id.is_empty() {
        let json = r#"{"ok":false,"error":"Notion target page not configured"}"#;
        return write_to_buf(json, out_buf, buf_len);
    }
    let target_kind = st
        .notion_target_kind
        .lock()
        .ok()
        .map(|s| s.clone())
        .unwrap_or_else(|| "page".to_string());
    let target_title = st
        .notion_target_title
        .lock()
        .ok()
        .map(|s| s.clone())
        .unwrap_or_default();
    let target = crate::notion::NotionTarget {
        id: target_id,
        kind: target_kind,
        title: target_title,
    };

    let recap_path = dir.join("recap.md");
    let markdown = match std::fs::read_to_string(&recap_path) {
        Ok(s) => s,
        Err(_) => {
            let json = r#"{"ok":false,"error":"recap.md not found in meeting dir"}"#;
            return write_to_buf(json, out_buf, buf_len);
        }
    };
    if markdown.trim().is_empty() {
        let json = r#"{"ok":false,"error":"recap.md is empty"}"#;
        return write_to_buf(json, out_buf, buf_len);
    }

    // Title: prefer transcripts.txt content for a hint of subject;
    // fall back to meta.json timestamp via meeting_dir_to_title.
    let transcript = std::fs::read_to_string(dir.join("transcripts.txt")).unwrap_or_default();
    let title = crate::notion::meeting_dir_to_title(&dir, &transcript);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(_) => return -1,
    };
    let result = rt.block_on(crate::notion::send_meeting_recap(
        &token, &title, &markdown, &target,
    ));
    let json = match result {
        Ok(page) => {
            log(&format!(
                "[Notion] recap sent for {} → {}",
                dir.display(),
                page.url
            ));
            serde_json::json!({
                "ok": true,
                "page_id": page.id,
                "page_url": page.url,
            })
            .to_string()
        }
        Err(e) => {
            log(&format!("[Notion] send failed: {}", e));
            serde_json::json!({
                "ok": false,
                "error": format!("{}", e),
            })
            .to_string()
        }
    };
    write_to_buf(&json, out_buf, buf_len)
}

/// Parse a `recap_model_override` config value into (provider, model).
///
/// Accepted shapes:
/// - `"local:<filename.gguf>"` → (`"local"`, `<filename.gguf>`)
/// - `"cloud:<model-id>"`      → (`"cloud"`, `<model-id>`)
/// - non-empty bare string     → (`"cloud"`, `<string>`)
///   Legacy path: every override written before the local-recap
///   feature was a cloud model id, so we keep that semantic.
/// - empty                     → (`""`, `""`)
///   Caller falls back to `llm_mode` + the configured dictation
///   `api_model` / `local_llm_model`.
///
/// Extracted so the routing logic gets unit-tested without standing
/// up the full FFI state machine.
pub(crate) fn parse_recap_override(input: &str) -> (&'static str, String) {
    if let Some(rest) = input.strip_prefix("local:") {
        ("local", rest.to_string())
    } else if let Some(rest) = input.strip_prefix("cloud:") {
        ("cloud", rest.to_string())
    } else if !input.is_empty() {
        ("cloud", input.to_string())
    } else {
        ("", String::new())
    }
}

/// Raw LLM call: send `prompt` to the configured LLM endpoint without
/// the dictation rewrite wrapper. Used by meeting-mode post-process
/// (recap + actions extraction) and any other caller that owns its
/// own prompt template.
///
/// `model_override` accepts the prefix-encoded shapes parsed by
/// [`parse_recap_override`]: bare cloud model id (legacy), `cloud:<id>`,
/// `local:<filename.gguf>`, or empty to fall back to llm_mode +
/// `api_model` / `local_llm_model`. Empty string is also accepted.
///
/// Returns the response byte length on success. Negative on error:
/// - -1 invalid args (null pointers, empty prompt)
/// - -2 no LLM API key / URL configured
/// - -3 generic HTTP / parsing error (back-compat — kept for
///   callers that just check `< 0`)
/// - -4 llm_mode=local (or `local:` prefix) but the requested Gemma
///   `.gguf` isn't on disk — caller surfaces "download from
///   Settings → LLM"
/// - -5 404 model-not-supported — the requested recap model is not
///   known to the configured provider URL. UI surfaces "Recap
///   model X is not supported by the configured provider. Settings
///   → Recap to fix." Most common when recap_model_override is a
///   Gemini / OpenAI model but the URL is Anthropic.
/// - -6 401 / 403 auth — invalid or missing API key for the
///   effective recap provider.
/// - -7 429 rate-limit — too many calls, retry later.
/// - -8 network / DNS / TLS — couldn't reach the endpoint.
///
/// # Safety
/// `prompt_ptr` must be a valid null-terminated UTF-8 C string.
/// `model_override_ptr` is allowed to be null (treated as empty); when
/// non-null it must also be a valid null-terminated UTF-8 C string.
/// `out_buf` must be a valid writable buffer of at least `buf_len`
/// bytes. The caller must not free any of the pointers while the FFI
/// call is in flight.
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
    // The recap-specific override wins. Empty string = "inherit"
    // (fall through to the dictation knob). Lets a user run fast
    // dictation via API key while routing the meeting recap via
    // subscription to avoid spending Opus credit.
    // Recap auth is independent of the dictation `llm_auth_method`.
    // The Settings UI exposes a dedicated toggle ("Use Anthropic
    // subscription for recap"); when off, the field is empty here and
    // we default to `api_key` — NEVER inherit `subscription` from the
    // dictation knob. Reason: a user can run dictation via subscription
    // (Anthropic Claude) and recap via Gemini API key — those two
    // paths are orthogonal. The previous inherit semantics routed the
    // Gemini recap through `claude` CLI, which doesn't know the model
    // and exits non-zero ("HTTP 1" surfaced in dimmy.log 2026-05-16).
    let recap_override = st
        .recap_auth_method
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let mut auth_method = if recap_override.is_empty() {
        "api_key".to_string()
    } else {
        recap_override
    };

    // ── Routing: explicit recap-model override > llm_mode default ─
    //
    // `model_override` (recap_model_override in config) accepts three
    // shapes, parsed here so the UI can offer a single picker spanning
    // local + cloud:
    //   - `local:<filename.gguf>`   → force local Gemma, this filename
    //   - `cloud:<model-id>`        → force cloud, this model
    //   - bare string (no `:` prefix) or empty → legacy: fall back to
    //     llm_mode + (api_model | local_llm_model)
    //
    // Why: lets the user keep llm_mode=local for fast/private dictation
    // AND pick Opus 4.7 (or Gemini 2.5) for the meeting recap, OR vice
    // versa (cloud dictation + local Gemma recap). Backward-compat:
    // existing recap_model_override values without a colon prefix are
    // treated as cloud model names, preserving current behaviour.
    let llm_mode = st
        .llm_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "cloud".to_string());
    let (forced_provider, parsed_model) = parse_recap_override(&model_override);
    let effective_mode = match forced_provider {
        "local" => "local",
        "cloud" => "cloud",
        _ => llm_mode.as_str(),
    };

    if effective_mode == "local" {
        let model_filename = if !parsed_model.is_empty() && forced_provider == "local" {
            parsed_model.clone()
        } else {
            st.local_llm_model
                .lock()
                .map(|m| m.clone())
                .unwrap_or_else(|_| crate::local_llm::DEFAULT_LLM_MODEL.to_string())
        };
        let model_path = crate::local_llm::model_path(&model_filename);
        if !model_path.is_file() {
            log(&format!(
                "[LlmRaw] local mode but model not on disk: {} — download from Settings → LLM",
                model_path.display()
            ));
            return -4;
        }
        let max_tokens_local = if max_tokens <= 0 {
            4096_u32
        } else {
            (max_tokens as u32).min(4096)
        };
        log(&format!(
            "[LlmRaw] local recap → {} ({} max tokens)",
            model_filename, max_tokens_local
        ));
        return match crate::local_llm::process_raw_prompt_local(
            &model_path,
            &prompt,
            max_tokens_local,
        ) {
            Ok(text) => {
                log(&format!(
                    "[LlmRaw] local ok — {} chars (model={})",
                    text.len(),
                    model_filename
                ));
                write_to_buf(&text, out_buf, buf_len)
            }
            Err(e) => {
                let msg = format!("{}", e);
                log(&format!(
                    "[LlmRaw] local failed: {}",
                    crate::truncate_utf8(&msg, 200)
                ));
                -3
            }
        };
    }

    // ── Cloud branch ─────────────────────────────────────────────
    // Recap vendor is DERIVED from the parsed model id (no separate
    // provider config field — single picker UI). Key resolution:
    //
    //   1. model empty / unknown vendor → inherit dictation URL+key
    //   2. model vendor == dictation vendor:
    //        - `recap_use_same_key` ON  → reuse dictation key
    //        - `recap_use_same_key` OFF → load `Recap(vendor)` key
    //   3. model vendor != dictation vendor:
    //        - `recap_use_same_key` ON  → try `Llm(vendor)`, fall
    //          back to `Stt(vendor)` (cross-layer skip — user's STT
    //          may share a vendor with the recap even if the LLM
    //          sits on a different one)
    //        - `recap_use_same_key` OFF → load `Recap(vendor)` key
    //
    // Subscription mode (Claude Code CLI) needs no api_url/api_key —
    // the local `claude` binary handles both. Legacy `claude-code://`
    // URL also counts as a "no key needed" signal for back-compat.
    let dictation_vendor = Provider::from_url(&api_url);
    let recap_vendor_derived = if parsed_model.is_empty() {
        None
    } else {
        Provider::from_model_id(&parsed_model).filter(|v| v.default_llm_url().is_some())
    };
    let use_kr = st.use_keyring.lock().map(|k| *k).unwrap_or(false);
    let recap_same_key = st.recap_use_same_key.lock().map(|b| *b).unwrap_or(true);
    let (effective_url, effective_key) = match recap_vendor_derived {
        None => {
            // "Pick best" / no explicit recap model → inherit the dictation
            // provider URL + key. The in-memory dictation key can be empty
            // even when the provider IS configured: the key is saved to the
            // keystore by the Providers page, but the in-memory llm_api_key
            // slot only gets populated when a dictation LLM rewrite runs. A
            // user who never triggers dictation rewrite (or uses a different
            // STT vendor) hits an empty in-memory key here and the recap
            // dies with -2 "configure a key" despite a perfectly good saved
            // key. Fall back to the keystore for the dictation vendor (Llm
            // scope, then Stt) so Pick-best recap authenticates like every
            // other branch already does. Burned 2026-06-02: Groq LLM +
            // Pick-best recap returned -2 with a saved Groq key.
            let key = if !api_key.is_empty() {
                api_key.clone()
            } else {
                crate::load_key_with_store(
                    &st.key_store,
                    crate::provider::KeyringScope::Llm(dictation_vendor),
                    use_kr,
                )
                .filter(|k| !k.is_empty())
                .or_else(|| {
                    crate::load_key_with_store(
                        &st.key_store,
                        crate::provider::KeyringScope::Stt(dictation_vendor),
                        use_kr,
                    )
                })
                .unwrap_or_default()
            };
            log(&format!(
                "[LlmRaw] recap pick-best (inherit {}) — in_mem_key={} resolved_key={}",
                dictation_vendor.as_str(),
                !api_key.is_empty(),
                !key.is_empty()
            ));
            (api_url.clone(), key)
        }
        Some(vendor) if vendor == dictation_vendor => {
            // Same vendor as dictation. Toggle picks: reuse dictation
            // key (default) or load a dedicated `Recap(vendor)` key.
            let url = api_url.clone();
            let key = if recap_same_key {
                api_key.clone()
            } else {
                crate::load_key_with_store(
                    &st.key_store,
                    crate::provider::KeyringScope::Recap(vendor),
                    use_kr,
                )
                .unwrap_or_default()
            };
            log(&format!(
                "[LlmRaw] recap same-vendor ({}) — same_key={} key_present={}",
                vendor.as_str(),
                recap_same_key,
                !key.is_empty()
            ));
            (url, key)
        }
        Some(vendor) => {
            // Different vendor from dictation. Derive URL; key comes
            // from `Recap(vendor)` if same_key is OFF; else fall back
            // to upstream-saved keys (LLM scope first, STT scope second).
            let derived_url = vendor.default_llm_url().unwrap_or("").to_string();
            let key = if recap_same_key {
                let llm_key = crate::load_key_with_store(
                    &st.key_store,
                    crate::provider::KeyringScope::Llm(vendor),
                    use_kr,
                )
                .unwrap_or_default();
                if !llm_key.is_empty() {
                    llm_key
                } else {
                    // Cross-layer skip: STT scope may have it. Useful
                    // when the user runs e.g. Anthropic STT + OpenAI
                    // LLM + Anthropic recap and wants to reuse the
                    // already-saved Anthropic STT key.
                    crate::load_key_with_store(
                        &st.key_store,
                        crate::provider::KeyringScope::Stt(vendor),
                        use_kr,
                    )
                    .unwrap_or_default()
                }
            } else {
                crate::load_key_with_store(
                    &st.key_store,
                    crate::provider::KeyringScope::Recap(vendor),
                    use_kr,
                )
                .unwrap_or_default()
            };
            log(&format!(
                "[LlmRaw] recap cross-vendor ({}) — same_key={} url={} key_present={}",
                vendor.as_str(),
                recap_same_key,
                derived_url,
                !key.is_empty()
            ));
            (derived_url, key)
        }
    };
    // Defensive guard: subscription path goes through the local
    // `claude` CLI which only knows Claude-family models. If the
    // recap model is Gemini / OpenAI / Groq, fall back to the api_key
    // HTTP path (so the user gets the recap they configured) and log
    // a single, actionable warning instead of letting the CLI exit 1
    // and surface the cryptic "HTTP 1" error to the user.
    if auth_method == "subscription" {
        let model_is_claude = Provider::from_model_id(&parsed_model)
            .map(|v| v.is_anthropic())
            .unwrap_or(false);
        if !model_is_claude {
            log(&format!(
                "[LlmRaw] subscription requested but model '{}' is not Claude-family — \
                 falling back to api_key path (subscription only routes to `claude` CLI)",
                parsed_model
            ));
            auth_method = "api_key".to_string();
        }
    }
    let is_subscription = auth_method == "subscription"
        || crate::claude_code::is_claude_code_url(&effective_url)
        || crate::codex::is_codex_url(&effective_url);
    if !is_subscription && (effective_url.is_empty() || effective_key.is_empty()) {
        log(&format!(
            "[LlmRaw] missing recap URL or key (effective_url={}, key_present={})",
            if effective_url.is_empty() {
                "<empty>"
            } else {
                "<set>"
            },
            !effective_key.is_empty(),
        ));
        return -2;
    }
    // Model: prefix-stripped override, OR legacy bare override, OR api_model.
    let model = if !parsed_model.is_empty() {
        parsed_model
    } else {
        api_model
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
    log(&format!(
        "[LlmRaw] dispatch url={} model={} auth={}",
        effective_url, model, auth_method,
    ));
    let result = runtime.block_on(async {
        crate::llm::process_raw_prompt(
            &effective_url,
            &model,
            &effective_key,
            &prompt,
            max_tokens_u,
            &auth_method,
        )
        .await
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
            // SECURITY: Display strips the body so the Sentry / event
            // path never leaks the prompt. The full body still lands
            // in dimmy.log for local debugging via the explicit log
            // call below.
            let display_short = format!("{}", e);
            log(&format!(
                "[LlmRaw] failed: {}",
                crate::truncate_utf8(&display_short, 200)
            ));

            // Categorize the error so the UI can render a specific,
            // actionable message instead of a generic "failed". The
            // body INSPECTION here is INTERNAL only — only the
            // categorical &'static str escapes via the return code,
            // never the body text itself.
            categorize_llm_error_to_rc(&e)
        }
    }
}

/// Map an LlmError into the dimmy_llm_call_raw return code so the
/// UI can show a categorical, actionable message without ever
/// seeing the response body (which would contain the user's prompt
/// echoed back by the provider — a transcript leak risk).
///
/// Return-code contract (documented also in `dimmy_llm_call_raw`'s
/// rustdoc — keep in sync):
/// - -3 generic HTTP / parse error (kept for back-compat)
/// - -5 404 model-not-supported — the requested model id is not
///   known to the configured provider URL. Most common cause:
///   user picked a Gemini / OpenAI model as recap_model_override
///   but the recap dispatch URL is `api.anthropic.com`. UI
///   should point at Settings → Recap to fix.
/// - -6 401 / 403 auth — invalid or missing API key for the
///   effective recap provider.
/// - -7 429 rate-limit — too many calls, retry later.
/// - -8 network / DNS — couldn't reach the endpoint.
/// - -9 413 payload too large — the recap prompt exceeds the model's
///   context window or the plan's per-request / per-minute token limit
///   (common on Groq free tier). UI should suggest a larger-context
///   model or a higher-tier provider.
fn categorize_llm_error_to_rc(err: &crate::error::LlmError) -> c_int {
    use crate::error::LlmError;
    match err {
        LlmError::Api { status, body } => {
            match *status {
                401 | 403 => -6,
                429 => -7,
                413 => -9, // payload / context / tokens-per-minute too large
                404 => {
                    // Model-not-found is the specific case we care
                    // about most. Anthropic 404 bodies contain
                    // `"type":"not_found_error"` + `"model:"`; OpenAI
                    // returns `"model_not_found"` in the error code
                    // field; Google returns `"NOT_FOUND"` and lists
                    // the bad model. All three include the substring
                    // "model" in the JSON body. False positives are
                    // unlikely because a generic 404 has no "model"
                    // mention.
                    let lower = body.to_ascii_lowercase();
                    if lower.contains("model") {
                        -5
                    } else {
                        -3
                    }
                }
                s if (500..600).contains(&s) => -3, // 5xx server — generic for now
                _ => -3,
            }
        }
        LlmError::Network(_) => -8,
        LlmError::NoApiKey(_) => -6, // user-visible as "key issue"
        LlmError::LocalModel(_) => -4,
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

/// Recording-consent notice text (see [`crate::consent`]). `kind` = "modal"
/// (the confirmation shown to the recorder before a meeting starts) or
/// "announcement" (spoken via the host TTS + pasted into the call chat for the
/// participants). `lang` is a BCP-47-ish tag; unsupported tags fall back to
/// English. The announcement's storage clause is made accurate to the current
/// config: it says the recording stays on-device unless STT or LLM is cloud,
/// in which case it discloses external processing. Returns bytes written, -1
/// on bad args.
///
/// # Safety
/// `kind_ptr` and `lang_ptr` must be valid null-terminated UTF-8 C strings;
/// `out_buf` must be writable for `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_consent_text(
    kind_ptr: *const c_char,
    lang_ptr: *const c_char,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if kind_ptr.is_null() || lang_ptr.is_null() {
        return -1;
    }
    let kind = match CStr::from_ptr(kind_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let lang = CStr::from_ptr(lang_ptr).to_str().unwrap_or("en");
    let text = match kind {
        "modal" => crate::consent::modal_text(lang),
        "announcement" => {
            let st = state();
            let stt_cloud = st
                .stt_mode
                .lock()
                .map(|m| m.eq_ignore_ascii_case("cloud"))
                .unwrap_or(false);
            let llm_cloud = st
                .llm_mode
                .lock()
                .map(|m| m.eq_ignore_ascii_case("cloud"))
                .unwrap_or(false);
            crate::consent::announcement_text(lang, stt_cloud || llm_cloud)
        }
        _ => return -1,
    };
    write_to_buf(&text, out_buf, buf_len)
}

/// Append a recording-consent event to the local audit log
/// (`<config_dir>/consent.jsonl`). `kind` is the host's event tag:
/// "confirmed" / "declined" / "announced" / "chat_copied". `lang` (may be
/// null) records which notice language was used. Returns 0, -1 on bad args.
///
/// # Safety
/// `kind_ptr` must be a valid null-terminated UTF-8 C string; `lang_ptr` may
/// be null or a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_consent_log_event(
    kind_ptr: *const c_char,
    lang_ptr: *const c_char,
) -> c_int {
    if kind_ptr.is_null() {
        return -1;
    }
    let kind = match CStr::from_ptr(kind_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let lang = if lang_ptr.is_null() {
        "en"
    } else {
        CStr::from_ptr(lang_ptr).to_str().unwrap_or("en")
    };
    crate::consent::append_event(kind, lang);
    // Categorical only: coerce the host-provided kind to a known set so no
    // free-form string ever reaches PostHog.
    let kind_static = match kind {
        "shown" => "shown",
        "accepted" => "accepted",
        "cancelled" => "cancelled",
        "announced" => "announced",
        "declined" => "declined",
        _ => "other",
    };
    crate::telemetry::track(crate::telemetry::Event::ConsentLogged { kind: kind_static });
    0
}

/// Get the per-install config-dir name as embedded at build time via
/// `DIMMY_CONFIG_NAMESPACE` (default `dimmy`). Native UIs MUST use this
/// instead of deriving the path from the build flavor — since 2026-05-16
/// flavor and config-dir are decoupled (a flavor=staging build that
/// ships under the prod packId keeps the prod config dir so a
/// channel-prerelease auto-update doesn't appear to wipe user data).
/// Reading from `build_flavor()` to derive the dir name produced the
/// 2026-05-16 onboarding-restart bug (C# looked under `dimmy-staging`
/// while Rust read+wrote under `dimmy`). Returns bytes written, or -1.
#[no_mangle]
pub extern "C" fn dimmy_config_dir_name(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    write_to_buf(crate::config_dir_name(), out_buf, buf_len)
}

/// Get the *effective* meeting storage directory as a UTF-8 path. This is
/// the single source every host UI must use to read/write meeting
/// artefacts — never re-derive `<config_dir>/meetings` locally. Resolves
/// the user's `meeting_storage_path` override (with writability fallback
/// to the default) via [`crate::meetings_dir`]. Returns bytes written, or
/// -1 on buffer error / unresolvable config dir.
#[no_mangle]
pub extern "C" fn dimmy_meetings_dir(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    match crate::meetings_dir() {
        Some(p) => write_to_buf(&p.to_string_lossy(), out_buf, buf_len),
        None => -1,
    }
}

/// Get the embedded model catalog as a JSON string (the single source of
/// truth for the cloud models offered per provider — see
/// [`crate::catalog`]). Every host UI builds its model/provider pickers
/// from this so Windows and macOS cannot diverge.
///
/// Contract: returns the *total* byte length of the catalog JSON, always
/// (independent of `buf_len`). Pass `out_buf = null` to query the length
/// without writing. If the return value is `>= buf_len` the buffer was too
/// small and the written content is truncated — allocate `(return + 1)`
/// bytes and call again.
#[no_mangle]
pub extern "C" fn dimmy_model_catalog_json(out_buf: *mut c_char, buf_len: c_int) -> c_int {
    let json = crate::catalog::MODEL_CATALOG_JSON;
    let needed = json.len() as c_int;
    if !out_buf.is_null() && buf_len > 0 {
        write_to_buf(json, out_buf, buf_len);
    }
    needed
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

    crate::telemetry::track(crate::telemetry::Event::ModelDownloadCompleted {
        kind: "whisper",
        success: result.is_ok(),
    });
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

    crate::telemetry::track(crate::telemetry::Event::ModelDownloadCompleted {
        kind: "llm",
        success: result.is_ok(),
    });
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
    crate::telemetry::track(crate::telemetry::Event::ModelDownloadCompleted {
        kind: "parakeet",
        success: result.is_ok(),
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

/// Set (or clear) the optional dedicated command-mode shortcut. This runs on
/// the SAME low-level keyboard hook as the dictation shortcut, so it supports
/// the identical combo grammar (modifier-only like "Win+Alt", modifier+key,
/// 2-modifiers+key) AND both toggle + push-to-talk semantics. A null/empty/
/// unparseable combo DISABLES the command hotkey.
///
/// # Safety
/// `combo_ptr` must be null or a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_hotkey_set_command(combo_ptr: *const c_char) {
    if combo_ptr.is_null() {
        crate::hotkey::set_command_shortcut("");
        crate::log("[Hotkey] set_command_shortcut(disabled)");
        return;
    }
    if let Ok(combo) = CStr::from_ptr(combo_ptr).to_str() {
        crate::hotkey::set_command_shortcut(combo);
        crate::log(&format!("[Hotkey] set_command_shortcut(\"{}\")", combo));
    }
}

/// Returns 1 if the two combos conflict (one is a subset of the other, so
/// pressing one would also trigger the other), else 0. Used by the host to
/// reject a command hotkey that collides with the dictation / dictionary
/// hotkey before binding it. A null/empty (disabled) combo never conflicts.
///
/// # Safety
/// `a` and `b` must each be null or a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_hotkey_combos_conflict(a: *const c_char, b: *const c_char) -> c_int {
    let sa = if a.is_null() {
        String::new()
    } else {
        CStr::from_ptr(a).to_string_lossy().into_owned()
    };
    let sb = if b.is_null() {
        String::new()
    } else {
        CStr::from_ptr(b).to_string_lossy().into_owned()
    };
    if crate::hotkey::combos_conflict(&sa, &sb) {
        1
    } else {
        0
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

/// Decode a WAV file via `hound` to (mono f32, sample_rate). Kept on
/// hound (not Symphonia) because the file-load tests + every recorded-
/// from-Dimmy artefact are WAV — no need to pay Symphonia's per-call
/// cost on the most common path.
fn decode_wav_via_hound(path: &str) -> Result<(Vec<f32>, u32), String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("open: {e}"))?;
    let spec = reader.spec();
    if spec.sample_rate == 0 || spec.channels == 0 {
        return Err("invalid WAV header (sample_rate or channels = 0)".into());
    }
    let raw_samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample as i32;
            if bits <= 0 {
                return Err(format!("invalid bits_per_sample {bits}"));
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
        return Err("WAV decoded to zero samples".into());
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
    Ok((mono, spec.sample_rate))
}

/// Decode m4a / mp3 / aac / alac / flac / ogg / vorbis via Symphonia
/// to (mono f32, sample_rate). Symphonia is pure Rust + no native deps;
/// codec features picked in `core/Cargo.toml`. Multi-track files: pick
/// the first decodable audio track; this matches what users expect
/// from a Voice Memo / podcast export.
fn decode_via_symphonia(path: &str) -> Result<(Vec<f32>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no decodable audio track".to_string())?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "missing sample rate in codec params".to_string())?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder: {e}"))?;
    let mut interleaved: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut consecutive_packet_errs: u32 = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => {
                consecutive_packet_errs = 0;
                p
            }
            // End-of-stream surfaces as IoError(UnexpectedEof) in
            // symphonia 0.5 — treat as normal termination.
            Err(SymphoniaError::IoError(_)) => break,
            Err(SymphoniaError::ResetRequired) => break,
            // Permissive: return whatever we decoded so far. Returning Err
            // here used to lose ALL successfully decoded peaks just because
            // the stream's tail was malformed — surfaced on Win as
            // `f39aceff` (and other meetings) showing a single-band
            // waveform instead of dual. Partial peaks > empty peaks.
            // Burned 2026-05-30.
            Err(SymphoniaError::DecodeError(_)) => {
                consecutive_packet_errs += 1;
                if consecutive_packet_errs > 64 {
                    break;
                }
                continue;
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                if sample_buf.is_none() {
                    let spec = *audio_buf.spec();
                    let duration = audio_buf.capacity() as u64;
                    sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
                }
                if let Some(buf) = &mut sample_buf {
                    buf.copy_interleaved_ref(audio_buf);
                    interleaved.extend_from_slice(buf.samples());
                }
            }
            // Single-packet decode errors are recoverable on most codecs;
            // resync on the next packet rather than aborting.
            Err(SymphoniaError::DecodeError(_)) => continue,
            // ResetRequired during decode (rare): stop cleanly with the
            // samples we've accumulated so far.
            Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(_)) => break,
            // Any other decoder error (Unsupported / Limit / Seek) — skip
            // this packet and continue. Partial peaks > nothing.
            Err(_) => continue,
        }
    }
    if interleaved.is_empty() {
        return Err("symphonia produced 0 samples".into());
    }
    let mono: Vec<f32> = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok((mono, sample_rate))
}

/// Decode any audio file the loader supports (WAV via hound, m4a /
/// mp3 / aac / flac / ogg via Symphonia) and write it as a real
/// mono int16 WAV at the source's native sample rate. Used by the
/// file-load → meeting bridge so downstream consumers that hardcode
/// "audio.wav" (waveform reader, media player, duration probe) keep
/// working without per-format branches.
///
/// Returns the destination file size on success, or:
/// - -1 invalid args (null pointer / bad UTF-8)
/// - -2 decode failure
/// - -3 WAV write failure
///
/// # Safety
/// Both pointers must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn dimmy_decode_audio_to_wav(
    src_ptr: *const c_char,
    dst_ptr: *const c_char,
) -> c_int {
    if src_ptr.is_null() || dst_ptr.is_null() {
        return -1;
    }
    let src = match CStr::from_ptr(src_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let dst = match CStr::from_ptr(dst_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let ext = std::path::Path::new(src)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let (mono, sample_rate) = match if ext == "wav" {
        decode_wav_via_hound(src)
    } else {
        decode_via_symphonia(src)
    } {
        Ok(v) => v,
        Err(e) => {
            log(&format!("[DecodeToWav] decode failed: {}", e));
            return -2;
        }
    };
    if mono.is_empty() || sample_rate == 0 {
        return -2;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = match hound::WavWriter::create(dst, spec) {
        Ok(w) => w,
        Err(e) => {
            log(&format!("[DecodeToWav] hound create {}: {}", dst, e));
            return -3;
        }
    };
    for &s in &mono {
        let clamped = s.clamp(-1.0, 1.0);
        let i = (clamped * i16::MAX as f32) as i16;
        if writer.write_sample(i).is_err() {
            return -3;
        }
    }
    if writer.finalize().is_err() {
        return -3;
    }
    std::fs::metadata(dst)
        .map(|m| m.len() as c_int)
        .unwrap_or(0)
}

/// Compute a waveform-peak summary from any audio file the loader
/// can decode (WAV via hound, m4a / mp3 / aac / flac / ogg via
/// Symphonia). Mirrors the role of the C#-side `WavPeaks.ReadPeaks`,
/// but reuses the multi-format Rust decoder so the host UI gets
/// peaks for every format `dimmy_transcribe_file` accepts.
///
/// Output JSON: `{"peaks":[f32; bucket_count], "duration_secs": f64}`
/// where peaks are in [0..1] (absolute peak per bucket, mono after
/// downmix).
///
/// Returns the number of bytes written (excluding null terminator),
/// or:
/// - -1 invalid args (null pointer / bad UTF-8 / `bucket_count <= 0`
///   / `buf_len <= 0`)
/// - -2 decode failure (unsupported codec, unreadable file, empty)
/// - -3 output buffer too small for the JSON (caller should retry
///   with a larger buffer; the host's "no waveform" fallback applies)
///
/// # Safety
/// `path_ptr` must be a valid null-terminated UTF-8 C string and
/// `out_buf` a valid writable buffer of `buf_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_compute_audio_peaks(
    path_ptr: *const c_char,
    bucket_count: c_int,
    out_buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    if path_ptr.is_null() || out_buf.is_null() || bucket_count <= 0 || buf_len <= 0 {
        return -1;
    }
    let path = match CStr::from_ptr(path_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let (mono, sample_rate) = match if ext == "wav" {
        decode_wav_via_hound(path)
    } else {
        decode_via_symphonia(path)
    } {
        Ok(v) => v,
        Err(_) => return -2,
    };
    if mono.is_empty() || sample_rate == 0 {
        return -2;
    }
    let buckets = bucket_count as usize;
    let total = mono.len();
    let frames_per_bucket = (total / buckets).max(1);
    let mut peaks: Vec<f32> = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let start = b * frames_per_bucket;
        if start >= total {
            peaks.push(0.0);
            continue;
        }
        let end = ((b + 1) * frames_per_bucket).min(total);
        let mut peak: f32 = 0.0;
        for &s in &mono[start..end] {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
        }
        peaks.push(peak.min(1.0));
    }
    let duration_secs = total as f64 / sample_rate as f64;
    // Manual JSON build with capped precision. `serde_json` would
    // serialise f32 values through their f64 promotion and emit the
    // full mantissa (~22 chars for `0.00005151328514330089`); 200
    // buckets of those overflow the C# host's 4 KB fixed buffer →
    // dimmy_compute_audio_peaks returns -3 and the host silently
    // falls back to "no waveform". Cap at 5 decimals (≈3 px @ 1000 px
    // canvas — well below the visual resolution of a waveform render)
    // so each peak is at most 7 chars ("0.12345"), making the payload
    // size deterministic in `bucket_count` and the buffer-sizing
    // heuristic safe. Burned 2026-05-30 on mic-mostly-silent meetings
    // (`8d490f65`, `f39aceff`, `bc8750d6`).
    let mut payload = String::with_capacity(64 + buckets * 10);
    payload.push_str(&format!(
        "{{\"duration_secs\":{duration_secs:.3},\"peaks\":["
    ));
    for (i, p) in peaks.iter().enumerate() {
        if i > 0 {
            payload.push(',');
        }
        payload.push_str(&format!("{p:.5}"));
    }
    payload.push_str("]}");
    let bytes = payload.as_bytes();
    if bytes.len() + 1 > buf_len as usize {
        return -3;
    }
    let dst = std::slice::from_raw_parts_mut(out_buf as *mut u8, buf_len as usize);
    dst[..bytes.len()].copy_from_slice(bytes);
    dst[bytes.len()] = 0;
    bytes.len() as c_int
}

/// Synchronously transcribe an audio file using the active local STT
/// backend (whisper.cpp or Parakeet, per `local_stt_backend`). Cloud
/// transcription via this entry point is unimplemented for now —
/// callers that need cloud should use the recording flow.
///
/// Supported containers / codecs:
/// - **WAV** (16/24/32-bit int, 32-bit float) — via hound, fast path.
/// - **m4a / mp4 / aac** (ISO BMFF container with AAC or ALAC) — via Symphonia.
/// - **mp3, flac, ogg/vorbis** — via Symphonia.
///
/// The file is decoded in-process, downmixed to mono, run through the
/// file-load preprocess pipeline (highpass only — AGC would corrupt
/// long files, see CLAUDE.md AUDIO-001), and routed to the configured
/// local backend. The resulting transcript is also written to the
/// history database with audio_path linking back to the source file
/// (so the user can replay it from the History UI).
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

    // ── Decode: dispatch by extension. WAV → hound (fast path, well
    // tested); everything else → Symphonia (pure-Rust multi-format).
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let (mono, sample_rate) = if ext == "wav" {
        match decode_wav_via_hound(path) {
            Ok(v) => v,
            Err(e) => {
                log(&format!("[FileLoad] WAV decode failed: {}", e));
                return -2;
            }
        }
    } else {
        match decode_via_symphonia(path) {
            Ok(v) => v,
            Err(e) => {
                log(&format!(
                    "[FileLoad] Symphonia decode failed (ext='{}'): {}",
                    ext, e
                ));
                return -2;
            }
        }
    };
    log(&format!(
        "[FileLoad] decoded: {} samples @ {} Hz mono ({:.1}s) [ext={}]",
        mono.len(),
        sample_rate,
        mono.len() as f64 / sample_rate as f64,
        if ext.is_empty() { "(none)" } else { &ext },
    ));

    // ── Preprocess: file-load mode (highpass only, no VAD, no AGC) ──
    // The live-recording pipeline normalises level via dagc, but on a
    // long recorded file dagc encounters natural silence stretches and
    // emits NaN, permanently corrupting all downstream samples (CLAUDE.md
    // AUDIO-001). On a 95-min meeting WAV this destroyed 97 % of the
    // audio, leaving only the first ~150 s transcribable. File-load
    // audio is already at a recorded level — only de-rumble is needed.
    let raw_samples_for_history = mono.clone();
    let processed_samples = crate::preprocess::process_buffer_for_file_load(&mono, sample_rate);
    let processed = crate::audio::ProcessedAudio {
        samples: processed_samples,
        sample_rate,
    };
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
    let total_secs_pre = raw_samples_for_history.len() as f64 / sample_rate as f64;

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
                // Stats: file load contributes to Total words +
                // Time saved the same way pill dictation does. Was
                // missing 2026-05-15; without it a 60-min imported
                // WAV vanished from the user's cumulative counters.
                let words = text.split_whitespace().count() as c_int;
                let _ = dimmy_update_stats(words, total_secs_pre);
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
    // Compose vocab biasing for the file-load chunked-whisper path so
    // imported recordings get the same user_dict / prompt advantage as
    // a live dictation. Parakeet branch ignores it (no boost-words
    // support in our ort wrapper).
    let prompt_base = st.prompt.lock().map(|p| p.clone()).unwrap_or_default();
    let user_dict_snapshot = st.user_dict.lock().map(|d| d.clone()).unwrap_or_default();
    let composed_prompt = crate::compose_stt_prompt(&prompt_base, &user_dict_snapshot);
    // Emit a starting event so the UI can flip its progress bar
    // from indeterminate to 0 % the moment we begin work.
    let total_secs = raw_samples_for_history.len() as f64 / sample_rate as f64;
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
            crate::transcribe::transcribe_audio_local(&chunk, &language, &model, &composed_prompt)
                .map(|t| (t, None))
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
                let duration = raw_samples_for_history.len() as f64 / sample_rate as f64;
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
    // Stats: same rationale as the cloud branch above — file load
    // contributes to the cumulative counters in Settings.
    let words = text.split_whitespace().count() as c_int;
    let _ = dimmy_update_stats(words, total_secs);

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

/// Poll for command-mode hotkey events (the optional dedicated shortcut).
/// Returns 0=none, 1=pressed, 2=released — same edges as
/// `dimmy_hotkey_take_event`, but for the command binding. Returns 0 forever
/// while the command hotkey is unconfigured.
#[no_mangle]
pub extern "C" fn dimmy_hotkey_take_command_event() -> c_int {
    crate::hotkey::take_command_event() as c_int
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
/// Returns a status the host UI surfaces truthfully (no blanket
/// "Sent!"):
///   `1`  enqueued for delivery (best-effort, no delivery confirmation)
///   `0`  empty message — no-op
///   `-1` precondition failure (invalid UTF-8)
///   `-2` telemetry disabled by the user → host shows "enable & send"
///   `-3` no Sentry DSN compiled in (dev / source build) — not available
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
        Ok(_) => return 0,   // empty → no-op
        Err(_) => return -1, // invalid UTF-8 → precondition failure
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
    crate::telemetry::capture_feedback(kind, message, email) as c_int
}

// ── Typed telemetry dispatcher (host-driven events) ──────────────
//
// Many events fire from UI surfaces that Rust doesn't see directly:
// pill scroll gestures, the "Run recap" button on the File Load
// card, Notion send result, update channel toggle, etc. Rather than
// add one bespoke FFI per event (and grow the surface area by ~14
// fns), we expose a single typed dispatcher that takes the event
// name + a JSON property object.
//
// The Rust side parses the name + props, constructs the matching
// typed `Event` variant, and falls into the existing
// `telemetry::track` path (so all the same privacy/scrubbing rules
// apply). Unknown event names or malformed props return -2 so the
// host can log + ignore — never abort.
//
// Privacy contract: the host MUST pass categorical values only —
// e.g. `source: "hotkey"`, `provider: "anthropic"`,
// `audio_secs_bucket: "120_600"`. The dispatcher does NOT bucket
// numeric values for you because the bucket choice is per-event.
// PostHog dashboards depend on stable bucket labels.

/// Track an event by name with a JSON props object. Returns:
///   0  emitted
///  -1  invalid input (null name, non-UTF-8, malformed JSON)
///  -2  unknown event name (host fell out of date with the Rust
///      schema; safe to ignore)
///
/// # Safety
/// `name_ptr` must be a valid null-terminated UTF-8 string.
/// `props_json_ptr` may be null (= no properties) or must be a
/// valid null-terminated UTF-8 string containing a JSON object.
#[no_mangle]
pub unsafe extern "C" fn dimmy_telemetry_track_typed(
    name_ptr: *const c_char,
    props_json_ptr: *const c_char,
) -> c_int {
    if name_ptr.is_null() {
        return -1;
    }
    let name = match unsafe { CStr::from_ptr(name_ptr) }.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let props: serde_json::Value = if props_json_ptr.is_null() {
        serde_json::Value::Null
    } else {
        let s = match unsafe { CStr::from_ptr(props_json_ptr) }.to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        };
        if s.trim().is_empty() {
            serde_json::Value::Null
        } else {
            match serde_json::from_str(s) {
                Ok(v) => v,
                Err(_) => return -1,
            }
        }
    };

    // Tiny helpers to fetch typed fields with safe defaults. We
    // never `unwrap()` — bad input from the host degrades to an
    // empty-string / false-bool, never a panic.
    let prop_str = |key: &str| -> String {
        props
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let prop_static = |key: &str, allowed: &[&'static str]| -> &'static str {
        let v = props.get(key).and_then(|v| v.as_str()).unwrap_or("");
        for &a in allowed {
            if a == v {
                return a;
            }
        }
        "unknown"
    };
    let prop_bool =
        |key: &str| -> bool { props.get(key).and_then(|v| v.as_bool()).unwrap_or(false) };

    let event = match name {
        "user_dict.word_added" => Some(crate::telemetry::Event::UserDictWordAdded {
            source: prop_static("source", &["hotkey", "services_menu", "settings_ui"]),
        }),
        "user_dict.word_removed" => Some(crate::telemetry::Event::UserDictWordRemoved),
        "meeting.recap_completed" => Some(crate::telemetry::Event::MeetingRecapCompleted {
            provider: prop_static(
                "provider",
                &[
                    "groq",
                    "openai",
                    "anthropic",
                    "gemini",
                    "openrouter",
                    "local",
                    "unset",
                ],
            ),
            recap_model_bucket: prop_static(
                "recap_model_bucket",
                &[
                    "opus",
                    "sonnet",
                    "haiku",
                    "gemini_pro",
                    "gemini_flash",
                    "gpt_5",
                    "gpt_4",
                    "llama",
                    "gemma",
                    "default",
                    "other",
                ],
            ),
            processing_ms_bucket: prop_static(
                "processing_ms_bucket",
                &[
                    "lt_500",
                    "500_2000",
                    "2000_10000",
                    "10000_60000",
                    "ge_60000",
                ],
            ),
            success: prop_bool("success"),
        }),
        "meeting.imported_from_file" => Some(crate::telemetry::Event::MeetingImportedFromFile),
        "file_load.started" => Some(crate::telemetry::Event::FileLoadStarted {
            source: prop_static("source", &["drop", "picker"]),
            audio_secs_bucket: prop_static(
                "audio_secs_bucket",
                &[
                    "lt_30",
                    "30_120",
                    "120_600",
                    "600_1800",
                    "1800_3600",
                    "ge_3600",
                ],
            ),
        }),
        "file_load.completed" => Some(crate::telemetry::Event::FileLoadCompleted {
            audio_secs_bucket: prop_static(
                "audio_secs_bucket",
                &[
                    "lt_30",
                    "30_120",
                    "120_600",
                    "600_1800",
                    "1800_3600",
                    "ge_3600",
                ],
            ),
            processing_ms_bucket: prop_static(
                "processing_ms_bucket",
                &[
                    "lt_500",
                    "500_2000",
                    "2000_10000",
                    "10000_60000",
                    "ge_60000",
                ],
            ),
            words_bucket: prop_static(
                "words_bucket",
                &["0", "1_50", "51_200", "201_1000", "1001_5000", "ge_5000"],
            ),
            success: prop_bool("success"),
        }),
        "notion.connected" => Some(crate::telemetry::Event::NotionConnected),
        "notion.disconnected" => Some(crate::telemetry::Event::NotionDisconnected),
        "notion.recap_sent" => Some(crate::telemetry::Event::NotionRecapSent {
            auto: prop_bool("auto"),
            ok: prop_bool("ok"),
        }),
        "app_rules.added" => Some(crate::telemetry::Event::AppRuleAdded),
        "app_rules.removed" => Some(crate::telemetry::Event::AppRuleRemoved),
        "app_rules.reordered" => Some(crate::telemetry::Event::AppRuleReordered),
        "pill.visibility_toggled" => Some(crate::telemetry::Event::PillVisibilityToggled {
            visible: prop_bool("visible"),
            source: prop_static(
                "source",
                &["settings", "tray", "jumplist", "hotkey", "dock_menu"],
            ),
        }),
        "pill.style_scrolled" => Some(crate::telemetry::Event::PillStyleScrolled),
        "pill.language_scrolled" => Some(crate::telemetry::Event::PillLanguageScrolled),
        "pill.context_menu_opened" => Some(crate::telemetry::Event::PillContextMenuOpened),
        "update.channel_changed" => Some(crate::telemetry::Event::UpdateChannelChanged {
            from: prop_static("from", &["stable", "prerelease"]),
            to: prop_static("to", &["stable", "prerelease"]),
        }),
        "update.apply_deferred" => Some(crate::telemetry::Event::UpdateApplyDeferred),
        "permission.granted" => Some(crate::telemetry::Event::PermissionGranted {
            service: prop_static(
                "service",
                &["microphone", "accessibility", "input_monitoring"],
            ),
        }),
        "permission.denied" => Some(crate::telemetry::Event::PermissionDenied {
            service: prop_static(
                "service",
                &["microphone", "accessibility", "input_monitoring"],
            ),
        }),
        // claude_code.* — login funnel emitted from the host polling
        // loop. The `claude_code.invocation` event lives Rust-side
        // (emitted from `process_text` / `process_raw_prompt`) so the
        // host doesn't dispatch it.
        "claude_code.login_completed" => Some(crate::telemetry::Event::ClaudeCodeLoginCompleted {
            outcome: prop_static("outcome", &["success", "timeout", "spawn_failed"]),
        }),
        // onboarding.* — wizard funnel emitted from the host wizard
        // window (Win OnboardingWindow, Mac OnboardingContainerView).
        // Step names are categorical so dashboards can build funnels
        // without per-version drift: welcome → permissions (Mac
        // only) → provider → shortcut → model_download (Mac only) →
        // try_it. `path` on `.completed` records whether the user
        // landed on cloud STT or local STT — drives the onboarding
        // success-by-path dashboard.
        "onboarding.started" => Some(crate::telemetry::Event::OnboardingStarted),
        "onboarding.step_completed" => Some(crate::telemetry::Event::OnboardingStepCompleted {
            step: prop_static(
                "step",
                &[
                    "welcome",
                    "permissions",
                    "provider",
                    "shortcut",
                    "model_download",
                    "try_it",
                ],
            ),
        }),
        "onboarding.completed" => Some(crate::telemetry::Event::OnboardingCompleted {
            path: prop_static("path", &["cloud", "local", "unknown"]),
            duration_secs: props
                .get("duration_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        }),
        "onboarding.abandoned" => Some(crate::telemetry::Event::OnboardingAbandoned {
            last_step: prop_static(
                "last_step",
                &[
                    "welcome",
                    "permissions",
                    "provider",
                    "shortcut",
                    "model_download",
                    "try_it",
                ],
            ),
        }),
        _ => None,
    };
    // `prop_str` is unused for now — kept available for any future
    // event variants that legitimately accept a free-form string
    // field (e.g. a future categorical that the host passes raw).
    let _ = prop_str;

    match event {
        Some(e) => {
            crate::telemetry::track(e);
            0
        }
        None => -2,
    }
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

// ── Call auto-detect singleton + FFI ────────────────────────────────

/// State machine for the audio-driven "is the user in a call?" detector.
/// Configured from AppState's `call_detect_*` fields each time the host
/// pushes a signal — that way the detector always reflects the latest
/// saved config without a separate apply step on init / set_config_json.
static CALL_DETECTOR: OnceLock<Mutex<crate::call_detector::CallDetectorState>> = OnceLock::new();

fn call_detector_lock() -> std::sync::MutexGuard<'static, crate::call_detector::CallDetectorState> {
    let m =
        CALL_DETECTOR.get_or_init(|| Mutex::new(crate::call_detector::CallDetectorState::new()));
    let mut g = m.lock().expect("call_detector mutex poisoned");
    if let Some(st) = GLOBAL_STATE.get() {
        let enabled = *st
            .call_detect_enabled
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let min_active = (*st
            .call_detect_min_active_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner()))
        .max(1);
        let cooldown = (*st
            .call_detect_cooldown_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner()))
        .max(1);
        let timeout_cd = (*st
            .call_detect_timeout_cooldown_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner()))
        .max(1);
        let excluded: std::collections::HashSet<String> = st
            .call_detect_excluded_apps
            .lock()
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        g.configure(enabled, min_active, cooldown, timeout_cd, excluded);
    }
    g
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Push one mic-in-use observation. Returns:
///   1 = call_detected event emitted on this call,
///   2 = call_ended event emitted on this call,
///   0 = no transition (suppressed / debouncing / unchanged),
///  -1 = malformed app_id ptr.
///
/// `mic_active` = 0/1. `app_id` = optional NUL-terminated lowercase
/// canonical id ("teams"/"zoom"/"slack"/"discord"/"webex") or NULL /
/// empty if the host could not infer a VoIP process. The detector
/// uses the app id (when present) for per-app cooldown + exclusion
/// list lookups; absent ids fall back to a single global cooldown
/// slot so a generic "mic in use" doesn't spam the user.
///
/// # Safety
/// `app_id` must be NULL or a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_call_signal(mic_active: c_int, app_id: *const c_char) -> c_int {
    let app: Option<String> = if app_id.is_null() {
        None
    } else {
        match CStr::from_ptr(app_id).to_str() {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_lowercase()),
            Ok(_) => None,
            Err(_) => return -1,
        }
    };

    let is_meeting_active = MEETING.lock().map(|g| g.is_some()).unwrap_or(false);
    let now = now_epoch_secs();

    let outcome = {
        let mut g = call_detector_lock();
        g.signal(mic_active != 0, app, is_meeting_active, now)
    };

    use crate::call_detector::CallSignalOutcome;
    match outcome {
        CallSignalOutcome::Detected { app, since_seconds } => {
            let payload = serde_json::json!({
                "app": app,
                "since_seconds": since_seconds,
            })
            .to_string();
            emit_event("call_detected", &payload);
            1
        }
        CallSignalOutcome::Ended { app } => {
            let payload = serde_json::json!({ "app": app }).to_string();
            emit_event("call_ended", &payload);
            2
        }
        CallSignalOutcome::StopSuggested {
            app,
            inactive_for_secs,
        } => {
            let payload = serde_json::json!({
                "app": app,
                "inactive_for_secs": inactive_for_secs,
                "reason": "call_ended",
            })
            .to_string();
            emit_event("meeting.stop_suggested", &payload);
            3
        }
        CallSignalOutcome::Suppressed(_) | CallSignalOutcome::NoChange => 0,
    }
}

/// Push one system-audio activity observation. `sys_active` = 1 iff
/// the call's render-side audio is currently emitting (WASAPI render
/// session for the whitelisted app peak-meter > floor). Calling this
/// switches the stop-suggestion gate to AND-with-sys mode for this
/// process — once enabled, BOTH mic and sys must be silent past their
/// respective thresholds (5 s each by default) before the meeting
/// stop popup is offered. Hosts that never call this stay in mic-only
/// mode (Mac today).
///
/// Return: 0 / 3 (`meeting.stop_suggested` emitted) / -1 (bad app_id).
///
/// # Safety
/// `app_id` must be NULL or a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_call_signal_sys(sys_active: c_int, app_id: *const c_char) -> c_int {
    // app_id is accepted for parity with `dimmy_call_signal` (host can
    // pass the same id it uses on the mic side); we don't use it on
    // the sys path because the detection identity is owned by the
    // mic-side state machine. Validate the encoding so callers can't
    // smuggle invalid UTF-8 in.
    if !app_id.is_null() && CStr::from_ptr(app_id).to_str().is_err() {
        return -1;
    }
    let is_meeting_active = MEETING.lock().map(|g| g.is_some()).unwrap_or(false);
    let now = now_epoch_secs();
    let outcome = {
        let mut g = call_detector_lock();
        g.signal_sys(sys_active != 0, is_meeting_active, now)
    };
    use crate::call_detector::CallSignalOutcome;
    match outcome {
        CallSignalOutcome::StopSuggested {
            app,
            inactive_for_secs,
        } => {
            let payload = serde_json::json!({
                "app": app,
                "inactive_for_secs": inactive_for_secs,
                "reason": "call_ended",
            })
            .to_string();
            emit_event("meeting.stop_suggested", &payload);
            3
        }
        _ => 0,
    }
}

/// Tell the detector whether the host is deterministically watching a
/// meeting-origin process (a detected call app). While set to 1, the
/// silence-based stop-suggestion is suppressed and only
/// `dimmy_call_signal_session_ended` decides when to offer "stop recording" —
/// this stops the silence heuristic from re-nagging "the call ended" on a
/// live call's quiet stretches (the 15-popups-per-meeting bug). Pass 1 when
/// binding/adopting an origin pid, 0 when clearing it. `dimmy_meeting_stop`
/// also clears it via `meeting_stopped()`. Returns 0.
#[no_mangle]
pub extern "C" fn dimmy_call_set_tracked_origin(tracked: c_int) -> c_int {
    let mut g = call_detector_lock();
    g.set_tracked_origin(tracked != 0);
    0
}

/// Authoritative "call ended" signal — host has positive evidence
/// the originating WASAPI session disappeared from the active
/// capture-session set. Bypasses the amplitude-based silence
/// thresholds entirely and fires `meeting.stop_suggested` (rc=3)
/// immediately, gated only by the same one-shot + KeepRecording
/// cooldown guards the silence path enforces.
///
/// Returns 3 / 0 (no state change — meeting wasn't ours / already
/// fired / cooldown active).
///
/// # Safety
///
/// Takes no pointer arguments and reads / writes only globally-owned
/// `Mutex`-guarded state (`MEETING`, `call_detector_lock()`). Safe to
/// call from any thread once `dimmy_init` has run; callers must NOT
/// invoke it concurrently with `dimmy_shutdown`.
#[no_mangle]
pub unsafe extern "C" fn dimmy_call_signal_session_ended() -> c_int {
    let is_meeting_active = MEETING.lock().map(|g| g.is_some()).unwrap_or(false);
    let now = now_epoch_secs();
    let outcome = {
        let mut g = call_detector_lock();
        g.signal_call_session_ended(is_meeting_active, now)
    };
    use crate::call_detector::CallSignalOutcome;
    match outcome {
        CallSignalOutcome::StopSuggested {
            app,
            inactive_for_secs,
        } => {
            let payload = serde_json::json!({
                "app": app,
                "inactive_for_secs": inactive_for_secs,
                "reason": "session_ended",
            })
            .to_string();
            emit_event("meeting.stop_suggested", &payload);
            3
        }
        _ => 0,
    }
}

/// Arm the stop-suggestion path for a meeting started OUTSIDE the
/// "Record now" nudge (manual start from the meeting window / pill) while
/// a call is detected by the host. Without this the call detector's
/// `recording_active_from_us` stays false and `signal_call_session_ended`
/// returns NoChange, so closing the call never suggests stop for a
/// manually-started meeting. Idempotent; returns 1.
#[no_mangle]
pub extern "C" fn dimmy_call_meeting_started_external() -> c_int {
    let mut g = call_detector_lock();
    g.meeting_started_external();
    1
}

/// Record the user's response to a call-detected nudge.
/// `response` ∈ {"record_now","not_now","never","timeout"}.
/// On "never" with a non-empty app_id the exclusion list is persisted
/// to disk via the AppState + save_config_file round-trip so the
/// decision survives restarts. Returns 0 / -1 (null) / -2 (unknown
/// response string).
///
/// # Safety
/// `app_id` must be NULL or a valid null-terminated UTF-8 C string.
/// `response` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn dimmy_call_signal_response(
    app_id: *const c_char,
    response: *const c_char,
) -> c_int {
    if response.is_null() {
        return -1;
    }
    let resp_str = match CStr::from_ptr(response).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    use crate::call_detector::NudgeResponse;
    let resp = match resp_str {
        "record_now" => NudgeResponse::RecordNow,
        "not_now" => NudgeResponse::NotNow,
        "never" => NudgeResponse::Never,
        "timeout" => NudgeResponse::Timeout,
        "stop_and_recap" => NudgeResponse::StopAndRecap,
        "keep_recording" => NudgeResponse::KeepRecording,
        "stop_timeout" => NudgeResponse::StopTimeout,
        _ => return -2,
    };
    let app: Option<String> = if app_id.is_null() {
        None
    } else {
        match CStr::from_ptr(app_id).to_str() {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_lowercase()),
            _ => None,
        }
    };

    let persist_excluded = matches!(resp, NudgeResponse::Never) && app.is_some();
    if persist_excluded {
        if let Some(a) = app.as_ref() {
            if let Some(st) = GLOBAL_STATE.get() {
                if let Ok(mut list) = st.call_detect_excluded_apps.lock() {
                    if !list.contains(a) {
                        list.push(a.clone());
                    }
                }
                if let Ok(cfg) = crate::snapshot_config(st) {
                    save_config_file(&cfg);
                }
            }
        }
    }

    let now = now_epoch_secs();
    let mut g = call_detector_lock();
    g.record_response(app, resp, now);
    0
}

/// Write a JSON snapshot of the detector's runtime state to `out`.
/// Surfaces enabled flag, excluded apps, active cooldowns (per app
/// with seconds remaining), mic-active status, current_app, debounce
/// position. Used by the Settings UI to render the "Auto-detect"
/// section without polling individual fields.
///
/// # Safety
/// `out` must point to a writable buffer of at least `out_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn dimmy_call_detector_state(out: *mut c_char, out_len: c_int) -> c_int {
    let now = now_epoch_secs();
    let json = {
        let g = call_detector_lock();
        g.state_snapshot(now).to_string()
    };
    write_to_buf(&json, out, out_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::c_char;

    // ── categorize_llm_error_to_rc tests ─────────────────────────
    //
    // Burned 2026-05-14: a recap_model_override of "gemini-3.1-pro"
    // with llm_api_url=anthropic.com produced a 404 + body
    // `{"error":{"type":"not_found_error","message":"model: gemini-3.1-pro"}}`
    // — the old dispatch returned generic rc=-3 and the UI rendered
    // "recap failed (rc=-3)" with no actionable hint. The fix
    // categorises the error so the UI can render
    // "Recap model X not supported by configured provider. Open
    // Settings → Recap to fix." Pin each branch.

    use crate::error::LlmError;

    #[test]
    fn categorize_404_model_not_found_returns_minus_5() {
        // The exact body shape Anthropic returned for the
        // 2026-05-14 incident.
        let e = LlmError::Api {
            status: 404,
            body: r#"{"type":"error","error":{"type":"not_found_error","message":"model: gemini-3.1-pro"},"request_id":"req_..."}"#.to_string(),
        };
        assert_eq!(categorize_llm_error_to_rc(&e), -5);
    }

    #[test]
    fn categorize_404_without_model_substring_returns_generic_minus_3() {
        let e = LlmError::Api {
            status: 404,
            body: "Not Found".to_string(),
        };
        assert_eq!(categorize_llm_error_to_rc(&e), -3);
    }

    #[test]
    fn categorize_401_403_auth_returns_minus_6() {
        for status in [401u16, 403] {
            let e = LlmError::Api {
                status,
                body: "Unauthorized".into(),
            };
            assert_eq!(
                categorize_llm_error_to_rc(&e),
                -6,
                "status {} should map to -6",
                status
            );
        }
    }

    #[test]
    fn categorize_429_rate_limit_returns_minus_7() {
        let e = LlmError::Api {
            status: 429,
            body: "Too Many Requests".into(),
        };
        assert_eq!(categorize_llm_error_to_rc(&e), -7);
    }

    #[test]
    fn categorize_5xx_returns_generic_minus_3() {
        for status in [500u16, 502, 503, 504] {
            let e = LlmError::Api {
                status,
                body: "server".into(),
            };
            assert_eq!(categorize_llm_error_to_rc(&e), -3, "status {}", status);
        }
    }

    #[test]
    fn categorize_network_returns_minus_8() {
        let e = LlmError::Network("connection refused".into());
        assert_eq!(categorize_llm_error_to_rc(&e), -8);
    }

    #[test]
    fn categorize_no_api_key_returns_minus_6() {
        // Treat NoApiKey as "auth issue" — same rc as 401/403 so
        // the Win UI can render a single "fix your key" message.
        let e = LlmError::NoApiKey("groq".into());
        assert_eq!(categorize_llm_error_to_rc(&e), -6);
    }

    #[test]
    fn categorize_local_model_returns_minus_4() {
        let e = LlmError::LocalModel("model file not found".into());
        assert_eq!(categorize_llm_error_to_rc(&e), -4);
    }

    /// SECURITY guard: the body field of LlmError::Api can contain
    /// the user's prompt echoed back. Categorisation must NEVER
    /// surface the body — only the categorical rc. This test
    /// asserts the function returns a plain integer (no body
    /// content can possibly leak via the rc).
    #[test]
    fn categorize_never_leaks_body_text_via_return() {
        let secret = "MY-SECRET-TRANSCRIPT-CONTENT";
        let e = LlmError::Api {
            status: 404,
            body: format!(r#"{{"error":"model:{}"}}"#, secret),
        };
        let rc = categorize_llm_error_to_rc(&e);
        // rc is a plain integer; if it were a string buffer, this
        // assertion would catch the leak. The test exists to fail
        // visibly if a future refactor changes the return type to
        // include the body in any form.
        assert!(rc.abs() < 100, "rc must be a small integer, got {}", rc);
    }

    // ── parse_recap_override tests ──────────────────────────────────

    #[test]
    fn parse_recap_override_local_prefix() {
        let (provider, model) = parse_recap_override("local:gemma-4-E4B-it-Q4_K_M.gguf");
        assert_eq!(provider, "local");
        assert_eq!(model, "gemma-4-E4B-it-Q4_K_M.gguf");
    }

    #[test]
    fn parse_recap_override_cloud_prefix() {
        let (provider, model) = parse_recap_override("cloud:claude-opus-4-7");
        assert_eq!(provider, "cloud");
        assert_eq!(model, "claude-opus-4-7");
    }

    #[test]
    fn parse_recap_override_bare_treated_as_cloud_legacy() {
        // A pre-prefix `recap_model_override` value — has to keep
        // working when the local-recap feature ships, otherwise
        // every Mac/Win install that already picked Opus 4.7 from
        // the dropdown silently switches to local Gemma on upgrade.
        let (provider, model) = parse_recap_override("claude-opus-4-7");
        assert_eq!(provider, "cloud");
        assert_eq!(model, "claude-opus-4-7");
    }

    #[test]
    fn parse_recap_override_empty_falls_back() {
        let (provider, model) = parse_recap_override("");
        assert_eq!(provider, "");
        assert_eq!(model, "");
    }

    #[test]
    fn parse_recap_override_local_prefix_stripped_cleanly() {
        // Make sure we don't accidentally leak the prefix into the
        // filename — `local:gemma-…` must become `gemma-…`, not
        // `:gemma-…` or `local:gemma-…`.
        let (_, model) = parse_recap_override("local:foo.gguf");
        assert!(!model.starts_with(':'), "leading colon leaked: {}", model);
        assert!(!model.starts_with("local"), "prefix leaked: {}", model);
    }

    #[test]
    fn parse_recap_override_unknown_prefix_treated_as_bare_cloud() {
        // A future-prefix we don't recognise yet (`auto:`, `xyz:`)
        // shouldn't crash — it falls through to the bare-string
        // path and is treated as a (probably bogus) cloud model id.
        // The cloud branch downstream handles the bogus id with
        // its existing HTTP-error rc.
        let (provider, model) = parse_recap_override("xyz:something");
        assert_eq!(provider, "cloud");
        assert_eq!(model, "xyz:something");
    }

    // ── resolve_live_final (work-loss guard) tests ──────────────────

    #[test]
    fn live_final_uses_nonempty_streaming() {
        // The happy path: Deepgram streaming produced text live → it wins.
        let r = resolve_live_final(Some("hello world".into()), None);
        assert_eq!(r.as_deref(), Some("hello world"));
    }

    #[test]
    fn live_final_empty_streaming_falls_back_to_batch() {
        // Work-loss guard: an empty streaming result (WS dropped / auth
        // failure) must NOT win — returning None tells the caller to run a
        // batch pass on the buffered audio instead of discarding it.
        assert_eq!(resolve_live_final(Some(String::new()), None), None);
        assert_eq!(resolve_live_final(Some("   ".into()), None), None);
        assert_eq!(resolve_live_final(Some("\n\t ".into()), None), None);
    }

    #[test]
    fn live_final_uses_nonempty_chunked_when_no_streaming() {
        let r = resolve_live_final(None, Some("chunked text".into()));
        assert_eq!(r.as_deref(), Some("chunked text"));
    }

    #[test]
    fn live_final_empty_chunked_falls_back_to_batch() {
        assert_eq!(resolve_live_final(None, Some("  ".into())), None);
    }

    #[test]
    fn live_final_none_when_no_live_workers() {
        // No streaming, no chunked — the pure-batch dictation path.
        assert_eq!(resolve_live_final(None, None), None);
    }

    #[test]
    fn live_final_streaming_wins_over_chunked() {
        // They never both run, but the ordering is fixed for determinism.
        let r = resolve_live_final(Some("stream".into()), Some("chunk".into()));
        assert_eq!(r.as_deref(), Some("stream"));
    }

    #[test]
    fn live_final_empty_streaming_yields_to_nonempty_chunked() {
        let r = resolve_live_final(Some(String::new()), Some("chunk".into()));
        assert_eq!(r.as_deref(), Some("chunk"));
    }

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
                user_dict: Mutex::new(Vec::new()),
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
                llm_auth_method: Mutex::new("api_key".to_string()),
                recap_auth_method: Mutex::new(String::new()),
                recap_use_same_key: Mutex::new(true),
                llm_api_key: Mutex::new(None),
                llm_log_enabled: Mutex::new(false),
                recap_model_override: Mutex::new(String::new()),
                chunk_streaming_enabled: Mutex::new(false),
                streaming_dictation: Mutex::new(false),
                preprocessing_enabled: Mutex::new(true),
                audio_debug_enabled: Mutex::new(false),
                ggml_debug_logging: Mutex::new(false),
                use_keyring: Mutex::new(false),
                stt_mode: Mutex::new("local".to_string()),
                local_model: Mutex::new("ggml-base-q8_0.bin".to_string()),
                local_stt_backend: Mutex::new("whisper".to_string()),
                live_captions_enabled: Mutex::new(true),
                call_detect_enabled: Mutex::new(true),
                call_detect_excluded_apps: Mutex::new(vec!["discord".to_string()]),
                call_detect_cooldown_secs: Mutex::new(1800),
                call_detect_min_active_secs: Mutex::new(5),
                call_detect_timeout_cooldown_secs: Mutex::new(300),
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
                loopback_gain: Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
                audio_source: Mutex::new("mic".to_string()),
                meeting_chunk_secs: Mutex::new(15.0f32),
                meeting_storage_path: Mutex::new(String::new()),
                notion_target_id: Mutex::new(String::new()),
                notion_target_kind: Mutex::new(String::new()),
                notion_target_title: Mutex::new(String::new()),
                notion_auto_send: Mutex::new(false),
                audio_buffer_secondary: Arc::new(Mutex::new(Vec::new())),
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
    #[serial]
    fn has_api_key_returns_1_when_key_set() {
        ensure_test_state();
        let result = dimmy_has_api_key();
        assert_eq!(result, 1, "test state has api key");
    }

    // ── dimmy_is_recording tests ────────────────────────────────────

    #[test]
    #[serial]
    fn is_recording_returns_0_when_not_recording() {
        ensure_test_state();
        // Ensure not recording
        if let Ok(mut r) = state().recording.lock() {
            *r = false;
        }
        assert_eq!(dimmy_is_recording(), 0);
    }

    #[test]
    #[serial]
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
    #[serial]
    fn get_amplitude_returns_zero_for_empty_buffer() {
        ensure_test_state();
        if let Ok(mut b) = state().audio_buffer.lock() {
            b.clear();
        }
        let amp = dimmy_get_amplitude();
        assert_eq!(amp, 0.0);
    }

    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
    fn get_config_json_null_buf_returns_neg1() {
        ensure_test_state();
        let result = dimmy_get_config_json(std::ptr::null_mut(), 100);
        assert_eq!(result, -1);
    }

    // ── dimmy_set_config_json tests ─────────────────────────────────

    #[test]
    #[serial]
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
    #[serial]
    fn set_config_json_null_ptr_returns_neg1() {
        ensure_test_state();
        let result = unsafe { dimmy_set_config_json(std::ptr::null()) };
        assert_eq!(result, -1);
    }

    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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

    // ── meeting_state event tests ──────────────────────────────────
    // Replaces the 500 ms `_meetingStatePollTimer` (Win) / 0.5 s
    // `meetingStatePollTimer` (Mac) on the pill. Each meeting state
    // transition fires a `meeting_state` envelope so the host UI
    // updates exactly when state changes — no wasted FFI calls per
    // second when nothing's happening, and no perceived latency.

    use std::sync::OnceLock as TestOnceLock;

    fn captured_events_slot() -> &'static Mutex<Vec<String>> {
        static SLOT: TestOnceLock<Mutex<Vec<String>>> = TestOnceLock::new();
        SLOT.get_or_init(|| Mutex::new(Vec::new()))
    }

    extern "C" fn capture_event_to_slot(json_ptr: *const c_char) {
        if json_ptr.is_null() {
            return;
        }
        let cstr = unsafe { CStr::from_ptr(json_ptr) };
        if let Ok(s) = cstr.to_str() {
            if let Ok(mut v) = captured_events_slot().lock() {
                v.push(s.to_string());
            }
        }
    }

    fn reset_event_capture() {
        if let Ok(mut v) = captured_events_slot().lock() {
            v.clear();
        }
        dimmy_set_event_callback(capture_event_to_slot);
    }

    fn drain_event_capture() -> Vec<String> {
        let out = captured_events_slot()
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        if let Ok(mut guard) = EVENT_CALLBACK.lock() {
            *guard = None;
        }
        out
    }

    fn last_meeting_state_event(events: &[String]) -> Option<(bool, bool)> {
        for ev in events.iter().rev() {
            let v: serde_json::Value = match serde_json::from_str(ev) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("event").and_then(|e| e.as_str()) == Some("meeting_state") {
                let p = v.get("payload")?;
                let active = p.get("active")?.as_bool()?;
                let paused = p.get("paused")?.as_bool()?;
                return Some((active, paused));
            }
        }
        None
    }

    #[test]
    fn emit_meeting_state_event_emits_envelope_on_running() {
        reset_event_capture();
        emit_meeting_state_event(true, false);
        let events = drain_event_capture();
        let state =
            last_meeting_state_event(&events).expect("meeting_state event must be in the capture");
        assert_eq!(state, (true, false), "running ⇒ active=true, paused=false");
    }

    #[test]
    fn emit_meeting_state_event_emits_envelope_on_paused() {
        reset_event_capture();
        emit_meeting_state_event(true, true);
        let events = drain_event_capture();
        let state =
            last_meeting_state_event(&events).expect("meeting_state event must be in the capture");
        assert_eq!(state, (true, true), "paused ⇒ active=true, paused=true");
    }

    #[test]
    fn emit_meeting_state_event_emits_envelope_on_stopped() {
        reset_event_capture();
        emit_meeting_state_event(false, false);
        let events = drain_event_capture();
        let state =
            last_meeting_state_event(&events).expect("meeting_state event must be in the capture");
        assert_eq!(
            state,
            (false, false),
            "stopped ⇒ active=false, paused=false"
        );
    }

    #[test]
    fn meeting_pause_resume_no_meeting_does_not_emit_event() {
        // Without an active meeting, dimmy_meeting_pause / _resume must
        // be silent no-ops — no event fires (avoids confusing the host
        // UI with bogus "paused" notifications when nothing is running).
        reset_event_capture();
        let pause_rc = dimmy_meeting_pause();
        let resume_rc = dimmy_meeting_resume();
        let events = drain_event_capture();

        assert_eq!(pause_rc, 0, "pause without meeting must return 0");
        assert_eq!(resume_rc, 0, "resume without meeting must return 0");
        assert_eq!(
            last_meeting_state_event(&events),
            None,
            "no meeting_state event should fire on no-op transitions"
        );
    }

    #[test]
    fn emit_meeting_state_payload_is_valid_json_with_correct_keys() {
        reset_event_capture();
        emit_meeting_state_event(true, true);
        let events = drain_event_capture();
        assert!(!events.is_empty(), "must capture at least one event");
        let v: serde_json::Value =
            serde_json::from_str(&events[events.len() - 1]).expect("envelope must be valid JSON");
        assert_eq!(v["event"], "meeting_state");
        assert_eq!(v["payload"]["active"], true);
        assert_eq!(v["payload"]["paused"], true);
        // Hard-asserts the payload SHAPE the C# / Swift / GTK4 host
        // parsers depend on — drift would silently break the no-poll
        // pill state mirror without surfacing during compilation.
        assert!(
            v["payload"].as_object().unwrap().len() == 2,
            "payload must have exactly active + paused keys"
        );
    }

    // ── Negative space: invalid input returns error codes ──────────

    #[test]
    #[serial]
    fn update_stats_rejects_negative_words() {
        ensure_test_state();
        let result = dimmy_update_stats(-1, 5.0);
        assert_eq!(result, -1, "negative words must return -1");
    }

    #[test]
    #[serial]
    fn update_stats_rejects_negative_secs() {
        ensure_test_state();
        let result = dimmy_update_stats(0, -1.0);
        assert_eq!(result, -1, "negative secs must return -1");
    }

    #[test]
    #[serial]
    fn update_stats_rejects_nan_secs() {
        ensure_test_state();
        let result = dimmy_update_stats(0, f64::NAN);
        assert_eq!(result, -1, "NaN secs must return -1");
    }

    #[test]
    #[serial]
    fn update_stats_rejects_inf_secs() {
        ensure_test_state();
        let result = dimmy_update_stats(0, f64::INFINITY);
        assert_eq!(result, -1, "Inf secs must return -1");
    }

    #[test]
    #[serial]
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
    #[serial]
    fn cycle_tone_ignores_invalid_direction() {
        ensure_test_state();
        let before = *state().llm_tone.lock().unwrap();
        dimmy_cycle_llm_tone(5);
        let after = *state().llm_tone.lock().unwrap();
        assert_eq!(before, after, "tone should not change on invalid direction");
    }

    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
    fn process_with_llm_rejects_null_buffer() {
        ensure_test_state();
        let text = CString::new("hello world").unwrap();
        let result = unsafe { dimmy_process_with_llm(text.as_ptr(), std::ptr::null_mut(), 1024) };
        assert_eq!(result, -1, "null out_buf must return -1");
    }

    #[test]
    #[serial]
    fn process_with_llm_rejects_zero_buf_len() {
        ensure_test_state();
        let text = CString::new("hello world").unwrap();
        let mut buf = vec![0u8; 64];
        let result =
            unsafe { dimmy_process_with_llm(text.as_ptr(), buf.as_mut_ptr() as *mut c_char, 0) };
        assert_eq!(result, -1, "zero buf_len must return -1");
    }

    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
    fn check_audio_health_rejects_null_buffer() {
        ensure_test_state();
        let result = dimmy_check_audio_health(std::ptr::null_mut(), 100);
        assert_eq!(result, -1, "null buffer must return -1");
    }

    #[test]
    #[serial]
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
    #[serial]
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

    // ── Meeting-pause FFI surface ────────────────────────────────────
    // These mirror the integration test in tests/meeting_pause_resume.rs
    // but at the lib-test level so they run without the dylib link
    // shenanigans. Coverage for the FFI return-code contract:
    //   1  → state actually flipped (was running, now paused; or vice
    //        versa)
    //   0  → no-op (already in target state, or no meeting active)
    //  -1  → internal lock failure (not exercised here)

    #[test]
    #[serial]
    fn meeting_pause_no_op_without_meeting() {
        ensure_test_state();
        // Defensive cleanup in case a prior test left a session.
        if let Ok(mut g) = MEETING.lock() {
            *g = None;
        }
        assert_eq!(dimmy_meeting_is_active(), 0, "no meeting active to start");
        assert_eq!(dimmy_meeting_pause(), 0, "pause without meeting → 0");
        assert_eq!(dimmy_meeting_resume(), 0, "resume without meeting → 0");
        assert_eq!(
            dimmy_meeting_is_paused(),
            0,
            "is_paused without meeting → 0"
        );
    }

    #[test]
    #[serial]
    fn start_recording_blocked_when_meeting_active() {
        ensure_test_state();
        // Drop any leftover MEETING from prior tests that touched it.
        if let Ok(mut g) = MEETING.lock() {
            *g = None;
        }
        // Inject a fake meeting session into the global slot via the
        // public start path with a cloud STT snapshot; we tear it down
        // immediately after the assert so other tests aren't affected.
        use crate::audio::AudioSource;
        use crate::meeting::{MeetingSession, SttSnapshot};
        use std::sync::Arc;
        let stt = SttSnapshot {
            mode: "cloud".to_string(),
            api_url: "https://test.invalid/".to_string(),
            api_model: "x".to_string(),
            api_key: Some("x".to_string()),
            prompt: String::new(),
            local_model: String::new(),
            local_backend: "whisper".to_string(),
            language: "en".to_string(),
            chunk_secs: Some(15.0),
        };
        let primary: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let secondary: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let session = match MeetingSession::start(
            primary,
            secondary,
            48_000,
            48_000,
            AudioSource::Mix,
            stt,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[skip] start failed: {e}");
                return;
            }
        };
        if let Ok(mut g) = MEETING.lock() {
            *g = Some(session);
        }

        // Now meeting is "active" — dimmy_start_recording must return
        // -7 immediately.
        let rc = dimmy_start_recording();
        // Restore: stop the meeting before checking so we don't leak.
        let session = MEETING.lock().ok().and_then(|mut g| g.take());
        if let Some(s) = session {
            let _ = s.stop();
        }
        assert_eq!(
            rc, -7,
            "dimmy_start_recording must return -7 when a meeting is active"
        );
    }
}

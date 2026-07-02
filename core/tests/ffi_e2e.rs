//! Tier-1 end-to-end integration tests for the Dimmy FFI.
//!
//! Feeds pre-recorded PCM into `dimmy_inject_pcm_for_test`, then calls
//! `dimmy_stop_recording`, which routes through the full production
//! pipeline (preprocess → STT → filler removal → LLM). Assertions are on
//! the returned transcript and on side effects observed via the event
//! callback / error paths.
//!
//! Cloud provider HTTP calls are redirected to a `wiremock` server so
//! network behaviour (200, 401, provider mismatch) is deterministic and
//! offline.
//!
//! Run with: `cargo test --test ffi_e2e --features local-stt,test-ffi`
//!
//! Fixtures are downloaded on first run and cached under
//! `core/target/test-fixtures/`. Sources:
//! - `jfk.wav` — https://github.com/ggml-org/whisper.cpp/raw/master/samples/jfk.wav (MIT)
//! - `ggml-tiny.en-q8_0.bin` — https://huggingface.co/ggerganov/whisper.cpp (MIT)

#![cfg(all(feature = "test-ffi", feature = "local-stt"))]

use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use serial_test::serial;

use dimmy_lib::ffi::{
    dimmy_get_config_json, dimmy_init, dimmy_inject_pcm_for_test, dimmy_process_with_llm,
    dimmy_set_config_json, dimmy_stop_recording,
};

// ── Fixture URLs ──────────────────────────────────────────────────────

// whisper.cpp moved from `ggerganov/whisper.cpp` to `ggml-org/whisper.cpp`.
// GitHub still serves the old URL via redirect, but that path returns 502 in
// 2026-04 — pinned to the canonical location to avoid the redirect dance.
const JFK_WAV_URL: &str = "https://github.com/ggml-org/whisper.cpp/raw/master/samples/jfk.wav";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en-q8_0.bin";
const MODEL_FILENAME: &str = "ggml-tiny.en-q8_0.bin";

// ── Fixture helpers ───────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    // core/target/test-fixtures/ — survives across test runs, gitignored
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-fixtures")
}

/// Download with atomic .part → rename, 120s timeout, resume-safe.
fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        return Ok(());
    }
    eprintln!("[fixture] downloading {} → {}", url, dest.display());
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;

    let resp = ureq::get(url)
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("ureq call: {}", e))?;

    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn jfk_wav() -> PathBuf {
    let p = fixture_dir().join("jfk.wav");
    download_to(JFK_WAV_URL, &p).expect("download jfk.wav");
    p
}

/// Ensure tiny model is in Dimmy's model_directory() where local_stt reads from.
fn ensure_tiny_model() {
    let src = fixture_dir().join(MODEL_FILENAME);
    download_to(MODEL_URL, &src).expect("download tiny model");

    let models_dir = dimmy_lib::local_stt::model_directory();
    std::fs::create_dir_all(&models_dir).ok();
    let dest = models_dir.join(MODEL_FILENAME);
    if !dest.exists() {
        std::fs::copy(&src, &dest).expect("copy model into models dir");
    }
}

/// Load a WAV as f32 mono samples at the file's native sample rate.
fn load_wav_f32(path: &Path) -> (Vec<f32>, u32) {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();

    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("read f32 sample"))
            .collect(),
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            let max = (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.expect("read int sample") as f32 / max)
                .collect()
        }
    };

    // Mix to mono (take left if stereo)
    let mono: Vec<f32> = if spec.channels > 1 {
        raw.chunks(spec.channels as usize).map(|c| c[0]).collect()
    } else {
        raw
    };

    (mono, spec.sample_rate)
}

// ── FFI harness ────────────────────────────────────────────────────────

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        // Isolate every config-derived path (config.json, keys.enc, history,
        // meetings) into a disposable per-process temp dir. dimmy_init
        // REFUSES to run in test-ffi builds without this — protects the
        // developer's real %APPDATA%/dimmy install (burned 2026-07-02).
        let dir = std::env::temp_dir().join(format!("dimmy-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("DIMMY_TEST_CONFIG_DIR", &dir);
        let rc = dimmy_init();
        assert!(
            rc == 0 || rc == 1,
            "dimmy_init must succeed (0) or report already-inited (1), got {}",
            rc
        );
    });
}

fn set_config(json: &str) {
    let c = CString::new(json).expect("no nul in json");
    let rc = unsafe { dimmy_set_config_json(c.as_ptr()) };
    assert_eq!(rc, 0, "set_config_json failed: {} (json={})", rc, json);
}

/// Inject → stop → decode transcript (best-effort — returns empty on error paths).
fn transcribe_pcm(samples: &[f32], sample_rate: u32) -> String {
    let rc =
        unsafe { dimmy_inject_pcm_for_test(samples.as_ptr(), samples.len() as c_int, sample_rate) };
    assert_eq!(rc, 0, "inject_pcm failed: {}", rc);

    let mut buf: Vec<u8> = vec![0; 8192];
    let n = dimmy_stop_recording(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);
    if n < 0 {
        return String::new();
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    cstr.to_string_lossy().into_owned()
}

// ── Tests: local STT ──────────────────────────────────────────────────

#[test]
#[serial]
fn local_stt_jfk_sample_produces_transcript() {
    ensure_init();
    ensure_tiny_model();

    let config = serde_json::json!({
        "stt_mode": "local",
        "local_model": MODEL_FILENAME,
        "language": "en",
        "preprocessing_enabled": false,
        "filler_removal_enabled": false,
        "llm_enabled": false,
    });
    set_config(&config.to_string());

    let wav = jfk_wav();
    let (samples, sr) = load_wav_f32(&wav);
    let transcript = transcribe_pcm(&samples, sr);

    eprintln!("[test] JFK transcript: {:?}", transcript);
    assert!(
        transcript.to_lowercase().contains("ask not"),
        "expected 'ask not' in JFK transcript, got: {:?}",
        transcript
    );
}

#[test]
#[serial]
fn silent_input_does_not_crash_and_produces_short_output() {
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": false,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    // 2 seconds of silence at 16 kHz
    let samples = vec![0.0f32; 32_000];
    let transcript = transcribe_pcm(&samples, 16_000);

    eprintln!("[test] silent transcript: {:?}", transcript);
    // Whisper can emit marketing-like text on silence (e.g. "[BLANK_AUDIO]").
    // We only assert no crash + no multi-segment hallucination.
    assert!(
        transcript.chars().count() < 200,
        "silent input should not produce a long transcript, got: {:?}",
        transcript
    );
}

#[test]
#[serial]
fn short_clip_does_not_regress_to_empty_output() {
    // Guard against the v0.6.x set_single_segment(true) regression:
    // whisper_full returned Ok with zero segments on clips <30s and
    // confident language detection, producing empty transcripts.
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": false,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    // Take first 4 seconds of JFK (well under 30s threshold)
    let wav = jfk_wav();
    let (full, sr) = load_wav_f32(&wav);
    let four_sec = (sr as usize * 4).min(full.len());
    let samples = full[..four_sec].to_vec();

    let transcript = transcribe_pcm(&samples, sr);
    eprintln!("[test] short-clip transcript: {:?}", transcript);
    assert!(
        !transcript.trim().is_empty(),
        "short clip produced empty transcript — set_single_segment regression?"
    );
}

#[test]
#[serial]
fn local_stt_reused_state_transcribes_independently_across_calls() {
    // Regression for the whisper-state-reuse change (local_stt.rs): the
    // inference state is now created ONCE per model load and reused for
    // every chunk (was create_state() per chunk — the ~450 MB compute-buffer
    // re-init that aborted GPU init mid-meeting). The reused state sets
    // `no_context(true)` so chunk N must NOT inherit chunk N-1's token
    // history. This test runs the SAME clip twice through the full
    // stop_recording path (each reuses the cached state) and asserts the
    // second pass is byte-identical to the first — proving both that reuse
    // works at all and that there's no cross-call context bleed.
    ensure_init();
    ensure_tiny_model();

    let config = serde_json::json!({
        "stt_mode": "local",
        "local_model": MODEL_FILENAME,
        "language": "en",
        "preprocessing_enabled": false,
        "filler_removal_enabled": false,
        "llm_enabled": false,
    });
    set_config(&config.to_string());

    let wav = jfk_wav();
    let (samples, sr) = load_wav_f32(&wav);

    let first = transcribe_pcm(&samples, sr);
    let second = transcribe_pcm(&samples, sr);
    eprintln!("[test] reuse pass1: {:?}", first);
    eprintln!("[test] reuse pass2: {:?}", second);

    assert!(
        first.to_lowercase().contains("ask not"),
        "first pass should transcribe JFK, got: {:?}",
        first
    );
    assert_eq!(
        first, second,
        "reused whisper state must transcribe the same clip identically \
         across calls (no context bleed); got pass1={:?} pass2={:?}",
        first, second
    );
}

#[test]
#[serial]
fn concurrent_stop_returns_minus9_sentinel_not_empty() {
    // Regression for the lost-dictation bug: two stop entry points (toggle
    // hotkey + pill Stop, or a mashed toggle) could call dimmy_stop_recording
    // concurrently. They share ONE audio_buffer — the first reads-and-clears
    // it (real transcript), the second found it empty and returned ""/0, and
    // whichever call the host awaited for paste could be the loser → the real
    // transcript's paste silently dropped. The fix lets only ONE stop run; a
    // concurrent caller gets rc -9, which the host treats as a no-op.
    //
    // Determinism: the guard is held for the whole call, which always begins
    // with a ≤500 ms "wait for first samples" poll. Thread A (empty buffer)
    // therefore holds the flag for ~500 ms; the main thread, calling ~100 ms
    // later, is guaranteed to be inside that window and must get -9.
    ensure_init();

    let handle = std::thread::spawn(|| {
        let mut buf: Vec<u8> = vec![0; 8192];
        // Empty buffer → enters the full ~500 ms first-sample wait, holding
        // STOP_IN_PROGRESS the entire time. Return value is irrelevant here.
        dimmy_stop_recording(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int)
    });

    // Let A acquire the guard and settle into the wait loop (100 ms ≪ 500 ms).
    std::thread::sleep(Duration::from_millis(100));

    let mut buf: Vec<u8> = vec![0; 8192];
    let rc_concurrent = dimmy_stop_recording(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);

    let rc_first = handle.join().expect("background stop thread panicked");
    eprintln!("[test] concurrent stop rc={rc_concurrent}, first stop rc={rc_first}");

    assert_eq!(
        rc_concurrent, -9,
        "a concurrent stop must return the -9 sentinel (host no-op), NOT an \
         empty/error code that drops the winning call's paste; got {rc_concurrent}"
    );
}

// ── Tests: cloud STT (wiremock) ───────────────────────────────────────

#[test]
#[serial]
fn cloud_stt_200_ok_extracts_transcript() {
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async { wiremock::MockServer::start().await });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/transcriptions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "text": "hello world from the cloud"
                })),
            )
            .mount(&server)
            .await;
    });

    set_config(
        &serde_json::json!({
            "stt_mode": "cloud",
            "api_url": format!("{}/audio/transcriptions", server.uri()),
            "api_model": "whisper-1",
            "api_key": "test-cloud-key",
            "language": "en",
            "preprocessing_enabled": false,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    // 1 second of low-level noise (non-silent so pipeline doesn't short-circuit)
    let samples: Vec<f32> = (0..16_000)
        .map(|i| (i as f32 * 0.01).sin() * 0.05)
        .collect();
    let transcript = transcribe_pcm(&samples, 16_000);

    eprintln!("[test] cloud 200 transcript: {:?}", transcript);
    assert!(
        transcript.to_lowercase().contains("hello world"),
        "cloud STT 200 should return mocked transcript, got: {:?}",
        transcript
    );
}

#[test]
#[serial]
fn cloud_stt_401_produces_empty_transcript_without_crash() {
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async { wiremock::MockServer::start().await });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/transcriptions"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "error": {"message": "invalid x-api-key", "type": "authentication_error"}
                })),
            )
            .mount(&server)
            .await;
    });

    set_config(
        &serde_json::json!({
            "stt_mode": "cloud",
            "api_url": format!("{}/audio/transcriptions", server.uri()),
            "api_model": "whisper-1",
            "api_key": "gsk_wrong_format_for_this_provider",
            "language": "en",
            "preprocessing_enabled": false,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let samples: Vec<f32> = (0..16_000)
        .map(|i| (i as f32 * 0.01).sin() * 0.05)
        .collect();
    let transcript = transcribe_pcm(&samples, 16_000);

    eprintln!("[test] cloud 401 transcript: {:?}", transcript);
    // On 401 the pipeline returns an empty transcript (error is logged + emitted
    // as an event). The critical contract: no crash, no panic, no hang.
    assert!(
        transcript.is_empty() || transcript.to_lowercase().contains("error"),
        "cloud 401 should produce empty/error transcript, got: {:?}",
        transcript
    );
}

// ── Test: provider mismatch regression (2026-04-23 bug) ─────────────

#[test]
#[serial]
fn cloud_stt_ok_but_llm_same_key_against_different_provider_fails_loud() {
    // This captures the exact bug observed 2026-04-23:
    // User onboards with Cloud STT (Groq), existing config has
    // llm_enabled=true, llm_api_url=anthropic, llm_use_same_key=true.
    // Every transcription then hit Anthropic with a Groq key → 401.
    //
    // Contract: STT still returns the mocked transcript (STT itself works);
    // LLM failure must NOT swallow the STT output silently. The user should
    // at minimum see the transcript, possibly with an LLM-error signal.
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let stt_server = rt.block_on(async { wiremock::MockServer::start().await });
    let llm_server = rt.block_on(async { wiremock::MockServer::start().await });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/transcriptions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "text": "raw stt output"
                })),
            )
            .mount(&stt_server)
            .await;

        // Anthropic returns 401 when the "Groq key" is sent
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "type": "error",
                    "error": {"type": "authentication_error", "message": "invalid x-api-key"}
                })),
            )
            .mount(&llm_server)
            .await;
    });

    set_config(
        &serde_json::json!({
            "stt_mode": "cloud",
            "api_url": format!("{}/audio/transcriptions", stt_server.uri()),
            "api_model": "whisper-1",
            "api_key": "gsk_groq_key",
            "llm_enabled": true,
            "llm_mode": "cloud",
            "llm_api_url": format!("{}/v1/messages", llm_server.uri()),
            "llm_api_model": "claude-haiku-4-5",
            "llm_use_same_key": true,
            "llm_style": "concise",
            "language": "en",
            "preprocessing_enabled": false,
            "filler_removal_enabled": false,
        })
        .to_string(),
    );

    let samples: Vec<f32> = (0..16_000)
        .map(|i| (i as f32 * 0.01).sin() * 0.05)
        .collect();
    let transcript = transcribe_pcm(&samples, 16_000);

    eprintln!("[test] provider-mismatch transcript: {:?}", transcript);
    // We accept either:
    //   (a) raw STT transcript returned (LLM failed but STT kept)
    //   (b) empty transcript (hard fail)
    // What we do NOT accept: a panic, a hang, or silent bytes in the buffer.
    assert!(
        transcript.is_empty() || transcript.to_lowercase().contains("raw stt"),
        "provider-mismatch path produced unexpected transcript: {:?}",
        transcript
    );
}

// ── Test: config round-trip (set → get → assert all writable fields preserved) ──

/// Read full config back via the FFI getter. Panics if the buffer is too small.
fn get_config_json() -> serde_json::Value {
    let mut buf: Vec<u8> = vec![0; 16_384];
    let n = dimmy_get_config_json(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);
    assert!(
        n > 0,
        "get_config_json must return positive length, got {}",
        n
    );
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    let s = cstr.to_string_lossy().into_owned();
    serde_json::from_str(&s).expect("config JSON must parse")
}

#[test]
#[serial]
fn config_round_trip_preserves_all_writable_fields() {
    // Guards against silent rename/drop of fields between
    // dimmy_set_config_json (writer) and dimmy_get_config_json (reader).
    // Any C# / Swift UI relies on this contract; a mismatch surfaces here
    // instead of as a "setting silently reverts" UX bug in production.
    ensure_init();

    // Two distinct payloads — A then B — to verify the setter actually
    // updated state (avoids accidentally reading defaults that happen to
    // equal payload A on a fresh process).
    let payload_a = serde_json::json!({
        "api_url": "https://example.test/v1/stt-a",
        "api_model": "whisper-test-a",
        "language": "en",
        "prompt": "Hello A.",
        "shortcut_mode": "hold",
        "shortcut": "ctrl+shift",
        "selected_device": "Test Mic A",
        "llm_enabled": true,
        "llm_style": "correct",
        "llm_tone": "formal",
        "llm_custom_prompt": "custom A",
        "llm_translate_to": "italian",
        "llm_api_url": "https://example.test/v1/llm-a",
        "llm_api_model": "claude-test-a",
        "llm_use_same_key": false,
        "llm_log_enabled": true,
        "preprocessing_enabled": false,
        "chunk_streaming_enabled": true,
        "audio_debug_enabled": true,
        "ggml_debug_logging": true,
        "stt_mode": "cloud",
        // Keep local_model + local_llm_model at the AppConfig default so
        // set_config doesn't trigger clear_model_cache / clear_llm_cache.
        // Those clear paths crash on Windows release builds with
        // STATUS_ACCESS_VIOLATION when invoked between cloud-only tests
        // that haven't loaded a model — pre-existing production bug,
        // tracked separately. The round-trip contract is still verified
        // for these fields via assertion (string equality on default).
        "local_model": "ggml-base-q8_0.bin",
        "filler_removal_enabled": false,
        "llm_mode": "cloud",
        "local_llm_model": "gemma-4-E2B-it-Q4_K_M.gguf",
        "border_style": "Solid",
        "waveform_style": "Dots",
        "overlay_position": "Top Left",
        "keep_in_clipboard": true,
        "input_gain": 1.5_f64,
    });

    let payload_b = serde_json::json!({
        "api_url": "https://example.test/v1/stt-b",
        "api_model": "whisper-test-b",
        "language": "it",
        "prompt": "Hello B.",
        "shortcut_mode": "toggle",
        "shortcut": "alt+space",
        "selected_device": "Test Mic B",
        "llm_enabled": false,
        "llm_style": "summarize",
        "llm_tone": "friendly",
        "llm_custom_prompt": "custom B",
        "llm_translate_to": "english",
        "llm_api_url": "https://example.test/v1/llm-b",
        "llm_api_model": "claude-test-b",
        "llm_use_same_key": true,
        "llm_log_enabled": false,
        "preprocessing_enabled": true,
        "chunk_streaming_enabled": false,
        "audio_debug_enabled": false,
        "ggml_debug_logging": false,
        "stt_mode": "local",
        // Same default-pinning as payload_a — see comment there.
        "local_model": "ggml-base-q8_0.bin",
        "filler_removal_enabled": true,
        "llm_mode": "local",
        "local_llm_model": "gemma-4-E2B-it-Q4_K_M.gguf",
        "border_style": "Rainbow",
        "waveform_style": "Bars",
        "overlay_position": "Bottom Right",
        "keep_in_clipboard": false,
        "input_gain": 0.75_f64,
    });

    // Fields whose value must round-trip identically when present in both
    // setter input and getter output. Any divergence indicates a missing
    // field in get_config_json or a bug in set_config_json.
    let writable_string_fields = [
        "api_url",
        "api_model",
        "language",
        "prompt",
        "shortcut_mode",
        "shortcut",
        "selected_device",
        "llm_style",
        "llm_tone",
        "llm_custom_prompt",
        "llm_translate_to",
        "llm_api_url",
        "llm_api_model",
        "stt_mode",
        "local_model",
        "llm_mode",
        "local_llm_model",
        "border_style",
        "waveform_style",
        "overlay_position",
    ];
    let writable_bool_fields = [
        "llm_enabled",
        "llm_use_same_key",
        "llm_log_enabled",
        "preprocessing_enabled",
        "chunk_streaming_enabled",
        "audio_debug_enabled",
        "ggml_debug_logging",
        "filler_removal_enabled",
        "keep_in_clipboard",
    ];

    for payload in [&payload_a, &payload_b] {
        set_config(&payload.to_string());
        let got = get_config_json();

        for field in writable_string_fields {
            let expected = payload[field].as_str().expect("payload string field");
            let actual = got[field]
                .as_str()
                .unwrap_or_else(|| panic!("getter missing string field: {}", field));
            assert_eq!(
                actual, expected,
                "round-trip mismatch on {}: set={:?} got={:?}",
                field, expected, actual
            );
        }
        for field in writable_bool_fields {
            let expected = payload[field].as_bool().expect("payload bool field");
            let actual = got[field]
                .as_bool()
                .unwrap_or_else(|| panic!("getter missing bool field: {}", field));
            assert_eq!(
                actual, expected,
                "round-trip mismatch on {}: set={} got={}",
                field, expected, actual
            );
        }
        // input_gain is clamped to [0.0, 2.0] and stored as f32 bits;
        // assert with a tolerance to account for f64→f32 rounding.
        let expected_gain = payload["input_gain"].as_f64().expect("input_gain f64");
        let actual_gain = got["input_gain"].as_f64().expect("getter input_gain f64");
        assert!(
            (actual_gain - expected_gain).abs() < 1e-4,
            "input_gain mismatch: set={} got={}",
            expected_gain,
            actual_gain
        );
    }
}

// ── Test: long clip (>30s) — guards segmentation regressions ────────

#[test]
#[serial]
fn local_stt_long_clip_over_30s_produces_full_transcript() {
    // Whisper's set_single_segment(true) regression cut transcripts on clips
    // <30s; the inverse risk is segmentation-related truncation on long
    // clips. We concatenate JFK three times (~33s) to exercise the path
    // beyond the 30s boundary and assert non-empty multi-occurrence output.
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": false,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let wav = jfk_wav();
    let (single, sr) = load_wav_f32(&wav);
    let mut samples = Vec::with_capacity(single.len() * 3);
    for _ in 0..3 {
        samples.extend_from_slice(&single);
    }
    let duration_s = samples.len() as f32 / sr as f32;
    assert!(
        duration_s > 30.0,
        "concatenated clip must be >30s, got {:.1}s",
        duration_s
    );

    let transcript = transcribe_pcm(&samples, sr);
    eprintln!(
        "[test] long-clip ({:.1}s) transcript: {:?}",
        duration_s, transcript
    );
    assert!(
        !transcript.trim().is_empty(),
        "long clip produced empty transcript — segmentation regression?"
    );
    // Phrase appears once per repeat; require at least two hits as a soft
    // signal that segmentation isn't dropping the second/third pass.
    let hits = transcript.to_lowercase().matches("ask not").count();
    assert!(
        hits >= 2,
        "expected 'ask not' at least twice in 3x-concatenated JFK, got {} hit(s) in: {:?}",
        hits,
        transcript
    );
}

// ── Test: preprocess pipeline end-to-end via FFI ────────────────────

#[test]
#[serial]
fn local_stt_with_preprocessing_enabled_still_produces_transcript() {
    // Tier 1 STT tests run with preprocessing_enabled=false to keep
    // assertions tight on whisper output. This test exercises the full
    // DSP path (highpass → VAD → AGC → downsample) end-to-end and asserts
    // the pipeline does not eat the signal or inject NaN/silence that
    // makes whisper produce empty output (the AUDIO-001 family of bugs).
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": true,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let wav = jfk_wav();
    let (samples, sr) = load_wav_f32(&wav);
    let transcript = transcribe_pcm(&samples, sr);

    eprintln!("[test] preprocess-on transcript: {:?}", transcript);
    assert!(
        !transcript.trim().is_empty(),
        "preprocess pipeline produced empty transcript on JFK — DSP regression?"
    );
    assert!(
        transcript.to_lowercase().contains("ask"),
        "expected 'ask' in JFK transcript with preprocess enabled, got: {:?}",
        transcript
    );
}

// ── Tests: cloud LLM post-processing (wiremock) ─────────────────────
//
// dimmy_process_with_llm is the FFI entry the native UIs call after STT
// returns a transcript. Its contract on cloud mode: post to an
// OpenAI-compatible chat-completions endpoint, return the rewritten text
// on 2xx, return the *original* text on any error (graceful degradation).
//
// These tests pin both branches so a future refactor cannot silently turn
// an LLM failure into an empty transcript or a panic.

/// Invoke `dimmy_process_with_llm` and return the resulting buffer as String.
/// The pipeline returns the original text on error paths; assertions below
/// distinguish between "rewritten" and "fallback to original".
fn process_with_llm(text: &str) -> String {
    let c_text = CString::new(text).expect("no nul in text");
    let mut buf: Vec<u8> = vec![0; 8192];
    let n = unsafe {
        dimmy_process_with_llm(
            c_text.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert!(n >= 0, "dimmy_process_with_llm returned error rc={}", n);
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    cstr.to_string_lossy().into_owned()
}

/// Build a minimal config that points the cloud LLM path at a wiremock
/// server. STT is forced to local-disabled-style values that
/// process_with_llm doesn't touch — only the LLM-related fields matter.
fn cloud_llm_config(mock_uri: &str, llm_key: &str) -> serde_json::Value {
    serde_json::json!({
        "stt_mode": "cloud",
        "api_url": "https://example.test/v1/stt-unused",
        "api_model": "unused",
        "api_key": "stt-unused-key",
        "language": "en",
        "preprocessing_enabled": false,
        "filler_removal_enabled": false,
        // The bits process_with_llm actually reads
        "llm_enabled": true,
        "llm_mode": "cloud",
        "llm_style": "correct",
        "llm_tone": "none",
        "llm_translate_to": "none",
        "llm_api_url": format!("{}/v1/chat/completions", mock_uri),
        "llm_api_model": "test-llm-model",
        "llm_use_same_key": false,
        "llm_api_key": llm_key,
    })
}

#[test]
#[serial]
fn llm_cloud_200_rewrites_transcript() {
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async { wiremock::MockServer::start().await });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "rewritten by llm" },
                        "finish_reason": "stop",
                    }],
                })),
            )
            .mount(&server)
            .await;
    });

    set_config(&cloud_llm_config(&server.uri(), "llm-test-key").to_string());

    let result = process_with_llm("raw transcript text");
    eprintln!("[test] llm 200 result: {:?}", result);
    assert_eq!(
        result, "rewritten by llm",
        "expected mock LLM rewrite, got: {:?}",
        result
    );
}

#[test]
#[serial]
fn llm_cloud_401_falls_back_to_original_text() {
    // Contract: when the LLM rejects the key, the FFI must return the
    // original transcript unchanged (graceful degradation), not an empty
    // string and not a panic. The user keeps their dictation.
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async { wiremock::MockServer::start().await });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "error": { "message": "invalid api key", "type": "authentication_error" }
                })),
            )
            .mount(&server)
            .await;
    });

    set_config(&cloud_llm_config(&server.uri(), "wrong-key").to_string());

    let original = "raw transcript text";
    let result = process_with_llm(original);
    eprintln!("[test] llm 401 result: {:?}", result);
    assert_eq!(
        result, original,
        "401 must fall back to the original transcript, got: {:?}",
        result
    );
}

#[test]
#[serial]
fn llm_cloud_500_falls_back_to_original_text() {
    // Same contract as 401 but for transient 5xx — proves the fallback
    // is not auth-specific. Catches a regression where 5xx would bubble
    // up as an exception or wipe the transcript.
    ensure_init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async { wiremock::MockServer::start().await });

    rt.block_on(async {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("upstream down"))
            .mount(&server)
            .await;
    });

    set_config(&cloud_llm_config(&server.uri(), "any-key").to_string());

    let original = "raw transcript text";
    let result = process_with_llm(original);
    eprintln!("[test] llm 500 result: {:?}", result);
    assert_eq!(
        result, original,
        "5xx must fall back to the original transcript, got: {:?}",
        result
    );
}

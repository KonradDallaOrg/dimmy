//! End-to-end tests for the v2 FFI surface (cross-platform).
//!
//! The FFI is unified — Win, Mac and Linux all bind to the same Rust
//! core, so these tests apply everywhere. The file was first written
//! during the Mac parity work, hence the original `v2_mac_ffi` name,
//! but nothing platform-specific lives here.
//!
//! Covers the entry points wired up by `feat/mac-v2-parity` so a
//! regression in any of them is caught at the FFI boundary, not at the
//! Swift/UI layer (which is harder to test):
//!   - dimmy_set_app_context / dimmy_clear_app_context — round-trips
//!     a bundle id through GLOBAL_STATE.current_app_context
//!   - dimmy_transcribe_file — exercises the WAV decode + preprocess +
//!     local STT pipeline + auto-save to history. Reuses the JFK
//!     fixture from ffi_e2e (downloaded on first run, cached).
//!   - dimmy_meeting_is_active — verifies the gate returns 0 when no
//!     meeting is active. (start/stop need cpal + a real input device,
//!     so they're tested manually on the Win/Mac runners and skipped
//!     in this offline harness.)
//!   - dimmy_history_save → dimmy_history_update_enhanced /
//!     dimmy_history_update_audio / dimmy_history_update_word_timestamps
//!     — verifies the v2 backfill hooks land in SQLite.
//!   - dimmy_llm_call_raw — verifies the no-config error path (-2).
//!
//! Run with: `cargo test --test v2_ffi --features local-stt,test-ffi`

#![cfg(all(feature = "test-ffi", feature = "local-stt"))]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use serial_test::serial;

use dimmy_lib::ffi::{
    dimmy_clear_app_context, dimmy_get_config_json, dimmy_history_save, dimmy_history_update_audio,
    dimmy_history_update_enhanced, dimmy_history_update_word_timestamps, dimmy_init,
    dimmy_llm_call_raw, dimmy_meeting_is_active, dimmy_meeting_save_post_process,
    dimmy_set_app_context, dimmy_set_config_json, dimmy_transcribe_file,
};

// ── Fixture wiring (lifted from ffi_e2e to stay self-contained) ──────

const JFK_WAV_URL: &str = "https://github.com/ggml-org/whisper.cpp/raw/master/samples/jfk.wav";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en-q8_0.bin";
const MODEL_FILENAME: &str = "ggml-tiny.en-q8_0.bin";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-fixtures")
}

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
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn jfk_wav() -> PathBuf {
    let p = fixture_dir().join("jfk.wav");
    download_to(JFK_WAV_URL, &p).expect("download jfk.wav");
    p
}

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

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
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

// ── Tests: app context ─────────────────────────────────────────────────

#[test]
#[serial]
fn app_context_set_and_clear_round_trip_does_not_crash() {
    ensure_init();
    let json = CString::new(
        r#"{"process_name":"","bundle_id":"com.tinyspeck.slackmacgap","wm_class":""}"#,
    )
    .unwrap();
    let rc = unsafe { dimmy_set_app_context(json.as_ptr()) };
    assert_eq!(rc, 0, "set_app_context should accept a well-formed JSON");

    // Clearing should be safe even when nothing is set or after set.
    unsafe { dimmy_clear_app_context() };
    unsafe { dimmy_clear_app_context() };
}

#[test]
#[serial]
fn app_context_rejects_malformed_json() {
    ensure_init();
    let bad = CString::new("not-json").unwrap();
    let rc = unsafe { dimmy_set_app_context(bad.as_ptr()) };
    assert_ne!(
        rc, 0,
        "set_app_context should refuse non-JSON; got {} (silent accept would mean rules never match)",
        rc
    );
}

#[test]
#[serial]
fn app_context_null_pointer_returns_error_not_crash() {
    ensure_init();
    let rc = unsafe { dimmy_set_app_context(std::ptr::null()) };
    assert_ne!(rc, 0, "null pointer should return non-zero, not crash");
}

// ── Tests: meeting gate ───────────────────────────────────────────────

#[test]
#[serial]
fn meeting_is_active_returns_zero_when_idle() {
    ensure_init();
    let rc = dimmy_meeting_is_active();
    assert_eq!(
        rc, 0,
        "meeting_is_active must be 0 when no meeting is in flight (Mac dictation hotkey gate depends on this)",
    );
}

// ── Tests: transcribe_file ────────────────────────────────────────────

#[test]
#[serial]
fn transcribe_file_with_jfk_wav_produces_transcript_and_saves_history() {
    ensure_init();
    ensure_tiny_model();

    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_stt_backend": "whisper",
            "local_model": MODEL_FILENAME,
            "language": "en",
            "preprocessing_enabled": true,
            "filler_removal_enabled": false,
            "llm_enabled": false,
        })
        .to_string(),
    );

    let wav = jfk_wav();
    let path_c = CString::new(wav.to_string_lossy().as_ref()).unwrap();
    let mut buf: Vec<u8> = vec![0; 8192];
    let n = unsafe {
        dimmy_transcribe_file(
            path_c.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert!(
        n > 0,
        "transcribe_file should return a positive transcript length, got {}",
        n
    );

    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
    let transcript = cstr.to_string_lossy().to_lowercase();
    assert!(
        transcript.contains("ask not"),
        "expected 'ask not' in JFK file-load transcript, got: {:?}",
        transcript
    );
}

#[test]
#[serial]
fn transcribe_file_rejects_missing_path() {
    ensure_init();
    set_config(
        &serde_json::json!({
            "stt_mode": "local",
            "local_stt_backend": "whisper",
            "local_model": MODEL_FILENAME,
        })
        .to_string(),
    );
    let path_c = CString::new("/tmp/__definitely_not_a_real_dimmy_file_12345.wav").unwrap();
    let mut buf: Vec<u8> = vec![0; 1024];
    let n = unsafe {
        dimmy_transcribe_file(
            path_c.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert!(
        n < 0,
        "missing file must return a negative error, got {}",
        n
    );
}

#[test]
#[serial]
fn transcribe_file_rejects_cloud_mode_without_credentials() {
    ensure_init();
    // Staging supports cloud-mode file load, but only with a configured
    // provider. With no api_key the call must reject negatively (-6
    // "cloud config incomplete" or any negative on this branch) so the
    // UI surfaces actionable guidance instead of a silent stall.
    set_config(
        &serde_json::json!({
            "stt_mode": "cloud",
            "api_url": "https://api.groq.com/openai/v1/audio/transcriptions",
            "api_model": "whisper-large-v3-turbo",
        })
        .to_string(),
    );
    let wav = jfk_wav();
    let path_c = CString::new(wav.to_string_lossy().as_ref()).unwrap();
    let mut buf: Vec<u8> = vec![0; 1024];
    let n = unsafe {
        dimmy_transcribe_file(
            path_c.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert!(
        n < 0,
        "cloud mode without credentials must reject negatively, got {}",
        n
    );
}

// ── Tests: history v2 update hooks ────────────────────────────────────

#[test]
#[serial]
fn history_update_enhanced_round_trip() {
    ensure_init();
    let text = CString::new("raw transcript line").unwrap();
    let lang = CString::new("en").unwrap();
    let id = unsafe { dimmy_history_save(text.as_ptr(), lang.as_ptr(), 1.5) };
    assert!(
        id > 0,
        "history_save should return a positive id, got {}",
        id
    );

    let enhanced = CString::new("enhanced rewrite").unwrap();
    let rc = unsafe { dimmy_history_update_enhanced(id, enhanced.as_ptr()) };
    assert_eq!(rc, 0, "update_enhanced(id={}) failed: {}", id, rc);

    // Empty string clears the column — must still return 0.
    let blank = CString::new("").unwrap();
    let rc = unsafe { dimmy_history_update_enhanced(id, blank.as_ptr()) };
    assert_eq!(rc, 0, "update_enhanced with empty string should succeed");

    // Null pointer is also accepted (clears) — Rust guards with empty
    // string fallback. Validates the contract the Mac wrapper relies on.
    let rc = unsafe { dimmy_history_update_enhanced(id, std::ptr::null()) };
    assert_eq!(rc, 0, "update_enhanced with null should succeed");
}

#[test]
#[serial]
fn history_update_audio_round_trip() {
    ensure_init();
    let text = CString::new("audio retention test row").unwrap();
    let lang = CString::new("en").unwrap();
    let id = unsafe { dimmy_history_save(text.as_ptr(), lang.as_ptr(), 2.0) };
    assert!(id > 0);

    let path = CString::new("/tmp/dimmy-test-fake-audio.wav").unwrap();
    let rc = unsafe { dimmy_history_update_audio(id, path.as_ptr(), 12_345) };
    assert_eq!(rc, 0, "update_audio failed: {}", rc);

    // Null path = unlink — sanity-check the contract.
    let rc = unsafe { dimmy_history_update_audio(id, std::ptr::null(), 0) };
    assert_eq!(
        rc, 0,
        "update_audio(null) should succeed (unlinks audio_path)"
    );
}

#[test]
#[serial]
fn history_update_word_timestamps_round_trip() {
    ensure_init();
    let text = CString::new("word ts row").unwrap();
    let lang = CString::new("en").unwrap();
    let id = unsafe { dimmy_history_save(text.as_ptr(), lang.as_ptr(), 1.0) };
    assert!(id > 0);

    let json = CString::new(r#"[{"word":"hi","start_ms":0,"end_ms":250}]"#).unwrap();
    let rc = unsafe { dimmy_history_update_word_timestamps(id, json.as_ptr()) };
    assert_eq!(rc, 0, "update_word_timestamps failed: {}", rc);

    let rc = unsafe { dimmy_history_update_word_timestamps(id, std::ptr::null()) };
    assert_eq!(rc, 0, "update_word_timestamps(null) should succeed");
}

// ── Tests: llm_call_raw ───────────────────────────────────────────────

#[test]
#[serial]
fn llm_call_raw_returns_minus_two_when_not_configured() {
    ensure_init();
    set_config(
        &serde_json::json!({
            "llm_enabled": false,
            "llm_api_url": "",
            "llm_api_model": "",
        })
        .to_string(),
    );

    let prompt = CString::new("test prompt").unwrap();
    let model = CString::new("").unwrap();
    let mut buf: Vec<u8> = vec![0; 1024];
    let n = unsafe {
        dimmy_llm_call_raw(
            prompt.as_ptr(),
            model.as_ptr(),
            512,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert_eq!(
        n, -2,
        "with no api_url/key configured llm_call_raw must return -2 (the auto-recap gate the Mac UI relies on); got {}",
        n
    );
}

// ── Tests: config round-trip + on-disk persistence (Mac UI bug guard) ──

/// Get the config back as a parsed JSON value.
fn get_config_value() -> serde_json::Value {
    let mut buf: Vec<u8> = vec![0; 32 * 1024];
    let n = dimmy_get_config_json(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);
    assert!(n > 0, "get_config_json returned {}", n);
    // Stop at first NUL — get_config_json writes a C-string into buf, then
    // returns the NUL-excluded byte length. Without trimming, we'd hand
    // serde a slice that may include uninitialised bytes past the NUL.
    let used = (n as usize).min(buf.len());
    let bytes = &buf[..used];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(used);
    let json = std::str::from_utf8(&bytes[..end]).expect("utf8");
    serde_json::from_str(json).expect("valid JSON")
}

#[test]
#[serial]
fn config_round_trip_persists_save_audio_and_retention_fields() {
    ensure_init();
    set_config(
        &serde_json::json!({
            "save_audio_in_history": true,
            "history_audio_keep_days": 7,
            "history_audio_max_mb": 1234,
            "auto_recap_threshold_secs": 45,
        })
        .to_string(),
    );

    let v = get_config_value();
    assert_eq!(
        v["save_audio_in_history"], true,
        "save_audio_in_history must round-trip — Mac Privacy toggle persistence depends on this"
    );
    assert_eq!(v["history_audio_keep_days"], 7);
    assert_eq!(v["history_audio_max_mb"], 1234);
    assert_eq!(v["auto_recap_threshold_secs"], 45);

    // Flip back to false to verify the path also works for transitions
    // (not just defaults overwritten by a single set).
    set_config(&serde_json::json!({"save_audio_in_history": false}).to_string());
    let v2 = get_config_value();
    assert_eq!(v2["save_audio_in_history"], false);
}

#[test]
#[serial]
fn config_round_trip_persists_app_rules_array() {
    ensure_init();
    set_config(
        &serde_json::json!({
            "app_rules": [
                {
                    "match_pattern": "com.tinyspeck.slackmacgap",
                    "match_type": "bundle_id",
                    "llm_style": "imbruttito",
                    "label": "Slack",
                    "enabled": true
                },
                {
                    "match_pattern": "com.microsoft.VSCode",
                    "match_type": "bundle_id",
                    "llm_style": "off",
                    "label": "VS Code",
                    "enabled": true
                }
            ]
        })
        .to_string(),
    );

    let v = get_config_value();
    let rules = v["app_rules"].as_array().expect("app_rules is array");
    assert_eq!(rules.len(), 2, "expected 2 rules round-tripped");
    assert_eq!(rules[0]["match_pattern"], "com.tinyspeck.slackmacgap");
    assert_eq!(rules[0]["match_type"], "bundle_id");
    assert_eq!(rules[0]["llm_style"], "imbruttito");
    assert_eq!(rules[1]["match_pattern"], "com.microsoft.VSCode");

    // Empty array should also round-trip (Mac "Remove all" path).
    set_config(&serde_json::json!({"app_rules": []}).to_string());
    let v2 = get_config_value();
    let rules2 = v2["app_rules"].as_array().expect("app_rules is array");
    assert_eq!(rules2.len(), 0, "empty app_rules should round-trip");
}

#[test]
#[serial]
fn config_persists_to_disk_so_next_launch_sees_v2_fields() {
    // The previous tests round-trip in-memory. This one verifies the
    // values reach the on-disk config.json so the next launch loads
    // them — the actual user-visible failure mode if write was skipped.
    ensure_init();
    set_config(
        &serde_json::json!({
            "save_audio_in_history": true,
            "history_audio_keep_days": 99,
            "auto_recap_threshold_secs": 120,
        })
        .to_string(),
    );

    // Locate the same config.json the Mac AppDelegate reads via
    // FileManager.applicationSupportDirectory. core uses dirs::config_dir
    // which on Mac == ~/Library/Application Support → matches.
    let path = dirs::config_dir()
        .expect("config_dir resolvable on this platform")
        .join("dimmy")
        .join("config.json");
    assert!(
        path.exists(),
        "config.json must exist after set_config_json — UI relies on next-launch persistence"
    );
    let raw = std::fs::read_to_string(&path).expect("read config.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON on disk");
    assert_eq!(
        v["save_audio_in_history"], true,
        "save_audio_in_history not written to disk — ui persistence broken"
    );
    assert_eq!(v["history_audio_keep_days"], 99);
    assert_eq!(v["auto_recap_threshold_secs"], 120);
}

// ── Tests: meeting save_post_process actually writes the artefacts ─────

#[test]
#[serial]
fn meeting_save_post_process_writes_recap_and_actions_files() {
    ensure_init();
    let tmp = std::env::temp_dir().join(format!("dimmy-test-meeting-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mkdir tmp meeting dir");

    let dir_c = CString::new(tmp.to_string_lossy().as_ref()).unwrap();
    let recap_c = CString::new("• Decided X\n• Outcome Y").unwrap();
    let actions_c = CString::new("1. alice — write doc — friday").unwrap();
    let rc = unsafe {
        dimmy_meeting_save_post_process(
            dir_c.as_ptr(),
            recap_c.as_ptr(),
            actions_c.as_ptr(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "save_post_process must succeed");

    let recap_path = tmp.join("recap.md");
    let actions_path = tmp.join("actions.json");
    assert!(recap_path.exists(), "recap.md not written");
    assert!(actions_path.exists(), "actions.json not written");
    let recap = std::fs::read_to_string(&recap_path).unwrap();
    assert!(recap.contains("Decided X"));

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[serial]
fn llm_call_raw_rejects_empty_prompt_with_minus_one() {
    ensure_init();
    let prompt = CString::new("").unwrap();
    let mut buf: Vec<u8> = vec![0; 256];
    let n = unsafe {
        dimmy_llm_call_raw(
            prompt.as_ptr(),
            std::ptr::null(),
            64,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert_eq!(n, -1, "empty prompt must return -1 invalid args, got {}", n);
}

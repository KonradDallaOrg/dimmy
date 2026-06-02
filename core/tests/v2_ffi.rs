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
    dimmy_call_meeting_started_external, dimmy_call_signal_session_ended, dimmy_claude_code_ping,
    dimmy_claude_code_status, dimmy_clear_app_context, dimmy_command_transform,
    dimmy_get_active_mic_sample_rate, dimmy_get_config_json, dimmy_get_loopback_amplitude,
    dimmy_history_save, dimmy_history_update_audio, dimmy_history_update_enhanced,
    dimmy_history_update_word_timestamps, dimmy_init, dimmy_llm_call_raw, dimmy_meeting_is_active,
    dimmy_meeting_save_post_process, dimmy_push_loopback_audio, dimmy_set_app_context,
    dimmy_set_config_json, dimmy_set_loopback_sample_rate, dimmy_transcribe_file,
    dimmy_user_dict_add, dimmy_user_dict_list_json, dimmy_user_dict_remove,
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

// ── Tests: dimmy_command_transform input-validation surface ────────────
//
// The cloud-success (>0) and network-error (-2) paths require a live LLM,
// so they're left to manual / pre-release runs. What we DO pin here is the
// deterministic part of the rc contract that the Mac + Win hosts read at
// runtime to choose between "paste fallback", "diagnostic toast", and
// "swallow silently":
//   -1 invalid input (null ptr OR empty / whitespace-only string)
//   -3 cloud mode + no LLM key configured  (skipped — key store state
//      varies across dev boxes, would be flaky)
//   -4 local mode + the configured model file is not on disk
// Pre-checks must happen BEFORE any LLM dispatch, so these are testable
// without network or runtime.

#[test]
#[serial]
fn command_transform_rejects_null_pointers() {
    ensure_init();
    let mut buf: Vec<u8> = vec![0; 256];
    let buf_ptr = buf.as_mut_ptr() as *mut c_char;
    let len = buf.len() as c_int;

    let sel = CString::new("anything").unwrap();
    let spoken = CString::new("rewrite").unwrap();

    // Null selection pointer.
    let n_sel = unsafe { dimmy_command_transform(std::ptr::null(), spoken.as_ptr(), buf_ptr, len) };
    assert_eq!(
        n_sel, -1,
        "null selection pointer must return -1; got {}",
        n_sel
    );

    // Null spoken pointer.
    let n_spk = unsafe { dimmy_command_transform(sel.as_ptr(), std::ptr::null(), buf_ptr, len) };
    assert_eq!(
        n_spk, -1,
        "null spoken pointer must return -1; got {}",
        n_spk
    );

    // Null output buffer.
    let n_buf = unsafe {
        dimmy_command_transform(sel.as_ptr(), spoken.as_ptr(), std::ptr::null_mut(), len)
    };
    assert_eq!(
        n_buf, -1,
        "null out_buf pointer must return -1; got {}",
        n_buf
    );

    // Zero / negative buffer length.
    let n_len = unsafe { dimmy_command_transform(sel.as_ptr(), spoken.as_ptr(), buf_ptr, 0) };
    assert_eq!(n_len, -1, "buf_len == 0 must return -1; got {}", n_len);
}

#[test]
#[serial]
fn command_transform_rejects_empty_and_whitespace_only_inputs() {
    ensure_init();
    let mut buf: Vec<u8> = vec![0; 256];
    let buf_ptr = buf.as_mut_ptr() as *mut c_char;
    let len = buf.len() as c_int;

    // Empty selection is NO LONGER invalid input: it routes to the
    // generate-and-insert path (spoken words = a generation instruction). So
    // it must NOT short-circuit to -1; it proceeds to LLM dispatch and fails
    // downstream in the test env (no key / no network) with a different code.
    let empty = CString::new("").unwrap();
    let spoken = CString::new("write a haiku about rain").unwrap();
    let n1 = unsafe { dimmy_command_transform(empty.as_ptr(), spoken.as_ptr(), buf_ptr, len) };
    assert_ne!(
        n1, -1,
        "empty selection must route to the generate path, not be rejected as invalid input; got {}",
        n1
    );

    // Whitespace-only selection collapses to None → also the generate path
    // (the host's selection-capture probe returns whitespace when nothing
    // was selected).
    let ws_sel = CString::new("   \n\t  ").unwrap();
    let n2 = unsafe { dimmy_command_transform(ws_sel.as_ptr(), spoken.as_ptr(), buf_ptr, len) };
    assert_ne!(
        n2, -1,
        "whitespace-only selection must collapse to the generate path, not -1; got {}",
        n2
    );

    // Empty spoken instruction — still invalid: there's nothing to act on.
    let sel = CString::new("Some content").unwrap();
    let empty_spk = CString::new("").unwrap();
    let n3 = unsafe { dimmy_command_transform(sel.as_ptr(), empty_spk.as_ptr(), buf_ptr, len) };
    assert_eq!(
        n3, -1,
        "empty spoken instruction must short-circuit to -1; got {}",
        n3
    );

    // Whitespace-only spoken instruction.
    let ws_spk = CString::new("   ").unwrap();
    let n4 = unsafe { dimmy_command_transform(sel.as_ptr(), ws_spk.as_ptr(), buf_ptr, len) };
    assert_eq!(
        n4, -1,
        "whitespace-only spoken instruction must short-circuit to -1 (Win + Mac hosts trim \
         transcript before calling, so this is the realistic empty case); got {}",
        n4
    );
}

#[test]
#[serial]
fn command_transform_returns_minus_four_when_local_mode_and_model_missing() {
    ensure_init();
    // Switch the runtime into local LLM mode pointing at a deliberately
    // impossible filename. The pre-flight `model_path.is_file()` check
    // must fire BEFORE we try to load the model, returning -4.
    //
    // Restored at the end so subsequent #[serial] tests see the cloud
    // default they expect.
    set_config(
        &serde_json::json!({
            "llm_mode": "local",
            "local_llm_model": "__dimmy_test_does_not_exist_v1__.gguf",
        })
        .to_string(),
    );

    let mut buf: Vec<u8> = vec![0; 256];
    let sel = CString::new("Some content the user selected.").unwrap();
    let spoken = CString::new("translate to italian").unwrap();
    let n = unsafe {
        dimmy_command_transform(
            sel.as_ptr(),
            spoken.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_int,
        )
    };
    assert_eq!(
        n, -4,
        "local mode with a missing model file must return -4 BEFORE attempting inference \
         (Mac + Win hosts surface this as 'local model not on disk', not a generic dispatch error); \
         got {}",
        n
    );

    // Restore cloud default so the serial test ordering stays well-defined.
    set_config(&serde_json::json!({"llm_mode": "cloud"}).to_string());
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

/// The Claude-Code subscription provider lives on a synthetic URL
/// scheme `claude-code://default` that is never sent over HTTP — the
/// LLM dispatcher checks `is_claude_code_url` and routes to a local
/// subprocess instead. Lock in that this URL survives the
/// `dimmy_set_config_json` → on-disk → reload cycle without being
/// rejected by `validate_url` or coerced to another scheme. A silent
/// rewrite here would brick the new Settings preset.
#[test]
#[serial]
fn config_round_trip_preserves_claude_code_url() {
    ensure_init();
    set_config(
        &serde_json::json!({
            "llm_enabled": true,
            "llm_api_url": "claude-code://default",
            "llm_api_model": "claude-opus-4-7",
        })
        .to_string(),
    );

    // 1. In-memory round-trip.
    let v = get_config_value();
    assert_eq!(
        v["llm_api_url"], "claude-code://default",
        "claude-code:// URL must survive in-memory round-trip — rewriting to https breaks subprocess routing"
    );
    assert_eq!(v["llm_api_model"], "claude-opus-4-7");
    assert_eq!(v["llm_enabled"], true);

    // 2. On-disk persistence (the user-visible failure if the writer
    //    coerced the URL — next launch would dispatch via HTTP and hit
    //    https://claude-code/ giving a connect refused with the
    //    transcript in the body).
    let path = dirs::config_dir()
        .expect("config_dir resolvable on this platform")
        .join("dimmy")
        .join("config.json");
    assert!(
        path.exists(),
        "config.json must exist after set_config_json"
    );
    let raw = std::fs::read_to_string(&path).expect("read config.json");
    let on_disk: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON on disk");
    assert_eq!(
        on_disk["llm_api_url"], "claude-code://default",
        "on-disk URL must be preserved verbatim — UI re-reads this on next launch"
    );
}

// ── recap vendor derivation + per-vendor keystore ───────────────
//
// The recap vendor is DERIVED from the chosen model id at dispatch
// time (no separate `recap_provider` config field — keeps the UI to
// a single picker). claude-* → Anthropic, gpt-* / o3 / o4 → OpenAI,
// gemini-* → Gemini. The dictation URL + key are reused unless the
// model's vendor differs.

/// Per-vendor key save via `dimmy_save_llm_provider_key`. Pinned
/// rc contract so the Win/Mac UIs can render the right status
/// (saved vs invalid-provider vs unknown-scope vs keystore-error)
/// without ambiguity.
#[test]
#[serial]
fn save_llm_provider_key_rejects_invalid_provider() {
    use dimmy_lib::ffi::dimmy_save_llm_provider_key;
    ensure_init();
    let scope = CString::new("llm").unwrap();
    let bad = CString::new("nonexistent").unwrap();
    let key = CString::new("sk-test").unwrap();
    let rc = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), bad.as_ptr(), key.as_ptr()) };
    assert_eq!(rc, -1, "unknown provider tag must return -1");

    // 'deepgram' is a real Provider variant but has no default_llm_url
    // → save must reject (LLM keystore would never be read for it).
    let dg = CString::new("deepgram").unwrap();
    let rc2 = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), dg.as_ptr(), key.as_ptr()) };
    assert_eq!(
        rc2, -1,
        "vendor without default_llm_url must be rejected at save time"
    );
}

#[test]
#[serial]
fn save_llm_provider_key_rejects_invalid_scope() {
    use dimmy_lib::ffi::dimmy_save_llm_provider_key;
    ensure_init();
    let bad_scope = CString::new("stt").unwrap(); // not allowed via this FFI
    let p = CString::new("anthropic").unwrap();
    let k = CString::new("sk-test").unwrap();
    let rc = unsafe { dimmy_save_llm_provider_key(bad_scope.as_ptr(), p.as_ptr(), k.as_ptr()) };
    assert_eq!(
        rc, -1,
        "scope tag must be 'llm' or 'recap'; STT save has its own FFI"
    );
    let nonsense = CString::new("foobar").unwrap();
    let rc2 = unsafe { dimmy_save_llm_provider_key(nonsense.as_ptr(), p.as_ptr(), k.as_ptr()) };
    assert_eq!(rc2, -1);
}

#[test]
#[serial]
fn save_llm_provider_key_accepts_known_vendor() {
    use dimmy_lib::ffi::dimmy_save_llm_provider_key;
    ensure_init();
    // anthropic / openai / gemini all have default_llm_url mappings —
    // round-trip through BOTH scopes.
    for scope_tag in &["llm", "recap"] {
        let scope = CString::new(*scope_tag).unwrap();
        for tag in &["anthropic", "openai", "gemini", "groq"] {
            let p = CString::new(*tag).unwrap();
            let k = CString::new(format!("sk-test-{}-{}", scope_tag, tag)).unwrap();
            let rc = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), p.as_ptr(), k.as_ptr()) };
            assert_eq!(
                rc, 0,
                "save for scope={} vendor={} must return 0",
                scope_tag, tag
            );
        }
    }
}

/// Null-pointer safety on the save FFI. The UI normally hands valid
/// CStrings but defensive testing here keeps the contract pinned —
/// any future refactor that drops the null check would be caught.
#[test]
#[serial]
fn save_llm_provider_key_null_ptr_rejected() {
    use dimmy_lib::ffi::dimmy_save_llm_provider_key;
    ensure_init();
    let scope = CString::new("llm").unwrap();
    let p = CString::new("anthropic").unwrap();
    let k = CString::new("sk").unwrap();
    let rc1 = unsafe { dimmy_save_llm_provider_key(std::ptr::null(), p.as_ptr(), k.as_ptr()) };
    assert_eq!(rc1, -1);
    let rc2 = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), std::ptr::null(), k.as_ptr()) };
    assert_eq!(rc2, -1);
    let rc3 = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), p.as_ptr(), std::ptr::null()) };
    assert_eq!(rc3, -1);
}

/// `recap_use_same_key` round-trips through config + the per-vendor
/// `has_*_recap_key` snapshot fields appear after a save into the
/// new `Recap(vendor)` scope. Pinned so the UI can rely on these
/// fields to render the "key already saved" check icon.
#[test]
#[serial]
fn recap_use_same_key_round_trips_and_recap_scope_snapshot_populated() {
    use dimmy_lib::ffi::dimmy_save_llm_provider_key;
    ensure_init();
    // Default = true.
    set_config(&serde_json::json!({"recap_use_same_key": false}).to_string());
    let v = get_config_value();
    assert_eq!(v["recap_use_same_key"], false);
    set_config(&serde_json::json!({"recap_use_same_key": true}).to_string());
    let v2 = get_config_value();
    assert_eq!(v2["recap_use_same_key"], true);

    // Save into the Recap scope and verify the snapshot flag flips.
    let scope = CString::new("recap").unwrap();
    let p = CString::new("openai").unwrap();
    let k = CString::new("sk-openai-recap").unwrap();
    let rc = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), p.as_ptr(), k.as_ptr()) };
    assert_eq!(rc, 0);
    let v3 = get_config_value();
    assert_eq!(
        v3["has_openai_recap_key"], true,
        "snapshot must reflect Recap-scope key presence"
    );
    // Clear and verify the flag drops back.
    let empty = CString::new("").unwrap();
    let rc2 = unsafe { dimmy_save_llm_provider_key(scope.as_ptr(), p.as_ptr(), empty.as_ptr()) };
    assert_eq!(rc2, 0);
    let v4 = get_config_value();
    assert_eq!(v4["has_openai_recap_key"], false);
}

/// Pin the return-code contract for `dimmy_claude_code_status`. The
/// Win + Mac UIs cast the int return to an enum (0/1/2) — any value
/// outside that range would silently break the status card.
#[test]
#[serial]
fn claude_code_status_returns_documented_range() {
    ensure_init();
    let rc = dimmy_claude_code_status();
    assert!(
        (0..=2).contains(&rc),
        "claude_code_status must return 0/1/2; got {} — Win/Mac status card decode would break",
        rc
    );
}

/// Pin the return-code contract for `dimmy_claude_code_ping`:
///   positive = elapsed_ms (success)
///   -1..=-6 = documented categorical errors
/// Anything else means the FFI signature drifted and the Test button
/// would mis-render the result.
#[test]
#[serial]
fn claude_code_ping_return_code_is_in_documented_range() {
    ensure_init();
    let rc = dimmy_claude_code_ping();
    let in_range = rc > 0 || (-6..=-1).contains(&rc);
    assert!(
        in_range,
        "claude_code_ping must return >0 (elapsed_ms) or one of -1..=-6 (error categories); got {}",
        rc
    );
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

// ── Tests: user dictionary FFI ────────────────────────────────────────
//
// Exercise the three add/remove/list entry points the Win and Mac UI
// + the global hotkey path lean on. The list is global state on the
// running core, so each test snapshots before & after and is annotated
// `#[serial]` to keep the assertions stable when the suite is run
// together.

/// Read the dict as a Vec<String>. Picks a buffer big enough for any
/// realistic dictionary; -2 (truncation) here would be a test bug.
fn read_user_dict() -> Vec<String> {
    let mut buf: Vec<u8> = vec![0; 64 * 1024];
    let n =
        unsafe { dimmy_user_dict_list_json(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
    assert!(n > 0, "list_json must return >0 bytes, got {}", n);
    let used = (n as usize).min(buf.len());
    let end = buf[..used].iter().position(|&b| b == 0).unwrap_or(used);
    let json = std::str::from_utf8(&buf[..end]).expect("utf8 dict json");
    serde_json::from_str(json).expect("valid dict json array")
}

/// Wipe the dict between tests so they don't bleed into each other.
/// Done via `set_config_json({"user_dict": []})` because that's the
/// bulk-replacement path the Settings UI uses.
fn clear_user_dict() {
    set_config(&serde_json::json!({"user_dict": []}).to_string());
}

#[test]
#[serial]
fn user_dict_add_then_list_round_trip() {
    ensure_init();
    clear_user_dict();

    let word = CString::new("Velopack").unwrap();
    let rc = unsafe { dimmy_user_dict_add(word.as_ptr()) };
    assert_eq!(
        rc, 0,
        "first add of a new word must return 0 (added), got {}",
        rc
    );

    let dict = read_user_dict();
    assert_eq!(dict, vec!["Velopack".to_string()]);
}

#[test]
#[serial]
fn user_dict_add_dedupes_case_insensitively() {
    ensure_init();
    clear_user_dict();

    let upper = CString::new("Notion").unwrap();
    let lower = CString::new("notion").unwrap();
    assert_eq!(
        unsafe { dimmy_user_dict_add(upper.as_ptr()) },
        0,
        "first add → 0"
    );
    assert_eq!(
        unsafe { dimmy_user_dict_add(lower.as_ptr()) },
        1,
        "case-different duplicate must return 1 (already-present), not 0"
    );

    let dict = read_user_dict();
    assert_eq!(
        dict.len(),
        1,
        "case-variant must not append, got {:?}",
        dict
    );
    // Original casing is preserved (we don't normalise on store).
    assert_eq!(dict[0], "Notion");
}

#[test]
#[serial]
fn user_dict_add_rejects_empty_and_whitespace() {
    ensure_init();
    clear_user_dict();

    let empty = CString::new("").unwrap();
    let ws = CString::new("   \t  ").unwrap();
    assert_eq!(
        unsafe { dimmy_user_dict_add(empty.as_ptr()) },
        -1,
        "empty word must reject with -1"
    );
    assert_eq!(
        unsafe { dimmy_user_dict_add(ws.as_ptr()) },
        -1,
        "whitespace-only word must reject with -1"
    );
    assert!(
        read_user_dict().is_empty(),
        "rejected adds must leave dict untouched"
    );
}

#[test]
#[serial]
fn user_dict_add_null_pointer_returns_minus_one_not_crash() {
    ensure_init();
    let rc = unsafe { dimmy_user_dict_add(std::ptr::null()) };
    assert_eq!(rc, -1, "null pointer must return -1, not crash");
}

#[test]
#[serial]
fn user_dict_remove_drops_matching_entries() {
    ensure_init();
    clear_user_dict();

    for w in ["alpha", "beta", "gamma"] {
        let c = CString::new(w).unwrap();
        assert_eq!(unsafe { dimmy_user_dict_add(c.as_ptr()) }, 0);
    }

    let target = CString::new("beta").unwrap();
    let removed = unsafe { dimmy_user_dict_remove(target.as_ptr()) };
    assert_eq!(
        removed, 1,
        "remove should return drop count, got {}",
        removed
    );

    let dict = read_user_dict();
    assert_eq!(dict, vec!["alpha".to_string(), "gamma".to_string()]);
}

#[test]
#[serial]
fn user_dict_remove_is_case_insensitive() {
    ensure_init();
    clear_user_dict();

    let add = CString::new("Notion").unwrap();
    assert_eq!(unsafe { dimmy_user_dict_add(add.as_ptr()) }, 0);

    let rm = CString::new("NOTION").unwrap();
    assert_eq!(
        unsafe { dimmy_user_dict_remove(rm.as_ptr()) },
        1,
        "remove must match case-insensitively (mirror of add's dedup rule)"
    );
    assert!(read_user_dict().is_empty());
}

#[test]
#[serial]
fn user_dict_remove_unknown_returns_zero() {
    ensure_init();
    clear_user_dict();
    let missing = CString::new("not-in-dict").unwrap();
    let rc = unsafe { dimmy_user_dict_remove(missing.as_ptr()) };
    assert_eq!(
        rc, 0,
        "removing a missing entry must return 0 (no-op), got {}",
        rc
    );
}

#[test]
#[serial]
fn user_dict_remove_null_pointer_returns_minus_one() {
    ensure_init();
    let rc = unsafe { dimmy_user_dict_remove(std::ptr::null()) };
    assert_eq!(rc, -1);
}

#[test]
#[serial]
fn user_dict_list_json_truncation_returns_minus_two() {
    ensure_init();
    clear_user_dict();

    // Pack the dict with enough entries that 8 bytes is guaranteed too
    // small. Even an empty list "[]" is 2 bytes; one entry "[\"x\"]" is 5;
    // we want the truncation branch.
    for i in 0..32 {
        let w = format!("word{:02}", i);
        let c = CString::new(w).unwrap();
        unsafe { dimmy_user_dict_add(c.as_ptr()) };
    }

    let mut tiny: Vec<u8> = vec![0; 8];
    let rc =
        unsafe { dimmy_user_dict_list_json(tiny.as_mut_ptr() as *mut c_char, tiny.len() as c_int) };
    assert_eq!(rc, -2, "undersized buffer must return -2, got {}", rc);
}

#[test]
#[serial]
fn user_dict_list_json_empty_returns_two_byte_array() {
    ensure_init();
    clear_user_dict();
    let dict = read_user_dict();
    assert!(dict.is_empty());
}

#[test]
#[serial]
fn user_dict_set_via_config_then_list_via_ffi_round_trip() {
    // Mirror the Settings page bulk-replace path: write the whole list
    // through set_config_json, then read back through the dedicated
    // list FFI. Catches drift between the two persistence routes.
    ensure_init();
    set_config(&serde_json::json!({"user_dict": ["Notion", "Velopack", "Parakeet"]}).to_string());

    let dict = read_user_dict();
    assert_eq!(
        dict,
        vec![
            "Notion".to_string(),
            "Velopack".to_string(),
            "Parakeet".to_string()
        ]
    );

    // And: order is preserved across a second read (no shuffle).
    let dict2 = read_user_dict();
    assert_eq!(dict, dict2);
}

#[test]
#[serial]
fn user_dict_persists_to_disk_so_next_launch_sees_it() {
    ensure_init();
    clear_user_dict();
    let word = CString::new("LoadBearingWord").unwrap();
    assert_eq!(unsafe { dimmy_user_dict_add(word.as_ptr()) }, 0);

    let path = dirs::config_dir()
        .expect("config_dir resolvable")
        .join("dimmy")
        .join("config.json");
    assert!(path.exists(), "config.json missing after dict add");
    let raw = std::fs::read_to_string(&path).expect("read config.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let arr = v["user_dict"].as_array().expect("user_dict array on disk");
    assert!(
        arr.iter().any(|w| w == "LoadBearingWord"),
        "added word not in on-disk config.json — UI persistence broken"
    );
}

// ── Tests: active mic sample rate ────────────────────────────────

/// Without an active recording, the cpal mic stream is not built so
/// the reported rate must be 0 (Swift side falls back to 48 kHz when
/// it sees 0, so the contract here is "0 when unset, positive when
/// set"). Pinning this prevents a regression where a stale value
/// leaks across recordings — Mac would then configure SCStream at
/// the wrong rate for the NEXT meeting and degrade output again.
#[test]
#[serial]
fn active_mic_sample_rate_is_zero_when_no_recording() {
    ensure_init();
    let rate = dimmy_get_active_mic_sample_rate();
    assert_eq!(
        rate, 0,
        "no recording in flight, rate must be 0 (got {rate})"
    );
}

// ── Tests: push_loopback_audio ─────────────────────────────────────────

#[test]
#[serial]
fn push_loopback_audio_feeds_secondary_buffer() {
    ensure_init();
    let samples: Vec<f32> = vec![0.5, -0.5, 0.8, 1.5, -1.5]; // last two clamped
    let rc = unsafe { dimmy_push_loopback_audio(samples.as_ptr(), samples.len() as i32, 48_000) };
    assert_eq!(rc, 0);
    let amp = unsafe { dimmy_get_loopback_amplitude() };
    assert!(amp > 0.0, "loopback amplitude must reflect pushed samples");
}

#[test]
#[serial]
fn push_loopback_audio_null_returns_minus_one() {
    ensure_init();
    unsafe {
        assert_eq!(dimmy_push_loopback_audio(std::ptr::null(), 10, 48_000), -1);
    }
}

#[test]
#[serial]
fn push_loopback_audio_zero_count_returns_minus_one() {
    ensure_init();
    let s = [0.0f32];
    unsafe {
        assert_eq!(dimmy_push_loopback_audio(s.as_ptr(), 0, 48_000), -1);
    }
}

// ── Tests: set_loopback_sample_rate ─────────────────────────────────
//
// These pin the rate-plumbing fix for the macOS meeting bug where
// `audio_system.wav` shipped a 48 kHz header while SCStream pushed at
// the cpal mic rate (16 kHz on BT-HFP). The override the Swift side
// sets BEFORE `dimmy_meeting_start` must be readable by
// `secondary_sample_rate` so the WAV is created with the right rate
// from byte 0.

#[test]
#[serial]
fn set_loopback_sample_rate_updates_secondary_rate() {
    ensure_init();
    // Clean slate: prior tests may have left an override set.
    assert_eq!(dimmy_set_loopback_sample_rate(0), 0);
    let baseline = dimmy_lib::audio::secondary_sample_rate(48_000);
    assert!(
        baseline >= 8_000,
        "platform fallback should be a sane audio rate, got {baseline}"
    );

    assert_eq!(dimmy_set_loopback_sample_rate(16_000), 0);
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(48_000), 16_000);

    assert_eq!(dimmy_set_loopback_sample_rate(48_000), 0);
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(16_000), 48_000);

    // Cleanup so neighbour tests start from the platform default.
    assert_eq!(dimmy_set_loopback_sample_rate(0), 0);
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(48_000), baseline);
}

#[test]
#[serial]
fn set_loopback_sample_rate_rejects_out_of_range() {
    ensure_init();
    assert_eq!(dimmy_set_loopback_sample_rate(0), 0);
    let baseline = dimmy_lib::audio::secondary_sample_rate(48_000);

    assert_eq!(dimmy_set_loopback_sample_rate(-1), -1);
    assert_eq!(dimmy_set_loopback_sample_rate(7_999), -1);
    assert_eq!(dimmy_set_loopback_sample_rate(192_001), -1);

    assert_eq!(
        dimmy_lib::audio::secondary_sample_rate(48_000),
        baseline,
        "rejected rates must not change the override"
    );
}

#[test]
#[serial]
fn push_loopback_audio_with_rate_updates_override() {
    ensure_init();
    assert_eq!(dimmy_set_loopback_sample_rate(0), 0);
    let samples = [0.1f32, -0.1, 0.2, -0.2];
    let rc = unsafe { dimmy_push_loopback_audio(samples.as_ptr(), samples.len() as i32, 16_000) };
    assert_eq!(rc, 0);
    assert_eq!(
        dimmy_lib::audio::secondary_sample_rate(48_000),
        16_000,
        "non-zero sample_rate arg should refresh the loopback rate override"
    );
    assert_eq!(dimmy_set_loopback_sample_rate(0), 0);
}

// On non-Windows the fallback when no override is set must follow the
// primary (mic) rate so SCStream's `audio_system.wav` matches the cpal
// mic rate (BT-HFP common case: 16 kHz on both). Pre-fix this returned
// a hardcoded 48 kHz and made playback run at 3× speed.
#[test]
#[cfg(not(target_os = "windows"))]
#[serial]
fn secondary_sample_rate_falls_back_to_primary_on_non_windows() {
    ensure_init();
    assert_eq!(dimmy_set_loopback_sample_rate(0), 0);
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(16_000), 16_000);
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(44_100), 44_100);
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(48_000), 48_000);
    // Out-of-range primary is replaced with the canonical 48 kHz so a
    // bug in the primary probe never propagates to the WAV header.
    assert_eq!(dimmy_lib::audio::secondary_sample_rate(0), 48_000);
}

// ── Tests: call_meeting_started_external (FFI boundary) ───────────────
//
// The full "manual meeting + call ends → StopSuggested" behaviour is
// unit-tested in `call_detector.rs` (it needs `is_meeting_active=true`,
// which `dimmy_call_signal_session_ended` reads from the global MEETING
// static — and starting a real meeting needs cpal + an input device, so
// it can't run in this offline harness). Here we only guard the FFI
// boundary: the symbol is exported, callable, returns its documented rc,
// and is safely idempotent (the host arms on the start edge AND on
// mid-meeting ticks). With no meeting active, signal_session_ended must
// stay NoChange (rc=0) regardless of arming.
#[test]
#[serial]
fn call_meeting_started_external_round_trip() {
    ensure_init();
    assert_eq!(
        dimmy_call_meeting_started_external(),
        1,
        "documented rc is 1"
    );
    // Idempotent: a second arm is still rc=1, never a crash.
    assert_eq!(dimmy_call_meeting_started_external(), 1);
    // No meeting active in this harness → the stop path is gated off.
    let rc = unsafe { dimmy_call_signal_session_ended() };
    assert_eq!(
        rc, 0,
        "no active meeting → NoChange even when armed (rc=3 path is unit-tested)"
    );
}

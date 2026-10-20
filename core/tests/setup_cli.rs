//! `dimmy-setup` end to end, in process, against a throwaway config dir.
//!
//! One test function on purpose: the CLI builds the core's process-wide
//! headless state once, so the steps share it the way one agent session
//! would. Nothing here touches the network or the real user config.
//!
//! Run with: `cargo test --test setup_cli --features local-stt,test-ffi`

#![cfg(feature = "test-ffi")]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;

use dimmy_lib::ffi::dimmy_setup_cli;
use dimmy_lib::setup;
use dimmy_lib::setup_cli::run;
use serde_json::Value;

fn cli(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out: Vec<u8> = Vec::new();
    let code = run(&args, &mut out);
    let text = String::from_utf8(out).expect("utf-8 output");
    let last = text.lines().last().expect("one JSON line");
    (code, serde_json::from_str(last).expect("JSON output"))
}

fn ffi(args: &[&str]) -> c_int {
    let owned: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
    let argv: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
    unsafe { dimmy_setup_cli(argv.len() as c_int, argv.as_ptr()) }
}

fn saved_config(dir: &std::path::Path) -> Value {
    let text = std::fs::read_to_string(dir.join("config.json")).expect("config.json written");
    serde_json::from_str(&text).unwrap()
}

#[test]
fn an_agent_session_against_a_throwaway_dir() {
    let dir: PathBuf = std::env::temp_dir().join(format!("dimmy-setup-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DIMMY_TEST_CONFIG_DIR", &dir);

    let (code, v) = cli(&["info"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["config_dir"], dir.display().to_string());
    assert_eq!(v["app"]["running"], false);
    assert_eq!(v["config_exists"], false);

    let (code, v) = cli(&["config", "get"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["settings"]["stt_mode"].is_string());
    assert!(v["settings"].get("api_key").is_none());

    // Strict: a typo is an error listing what is allowed, and nothing is saved.
    let (code, v) = cli(&["config", "set", "stt_mode=locall"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error"]["code"], "invalid_settings");
    assert_eq!(v["error"]["details"][0]["allowed"][0], "local");
    assert!(!dir.join("config.json").exists());

    let (code, v) = cli(&[
        "config",
        "set",
        "llm_style=correct",
        "llm_enabled=true",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["changes"].as_array().unwrap().len(), 2);
    assert!(!dir.join("config.json").exists(), "dry run writes nothing");

    let (code, v) = cli(&["config", "set", "llm_style=correct", "llm_enabled=true"]);
    assert_eq!(code, 0, "{v}");
    let saved = saved_config(&dir);
    assert_eq!(saved["llm_style"], "correct");
    assert_eq!(saved["llm_enabled"], true);

    // Settings that are not switches: a list, a number, text.
    let (code, v) = cli(&[
        "config",
        "set",
        r#"user_dict=["Dimmy","Temera"]"#,
        "input_gain=0.7",
        "prompt=Konrad",
    ]);
    assert_eq!(code, 0, "{v}");
    let saved = saved_config(&dir);
    assert_eq!(saved["user_dict"], serde_json::json!(["Dimmy", "Temera"]));
    assert_eq!(saved["prompt"], "Konrad");

    // A setting the Windows app stores itself lands in ui_prefs.json, under
    // the app's own property name, and comes back from `config get`.
    #[cfg(target_os = "windows")]
    {
        let (code, v) = cli(&["config", "set", "meeting_hotkey=", "theme=Dark"]);
        assert_eq!(code, 0, "{v}");
        let prefs: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("ui_prefs.json")).expect("ui_prefs.json"),
        )
        .unwrap();
        assert_eq!(prefs["MeetingHotkey"], "");
        assert_eq!(prefs["Theme"], "Dark");
        let (_, v) = cli(&["config", "get"]);
        assert_eq!(v["settings"]["meeting_hotkey"], "");
        assert_eq!(v["settings"]["dict_add_hotkey"], "ctrl+shift+d");
        let (code, v) = cli(&["config", "set", "meeting_hotkey=ctrl+alt+nope"]);
        assert_eq!(code, 2, "{v}");
    }

    // The running app holds the lock: writes refuse with 3, reads still work.
    {
        let held = setup::try_lock_dir(&dir).unwrap().expect("lock");
        let (code, v) = cli(&["config", "set", "llm_enabled=false"]);
        assert_eq!(code, 3, "{v}");
        assert_eq!(v["error"]["code"], "app_running");
        assert_eq!(v["error"]["fix"], "dimmy-setup app quit");
        let (code, _) = cli(&["onboarding", "done"]);
        assert_eq!(code, 3);
        let (code, v) = cli(&["app", "status"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["app"]["running"], true);
        assert_eq!(cli(&["config", "get"]).0, 0);
        drop(held);
    }
    assert_eq!(saved_config(&dir)["llm_enabled"], true);

    let (code, v) = cli(&[
        "profile",
        "apply",
        "--use",
        "meeting",
        "--lang",
        "it",
        "--stt",
        "cloud",
        "--recap",
        "claude-sub",
    ]);
    assert_eq!(code, 0, "{v}");
    let saved = saved_config(&dir);
    assert_eq!(saved["recap_auth_method"], "subscription");
    assert_eq!(saved["language"], "it");
    assert_eq!(saved["call_detect_enabled"], true);
    assert_eq!(saved["meeting_generate_recap"], true);

    let (code, v) = cli(&["onboarding", "status"]);
    assert_eq!(code, 0);
    assert_eq!(v["done"], false);
    assert_eq!(cli(&["onboarding", "done"]).0, 0);
    assert_eq!(
        std::fs::read_to_string(dir.join(".onboarding_done")).unwrap(),
        "1"
    );

    let (code, v) = cli(&["key", "status"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["keys"]["groq"]["any"], false);

    let (code, v) = cli(&["key", "test", "groq"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "no_key");
    let (code, v) = cli(&["key", "test", "deepgram"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "not_supported");

    let (code, v) = cli(&["diag", "--lines", "5"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["log_tail"].as_array().unwrap().len() <= 5);
    assert_eq!(v["settings"]["language"], "it");

    let (code, v) = cli(&["models", "download", "not-a-model.bin"]);
    assert_eq!(code, 1);
    assert_eq!(v["error"]["code"], "not_found");
    let (code, v) = cli(&["models", "prepare", "not-a-model.bin"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "not_found");

    let (code, v) = cli(&["check"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["warnings"].is_array());
    assert_eq!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "no_microphone"),
        v["microphones"].as_array().unwrap().is_empty()
    );

    // `last` reports what is on disk, nothing more.
    let (code, v) = cli(&["last"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["dictation"].is_null());
    assert!(v["meeting"].is_null());
    {
        let h = dimmy_lib::history::HistoryStore::new(&dir.join("history.db")).unwrap();
        h.save("prova di dettatura", "it", 1.5).unwrap();
    }
    let m = dir.join("meetings").join("m-short");
    std::fs::create_dir_all(&m).unwrap();
    std::fs::write(
        m.join("meta.json"),
        r#"{"started_at": 1000.0, "ended_at": 1002.8, "duration_secs": 2.8}"#,
    )
    .unwrap();
    std::fs::write(m.join("transcripts.txt"), "").unwrap();
    std::fs::write(m.join("audio_mic.ogg"), [0u8; 512]).unwrap();
    let (code, v) = cli(&["last"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dictation"]["words"], 3);
    assert_eq!(v["dictation"]["ok"], true);
    assert!(!v.to_string().contains("dettatura"), "no transcript text");
    assert_eq!(v["meeting"]["id"], "m-short");
    assert_eq!(v["meeting"]["duration_secs"], 2.8);
    assert_eq!(v["meeting"]["transcript_nonempty"], false);
    assert_eq!(v["meeting"]["recap_present"], false);
    assert_eq!(v["meeting"]["audio_tracks_bytes"]["audio_mic.ogg"], 512);

    let (code, v) = cli(&["perm", "status"]);
    assert_eq!(code, 0, "{v}");
    for p in setup::PERMISSIONS {
        assert!(v["permissions"][p].is_string(), "{p}: {v}");
    }
    assert!(v["required_granted"].is_boolean());
    let (code, _) = cli(&["perm", "open", "camera"]);
    assert_eq!(code, 2);
    let (code, _) = cli(&["perm"]);
    assert_eq!(code, 2);

    // A second --config-dir in the same process cannot move the state.
    let (code, v) = cli(&["--config-dir", "E:/elsewhere", "info"]);
    assert_eq!(code, 2, "{v}");

    // The C ABI entry: same contract, exit codes straight through.
    assert_eq!(ffi(&["schema"]), 0);
    assert_eq!(ffi(&["bogus"]), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

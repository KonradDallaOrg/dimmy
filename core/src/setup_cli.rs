//! `dimmy-setup`: argument parsing and command dispatch.
//!
//! Every command prints one JSON object on stdout (long operations print one
//! progress object per line first). Exit codes: 0 ok, 1 failed, 2 bad usage
//! or invalid settings, 3 Dimmy is running and the command would write.
//! Contract and rationale: `docs/dev/agent-setup.md`. The pure logic is in
//! `setup.rs`; this file does I/O.

use std::ffi::{c_char, c_int, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::provider::{KeyringScope, Provider};
use crate::setup;

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_APP_RUNNING: i32 = 3;

const PING_PROMPT: &str = "reply with the single word: pong";
const RECAP_TEST_PROMPT: &str = "Summarise this meeting transcript in one sentence.\n\n\
[00:00:01] [mic] Let's ship the new release on Friday.\n\
[00:00:05] [system] Agreed, and Anna writes the release notes by Thursday.";
const OUT_BUF: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct CliError {
    pub exit: i32,
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    pub details: Option<Value>,
}

impl CliError {
    fn new(
        exit: i32,
        code: &'static str,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        assert!(exit != EXIT_OK, "an error never exits 0");
        Self {
            exit,
            code,
            message: crate::truncate_utf8(&message.into(), 200).to_string(),
            fix: fix.into(),
            details: None,
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self::new(EXIT_USAGE, "usage", message, "Run `dimmy-setup help`.")
    }

    fn failed(code: &'static str, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::new(EXIT_FAILED, code, message, fix)
    }

    fn with_details(mut self, d: Value) -> Self {
        self.details = Some(d);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut e = json!({ "code": self.code, "message": self.message, "fix": self.fix });
        if let Some(d) = &self.details {
            e["details"] = d.clone();
        }
        json!({ "ok": false, "error": e })
    }
}

type CmdResult = Result<Value, CliError>;

fn print(out: &mut dyn Write, v: &Value) {
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// Run one `dimmy-setup` invocation. `args` excludes the program name.
pub fn run(args: &[String], out: &mut dyn Write) -> i32 {
    let mut args: Vec<String> = args.to_vec();
    let result = take_config_dir(&mut args).and_then(|()| dispatch(&mut args, out));
    let (code, v) = match result {
        Ok(mut v) => {
            v["ok"] = json!(true);
            (EXIT_OK, v)
        }
        Err(e) => (e.exit, e.to_json()),
    };
    print(out, &v);
    code
}

// ── Argument helpers ────────────────────────────────────────────────

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, CliError> {
    let Some(i) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if i + 1 >= args.len() || args[i + 1].starts_with("--") {
        return Err(CliError::usage(format!("{flag} needs a value")));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Ok(Some(v))
}

fn take_switch(args: &mut Vec<String>, flag: &str) -> bool {
    match args.iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn no_more(args: &[String]) -> Result<(), CliError> {
    match args.first() {
        None => Ok(()),
        Some(a) => Err(CliError::usage(format!("unexpected argument '{a}'"))),
    }
}

fn take_config_dir(args: &mut Vec<String>) -> Result<(), CliError> {
    let Some(raw) = take_value(args, "--config-dir")? else {
        return Ok(());
    };
    let dir =
        std::path::absolute(&raw).map_err(|e| CliError::usage(format!("--config-dir: {e}")))?;
    if let Some(cur) = crate::config_dir_override() {
        if cur != dir {
            return Err(CliError::usage(
                "--config-dir was already set to a different directory in this process",
            ));
        }
        return Ok(());
    }
    if crate::ffi::is_initialized() {
        return Err(CliError::usage(
            "--config-dir must come before Dimmy is initialised in this process",
        ));
    }
    crate::set_config_dir_override(dir);
    Ok(())
}

fn config_dir() -> Result<PathBuf, CliError> {
    crate::config_dir_path().ok_or_else(|| {
        CliError::failed(
            "no_config_dir",
            "could not determine the settings folder",
            "Pass --config-dir <path>.",
        )
    })
}

/// Held for the rest of a writing command. Exit 3 while the app runs.
fn write_lock() -> Result<std::fs::File, CliError> {
    let dir = config_dir()?;
    match setup::try_lock_dir(&dir) {
        Ok(Some(f)) => Ok(f),
        Ok(None) => Err(CliError::new(
            EXIT_APP_RUNNING,
            "app_running",
            "Dimmy is running, and only one program may write its settings at a time",
            "dimmy-setup app quit",
        )),
        Err(e) => Err(CliError::failed(
            "lock_failed",
            e,
            "Check the settings folder is writable.",
        )),
    }
}

fn state() {
    crate::ffi::init_headless();
}

fn read_buf(f: impl FnOnce(*mut c_char, c_int) -> c_int) -> Result<String, i32> {
    let mut buf = vec![0u8; OUT_BUF];
    let rc = f(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int);
    if rc < 0 {
        return Err(rc);
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

fn current_config() -> Result<Value, CliError> {
    state();
    let s = read_buf(|p, n| crate::ffi::dimmy_get_config_json(p, n))
        .map_err(|rc| CliError::failed("failed", format!("reading settings failed ({rc})"), ""))?;
    serde_json::from_str(&s).map_err(|e| CliError::failed("failed", e.to_string(), ""))
}

fn schema_view(cfg: &Value) -> Value {
    let mut m = Map::new();
    for s in setup::settings() {
        m.insert(s.key.to_string(), cfg[s.key].clone());
    }
    Value::Object(m)
}

// ── Dispatch ────────────────────────────────────────────────────────

fn dispatch(args: &mut Vec<String>, out: &mut dyn Write) -> CmdResult {
    if args.is_empty() {
        return Err(CliError::usage("no command given"));
    }
    let cmd = args.remove(0);
    let sub = |args: &mut Vec<String>| -> Option<String> {
        (!args.is_empty() && !args[0].starts_with("--")).then(|| args.remove(0))
    };
    match cmd.as_str() {
        "help" | "--help" | "-h" => Ok(help()),
        "info" => {
            no_more(args)?;
            info()
        }
        "schema" => {
            no_more(args)?;
            Ok(json!({ "schema": setup::schema() }))
        }
        "check" => {
            no_more(args)?;
            check()
        }
        "config" => match sub(args).as_deref() {
            Some("get") => {
                no_more(args)?;
                Ok(json!({ "settings": schema_view(&current_config()?) }))
            }
            Some("set") => config_set(args),
            _ => Err(CliError::usage("config get | config set")),
        },
        "profile" => match sub(args).as_deref() {
            Some("apply") => profile_apply(args),
            _ => Err(CliError::usage(
                "profile apply --use ... --lang ... --stt ... --recap ...",
            )),
        },
        "models" => match sub(args).as_deref() {
            Some("list") => {
                no_more(args)?;
                models_list()
            }
            Some("download") => {
                let id = sub(args).ok_or_else(|| CliError::usage("models download <id>"))?;
                no_more(args)?;
                models_download(&id, out)
            }
            _ => Err(CliError::usage("models list | models download <id>")),
        },
        "login" => {
            let action = sub(args);
            let cli = sub(args);
            no_more(args)?;
            match (action.as_deref(), cli.as_deref()) {
                (Some(a @ ("status" | "start" | "test")), Some(c)) => login(a, c),
                _ => Err(CliError::usage(
                    "login status|start|test claude|codex|gemini",
                )),
            }
        }
        "key" => match sub(args).as_deref() {
            Some("status") => {
                no_more(args)?;
                key_status()
            }
            Some("test") => {
                let p = sub(args).ok_or_else(|| CliError::usage("key test <provider>"))?;
                no_more(args)?;
                key_test(&p)
            }
            _ => Err(CliError::usage("key status | key test <provider>")),
        },
        "test" => match sub(args).as_deref() {
            Some("stt") => {
                let file = take_value(args, "--file")?
                    .ok_or_else(|| CliError::usage("test stt --file <audio file>"))?;
                no_more(args)?;
                test_stt(Path::new(&file))
            }
            Some("recap") => {
                no_more(args)?;
                test_recap()
            }
            _ => Err(CliError::usage("test stt --file <wav> | test recap")),
        },
        "onboarding" => match sub(args).as_deref() {
            Some("status") => {
                no_more(args)?;
                onboarding_status()
            }
            Some("done") => {
                no_more(args)?;
                onboarding_done()
            }
            _ => Err(CliError::usage("onboarding status | onboarding done")),
        },
        "diag" => {
            let lines = match take_value(args, "--lines")? {
                Some(n) => n
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=5000).contains(n))
                    .ok_or_else(|| CliError::usage("--lines must be 1..5000"))?,
                None => 200,
            };
            no_more(args)?;
            diag(lines)
        }
        "app" => {
            let action = sub(args);
            no_more(args)?;
            match action.as_deref() {
                Some("status") => app_status(),
                Some("start") => app_start(),
                Some("quit") => app_quit(),
                _ => Err(CliError::usage("app status | app start | app quit")),
            }
        }
        other => Err(CliError::usage(format!("unknown command '{other}'"))),
    }
}

fn help() -> Value {
    json!({
        "usage": "dimmy-setup [--config-dir <path>] <command>",
        "commands": [
            "info", "schema", "check", "config get",
            "config set key=value ... [--json '{...}'] [--dry-run]",
            "profile apply --use meeting|dictation|both --lang <code|auto> --stt local|cloud [--stt-provider <id>] [--stt-backend <engine>] --recap claude-sub|codex-sub|api|local|off [--recap-model <id>] [--dry-run]",
            "models list", "models download <id>",
            "login status|start|test claude|codex|gemini",
            "key status", "key test <provider>",
            "test stt --file <wav>", "test recap",
            "onboarding status|done", "diag [--lines N]", "app status|start|quit",
        ],
        "exit_codes": { "0": "ok", "1": "failed", "2": "bad usage or invalid settings", "3": "Dimmy is running; quit it to change settings" },
    })
}

// ── info / check ────────────────────────────────────────────────────

fn app_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    if cfg!(target_os = "windows") {
        Some(me.parent()?.join("Dimmy.Windows.exe"))
    } else if cfg!(target_os = "macos") {
        me.ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf)
    } else {
        None
    }
}

fn app_running() -> Result<bool, CliError> {
    setup::app_running(&config_dir()?)
        .map_err(|e| CliError::failed("lock_failed", e, "Check the settings folder is readable."))
}

fn app_json() -> Result<Value, CliError> {
    let exe = app_exe();
    Ok(json!({
        "installed": exe.as_ref().is_some_and(|p| p.exists()),
        "path": exe.map(|p| p.display().to_string()),
        "running": app_running()?,
    }))
}

fn info() -> CmdResult {
    let dir = config_dir()?;
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "flavor": if crate::build_flavor().is_empty() { "prod" } else { crate::build_flavor() },
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "config_dir": dir.display().to_string(),
        "config_exists": dir.join("config.json").exists(),
        "app": app_json()?,
    }))
}

fn mb(bytes: Option<u64>) -> Option<u64> {
    bytes.map(|b| b / (1024 * 1024))
}

fn cli_states() -> (Value, bool, bool) {
    let claude = crate::claude_code::status();
    let codex = crate::codex::status();
    let gemini = crate::gemini_cli::status();
    let c = crate::claude_code::status_label(&claude);
    let x = crate::codex::status_label(&codex);
    let g = crate::gemini_cli::status_label(&gemini);
    (
        json!({ "claude": c, "codex": x, "gemini": g }),
        c == "ready",
        x == "ready",
    )
}

fn check() -> CmdResult {
    let dir = config_dir()?;
    state();
    let gpu = crate::hardware::detect();
    let fitness = crate::hardware::assess(&gpu);
    let models_dir = crate::local_stt::model_directory();
    let ram = setup::total_ram_bytes();
    let models_free = setup::free_disk_bytes(&models_dir);
    let (clis, claude_ready, codex_ready) = cli_states();
    let rec = setup::recommended_profile(&setup::MachineFacts {
        fitness,
        apple_silicon: gpu.apple_silicon,
        ram_mb: mb(ram),
        models_disk_free_mb: mb(models_free),
        claude_ready,
        codex_ready,
        local_backend: setup::default_local_backend(),
    });
    let license: Value = read_buf(|p, n| crate::ffi::dimmy_license_status_json(p, n))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(json!({
        "os": { "name": std::env::consts::OS, "version": setup::os_version() },
        "arch": std::env::consts::ARCH,
        "ram_mb": mb(ram),
        "disk_free_mb": { "settings": mb(setup::free_disk_bytes(&dir)), "models": mb(models_free) },
        "gpu": {
            "name": gpu.name,
            "vram_mb": gpu.vram_mb,
            "dedicated": gpu.dedicated,
            "apple_silicon": gpu.apple_silicon,
            "fitness": format!("{fitness:?}").to_ascii_lowercase(),
            "known_bad": gpu_known_bad(),
        },
        "microphones": crate::audio::list_input_devices(),
        "local_stt_backends": setup::available_backends(),
        "subscription_clis": clis,
        "keys": key_flags(),
        "license": license_kind(&license),
        "onboarding_done": onboarding_marker_exists(),
        "recommended_profile": rec.to_json(),
    }))
}

#[cfg(any(feature = "local-stt", feature = "local-llm"))]
fn gpu_known_bad() -> bool {
    crate::gpu_health::read_known_bad().is_some()
}

#[cfg(not(any(feature = "local-stt", feature = "local-llm")))]
fn gpu_known_bad() -> bool {
    false
}

fn license_kind(v: &Value) -> Value {
    json!({ "kind": v["kind"], "tier": v["tier"] })
}

// ── config ──────────────────────────────────────────────────────────

/// `key=value` pairs. A value that parses as JSON keeps its type
/// (`true`, `60`, `"x"`); anything else is a plain string.
fn parse_pairs(args: &[String]) -> Result<Map<String, Value>, CliError> {
    let mut m = Map::new();
    for a in args {
        let (k, v) = a
            .split_once('=')
            .ok_or_else(|| CliError::usage(format!("'{a}' is not key=value")))?;
        if k.is_empty() {
            return Err(CliError::usage(format!("'{a}' has no key")));
        }
        let parsed = match serde_json::from_str::<Value>(v) {
            Ok(j @ (Value::Bool(_) | Value::Number(_) | Value::String(_))) => j,
            _ => Value::String(v.to_string()),
        };
        m.insert(k.to_string(), parsed);
    }
    Ok(m)
}

const PAIRED: [(&str, &str); 4] = [
    ("api_url", "api_model"),
    ("llm_api_url", "llm_api_model"),
    ("call_detect_enabled", "call_detect_auto_record"),
    ("recap_auth_method", "recap_model_override"),
];

fn invalid(errs: Vec<setup::FieldError>) -> CliError {
    CliError::new(
        EXIT_USAGE,
        "invalid_settings",
        format!("{} setting(s) rejected; nothing was changed", errs.len()),
        "Use the allowed values listed in details, or see `dimmy-setup schema`.",
    )
    .with_details(Value::Array(errs.iter().map(|e| e.to_json()).collect()))
}

fn validate_against(patch: &Value, current: &Value) -> Result<(), CliError> {
    setup::validate_patch(patch).map_err(invalid)?;
    let p = patch.as_object().expect("validated patch is an object");
    let mut combined = p.clone();
    for (a, b) in PAIRED {
        match (p.contains_key(a), p.contains_key(b)) {
            (true, false) => {
                combined.insert(b.into(), current[b].clone());
            }
            (false, true) => {
                combined.insert(a.into(), current[a].clone());
            }
            _ => {}
        }
    }
    if combined.len() != p.len() {
        setup::validate_patch(&Value::Object(combined)).map_err(invalid)?;
    }
    if let Some(dev) = p.get("selected_device").and_then(|d| d.as_str()) {
        let devices = crate::audio::list_input_devices();
        if !devices.iter().any(|d| d == dev) {
            return Err(invalid(vec![setup::FieldError {
                key: "selected_device".into(),
                problem: "not_allowed",
                message: format!("no microphone is called '{dev}'"),
                allowed: Some(json!(devices)),
            }]));
        }
    }
    Ok(())
}

fn diff(patch: &Value, current: &Value) -> Vec<Value> {
    patch
        .as_object()
        .expect("patch is an object")
        .iter()
        .filter(|(k, v)| current[k.as_str()] != **v)
        .map(|(k, v)| json!({ "key": k, "from": current[k.as_str()], "to": v }))
        .collect()
}

/// Validate, then hand the patch to the same setter the app uses, then read
/// it back: a value the core still adjusted is reported, not hidden.
fn apply_patch(patch: Value, dry_run: bool) -> CmdResult {
    assert!(patch.is_object(), "apply_patch takes an object");
    if patch.as_object().is_some_and(|m| m.is_empty()) {
        return Err(CliError::usage("no settings given"));
    }
    let _lock = if dry_run { None } else { Some(write_lock()?) };
    let current = current_config()?;
    validate_against(&patch, &current)?;
    let changes = diff(&patch, &current);
    if dry_run {
        return Ok(json!({ "dry_run": true, "changes": changes }));
    }
    let c = CString::new(patch.to_string()).expect("JSON has no NUL");
    let rc = unsafe { crate::ffi::dimmy_set_config_json(c.as_ptr()) };
    if rc != 0 {
        return Err(CliError::failed(
            "failed",
            format!("saving settings failed ({rc})"),
            "",
        ));
    }
    let after = current_config()?;
    let not_applied: Vec<Value> = patch
        .as_object()
        .expect("object")
        .iter()
        .filter(|(k, v)| after[k.as_str()] != **v)
        .map(|(k, v)| json!({ "key": k, "wanted": v, "got": after[k.as_str()] }))
        .collect();
    if !not_applied.is_empty() {
        return Err(CliError::failed(
            "not_applied",
            "Dimmy adjusted some values while saving",
            "See details; run `dimmy-setup config get`.",
        )
        .with_details(Value::Array(not_applied)));
    }
    Ok(json!({ "dry_run": false, "changes": changes }))
}

fn config_set(args: &mut Vec<String>) -> CmdResult {
    let dry_run = take_switch(args, "--dry-run");
    let mut patch = match take_value(args, "--json")? {
        Some(j) => match serde_json::from_str::<Value>(&j) {
            Ok(Value::Object(m)) => m,
            _ => return Err(CliError::usage("--json must be a JSON object")),
        },
        None => Map::new(),
    };
    if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
        return Err(CliError::usage(format!("unknown option '{flag}'")));
    }
    patch.extend(parse_pairs(args)?);
    apply_patch(Value::Object(patch), dry_run)
}

fn profile_apply(args: &mut Vec<String>) -> CmdResult {
    let dry_run = take_switch(args, "--dry-run");
    let need = |args: &mut Vec<String>, f: &str| -> Result<String, CliError> {
        take_value(args, f)?.ok_or_else(|| CliError::usage(format!("{f} is required")))
    };
    let answers = setup::ProfileAnswers {
        usage: need(args, "--use")?,
        lang: need(args, "--lang")?,
        stt: need(args, "--stt")?,
        stt_provider: take_value(args, "--stt-provider")?,
        stt_backend: take_value(args, "--stt-backend")?,
        recap: need(args, "--recap")?,
        recap_model: take_value(args, "--recap-model")?,
    };
    no_more(args)?;
    let patch = setup::profile_patch(&answers).map_err(invalid)?;
    let mut v = apply_patch(patch.clone(), dry_run)?;
    v["settings"] = patch;
    Ok(v)
}

// ── models ──────────────────────────────────────────────────────────

struct ModelRow {
    id: String,
    kind: &'static str,
    name: String,
    size_mb: u64,
    present: bool,
    dir: Option<PathBuf>,
}

fn model_rows() -> Vec<ModelRow> {
    let stt_dir = crate::local_stt::model_directory();
    let mut rows: Vec<ModelRow> = crate::local_stt::AVAILABLE_MODELS
        .iter()
        .map(|m| ModelRow {
            id: m.filename.into(),
            kind: "whisper",
            name: m.name.into(),
            size_mb: m.size_mb as u64,
            present: crate::local_stt::model_exists(m.filename),
            dir: Some(stt_dir.clone()),
        })
        .collect();
    if setup::available_backends().contains(&"parakeet") {
        rows.push(ModelRow {
            id: crate::download_center::PARAKEET_ID.into(),
            kind: "parakeet",
            name: "Parakeet TDT v3".into(),
            size_mb: crate::parakeet::BUNDLE_SIZE_MB as u64,
            present: crate::parakeet::active_bundle_present(),
            dir: crate::parakeet::bundle_dir(),
        });
    }
    for m in crate::qwen_asr::AVAILABLE_MODELS
        .iter()
        .filter(|m| crate::qwen_asr::available(m))
    {
        rows.push(ModelRow {
            id: format!("qwen:{}", m.model_file),
            kind: "qwen",
            name: m.name.into(),
            size_mb: m.size_mb as u64,
            present: crate::qwen_asr::bundle_present(m.model_file),
            dir: Some(stt_dir.clone()),
        });
    }
    if crate::whistle::engine_available() {
        rows.push(ModelRow {
            id: crate::download_center::WHISTLE_ID.into(),
            kind: "whistle",
            name: "Whistle".into(),
            size_mb: crate::whistle::size_mb() as u64,
            present: crate::whistle::bundle_present(),
            dir: Some(stt_dir.clone()),
        });
    }
    for m in crate::local_llm::AVAILABLE_LLM_MODELS {
        rows.push(ModelRow {
            id: m.filename.into(),
            kind: "llm",
            name: m.name.into(),
            size_mb: m.size_mb as u64,
            present: crate::local_llm::model_exists(m.filename),
            dir: crate::local_llm::model_path(m.filename)
                .parent()
                .map(Path::to_path_buf),
        });
    }
    rows
}

fn models_list() -> CmdResult {
    let rows: Vec<Value> = model_rows()
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "kind": r.kind, "name": r.name,
                "size_mb": r.size_mb, "present": r.present,
            })
        })
        .collect();
    Ok(json!({
        "models": rows,
        "disk_free_mb": mb(setup::free_disk_bytes(&crate::local_stt::model_directory())),
    }))
}

/// Room for the file plus a margin, so a download never fills the disk.
fn needed_mb(size_mb: u64) -> u64 {
    size_mb + size_mb / 10 + 512
}

fn models_download(id: &str, out: &mut dyn Write) -> CmdResult {
    let row = model_rows()
        .into_iter()
        .find(|r| r.id == id)
        .filter(|_| crate::download_center::is_known_id(id))
        .ok_or_else(|| {
            CliError::failed(
                "not_found",
                format!("'{id}' is not a model this build can download"),
                "dimmy-setup models list",
            )
        })?;
    if row.present {
        return Ok(json!({ "id": id, "state": "done", "already_present": true }));
    }
    let free = row
        .dir
        .as_deref()
        .and_then(setup::free_disk_bytes)
        .map(|b| b / (1024 * 1024));
    let need = needed_mb(row.size_mb);
    match free {
        Some(f) if f >= need => {}
        _ => {
            return Err(CliError::failed(
                "disk_full",
                format!(
                    "{} MB needed, {} MB free",
                    need,
                    free.map_or("unknown".into(), |f| f.to_string())
                ),
                "Free some disk space, or pick a smaller model.",
            ))
        }
    }
    let _lock = write_lock()?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let center = crate::download_center::with_emit(Box::new(move |p| {
        let _ = tx.send(p.to_string());
    }));
    center.enqueue(id);
    let mut last = json!({});
    for payload in rx.iter() {
        let v: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        if v["id"] != id {
            continue;
        }
        let mut line = v.clone();
        line["event"] = json!("progress");
        print(out, &line);
        let state = v["state"].as_str().unwrap_or("");
        last = v.clone();
        if matches!(state, "done" | "failed" | "cancelled") {
            break;
        }
    }
    match last["state"].as_str() {
        Some("done") => Ok(json!({ "id": id, "state": "done", "already_present": false })),
        _ => Err(CliError::failed(
            "download_failed",
            last["error"]
                .as_str()
                .unwrap_or("download did not finish")
                .to_string(),
            "Check the connection and run the same command again; it resumes.",
        )),
    }
}

// ── login / keys ────────────────────────────────────────────────────

fn install_hint(cli: &str) -> &'static str {
    match cli {
        "claude" => "npm install -g @anthropic-ai/claude-code",
        "codex" => "npm install -g @openai/codex",
        _ => "npm install -g @google/gemini-cli",
    }
}

fn login(action: &str, cli: &str) -> CmdResult {
    if !matches!(cli, "claude" | "codex" | "gemini") {
        return Err(CliError::usage(format!(
            "'{cli}' is not claude, codex or gemini"
        )));
    }
    match action {
        "status" => {
            let (all, _, _) = cli_states();
            Ok(json!({ "cli": cli, "status": all[cli] }))
        }
        "start" => {
            let r: Result<(), String> = match cli {
                "claude" => crate::claude_code::spawn_login().map_err(|e| e.to_string()),
                "codex" => crate::codex::spawn_login().map_err(|e| e.to_string()),
                _ => crate::gemini_cli::spawn_login().map_err(|e| e.to_string()),
            };
            r.map_err(|e| CliError::failed("login_failed", e, install_hint(cli)))?;
            Ok(json!({
                "cli": cli,
                "started": true,
                "next": format!("Finish signing in in the window that opened, then run `dimmy-setup login test {cli}`."),
            }))
        }
        _ => {
            let started = std::time::Instant::now();
            let r: Result<(), (&'static str, String)> = match cli {
                "claude" => {
                    crate::claude_code::run_blocking(PING_PROMPT, "", Duration::from_secs(30))
                        .map(|_| ())
                        .map_err(|e| (crate::claude_code::error_category(&e), e.to_string()))
                }
                "codex" => crate::codex::run_blocking(PING_PROMPT, "", Duration::from_secs(30))
                    .map(|_| ())
                    .map_err(|e| (crate::codex::error_category(&e), e.to_string())),
                _ => crate::gemini_cli::run_blocking(PING_PROMPT, "", Duration::from_secs(60))
                    .map(|_| ())
                    .map_err(|e| (crate::gemini_cli::error_category(&e), e.to_string())),
            };
            match r {
                Ok(()) => {
                    Ok(json!({ "cli": cli, "elapsed_ms": started.elapsed().as_millis() as u64 }))
                }
                Err((cat, msg)) => Err(CliError::failed(
                    "login_test_failed",
                    msg,
                    match cat {
                        "not_installed" => install_hint(cli).to_string(),
                        "not_logged_in" => format!("dimmy-setup login start {cli}"),
                        _ => "Run `dimmy-setup diag` for the log.".to_string(),
                    },
                )
                .with_details(json!({ "category": cat }))),
            }
        }
    }
}

const KEY_PROVIDERS: [Provider; 9] = [
    Provider::Groq,
    Provider::OpenAI,
    Provider::Anthropic,
    Provider::Gemini,
    Provider::Deepgram,
    Provider::OpenRouter,
    Provider::Together,
    Provider::Fireworks,
    Provider::Custom,
];

fn key_flags() -> Value {
    state();
    crate::ffi::with_key_store(|store, use_kr| {
        let mut m = Map::new();
        for p in KEY_PROVIDERS {
            m.insert(
                p.as_str().into(),
                json!({
                    "stt": store.has_key(KeyringScope::Stt(p), use_kr),
                    "llm": store.has_key(KeyringScope::Llm(p), use_kr),
                    "recap": store.has_key(KeyringScope::Recap(p), use_kr),
                    "any": crate::ffi::has_any_scope_key(store, p, use_kr),
                }),
            );
        }
        Value::Object(m)
    })
}

fn key_status() -> CmdResult {
    Ok(json!({ "keys": key_flags() }))
}

fn key_test(tag: &str) -> CmdResult {
    let provider = Provider::from_tag(tag)
        .ok_or_else(|| CliError::usage(format!("'{tag}' is not a provider id")))?;
    let Some(url) = provider.default_llm_url() else {
        return Err(CliError::failed(
            "not_supported",
            format!("testing a {tag} speech key is not available yet"),
            "Try it in Dimmy: Settings > Providers.",
        ));
    };
    let model =
        setup::pick_model(provider.as_str(), "llm", &["fastest", "fast"]).ok_or_else(|| {
            CliError::failed("not_supported", format!("no {tag} model to test with"), "")
        })?;
    state();
    let key = crate::ffi::with_key_store(|store, use_kr| {
        [
            KeyringScope::Llm(provider),
            KeyringScope::Stt(provider),
            KeyringScope::Recap(provider),
        ]
        .into_iter()
        .find_map(|s| crate::load_key_with_store(store, s, use_kr).filter(|k| !k.is_empty()))
    })
    .ok_or_else(|| {
        CliError::failed(
            "no_key",
            format!("no {tag} key is saved"),
            "Open Dimmy, Settings > Providers, and paste the key there (never through the agent).",
        )
    })?;
    let started = std::time::Instant::now();
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| CliError::failed("failed", e.to_string(), ""))?;
    let r = rt.block_on(crate::llm::process_raw_prompt(
        url,
        &model,
        &key,
        PING_PROMPT,
        1024,
        "api_key",
    ));
    drop(key);
    let accepted = match &r {
        Ok(_) => true,
        Err(crate::error::LlmError::Truncated | crate::error::LlmError::Refusal) => true,
        Err(_) => false,
    };
    if accepted {
        return Ok(json!({
            "provider": tag,
            "model": model,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }));
    }
    let e = r.expect_err("not accepted means an error");
    let (code, fix) = setup::llm_rc_error(crate::ffi::categorize_llm_error_to_rc(&e));
    Err(CliError::failed(code, e.to_string(), fix)
        .with_details(json!({ "provider": tag, "model": model })))
}

// ── tests ───────────────────────────────────────────────────────────

fn test_stt(file: &Path) -> CmdResult {
    state();
    let cfg = current_config()?;
    if cfg["stt_mode"] != "local" {
        return Err(CliError::failed(
            "not_supported",
            "testing cloud transcription from the command line is not available yet",
            "Try a dictation in Dimmy, or set stt_mode=local to test an on-device engine.",
        ));
    }
    if !file.is_file() {
        return Err(CliError::usage(format!("no file at {}", file.display())));
    }
    let selected = cfg["local_stt_backend"]
        .as_str()
        .unwrap_or("whisper")
        .to_string();
    let whisper_model = cfg["local_model"].as_str().unwrap_or_default().to_string();
    let qwen = cfg["qwen_asr_model"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let ready = match selected.as_str() {
        "parakeet" => crate::parakeet::active_bundle_present(),
        "qwen" => crate::qwen_asr::bundle_present(&qwen),
        "whistle" => crate::whistle::bundle_present(),
        _ => crate::local_stt::model_exists(&whisper_model),
    };
    if !ready {
        let id = match selected.as_str() {
            "parakeet" => crate::download_center::PARAKEET_ID.to_string(),
            "qwen" => format!("qwen:{qwen}"),
            "whistle" => crate::download_center::WHISTLE_ID.to_string(),
            _ => whisper_model.clone(),
        };
        return Err(CliError::failed(
            "model_missing",
            format!("the {selected} model is not downloaded"),
            format!("dimmy-setup models download {id}"),
        ));
    }
    let path = file.to_string_lossy().to_string();
    let is_wav = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    let decoded = if is_wav {
        crate::ffi::decode_wav_via_hound(&path)
    } else {
        crate::ffi::decode_via_symphonia(&path)
    };
    let (mono, rate) = decoded.map_err(|e| {
        CliError::failed("decode_failed", e, "Use a WAV, MP3, M4A, FLAC or OGG file.")
    })?;
    let secs = mono.len() as f64 / rate.max(1) as f64;
    let samples = crate::preprocess::process_buffer_for_file_load(&mono, rate);
    drop(mono);
    if samples.is_empty() {
        return Err(CliError::failed(
            "silent",
            "the file has no audio",
            "Use a recording with speech.",
        ));
    }
    let language = cfg["language"].as_str().unwrap_or_default().to_string();
    let backend = crate::ffi::effective_local_backend(&selected);
    let started = std::time::Instant::now();
    let audio = crate::audio::ProcessedAudio {
        samples,
        sample_rate: rate,
    };
    let mut text = String::new();
    for chunk in audio.split_at_silence(30 * rate as usize) {
        let part = match backend {
            "parakeet" => crate::transcribe::transcribe_audio_local_parakeet(&chunk),
            "qwen" => crate::transcribe::transcribe_audio_local_qwen(&chunk, &qwen, &language),
            "whistle" => crate::transcribe::transcribe_audio_local_whistle(&chunk, &language, &[]),
            _ => crate::transcribe::transcribe_audio_local(&chunk, &language, &whisper_model, ""),
        }
        .map_err(|e| {
            CliError::failed(
                "transcribe_failed",
                e.to_string(),
                "Run `dimmy-setup diag` for the log.",
            )
        })?;
        let delta = crate::chunked_stt::dedup_last_3_words(&text, &part);
        if !delta.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&delta);
        }
    }
    Ok(json!({
        "backend": backend,
        "audio_secs": secs,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "text": text,
    }))
}

fn test_recap() -> CmdResult {
    let cfg = current_config()?;
    let prompt = CString::new(RECAP_TEST_PROMPT).expect("no NUL");
    let model = cfg["recap_model_override"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let model_c = CString::new(model.clone()).expect("no NUL");
    let started = std::time::Instant::now();
    let r = read_buf(|p, n| unsafe {
        crate::ffi::dimmy_llm_call_raw(prompt.as_ptr(), model_c.as_ptr(), 512, p, n)
    });
    match r {
        Ok(text) => Ok(json!({
            "model": model,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "summary": crate::truncate_utf8(&text, 2000),
        })),
        Err(rc) => {
            let (code, fix) = setup::llm_rc_error(rc);
            Err(
                CliError::failed(code, format!("the test summary failed (rc {rc})"), fix)
                    .with_details(json!({ "rc": rc, "model": model })),
            )
        }
    }
}

// ── onboarding / diag / app ─────────────────────────────────────────

fn onboarding_marker_exists() -> bool {
    crate::onboarding_marker_path().is_some_and(|p| p.exists())
}

fn onboarding_status() -> CmdResult {
    let dir = config_dir()?;
    Ok(json!({
        "done": onboarding_marker_exists(),
        "config_exists": dir.join("config.json").exists(),
    }))
}

fn onboarding_done() -> CmdResult {
    let _lock = write_lock()?;
    crate::mark_onboarding_done()
        .map_err(|e| CliError::failed("failed", e, "Check the settings folder is writable."))?;
    assert!(onboarding_marker_exists());
    Ok(json!({ "done": true }))
}

fn diag(lines: usize) -> CmdResult {
    let log = crate::log_path()
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let (tail, counts) = setup::diag_log(&log, lines);
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "flavor": crate::build_flavor(),
        "os": { "name": std::env::consts::OS, "version": setup::os_version(), "arch": std::env::consts::ARCH },
        "settings": schema_view(&current_config()?),
        "error_categories": counts,
        "log_tail": tail,
    }))
}

fn app_status() -> CmdResult {
    Ok(json!({ "app": app_json()? }))
}

fn app_start() -> CmdResult {
    if app_running()? {
        return Ok(json!({ "already_running": true }));
    }
    let exe = app_exe().filter(|p| p.exists()).ok_or_else(|| {
        CliError::failed(
            "not_installed",
            "Dimmy is not installed next to dimmy-setup",
            "Install Dimmy from https://dimmy.app.",
        )
    })?;
    let spawned = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/open")
            .arg(&exe)
            .spawn()
    } else {
        std::process::Command::new(&exe).spawn()
    };
    spawned.map_err(|e| CliError::failed("start_failed", e.to_string(), ""))?;
    Ok(json!({ "started": true }))
}

fn app_quit() -> CmdResult {
    if !app_running()? {
        return Ok(json!({ "was_running": false }));
    }
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "quit app \"Dimmy\""])
            .status()
    } else if cfg!(target_os = "windows") {
        let exe = app_exe().filter(|p| p.exists()).ok_or_else(|| {
            CliError::failed(
                "not_installed",
                "Dimmy.Windows.exe is not next to dimmy-setup",
                "Quit Dimmy from its tray icon.",
            )
        })?;
        std::process::Command::new(exe)
            .args(["--command", "quit"])
            .status()
    } else {
        return Err(CliError::failed(
            "not_supported",
            "app quit is not available on this OS",
            "Quit Dimmy yourself.",
        ));
    };
    status.map_err(|e| {
        CliError::failed(
            "quit_failed",
            e.to_string(),
            "Quit Dimmy from its tray icon.",
        )
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if !app_running()? {
            return Ok(json!({ "was_running": true }));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(CliError::failed(
        "still_running",
        "Dimmy did not quit within 15 s",
        "Quit Dimmy from its tray icon.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn pairs_keep_json_types_and_fall_back_to_strings() {
        let m = parse_pairs(&s(&[
            "llm_enabled=true",
            "auto_recap_threshold_secs=60",
            "shortcut=ctrl+shift",
            "language=",
            "x=\"7\"",
        ]))
        .unwrap();
        assert_eq!(m["llm_enabled"], json!(true));
        assert_eq!(m["auto_recap_threshold_secs"], json!(60));
        assert_eq!(m["shortcut"], json!("ctrl+shift"));
        assert_eq!(m["language"], json!(""));
        assert_eq!(m["x"], json!("7"));
        assert!(parse_pairs(&s(&["novalue"])).is_err());
        assert!(parse_pairs(&s(&["=1"])).is_err());
    }

    #[test]
    fn value_flags_are_taken_and_checked() {
        let mut a = s(&["--use", "both", "--dry-run", "--lang"]);
        assert_eq!(take_value(&mut a, "--use").unwrap(), Some("both".into()));
        assert!(take_switch(&mut a, "--dry-run"));
        assert!(take_value(&mut a, "--lang").is_err());
        let mut a = s(&["--lang", "--stt"]);
        assert!(take_value(&mut a, "--lang").is_err());
    }

    fn run_args(args: &[&str]) -> (i32, Value) {
        let mut out: Vec<u8> = Vec::new();
        let code = run(&s(args), &mut out);
        let text = String::from_utf8(out).unwrap();
        let last = text.lines().last().unwrap().to_string();
        (code, serde_json::from_str(&last).unwrap())
    }

    #[test]
    fn usage_errors_exit_2_with_a_json_error() {
        for args in [
            &[][..],
            &["nope"],
            &["config"],
            &["login", "status", "bard"],
            &["info", "extra"],
        ] {
            let (code, v) = run_args(args);
            assert_eq!(code, EXIT_USAGE, "{args:?}");
            assert_eq!(v["ok"], false);
            assert_eq!(v["error"]["code"], "usage");
        }
    }

    #[test]
    fn schema_and_help_need_no_state() {
        let (code, v) = run_args(&["schema"]);
        assert_eq!(code, EXIT_OK);
        assert!(v["schema"]["settings"].as_array().unwrap().len() > 20);
        let (code, v) = run_args(&["help"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn profile_dry_run_rejects_bad_answers_with_details() {
        let (code, v) = run_args(&[
            "profile",
            "apply",
            "--use",
            "party",
            "--lang",
            "it",
            "--stt",
            "cloud",
            "--recap",
            "api",
            "--dry-run",
        ]);
        assert_eq!(code, EXIT_USAGE);
        assert_eq!(v["error"]["code"], "invalid_settings");
        assert_eq!(v["error"]["details"][0]["key"], "--use");
    }

    #[test]
    fn errors_are_truncated() {
        let e = CliError::failed("x", "é".repeat(500), "");
        assert!(e.message.len() <= 200);
    }
}

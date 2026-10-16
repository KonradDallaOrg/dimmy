//! Agent setup: the pure half of `dimmy-setup`.
//!
//! Which settings an agent may set, strict validation of a patch, the
//! "few answers -> consistent settings" profile, the machine facts the core
//! does not already collect, log redaction for `diag`, and the lock that
//! tells the CLI the app is running. Command dispatch and I/O live in
//! `setup_cli.rs`. Design: `docs/dev/agent-setup.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::hardware::LocalFitness;
use crate::provider::Provider;
use crate::AppConfig;

// ── Schema ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Bool,
    Enum(Vec<String>),
    Int {
        min: i64,
        max: i64,
    },
    /// A hotkey combo the global hook can bind, e.g. `ctrl+shift+space`.
    Shortcut,
    /// The exact name of an input device, as `check` lists them.
    Device,
}

#[derive(Debug, Clone)]
pub struct Setting {
    pub key: &'static str,
    pub kind: Kind,
    pub help: &'static str,
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn catalog() -> Value {
    serde_json::from_str(crate::catalog::MODEL_CATALOG_JSON)
        .expect("embedded catalog is valid JSON")
}

fn catalog_providers() -> Vec<Value> {
    catalog()["providers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn has_task(m: &Value, task: &str) -> bool {
    m["tasks"]
        .as_array()
        .is_some_and(|t| t.iter().any(|x| x == task))
}

/// Catalog model ids for `task` ("stt" | "llm" | "recap"), catalog order.
pub fn catalog_models(task: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in catalog_providers() {
        for m in p["models"].as_array().cloned().unwrap_or_default() {
            if has_task(&m, task) {
                if let Some(id) = m["id"].as_str() {
                    if !out.iter().any(|x| x == id) {
                        out.push(id.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Provider ids that list `model` for `task`.
fn providers_of_model(task: &str, model: &str) -> Vec<String> {
    catalog_providers()
        .iter()
        .filter(|p| {
            p["models"]
                .as_array()
                .is_some_and(|ms| ms.iter().any(|m| m["id"] == model && has_task(m, task)))
        })
        .filter_map(|p| p["id"].as_str().map(String::from))
        .collect()
}

/// First catalog model of `provider` for `task`, preferring the given tiers.
pub fn pick_model(provider: &str, task: &str, tiers: &[&str]) -> Option<String> {
    let p = catalog_providers()
        .into_iter()
        .find(|p| p["id"] == provider)?;
    let models: Vec<Value> = p["models"]
        .as_array()?
        .iter()
        .filter(|m| has_task(m, task))
        .cloned()
        .collect();
    for tier in tiers {
        if let Some(m) = models.iter().find(|m| m["tier"] == *tier) {
            return m["id"].as_str().map(String::from);
        }
    }
    models
        .first()
        .and_then(|m| m["id"].as_str().map(String::from))
}

fn catalog_urls(field: &str) -> Vec<String> {
    catalog_providers()
        .iter()
        .filter_map(|p| p[field].as_str())
        .filter(|u| !u.is_empty())
        .map(String::from)
        .collect()
}

/// Provider ids with a cloud speech-to-text endpoint.
pub fn stt_providers() -> Vec<String> {
    catalog_providers()
        .iter()
        .filter(|p| p["stt_url"].as_str().is_some_and(|u| !u.is_empty()))
        .filter_map(|p| p["id"].as_str().map(String::from))
        .collect()
}

fn stt_url_of(provider: &str) -> Option<String> {
    catalog_providers()
        .iter()
        .find(|p| p["id"] == provider)
        .and_then(|p| p["stt_url"].as_str())
        .filter(|u| !u.is_empty())
        .map(String::from)
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

fn stt_url_values() -> Vec<String> {
    let mut v = catalog_urls("stt_url");
    push_unique(&mut v, crate::DEFAULT_API_URL);
    v
}

fn llm_url_values() -> Vec<String> {
    let mut v = catalog_urls("llm_url");
    push_unique(&mut v, crate::DEFAULT_LLM_URL);
    for p in [
        Provider::Anthropic,
        Provider::OpenAI,
        Provider::Gemini,
        Provider::Groq,
        Provider::OpenRouter,
        Provider::Fireworks,
        Provider::Together,
    ] {
        if let Some(u) = p.default_llm_url() {
            push_unique(&mut v, u);
        }
    }
    push_unique(&mut v, crate::claude_code::PROVIDER_URL);
    push_unique(&mut v, crate::codex::PROVIDER_URL);
    v
}

fn language_values() -> Vec<String> {
    let mut v = vec![String::new()];
    v.extend(strs(crate::llm::SUPPORTED_TRANSLATE_LANGS));
    v
}

fn local_llm_files() -> Vec<String> {
    crate::local_llm::AVAILABLE_LLM_MODELS
        .iter()
        .map(|m| m.filename.to_string())
        .collect()
}

fn recap_model_values() -> Vec<String> {
    let mut v = vec![String::new()];
    v.extend(catalog_models("recap"));
    v.extend(local_llm_files().into_iter().map(|f| format!("local:{f}")));
    v
}

/// The local STT engines this build can actually run.
pub fn available_backends() -> Vec<&'static str> {
    let mut v = Vec::new();
    if cfg!(feature = "local-stt") {
        v.push("whisper");
    }
    if cfg!(any(
        feature = "local-stt-parakeet",
        feature = "local-stt-parakeet-fluid"
    )) {
        v.push("parakeet");
    }
    if crate::qwen_asr::engine_available() {
        v.push("qwen");
    }
    if crate::whistle::engine_available() {
        v.push("whistle");
    }
    v
}

/// Every setting an agent may set during setup. Pure UI cosmetics, power
/// knobs and integrations with their own sign-in flow are deliberately left
/// out: they belong to the user, in the app.
pub fn settings() -> Vec<Setting> {
    let styles: Vec<String> = crate::llm::LlmStyle::ALL
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    let tones: Vec<String> = crate::llm::LlmTone::ALL
        .iter()
        .map(|t| t.as_str().to_string())
        .collect();
    let mut translate = vec!["none".to_string()];
    translate.extend(strs(crate::llm::SUPPORTED_TRANSLATE_LANGS));
    vec![
        Setting {
            key: "stt_mode",
            kind: Kind::Enum(strs(&["local", "cloud"])),
            help: "Where speech becomes text: on this computer (private, no account needed) or with an online service (needs that service's key).",
        },
        Setting {
            key: "api_url",
            kind: Kind::Enum(stt_url_values()),
            help: "Which online service transcribes speech when the mode is cloud.",
        },
        Setting {
            key: "api_model",
            kind: Kind::Enum(catalog_models("stt")),
            help: "Which of that online service's speech models to use.",
        },
        Setting {
            key: "local_stt_backend",
            kind: Kind::Enum(strs(&["whisper", "parakeet", "qwen", "whistle"])),
            help: "Which on-device speech engine to use when the mode is local. Each needs its model downloaded once.",
        },
        Setting {
            key: "local_model",
            kind: Kind::Enum(
                crate::local_stt::AVAILABLE_MODELS
                    .iter()
                    .map(|m| m.filename.to_string())
                    .collect(),
            ),
            help: "Which Whisper model the on-device Whisper engine uses: bigger is more accurate and slower.",
        },
        Setting {
            key: "qwen_asr_model",
            kind: Kind::Enum(
                crate::qwen_asr::AVAILABLE_MODELS
                    .iter()
                    .map(|m| m.model_file.to_string())
                    .collect(),
            ),
            help: "Which Qwen speech model the on-device Qwen engine uses.",
        },
        Setting {
            key: "language",
            kind: Kind::Enum(language_values()),
            help: "The language you speak, as a two-letter code. Empty means detect it automatically.",
        },
        Setting {
            key: "streaming_dictation",
            kind: Kind::Bool,
            help: "Type the words as you speak instead of all at once when you stop.",
        },
        Setting {
            key: "llm_enabled",
            kind: Kind::Bool,
            help: "Let an AI model clean up or restyle what you dictated before it is pasted.",
        },
        Setting {
            key: "llm_mode",
            kind: Kind::Enum(strs(&["cloud", "local"])),
            help: "Whether that AI clean-up runs online or on this computer.",
        },
        Setting {
            key: "llm_api_url",
            kind: Kind::Enum(llm_url_values()),
            help: "Which online AI service does the clean-up when it runs online.",
        },
        Setting {
            key: "llm_api_model",
            kind: Kind::Enum(catalog_models("llm")),
            help: "Which of that service's models does the clean-up.",
        },
        Setting {
            key: "llm_auth_method",
            kind: Kind::Enum(strs(&["api_key", "subscription"])),
            help: "Pay for the clean-up with an API key, or use a Claude or ChatGPT subscription you are already signed in to.",
        },
        Setting {
            key: "llm_style",
            kind: Kind::Enum(styles),
            help: "How the AI rewrites your dictation. off leaves it as spoken; correct fixes grammar and removes filler words.",
        },
        Setting {
            key: "llm_tone",
            kind: Kind::Enum(tones),
            help: "The tone the AI writes in when it rewrites.",
        },
        Setting {
            key: "llm_translate_to",
            kind: Kind::Enum(translate),
            help: "Translate every dictation into this language. none keeps the language you spoke.",
        },
        Setting {
            key: "local_llm_model",
            kind: Kind::Enum(local_llm_files()),
            help: "Which on-device AI model does the clean-up when it runs on this computer.",
        },
        Setting {
            key: "recap_auth_method",
            kind: Kind::Enum(strs(&["", "api_key", "subscription"])),
            help: "How meeting summaries are paid for: an API key, or a Claude or ChatGPT subscription. Empty means API key.",
        },
        Setting {
            key: "recap_model_override",
            kind: Kind::Enum(recap_model_values()),
            help: "Which AI model writes meeting summaries. Empty picks one for you; local:<file> runs on this computer.",
        },
        Setting {
            key: "meeting_generate_recap",
            kind: Kind::Bool,
            help: "Write a summary with decisions and action items when a meeting recording stops.",
        },
        Setting {
            key: "auto_recap_threshold_secs",
            kind: Kind::Int { min: 0, max: 3600 },
            help: "A dictation longer than this many seconds also gets a meeting-style summary. 0 turns it off.",
        },
        Setting {
            key: "call_detect_enabled",
            kind: Kind::Bool,
            help: "Notice when a call starts (Teams, Zoom, Meet...) and offer to record it.",
        },
        Setting {
            key: "call_detect_auto_record",
            kind: Kind::Bool,
            help: "Start recording a detected call without asking first. Only works while call detection is on.",
        },
        Setting {
            key: "diarization_enabled",
            kind: Kind::Bool,
            help: "Label who said what in meeting transcripts. Needs a 104 MB model.",
        },
        Setting {
            key: "shortcut",
            kind: Kind::Shortcut,
            help: "The key combination that starts dictation, like ctrl+shift+space.",
        },
        Setting {
            key: "shortcut_mode",
            kind: Kind::Enum(strs(&["hold", "toggle"])),
            help: "hold: record while the keys are held down. toggle: press once to start and again to stop.",
        },
        Setting {
            key: "selected_device",
            kind: Kind::Device,
            help: "Which microphone to record from, by its exact name.",
        },
        Setting {
            key: "save_audio_in_history",
            kind: Kind::Bool,
            help: "Keep the audio of each dictation so you can play it back later. Uses disk space.",
        },
        Setting {
            key: "history_audio_keep_days",
            kind: Kind::Int { min: 0, max: 3650 },
            help: "Delete saved dictation audio after this many days. 0 keeps it until the size limit.",
        },
        Setting {
            key: "filler_removal_enabled",
            kind: Kind::Bool,
            help: "Remove filler words like um and uh from dictation.",
        },
        Setting {
            key: "keep_in_clipboard",
            kind: Kind::Bool,
            help: "Leave the dictated text on the clipboard after it is pasted.",
        },
    ]
}

pub fn setting(key: &str) -> Option<Setting> {
    settings().into_iter().find(|s| s.key == key)
}

/// The value `cfg` holds for a schema key, in the shape `config set` takes.
pub fn config_value(cfg: &AppConfig, key: &str) -> Value {
    match key {
        "stt_mode" => json!(cfg.stt_mode),
        "api_url" => json!(cfg.api_url),
        "api_model" => json!(cfg.api_model),
        "local_stt_backend" => json!(cfg.local_stt_backend),
        "local_model" => json!(cfg.local_model),
        "qwen_asr_model" => json!(cfg.qwen_asr_model),
        "language" => json!(cfg.language),
        "streaming_dictation" => json!(cfg.streaming_dictation),
        "llm_enabled" => json!(cfg.llm_enabled),
        "llm_mode" => json!(cfg.llm_mode),
        "llm_api_url" => json!(cfg.llm_api_url),
        "llm_api_model" => json!(cfg.llm_api_model),
        "llm_auth_method" => json!(cfg.llm_auth_method),
        "llm_style" => json!(cfg.llm_style.as_str()),
        "llm_tone" => json!(cfg.llm_tone.as_str()),
        "llm_translate_to" => json!(cfg.llm_translate_to),
        "local_llm_model" => json!(cfg.local_llm_model),
        "recap_auth_method" => json!(cfg.recap_auth_method),
        "recap_model_override" => json!(cfg.recap_model_override),
        "meeting_generate_recap" => json!(cfg.meeting_generate_recap),
        "auto_recap_threshold_secs" => json!(cfg.auto_recap_threshold_secs),
        "call_detect_enabled" => json!(cfg.call_detect_enabled),
        "call_detect_auto_record" => json!(cfg.call_detect_auto_record),
        "diarization_enabled" => json!(cfg.diarization_enabled),
        "shortcut" => json!(cfg.shortcut),
        "shortcut_mode" => json!(cfg.shortcut_mode),
        "selected_device" => json!(cfg.selected_device),
        "save_audio_in_history" => json!(cfg.save_audio_in_history),
        "history_audio_keep_days" => json!(cfg.history_audio_keep_days),
        "filler_removal_enabled" => json!(cfg.filler_removal_enabled),
        "keep_in_clipboard" => json!(cfg.keep_in_clipboard),
        other => panic!("config_value: '{other}' is not a schema key"),
    }
}

pub fn schema() -> Value {
    let defaults = AppConfig::default();
    let list: Vec<Value> = settings()
        .iter()
        .map(|s| {
            let mut v = json!({
                "key": s.key,
                "default": config_value(&defaults, s.key),
                "help": s.help,
            });
            match &s.kind {
                Kind::Bool => v["type"] = json!("bool"),
                Kind::Enum(allowed) => {
                    v["type"] = json!("enum");
                    v["allowed"] = json!(allowed);
                }
                Kind::Int { min, max } => {
                    v["type"] = json!("int");
                    v["min"] = json!(min);
                    v["max"] = json!(max);
                }
                Kind::Shortcut => v["type"] = json!("shortcut"),
                Kind::Device => v["type"] = json!("device"),
            }
            v
        })
        .collect();
    assert_eq!(list.len(), settings().len());
    json!({ "settings": list, "local_stt_backends_in_this_build": available_backends() })
}

// ── Strict validation ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct FieldError {
    pub key: String,
    /// unknown_key | wrong_type | not_allowed | out_of_range | conflict | missing
    pub problem: &'static str,
    pub message: String,
    pub allowed: Option<Value>,
}

impl FieldError {
    fn new(key: &str, problem: &'static str, message: String, allowed: Option<Value>) -> Self {
        Self {
            key: key.to_string(),
            problem,
            message,
            allowed,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "key": self.key, "problem": self.problem, "message": self.message });
        if let Some(a) = &self.allowed {
            v["allowed"] = a.clone();
        }
        v
    }
}

fn check_value(s: &Setting, v: &Value) -> Option<FieldError> {
    let key = s.key;
    match &s.kind {
        Kind::Bool => (!v.is_boolean()).then(|| {
            FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be true or false"),
                Some(json!([true, false])),
            )
        }),
        Kind::Enum(allowed) => match v.as_str() {
            None => Some(FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be a string"),
                Some(json!(allowed)),
            )),
            Some(x) if !allowed.iter().any(|a| a == x) => Some(FieldError::new(
                key,
                "not_allowed",
                format!("'{x}' is not a valid {key}"),
                Some(json!(allowed)),
            )),
            Some(x) if key == "local_stt_backend" && !available_backends().contains(&x) => {
                Some(FieldError::new(
                    key,
                    "not_allowed",
                    format!("the '{x}' engine is not in this build of Dimmy"),
                    Some(json!(available_backends())),
                ))
            }
            Some(_) => None,
        },
        Kind::Int { min, max } => match v.as_i64() {
            None => Some(FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be a whole number"),
                Some(json!({ "min": min, "max": max })),
            )),
            Some(n) if n < *min || n > *max => Some(FieldError::new(
                key,
                "out_of_range",
                format!("{key} must be between {min} and {max}, got {n}"),
                Some(json!({ "min": min, "max": max })),
            )),
            Some(_) => None,
        },
        Kind::Shortcut => match v.as_str() {
            Some(c) if crate::hotkey::is_valid_combo(c) => None,
            _ => Some(FieldError::new(
                key,
                "not_allowed",
                format!(
                    "{key} must be two modifiers, or one or two modifiers plus a key, joined with +"
                ),
                Some(json!([
                    "ctrl+shift",
                    "win+alt",
                    "cmd+option",
                    "ctrl+shift+space"
                ])),
            )),
        },
        Kind::Device => match v.as_str() {
            Some(d) if !d.trim().is_empty() => None,
            _ => Some(FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be a microphone name as listed by `check`"),
                None,
            )),
        },
    }
}

fn str_of<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(|v| v.as_str())
}

fn cross_field(m: &Map<String, Value>) -> Vec<FieldError> {
    let mut errs = Vec::new();
    if m.get("call_detect_auto_record") == Some(&json!(true))
        && m.get("call_detect_enabled") == Some(&json!(false))
    {
        errs.push(FieldError::new(
            "call_detect_auto_record",
            "conflict",
            "auto-record needs call detection on; Dimmy would switch it off again".into(),
            None,
        ));
    }
    if let (Some(url), Some(model)) = (str_of(m, "api_url"), str_of(m, "api_model")) {
        let vendor = Provider::from_url(url).as_str();
        let owners = providers_of_model("stt", model);
        if !owners.iter().any(|o| o == vendor) {
            errs.push(FieldError::new(
                "api_model",
                "conflict",
                format!("'{model}' is not a speech model of the service at api_url ({vendor})"),
                Some(json!(models_of(vendor, "stt"))),
            ));
        }
    }
    if let (Some(url), Some(model)) = (str_of(m, "llm_api_url"), str_of(m, "llm_api_model")) {
        let synthetic =
            crate::claude_code::is_claude_code_url(url) || crate::codex::is_codex_url(url);
        let vendor = Provider::from_url(url).as_str();
        if !synthetic && !providers_of_model("llm", model).iter().any(|o| o == vendor) {
            errs.push(FieldError::new(
                "llm_api_model",
                "conflict",
                format!("'{model}' is not a model of the service at llm_api_url ({vendor})"),
                Some(json!(models_of(vendor, "llm"))),
            ));
        }
    }
    if str_of(m, "recap_auth_method") == Some("subscription") {
        if let Some(model) = str_of(m, "recap_model_override") {
            if !subscription_capable(model) {
                errs.push(FieldError::new(
                    "recap_model_override",
                    "conflict",
                    format!(
                        "a subscription can only run Claude or OpenAI models, not '{model}'; Dimmy would quietly use an API key instead"
                    ),
                    None,
                ));
            }
        }
    }
    errs
}

fn models_of(provider: &str, task: &str) -> Vec<String> {
    catalog_providers()
        .iter()
        .find(|p| p["id"] == provider)
        .and_then(|p| p["models"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|m| has_task(m, task))
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect()
}

fn subscription_capable(model: &str) -> bool {
    matches!(
        Provider::from_model_id(model).or_else(|| Provider::from_catalog_model_id(model)),
        Some(Provider::Anthropic) | Some(Provider::OpenAI)
    )
}

/// Every problem in `patch`, each with the values that would be accepted.
/// Nothing is mapped, trimmed or defaulted: an agent that sends a wrong
/// value gets told, instead of a setting it did not ask for.
pub fn validate_patch(patch: &Value) -> Result<(), Vec<FieldError>> {
    let Some(m) = patch.as_object() else {
        return Err(vec![FieldError::new(
            "",
            "wrong_type",
            "the settings must be a JSON object of key: value".into(),
            None,
        )]);
    };
    let all = settings();
    let mut errs: Vec<FieldError> = Vec::new();
    for (k, v) in m {
        match all.iter().find(|s| s.key == k) {
            None => errs.push(FieldError::new(
                k,
                "unknown_key",
                format!("'{k}' is not a setting dimmy-setup can change"),
                Some(json!(all.iter().map(|s| s.key).collect::<Vec<_>>())),
            )),
            Some(s) => errs.extend(check_value(s, v)),
        }
    }
    if errs.is_empty() {
        errs.extend(cross_field(m));
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

// ── Profiles ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ProfileAnswers {
    /// meeting | dictation | both
    pub usage: String,
    /// two-letter code, or "auto"
    pub lang: String,
    /// local | cloud
    pub stt: String,
    /// cloud only: a provider id from the catalog
    pub stt_provider: Option<String>,
    /// local only: whisper | parakeet | qwen | whistle
    pub stt_backend: Option<String>,
    /// claude-sub | codex-sub | api | local | off
    pub recap: String,
    pub recap_model: Option<String>,
}

pub const CLAUDE_SUB_DEFAULT_MODEL_TIER: &str = "top";
/// The one model a ChatGPT plan is known to serve through `codex exec`.
pub const CODEX_SUB_DEFAULT_MODEL: &str = "gpt-5.5";

fn answer_err(flag: &str, message: String, allowed: Option<Value>) -> FieldError {
    FieldError::new(flag, "not_allowed", message, allowed)
}

fn one_of(flag: &str, value: &str, allowed: &[&str]) -> Result<(), FieldError> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(answer_err(
            flag,
            format!("'{value}' is not a valid {flag}"),
            Some(json!(allowed)),
        ))
    }
}

/// The local engine a profile picks when the user did not name one.
pub fn default_local_backend() -> Option<&'static str> {
    let avail = available_backends();
    ["parakeet", "whisper"]
        .into_iter()
        .find(|b| avail.contains(b))
}

/// One consistent set of settings from a few answers. The result always
/// passes `validate_patch`.
pub fn profile_patch(a: &ProfileAnswers) -> Result<Value, Vec<FieldError>> {
    let mut errs: Vec<FieldError> = Vec::new();
    let mut p = Map::new();

    if let Err(e) = one_of("--use", &a.usage, &["meeting", "dictation", "both"]) {
        errs.push(e);
    }
    let lang = if a.lang == "auto" {
        ""
    } else {
        a.lang.as_str()
    };
    if !language_values().iter().any(|l| l == lang) {
        let mut allowed = language_values();
        allowed[0] = "auto".into();
        errs.push(answer_err(
            "--lang",
            format!("'{}' is not a supported language code", a.lang),
            Some(json!(allowed)),
        ));
    }
    p.insert("language".into(), json!(lang));

    match a.stt.as_str() {
        "local" => {
            if a.stt_provider.is_some() {
                errs.push(answer_err(
                    "--stt-provider",
                    "--stt-provider is for --stt cloud".into(),
                    None,
                ));
            }
            let backend = match (&a.stt_backend, default_local_backend()) {
                (Some(b), _) => Some(b.as_str()),
                (None, Some(b)) => Some(b),
                (None, None) => {
                    errs.push(answer_err(
                        "--stt",
                        "this build of Dimmy has no on-device speech engine".into(),
                        Some(json!(["cloud"])),
                    ));
                    None
                }
            };
            if let Some(b) = backend {
                p.insert("stt_mode".into(), json!("local"));
                p.insert("local_stt_backend".into(), json!(b));
            }
        }
        "cloud" => {
            if a.stt_backend.is_some() {
                errs.push(answer_err(
                    "--stt-backend",
                    "--stt-backend is for --stt local".into(),
                    None,
                ));
            }
            let provider = a.stt_provider.clone().unwrap_or_else(|| "groq".into());
            match (
                stt_url_of(&provider),
                pick_model(&provider, "stt", &["top"]),
            ) {
                (Some(url), Some(model)) => {
                    p.insert("stt_mode".into(), json!("cloud"));
                    p.insert("api_url".into(), json!(url));
                    p.insert("api_model".into(), json!(model));
                }
                _ => errs.push(answer_err(
                    "--stt-provider",
                    format!("'{provider}' has no speech-to-text service"),
                    Some(json!(stt_providers())),
                )),
            }
        }
        other => errs.push(answer_err(
            "--stt",
            format!("'{other}' is not a valid --stt"),
            Some(json!(["local", "cloud"])),
        )),
    }

    let meetings = a.usage != "dictation";
    match a.recap.as_str() {
        "claude-sub" => {
            let model = a
                .recap_model
                .clone()
                .or_else(|| pick_model("anthropic", "recap", &[CLAUDE_SUB_DEFAULT_MODEL_TIER]));
            match model {
                Some(m) if Provider::from_model_id(&m) == Some(Provider::Anthropic) => {
                    p.insert("recap_auth_method".into(), json!("subscription"));
                    p.insert("recap_model_override".into(), json!(m));
                }
                other => errs.push(answer_err(
                    "--recap-model",
                    format!(
                        "a Claude subscription runs Claude models only, not '{}'",
                        other.unwrap_or_default()
                    ),
                    Some(json!(models_of("anthropic", "recap"))),
                )),
            }
        }
        "codex-sub" => {
            let m = a
                .recap_model
                .clone()
                .unwrap_or_else(|| CODEX_SUB_DEFAULT_MODEL.into());
            if Provider::from_model_id(&m) == Some(Provider::OpenAI) {
                p.insert("recap_auth_method".into(), json!("subscription"));
                p.insert("recap_model_override".into(), json!(m));
            } else {
                errs.push(answer_err(
                    "--recap-model",
                    format!("a ChatGPT subscription runs OpenAI models only, not '{m}'"),
                    Some(json!([CODEX_SUB_DEFAULT_MODEL])),
                ));
            }
        }
        "api" => {
            p.insert("recap_auth_method".into(), json!("api_key"));
            p.insert(
                "recap_model_override".into(),
                json!(a.recap_model.clone().unwrap_or_default()),
            );
        }
        "local" => {
            let file = a
                .recap_model
                .clone()
                .unwrap_or_else(|| crate::local_llm::DEFAULT_LLM_MODEL.into());
            p.insert("recap_auth_method".into(), json!(""));
            p.insert(
                "recap_model_override".into(),
                json!(format!("local:{file}")),
            );
        }
        "off" => {
            if a.recap_model.is_some() {
                errs.push(answer_err(
                    "--recap-model",
                    "--recap off takes no model".into(),
                    None,
                ));
            }
            p.insert("meeting_generate_recap".into(), json!(false));
            p.insert("auto_recap_threshold_secs".into(), json!(0));
        }
        other => errs.push(answer_err(
            "--recap",
            format!("'{other}' is not a valid --recap"),
            Some(json!(["claude-sub", "codex-sub", "api", "local", "off"])),
        )),
    }
    if a.recap != "off" {
        p.insert("meeting_generate_recap".into(), json!(meetings));
    }
    p.insert("call_detect_enabled".into(), json!(meetings));
    if !meetings {
        p.insert("call_detect_auto_record".into(), json!(false));
    }

    if !errs.is_empty() {
        return Err(errs);
    }
    let patch = Value::Object(p);
    validate_patch(&patch)?;
    Ok(patch)
}

// ── System check ────────────────────────────────────────────────────

/// Free bytes on the volume holding `path` (or its nearest existing
/// ancestor, since a fresh install's config dir may not exist yet).
pub fn free_disk_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    platform::free_disk_bytes(existing)
}

pub fn total_ram_bytes() -> Option<u64> {
    platform::total_ram_bytes().filter(|n| *n > 0)
}

pub fn os_version() -> Option<String> {
    platform::os_version().filter(|s| !s.trim().is_empty())
}

#[cfg(target_os = "windows")]
mod platform {
    use std::path::Path;

    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        csd_version: [u16; 128],
    }

    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
        fn GlobalMemoryStatusEx(buf: *mut MemoryStatusEx) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }

    pub fn free_disk_bytes(path: &Path) -> Option<u64> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }

    pub fn total_ram_bytes() -> Option<u64> {
        let mut st = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        let ok = unsafe { GlobalMemoryStatusEx(&mut st) };
        (ok != 0).then_some(st.total_phys)
    }

    pub fn os_version() -> Option<String> {
        let mut info = OsVersionInfoW {
            size: std::mem::size_of::<OsVersionInfoW>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform_id: 0,
            csd_version: [0; 128],
        };
        let status = unsafe { RtlGetVersion(&mut info) };
        (status == 0).then(|| format!("{}.{}.{}", info.major, info.minor, info.build))
    }
}

#[cfg(unix)]
mod platform {
    use std::path::Path;

    pub fn free_disk_bytes(path: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
        (rc == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
    }

    #[cfg(target_os = "macos")]
    pub fn total_ram_bytes() -> Option<u64> {
        let mut size: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = c"hw.memsize";
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut size as *mut u64 as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0).then_some(size)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn total_ram_bytes() -> Option<u64> {
        let mut info: libc::sysinfo = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::sysinfo(&mut info) };
        (rc == 0).then(|| info.totalram as u64 * info.mem_unit as u64)
    }

    #[cfg(target_os = "macos")]
    pub fn os_version() -> Option<String> {
        let out = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn os_version() -> Option<String> {
        let text = std::fs::read_to_string("/etc/os-release").ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
    }
}

#[cfg(not(any(target_os = "windows", unix)))]
mod platform {
    use std::path::Path;
    pub fn free_disk_bytes(_: &Path) -> Option<u64> {
        None
    }
    pub fn total_ram_bytes() -> Option<u64> {
        None
    }
    pub fn os_version() -> Option<String> {
        None
    }
}

/// What `recommended_profile` decides from.
#[derive(Debug, Clone)]
pub struct MachineFacts {
    pub fitness: LocalFitness,
    pub apple_silicon: bool,
    pub ram_mb: Option<u64>,
    pub models_disk_free_mb: Option<u64>,
    pub claude_ready: bool,
    pub codex_ready: bool,
    pub local_backend: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub stt: &'static str,
    pub stt_backend: Option<&'static str>,
    pub recap: &'static str,
    pub reasons: Vec<String>,
}

impl Recommendation {
    pub fn to_json(&self) -> Value {
        let mut args = vec!["--stt".to_string(), self.stt.to_string()];
        if let Some(b) = self.stt_backend {
            args.extend(["--stt-backend".into(), b.into()]);
        }
        args.extend(["--recap".into(), self.recap.into()]);
        json!({
            "stt": self.stt,
            "stt_backend": self.stt_backend,
            "recap": self.recap,
            "reasons": self.reasons,
            "profile_args": args.join(" "),
        })
    }
}

/// Room the largest default download (Parakeet, 2.5 GB) needs, with margin.
const LOCAL_STT_DISK_MB: u64 = 4 * 1024;
const LOCAL_STT_RAM_MB: u64 = 8 * 1024;

/// A starting point, not a verdict: the user is asked anyway. Local only
/// when the machine is known to fit it; anything unknown leans to cloud.
pub fn recommended_profile(f: &MachineFacts) -> Recommendation {
    let mut reasons = Vec::new();
    let gpu_ok = f.apple_silicon || f.fitness == LocalFitness::Good;
    let ram_ok = f.ram_mb.is_some_and(|r| r >= LOCAL_STT_RAM_MB);
    let disk_ok = f
        .models_disk_free_mb
        .is_some_and(|d| d >= LOCAL_STT_DISK_MB);
    let local = match f.local_backend {
        None => {
            reasons.push("this build has no on-device speech engine".into());
            false
        }
        Some(_) if !gpu_ok => {
            reasons.push(format!(
                "graphics are {} for on-device models",
                match f.fitness {
                    LocalFitness::Tight => "tight",
                    LocalFitness::Poor => "too weak",
                    _ => "unknown",
                }
            ));
            false
        }
        Some(_) if !ram_ok => {
            reasons.push("less than 8 GB of memory, or unknown".into());
            false
        }
        Some(_) if !disk_ok => {
            reasons.push("less than 4 GB free for the speech model, or unknown".into());
            false
        }
        Some(_) => {
            reasons.push("the machine can run speech recognition on its own".into());
            true
        }
    };
    let recap = if f.claude_ready {
        reasons.push("Claude Code is installed and signed in".into());
        "claude-sub"
    } else if f.codex_ready {
        reasons.push("Codex is installed and signed in".into());
        "codex-sub"
    } else {
        reasons.push("no signed-in subscription CLI, so meeting summaries need a key".into());
        "api"
    };
    let rec = Recommendation {
        stt: if local { "local" } else { "cloud" },
        stt_backend: if local { f.local_backend } else { None },
        recap,
        reasons,
    };
    assert!(rec.stt == "cloud" || rec.stt_backend.is_some());
    rec
}

// ── Diagnostics ─────────────────────────────────────────────────────

const REDACTED: &str = "[REDACTED]";

fn is_email(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric());
    let Some((user, domain)) = w.split_once('@') else {
        return false;
    };
    !user.is_empty() && domain.contains('.') && !domain.starts_with('.')
}

/// A long run of key-alphabet characters: API keys without a known prefix
/// (Gemini `AIza…`, Deepgram hex) look like this. Hashes do too, and lose
/// nothing a reader of the log needs.
fn is_opaque_token(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric());
    w.len() >= 32
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && w.chars().any(|c| c.is_ascii_digit())
}

/// One log line with secrets, user directory names and emails removed.
pub fn redact_line(line: &str) -> String {
    let scrubbed = crate::telemetry::sanitize::scrub_user_paths(line);
    let mut out: Vec<String> = Vec::new();
    let mut prev_bearer = false;
    for word in scrubbed.split(' ') {
        let redacted = if prev_bearer
            || crate::telemetry::sanitize::looks_like_secret(word)
            || is_opaque_token(word)
        {
            REDACTED.to_string()
        } else if is_email(word) {
            "[EMAIL]".to_string()
        } else {
            word.to_string()
        };
        prev_bearer = word.eq_ignore_ascii_case("bearer");
        out.push(redacted);
    }
    let result = out.join(" ");
    assert!(
        !crate::telemetry::sanitize::looks_like_secret(&result)
            || result.to_ascii_lowercase().contains("bearer "),
        "redact_line left a secret-shaped token"
    );
    result
}

/// Known failure shapes in `dimmy.log`, mapped to a category an agent can
/// act on. Ordered: the first match wins.
const CATEGORIES: &[(&str, &[&str])] = &[
    ("claude_cli_failed", &["[claudecode] non-zero exit"]),
    ("codex_cli_failed", &["[codex] non-zero exit"]),
    ("not_signed_in", &["not logged in", "not signed in"]),
    (
        "no_key",
        &["missing recap url or key", "no api key", "has_key=false"],
    ),
    (
        "auth_failed",
        &[
            "http 401",
            "http 403",
            "status 401",
            "status 403",
            "unauthorized",
        ],
    ),
    ("rate_limited", &["http 429", "status 429", "rate limit"]),
    (
        "model_not_supported",
        &[
            "model_not_found",
            "not_found_error",
            "unrecognized_model",
            "model is not supported",
            "model_decommissioned",
        ],
    ),
    (
        "gpu_out_of_memory",
        &[
            "failed to allocate",
            "erroroutofdevicememory",
            "out of device memory",
            "out of memory",
        ],
    ),
    (
        "model_missing",
        &[
            "model not on disk",
            "model file not found",
            "has no model on disk",
        ],
    ),
];

pub fn categorize_line(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    CATEGORIES
        .iter()
        .find(|(_, needles)| needles.iter().any(|n| lower.contains(n)))
        .map(|(cat, _)| *cat)
}

/// The last `lines` lines of `log`, redacted, plus how often each known
/// failure category appears in the whole of it.
pub fn diag_log(log: &str, lines: usize) -> (Vec<String>, BTreeMap<&'static str, usize>) {
    assert!(lines > 0, "diag_log needs at least one line");
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for l in log.lines() {
        if let Some(c) = categorize_line(l) {
            *counts.entry(c).or_default() += 1;
        }
    }
    let all: Vec<&str> = log.lines().collect();
    let tail: Vec<String> = all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|l| redact_line(l))
        .collect();
    assert!(tail.len() <= lines);
    (tail, counts)
}

/// What an agent should do about a `dimmy_llm_call_raw` return code.
pub fn llm_rc_error(rc: i32) -> (&'static str, &'static str) {
    assert!(rc < 0, "llm_rc_error is for failures");
    match rc {
        -2 => (
            "no_key",
            "Open Dimmy, Settings > Providers, and paste the key there (never through the agent).",
        ),
        -4 => (
            "model_missing",
            "Download the local model first: dimmy-setup models download <file>.",
        ),
        -5 => (
            "model_not_supported",
            "Pick a model this provider serves: dimmy-setup schema lists them.",
        ),
        -6 => (
            "auth_failed",
            "The saved key was refused. Replace it in Dimmy, Settings > Providers.",
        ),
        -7 => ("rate_limited", "Wait a minute and try again."),
        -8 => ("network", "Check the internet connection and try again."),
        -9 | -10 => ("too_long", "Use a model with a larger context window."),
        -11 => ("refused", "The model declined the request."),
        -12 => (
            "gpu_out_of_memory",
            "Close other GPU-heavy apps or pick a smaller local model.",
        ),
        _ => ("failed", "Run dimmy-setup diag for the log."),
    }
}

// ── App-running lock ────────────────────────────────────────────────

pub const APP_LOCK_FILE: &str = ".app.lock";

static APP_LOCK: OnceLock<std::fs::File> = OnceLock::new();

pub fn app_lock_path(dir: &Path) -> PathBuf {
    dir.join(APP_LOCK_FILE)
}

/// Take the exclusive lock on `<dir>/.app.lock`. `Ok(None)` when another
/// process (or another handle in this one) holds it.
pub fn try_lock_dir(dir: &Path) -> Result<Option<std::fs::File>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create config dir: {e}"))?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(app_lock_path(dir))
        .map_err(|e| format!("open lock file: {e}"))?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("lock: {e}")),
    }
}

/// Is the app running against `dir`? Probes by taking and dropping the lock.
pub fn app_running(dir: &Path) -> Result<bool, String> {
    if !dir.exists() {
        return Ok(false);
    }
    Ok(try_lock_dir(dir)?.is_none())
}

/// Held by the app for its lifetime, from `dimmy_init`.
pub fn hold_app_lock() -> Result<(), String> {
    if APP_LOCK.get().is_some() {
        return Ok(());
    }
    let dir = crate::config_dir_path().ok_or("no config dir")?;
    match try_lock_dir(&dir)? {
        Some(f) => {
            let _ = APP_LOCK.set(f);
            Ok(())
        }
        None => Err("already held by another process".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_key_is_applied_by_set_config_json() {
        let ffi = include_str!("ffi.rs");
        for s in settings() {
            let needle = format!("v[\"{}\"]", s.key);
            assert!(
                ffi.contains(&needle),
                "dimmy_set_config_json does not apply schema key '{}'",
                s.key
            );
        }
    }

    #[test]
    fn every_schema_key_is_returned_by_get_config_json() {
        let ffi = include_str!("ffi.rs");
        for s in settings() {
            let inline = format!("\"{}\":", s.key);
            let mutated = format!("json[\"{}\"]", s.key);
            assert!(
                ffi.contains(&inline) || ffi.contains(&mutated),
                "dimmy_get_config_json does not return schema key '{}'",
                s.key
            );
        }
    }

    #[test]
    fn every_schema_key_has_a_default_and_it_validates() {
        let d = AppConfig::default();
        for s in settings() {
            let v = config_value(&d, s.key);
            if s.key == "selected_device" {
                assert!(v.is_null());
                continue;
            }
            if s.key == "local_stt_backend" && !available_backends().contains(&v.as_str().unwrap())
            {
                continue;
            }
            let patch = json!({ s.key: v });
            assert_eq!(validate_patch(&patch), Ok(()), "default of {}", s.key);
        }
    }

    #[test]
    fn schema_keys_are_unique_and_documented() {
        let all = settings();
        for (i, s) in all.iter().enumerate() {
            assert!(!s.help.is_empty());
            assert!(s.help.ends_with('.'), "{} help is a sentence", s.key);
            assert!(!s.help.contains('\u{2014}'), "no em-dash in {}", s.key);
            assert!(all[i + 1..].iter().all(|o| o.key != s.key));
        }
        let sc = schema();
        assert_eq!(sc["settings"].as_array().unwrap().len(), all.len());
    }

    #[test]
    fn schema_never_offers_a_secret() {
        for s in settings() {
            assert!(
                !s.key.ends_with("_key") && !s.key.contains("token"),
                "{} looks like a secret",
                s.key
            );
        }
    }

    #[test]
    fn unknown_key_is_rejected_with_the_known_keys() {
        let e = validate_patch(&json!({ "api_key": "sk-x" })).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].problem, "unknown_key");
        assert!(e[0]
            .allowed
            .as_ref()
            .unwrap()
            .to_string()
            .contains("stt_mode"));
    }

    #[test]
    fn wrong_type_is_rejected() {
        let e = validate_patch(&json!({ "llm_enabled": "yes" })).unwrap_err();
        assert_eq!(e[0].problem, "wrong_type");
        let e = validate_patch(&json!({ "stt_mode": 1 })).unwrap_err();
        assert_eq!(e[0].problem, "wrong_type");
        let e = validate_patch(&json!({ "auto_recap_threshold_secs": "60" })).unwrap_err();
        assert_eq!(e[0].problem, "wrong_type");
    }

    #[test]
    fn value_outside_the_set_is_rejected_not_mapped() {
        let e = validate_patch(&json!({ "stt_mode": "locall" })).unwrap_err();
        assert_eq!(e[0].problem, "not_allowed");
        assert_eq!(e[0].allowed, Some(json!(["local", "cloud"])));
        let e = validate_patch(&json!({ "llm_style": "pirate" })).unwrap_err();
        assert!(e[0]
            .allowed
            .as_ref()
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!("correct")));
    }

    #[test]
    fn number_out_of_range_is_rejected() {
        let e = validate_patch(&json!({ "auto_recap_threshold_secs": -1 })).unwrap_err();
        assert_eq!(e[0].problem, "out_of_range");
        let e = validate_patch(&json!({ "auto_recap_threshold_secs": 3601 })).unwrap_err();
        assert_eq!(e[0].problem, "out_of_range");
        assert_eq!(
            validate_patch(&json!({ "auto_recap_threshold_secs": 3600 })),
            Ok(())
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let e =
            validate_patch(&json!({ "nope": 1, "stt_mode": "x", "llm_enabled": 3 })).unwrap_err();
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn non_object_patch_is_rejected() {
        assert!(validate_patch(&json!([1, 2])).is_err());
    }

    #[test]
    fn bad_shortcut_is_rejected() {
        assert!(validate_patch(&json!({ "shortcut": "space" })).is_err());
        assert!(validate_patch(&json!({ "shortcut": "ctrl+shift+space" })).is_ok());
    }

    #[test]
    fn empty_device_is_rejected() {
        assert!(validate_patch(&json!({ "selected_device": " " })).is_err());
        assert!(validate_patch(&json!({ "selected_device": "Mic (USB)" })).is_ok());
    }

    #[test]
    fn auto_record_without_detection_conflicts() {
        let e = validate_patch(
            &json!({ "call_detect_auto_record": true, "call_detect_enabled": false }),
        )
        .unwrap_err();
        assert_eq!(e[0].problem, "conflict");
    }

    #[test]
    fn stt_model_must_belong_to_the_stt_service() {
        let groq = stt_url_of("groq").unwrap();
        assert!(validate_patch(&json!({ "api_url": groq, "api_model": "nova-3" })).is_err());
        assert!(
            validate_patch(&json!({ "api_url": groq, "api_model": "whisper-large-v3-turbo" }))
                .is_ok()
        );
    }

    #[test]
    fn subscription_recap_rejects_a_model_it_cannot_run() {
        let e = validate_patch(&json!({
            "recap_auth_method": "subscription",
            "recap_model_override": "gemini-2.5-pro",
        }))
        .unwrap_err();
        assert_eq!(e[0].problem, "conflict");
    }

    #[test]
    fn unavailable_backend_is_rejected() {
        for b in ["whisper", "parakeet", "qwen", "whistle"] {
            let r = validate_patch(&json!({ "local_stt_backend": b }));
            assert_eq!(r.is_ok(), available_backends().contains(&b), "{b}");
        }
    }

    fn answers(usage: &str, stt: &str, recap: &str) -> ProfileAnswers {
        ProfileAnswers {
            usage: usage.into(),
            lang: "it".into(),
            stt: stt.into(),
            recap: recap.into(),
            ..Default::default()
        }
    }

    #[test]
    fn every_profile_combination_validates() {
        for usage in ["meeting", "dictation", "both"] {
            for stt in ["local", "cloud"] {
                for recap in ["claude-sub", "codex-sub", "api", "local", "off"] {
                    let a = answers(usage, stt, recap);
                    if stt == "local" && default_local_backend().is_none() {
                        assert!(profile_patch(&a).is_err());
                        continue;
                    }
                    let p = profile_patch(&a)
                        .unwrap_or_else(|e| panic!("{usage}/{stt}/{recap}: {e:?}"));
                    assert_eq!(validate_patch(&p), Ok(()));
                }
            }
        }
    }

    #[test]
    fn claude_sub_profile_uses_the_subscription_and_an_anthropic_model() {
        let p = profile_patch(&answers("meeting", "cloud", "claude-sub")).unwrap();
        assert_eq!(p["recap_auth_method"], "subscription");
        let m = p["recap_model_override"].as_str().unwrap();
        assert_eq!(Provider::from_model_id(m), Some(Provider::Anthropic));
        assert_eq!(p["meeting_generate_recap"], true);
        assert_eq!(p["call_detect_enabled"], true);
    }

    #[test]
    fn dictation_profile_turns_call_detection_off() {
        let p = profile_patch(&answers("dictation", "cloud", "api")).unwrap();
        assert_eq!(p["call_detect_enabled"], false);
        assert_eq!(p["call_detect_auto_record"], false);
        assert_eq!(p["meeting_generate_recap"], false);
    }

    #[test]
    fn recap_off_turns_both_recaps_off() {
        let p = profile_patch(&answers("both", "cloud", "off")).unwrap();
        assert_eq!(p["meeting_generate_recap"], false);
        assert_eq!(p["auto_recap_threshold_secs"], 0);
    }

    #[test]
    fn cloud_profile_defaults_to_groq_and_names_its_model() {
        let p = profile_patch(&answers("both", "cloud", "api")).unwrap();
        assert_eq!(p["stt_mode"], "cloud");
        assert_eq!(p["api_url"], json!(stt_url_of("groq").unwrap()));
        assert!(p["api_model"].as_str().is_some());
    }

    #[test]
    fn local_profile_prefers_parakeet_when_built_in() {
        if let Ok(p) = profile_patch(&answers("both", "local", "api")) {
            assert_eq!(p["stt_mode"], "local");
            assert_eq!(
                p["local_stt_backend"],
                json!(default_local_backend().unwrap())
            );
            if available_backends().contains(&"parakeet") {
                assert_eq!(p["local_stt_backend"], "parakeet");
            }
        }
    }

    #[test]
    fn auto_language_means_empty() {
        let mut a = answers("both", "cloud", "api");
        a.lang = "auto".into();
        assert_eq!(profile_patch(&a).unwrap()["language"], "");
    }

    #[test]
    fn bad_profile_answers_are_errors_not_guesses() {
        let mut a = answers("party", "cloud", "api");
        a.lang = "xx".into();
        let e = profile_patch(&a).unwrap_err();
        assert_eq!(e.len(), 2);
        let mut a = answers("both", "cloud", "claude-sub");
        a.recap_model = Some("gpt-5.5".into());
        assert!(profile_patch(&a).is_err());
        let mut a = answers("both", "cloud", "api");
        a.stt_provider = Some("anthropic".into());
        assert!(profile_patch(&a).is_err());
    }

    fn facts() -> MachineFacts {
        MachineFacts {
            fitness: LocalFitness::Good,
            apple_silicon: false,
            ram_mb: Some(16 * 1024),
            models_disk_free_mb: Some(50 * 1024),
            claude_ready: false,
            codex_ready: false,
            local_backend: Some("parakeet"),
        }
    }

    #[test]
    fn good_gpu_recommends_local_parakeet() {
        let r = recommended_profile(&facts());
        assert_eq!((r.stt, r.stt_backend), ("local", Some("parakeet")));
        assert_eq!(r.recap, "api");
    }

    #[test]
    fn apple_silicon_recommends_local_even_without_a_fitness_verdict() {
        let mut f = facts();
        f.fitness = LocalFitness::Unknown;
        f.apple_silicon = true;
        assert_eq!(recommended_profile(&f).stt, "local");
    }

    #[test]
    fn weak_or_unknown_machine_recommends_cloud() {
        for fit in [
            LocalFitness::Poor,
            LocalFitness::Tight,
            LocalFitness::Unknown,
        ] {
            let mut f = facts();
            f.fitness = fit;
            assert_eq!(recommended_profile(&f).stt, "cloud");
        }
        let mut f = facts();
        f.ram_mb = None;
        assert_eq!(recommended_profile(&f).stt, "cloud");
        let mut f = facts();
        f.models_disk_free_mb = Some(1024);
        assert_eq!(recommended_profile(&f).stt, "cloud");
        let mut f = facts();
        f.local_backend = None;
        assert_eq!(recommended_profile(&f).stt, "cloud");
    }

    #[test]
    fn signed_in_cli_picks_the_subscription_recap() {
        let mut f = facts();
        f.codex_ready = true;
        assert_eq!(recommended_profile(&f).recap, "codex-sub");
        f.claude_ready = true;
        assert_eq!(recommended_profile(&f).recap, "claude-sub");
    }

    #[test]
    fn recommendation_args_parse_as_a_profile() {
        let j = recommended_profile(&facts()).to_json();
        assert_eq!(
            j["profile_args"],
            "--stt local --stt-backend parakeet --recap api"
        );
    }

    #[test]
    fn system_facts_are_plausible_here() {
        let ram = total_ram_bytes().expect("ram");
        assert!(ram > 256 * 1024 * 1024);
        let free = free_disk_bytes(&std::env::temp_dir().join("does/not/exist")).expect("disk");
        assert!(free > 0);
        assert!(os_version().is_some());
    }

    #[test]
    fn redaction_strips_keys_paths_and_emails() {
        let line = r"[x] key sk-ant-api03-abcdefgh at C:\Users\konrad\AppData\x for me@example.com Bearer abc.def AIzaSyA1234567890abcdefghijklmnopqrstu";
        let r = redact_line(line);
        assert!(!r.contains("sk-ant"), "{r}");
        assert!(!r.contains("konrad"), "{r}");
        assert!(!r.contains("me@example.com"), "{r}");
        assert!(!r.contains("abc.def"), "{r}");
        assert!(!r.contains("AIzaSy"), "{r}");
        assert!(r.contains("<USER>"));
        assert!(r.contains("[EMAIL]"));
    }

    #[test]
    fn redaction_keeps_ordinary_lines() {
        let line =
            "[LlmRaw] dispatch url=https://api.groq.com model=openai/gpt-oss-120b auth=api_key";
        assert_eq!(redact_line(line), line);
    }

    #[test]
    fn known_errors_are_categorised() {
        let cases = [
            (
                "[ClaudeCode] non-zero exit code=Some(1) stderr=x",
                "claude_cli_failed",
            ),
            ("[Codex] non-zero exit code=Some(1)", "codex_cli_failed"),
            ("Claude Code not logged in", "not_signed_in"),
            ("[LlmRaw] missing recap URL or key", "no_key"),
            ("[LlmRaw] failed: HTTP 401", "auth_failed"),
            ("[LlmRaw] failed: HTTP 429", "rate_limited"),
            ("[claude-code:unrecognized_model] x", "model_not_supported"),
            (
                "ggml_vulkan: failed to allocate Vulkan1 buffer",
                "gpu_out_of_memory",
            ),
        ];
        for (line, cat) in cases {
            assert_eq!(categorize_line(line), Some(cat), "{line}");
        }
        assert_eq!(categorize_line("FFI init complete"), None);
    }

    #[test]
    fn diag_log_tails_and_counts() {
        let log = "a\n[LlmRaw] failed: HTTP 429\nb\n[LlmRaw] failed: HTTP 429\nc sk-abcdefghijk\n";
        let (tail, counts) = diag_log(log, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1], "c [REDACTED]");
        assert_eq!(counts.get("rate_limited"), Some(&2));
    }

    #[test]
    fn every_llm_rc_has_advice() {
        for rc in -13..=-1 {
            let (code, fix) = llm_rc_error(rc);
            assert!(!code.is_empty() && !fix.is_empty());
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dimmy-setup-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let d = temp_dir("lock");
        assert!(!app_running(&d).unwrap(), "no dir means not running");
        let held = try_lock_dir(&d).unwrap().expect("first lock");
        assert!(app_running(&d).unwrap());
        assert!(try_lock_dir(&d).unwrap().is_none());
        drop(held);
        assert!(!app_running(&d).unwrap());
        assert!(app_lock_path(&d).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

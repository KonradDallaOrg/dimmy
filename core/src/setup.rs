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
    Text {
        max_len: usize,
    },
    Number {
        min: f64,
        max: f64,
    },
    TextList,
    /// The app's per-app rules list, shaped like `app_rules::AppRule`.
    AppRules,
    /// A shortcut the app binds itself (`setup_prefs`); `""` is off when
    /// `may_be_off`.
    AppHotkey {
        needs_key: bool,
        may_be_off: bool,
    },
}

/// `AppConfig` fields that are not the user's choices: connection state the
/// sign-in flows write, caches, counters, window positions, and knobs the
/// app migrates or forces. `every_user_setting_in_config_is_in_the_schema`
/// makes a new field land either here or in `settings()`.
pub const NOT_SETTINGS: &[&str] = &[
    "use_keyring",
    "audio_source",
    "window_anchor_right",
    "window_anchor_bottom",
    "stats_total_words",
    "stats_total_speaking_secs",
    "meeting_chunk_secs",
    "loopback_gain",
    "terms_accepted_version",
    "notion_target_id",
    "notion_target_kind",
    "notion_target_title",
    "confluence_site",
    "confluence_email",
    "confluence_space_id",
    "confluence_space_key",
    "confluence_space_name",
    "confluence_parent_id",
];

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

/// Every user setting in `config.json`. Secrets never appear here, and the
/// connection state of Notion and Confluence is written by their own sign-in
/// flows (see `NOT_SETTINGS`).
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
    let mut v = vec![
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
            key: "analytics_consent",
            kind: Kind::Bool,
            help: "Send anonymous usage statistics (which features are used, never audio, text, prompts, file names or keys). Ask the person and write their answer; never choose for them. Unset means the app asks at launch.",
        },
        Setting {
            key: "crash_reports_consent",
            kind: Kind::Bool,
            help: "Send crash reports (the technical error, with user names removed from paths). Ask the person and write their answer; never choose for them. Unset means the app asks at launch.",
        },
        Setting {
            key: "keep_in_clipboard",
            kind: Kind::Bool,
            help: "Leave the dictated text on the clipboard after it is pasted.",
        },
        Setting {
            key: "preprocessing_enabled",
            kind: Kind::Bool,
            help: "Clean up the microphone audio before local transcription (voice detection and level). Off is usually more accurate.",
        },
        Setting {
            key: "live_captions_enabled",
            kind: Kind::Bool,
            help: "Show the words in a small caption window while you speak.",
        },
        Setting {
            key: "chunk_streaming_enabled",
            kind: Kind::Bool,
            help: "Transcribe long dictations in pieces while you speak, so the text is ready sooner when you stop.",
        },
        Setting {
            key: "prompt",
            kind: Kind::Text { max_len: 4000 },
            help: "Words or a short context sentence given to the speech engine to help it spell names and terms.",
        },
        Setting {
            key: "user_dict",
            kind: Kind::TextList,
            help: "The personal dictionary: names and terms the speech engine should spell exactly like this.",
        },
        Setting {
            key: "llm_custom_prompt",
            kind: Kind::Text { max_len: 8000 },
            help: "The instructions used when the text style is custom.",
        },
        Setting {
            key: "llm_use_same_key",
            kind: Kind::Bool,
            help: "Use the speech service key for text improvement too, when it is the same company.",
        },
        Setting {
            key: "recap_use_same_key",
            kind: Kind::Bool,
            help: "Use the text improvement key for meeting recaps too.",
        },
        Setting {
            key: "llm_log_enabled",
            kind: Kind::Bool,
            help: "Write the text improvement requests and answers to the log, for troubleshooting.",
        },
        Setting {
            key: "gemini_cli_enabled",
            kind: Kind::Bool,
            help: "Offer the Gemini command line tool as a way to sign in for text improvement.",
        },
        Setting {
            key: "telegram_enabled",
            kind: Kind::Bool,
            help: "Receive voice messages sent to your Telegram inbox and transcribe them.",
        },
        Setting {
            key: "telegram_auto_process",
            kind: Kind::Bool,
            help: "Transcribe Telegram voice messages as soon as they arrive.",
        },
        Setting {
            key: "audio_debug_enabled",
            kind: Kind::Bool,
            help: "Keep a copy of each recording's audio for troubleshooting.",
        },
        Setting {
            key: "ggml_debug_logging",
            kind: Kind::Bool,
            help: "Write the local engines' detailed messages to the log, for troubleshooting.",
        },
        Setting {
            key: "call_detect_excluded_apps",
            kind: Kind::TextList,
            help: "Apps whose calls are never suggested for recording.",
        },
        Setting {
            key: "call_detect_cooldown_secs",
            kind: Kind::Int { min: 0, max: 86400 },
            help: "Seconds to wait before suggesting recording again in the same app after a call.",
        },
        Setting {
            key: "call_detect_min_active_secs",
            kind: Kind::Int { min: 0, max: 600 },
            help: "Seconds the microphone must be in use before a call is suggested for recording.",
        },
        Setting {
            key: "call_detect_timeout_cooldown_secs",
            kind: Kind::Int { min: 0, max: 86400 },
            help: "Seconds to wait before suggesting again after a suggestion was ignored.",
        },
        Setting {
            key: "history_audio_max_mb",
            kind: Kind::Int {
                min: 0,
                max: 1_000_000,
            },
            help: "Most megabytes of dictation audio kept in the history; 0 means no limit.",
        },
        Setting {
            key: "border_style",
            kind: Kind::Enum(strs(&[
                "Rainbow", "Blue", "Green", "Purple", "Orange", "None",
            ])),
            help: "Colour of the border around the recording pill.",
        },
        Setting {
            key: "waveform_style",
            kind: Kind::Enum(strs(&["Bars", "Bars Center", "Bars Round", "Line", "Dots"])),
            help: "How the recording pill draws your voice.",
        },
        Setting {
            key: "overlay_position",
            kind: Kind::Enum(strs(&[
                "Top Left",
                "Top Center",
                "Top Right",
                "Bottom Left",
                "Bottom Center",
                "Bottom Right",
            ])),
            help: "Where the recording pill sits on the screen.",
        },
        Setting {
            key: "input_gain",
            kind: Kind::Number { min: 0.0, max: 2.0 },
            help: "Microphone volume boost: 1 leaves it as it is, below 1 is quieter, above 1 is louder.",
        },
        Setting {
            key: "meeting_storage_path",
            kind: Kind::Text { max_len: 1024 },
            help: "Folder where meetings are saved; empty means Dimmy's own data folder.",
        },
        Setting {
            key: "notion_auto_send",
            kind: Kind::Bool,
            help: "Send every meeting recap to the linked Notion page on its own. Needs Notion connected in the app.",
        },
        Setting {
            key: "confluence_auto_send",
            kind: Kind::Bool,
            help: "Send every meeting recap to the linked Confluence space on its own. Needs Confluence connected in the app.",
        },
        Setting {
            key: "calendar_context_enabled",
            kind: Kind::Bool,
            help: "Use the title and attendees of the current calendar event in meeting recaps.",
        },
        Setting {
            key: "app_rules",
            kind: Kind::AppRules,
            help: "Per-app text style and translation, as the app's rules list (match_pattern, match_type, llm_style, llm_translate_to).",
        },
    ];
    // The Mac app keeps its dictation shortcut in its own settings
    // (`setup_prefs`), and never reads this one.
    if cfg!(target_os = "macos") {
        v.retain(|s| s.key != "shortcut");
    }
    v
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
        "preprocessing_enabled" => json!(cfg.preprocessing_enabled),
        "live_captions_enabled" => json!(cfg.live_captions_enabled),
        "chunk_streaming_enabled" => json!(cfg.chunk_streaming_enabled),
        "prompt" => json!(cfg.prompt),
        "user_dict" => json!(cfg.user_dict),
        "llm_custom_prompt" => json!(cfg.llm_custom_prompt),
        "llm_use_same_key" => json!(cfg.llm_use_same_key),
        "recap_use_same_key" => json!(cfg.recap_use_same_key),
        "llm_log_enabled" => json!(cfg.llm_log_enabled),
        "gemini_cli_enabled" => json!(cfg.gemini_cli_enabled),
        "telegram_enabled" => json!(cfg.telegram_enabled),
        "telegram_auto_process" => json!(cfg.telegram_auto_process),
        "audio_debug_enabled" => json!(cfg.audio_debug_enabled),
        "ggml_debug_logging" => json!(cfg.ggml_debug_logging),
        "call_detect_excluded_apps" => json!(cfg.call_detect_excluded_apps),
        "call_detect_cooldown_secs" => json!(cfg.call_detect_cooldown_secs),
        "call_detect_min_active_secs" => json!(cfg.call_detect_min_active_secs),
        "call_detect_timeout_cooldown_secs" => json!(cfg.call_detect_timeout_cooldown_secs),
        "history_audio_max_mb" => json!(cfg.history_audio_max_mb),
        "border_style" => json!(cfg.border_style),
        "waveform_style" => json!(cfg.waveform_style),
        "overlay_position" => json!(cfg.overlay_position),
        "input_gain" => json!(cfg.input_gain),
        "meeting_storage_path" => json!(cfg.meeting_storage_path),
        "notion_auto_send" => json!(cfg.notion_auto_send),
        "confluence_auto_send" => json!(cfg.confluence_auto_send),
        "calendar_context_enabled" => json!(cfg.calendar_context_enabled),
        "app_rules" => json!(cfg.app_rules),
        "analytics_consent" => json!(cfg.analytics_consent),
        "crash_reports_consent" => json!(cfg.crash_reports_consent),
        other => panic!("config_value: '{other}' is not a schema key"),
    }
}

pub fn schema() -> Value {
    let defaults = AppConfig::default();
    let mut list: Vec<Value> = settings()
        .iter()
        .map(|s| {
            let mut v = json!({
                "key": s.key,
                "default": config_value(&defaults, s.key),
                "help": s.help,
            });
            describe_kind(&s.kind, &mut v);
            v
        })
        .collect();
    list.extend(crate::setup_prefs::app_prefs().iter().map(|p| {
        let mut v =
            json!({ "key": p.key, "default": p.default, "help": p.help, "stored_by": "app" });
        describe_kind(&p.kind, &mut v);
        v
    }));
    assert_eq!(
        list.len(),
        settings().len() + crate::setup_prefs::app_prefs().len()
    );
    json!({ "settings": list, "local_stt_backends_in_this_build": available_backends() })
}

fn describe_kind(kind: &Kind, v: &mut Value) {
    match kind {
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
        Kind::Text { max_len } => {
            v["type"] = json!("text");
            v["max_len"] = json!(max_len);
        }
        Kind::Number { min, max } => {
            v["type"] = json!("number");
            v["min"] = json!(min);
            v["max"] = json!(max);
        }
        Kind::TextList => v["type"] = json!("text_list"),
        Kind::AppRules => v["type"] = json!("app_rules"),
        Kind::AppHotkey {
            needs_key,
            may_be_off,
        } => {
            v["type"] = json!("shortcut");
            v["needs_key"] = json!(needs_key);
            v["may_be_off"] = json!(may_be_off);
        }
    }
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
    pub(crate) fn new(
        key: &str,
        problem: &'static str,
        message: String,
        allowed: Option<Value>,
    ) -> Self {
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
        Kind::Text { max_len } => match v.as_str() {
            None => Some(FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be a string"),
                None,
            )),
            Some(t) if t.chars().count() > *max_len => Some(FieldError::new(
                key,
                "out_of_range",
                format!("{key} is longer than {max_len} characters"),
                Some(json!({ "max_len": max_len })),
            )),
            Some(_) => None,
        },
        Kind::Number { min, max } => match v.as_f64() {
            None => Some(FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be a number"),
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
        Kind::TextList => match v.as_array() {
            Some(a) if a.iter().all(|x| x.is_string()) => None,
            _ => Some(FieldError::new(
                key,
                "wrong_type",
                format!("{key} must be a JSON list of strings"),
                None,
            )),
        },
        Kind::AppHotkey {
            needs_key,
            may_be_off,
        } => crate::setup_prefs::check_hotkey(key, v, *needs_key, *may_be_off),
        Kind::AppRules => {
            match serde_json::from_value::<Vec<crate::app_rules::AppRule>>(v.clone()) {
                Ok(_) => None,
                Err(e) => Some(FieldError::new(
                    key,
                    "wrong_type",
                    format!("{key} is not a valid rules list: {e}"),
                    None,
                )),
            }
        }
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
    let app = crate::setup_prefs::app_prefs();
    let mut errs: Vec<FieldError> = Vec::new();
    for (k, v) in m {
        let found = all
            .iter()
            .map(|s| (s.key, &s.kind))
            .chain(app.iter().map(|p| (p.key, &p.kind)))
            .find(|(key, _)| key == k);
        match found {
            None => errs.push(FieldError::new(
                k,
                "unknown_key",
                format!("'{k}' is not a setting dimmy-setup can change"),
                Some(json!(all
                    .iter()
                    .map(|s| s.key)
                    .chain(app.iter().map(|p| p.key))
                    .collect::<Vec<_>>())),
            )),
            Some((key, kind)) => errs.extend(check_value(
                &Setting {
                    key,
                    kind: kind.clone(),
                    help: "",
                },
                v,
            )),
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

    // statvfs fields are u32 or u64 depending on the OS.
    #[allow(clippy::unnecessary_cast)]
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
        (rc == 0).then(|| info.totalram * u64::from(info.mem_unit))
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

// ── macOS app bundle ────────────────────────────────────────────────

/// The `.app` bundle the CLI runs from. On macOS the CLI is the app binary
/// itself, `<X>.app/Contents/MacOS/<X>`, and the bundle may be renamed (a
/// test build is "Dimmy Agent.app"), so it is derived, never assumed.
pub fn mac_app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let shaped = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    shaped.then(|| app.to_path_buf())
}

/// What `defaults read` takes for the bundle's Info.plist: the path without
/// the `.plist` extension.
pub fn mac_info_plist_domain(app: &Path) -> PathBuf {
    app.join("Contents").join("Info")
}

/// A reverse-DNS bundle id, and nothing that could break out of the quoted
/// AppleScript string it is put in.
pub fn valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id.contains('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

pub fn mac_quit_script(bundle_id: &str) -> String {
    assert!(
        valid_bundle_id(bundle_id),
        "bundle id must be validated first"
    );
    format!("tell application id \"{bundle_id}\" to quit")
}

// ── check warnings ──────────────────────────────────────────────────

pub fn check_warnings(microphones: &[String]) -> Vec<Value> {
    let mut w = Vec::new();
    if microphones.is_empty() {
        w.push(json!({
            "code": "no_microphone",
            "message": "No microphone found on this computer. Dictation and meetings need one.",
        }));
    }
    w
}

// ── Permissions ─────────────────────────────────────────────────────

pub const PERMISSIONS: [&str; 4] = [
    "microphone",
    "accessibility",
    "input_monitoring",
    "system_audio",
];

/// The two Dimmy cannot work without; the others only matter on some Macs.
pub const REQUIRED_PERMISSIONS: [&str; 2] = ["microphone", "accessibility"];

pub const PERMISSIONS_FILE: &str = "permissions.json";
pub const PERMISSION_GUIDE_TRIGGER: &str = ".show-permission-guide";

/// The System Settings pane (macOS) or Settings page (Windows) for one
/// permission. None where the OS has no such permission.
pub fn permission_settings_url(perm: &str, os: &str) -> Option<&'static str> {
    match (os, perm) {
        ("macos", "microphone") => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        }
        ("macos", "accessibility") => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        ("macos", "input_monitoring") => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        }
        ("macos", "system_audio") => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        }
        ("windows", "microphone") => Some("ms-settings:privacy-microphone"),
        _ => None,
    }
}

fn permission_value(perm: &str, raw: &Value) -> &'static str {
    let allowed: &[&'static str] = match perm {
        "microphone" => &["granted", "denied", "not_determined", "restricted"],
        "accessibility" => &["granted", "denied"],
        _ => &["granted", "denied", "unknown"],
    };
    raw.as_str()
        .and_then(|s| allowed.iter().find(|a| **a == s).copied())
        .unwrap_or("unknown")
}

/// `perm status` on macOS. macOS attributes a permission to the process
/// that is RESPONSIBLE for the caller, which for the CLI is the assistant
/// that launched it, so the CLI cannot ask the system about Dimmy's grants.
/// The app writes what it sees to `permissions.json`; this reads it back.
pub fn permissions_report(file: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Value {
    let unknown = || {
        let mut m = Map::new();
        for p in PERMISSIONS {
            m.insert(p.into(), json!("unknown"));
        }
        Value::Object(m)
    };
    let Some(text) = file else {
        return json!({
            "source": "app",
            "known": false,
            "message": "Dimmy has not run on this Mac yet, so its permissions are unknown.",
            "fix": "dimmy-setup app start",
            "permissions": unknown(),
            "required_granted": false,
        });
    };
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let updated = v["updated_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc));
    let Some(updated) = updated else {
        return json!({
            "source": "app",
            "known": false,
            "message": "The permissions Dimmy saved could not be read.",
            "fix": "dimmy-setup app start",
            "permissions": unknown(),
            "required_granted": false,
        });
    };
    let mut perms = Map::new();
    for p in PERMISSIONS {
        perms.insert(p.into(), json!(permission_value(p, &v[p])));
    }
    let required = REQUIRED_PERMISSIONS
        .iter()
        .all(|p| perms[*p] == json!("granted"));
    let stale = (now - updated).num_seconds().max(0);
    assert!(stale >= 0, "stale_secs is never negative");
    json!({
        "source": "app",
        "known": true,
        "updated_at": updated.to_rfc3339(),
        "stale_secs": stale,
        "permissions": Value::Object(perms),
        "required_granted": required,
    })
}

/// The microphone privacy switches on Windows, as `Value` strings from
/// `...\CapabilityAccessManager\ConsentStore\microphone`: the device-wide
/// one (HKLM), the per-user one (HKCU) and the per-user "let desktop apps
/// use the microphone" one (HKCU `NonPackaged`). A missing value is the
/// Windows default, which is Allow.
pub fn windows_mic_state(
    device: Option<&str>,
    user: Option<&str>,
    desktop_apps: Option<&str>,
) -> &'static str {
    let vals = [device, user, desktop_apps];
    if vals
        .iter()
        .any(|v| v.is_some_and(|s| s.eq_ignore_ascii_case("Deny")))
    {
        return "denied";
    }
    if vals
        .iter()
        .all(|v| v.is_none_or(|s| s.eq_ignore_ascii_case("Allow")))
    {
        return "granted";
    }
    "unknown"
}

// ── last dictation / meeting ────────────────────────────────────────

fn rfc3339(epoch_secs: f64) -> Option<String> {
    if !epoch_secs.is_finite() || epoch_secs <= 0.0 {
        return None;
    }
    chrono::DateTime::from_timestamp(epoch_secs as i64, 0).map(|d| d.to_rfc3339())
}

fn age_secs(epoch_secs: f64, now: f64) -> Option<u64> {
    (epoch_secs.is_finite() && epoch_secs > 0.0).then(|| (now - epoch_secs).max(0.0) as u64)
}

/// The latest dictation, without its text: an agent needs to know that
/// something was transcribed and how much, not what was said.
pub fn dictation_summary(t: &crate::history::Transcript, now: f64) -> Value {
    let text = t.enhanced_text.as_deref().unwrap_or(&t.text);
    json!({
        "at": rfc3339(t.timestamp),
        "age_secs": age_secs(t.timestamp, now),
        "duration_secs": t.duration,
        "chars": t.text.chars().count(),
        "words": t.word_count,
        "language": t.language,
        "enhanced": t.enhanced_text.is_some(),
        "llm_style": t.llm_style,
        "ok": !text.trim().is_empty(),
    })
}

fn read_meta(dir: &Path) -> Value {
    std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// Newest meeting under `meetings`, ordered by `started_at` like the app's
/// list (folder mtime when a meeting has none).
pub fn latest_meeting_dir(meetings: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(meetings).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .map(|p| {
            let started = read_meta(&p)["started_at"].as_f64().unwrap_or_else(|| {
                std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0.0, |d| d.as_secs_f64())
            });
            (started, p)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
}

/// Warnings and errors the log holds for one meeting: the lines after its
/// `started id=` line, up to the next meeting's.
pub fn meeting_log_notes(log: &str, id: &str) -> Vec<String> {
    assert!(!id.is_empty(), "meeting id must not be empty");
    let start_tag = format!("started id={id}");
    let Some(start) = log.lines().position(|l| l.contains(&start_tag)) else {
        return Vec::new();
    };
    let mut notes: Vec<String> = log
        .lines()
        .skip(start + 1)
        .take_while(|l| !l.contains("[Meeting] started id="))
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains("warn") || lower.contains("error") || lower.contains("failed")
        })
        .map(redact_line)
        .collect();
    let keep = notes.len().saturating_sub(10);
    notes.drain(..keep);
    assert!(notes.len() <= 10);
    notes
}

/// What an agent needs to tell whether a test meeting really worked: how
/// long it ran, whether anything was transcribed, how big each audio track
/// is (a mic track of a few KB is silence), and whether a recap exists.
pub fn meeting_summary(dir: &Path, now: f64, log: &str) -> Value {
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(!id.is_empty(), "a meeting dir has a name");
    let meta = read_meta(dir);
    let started = meta["started_at"].as_f64().unwrap_or(0.0);
    let ended = meta["ended_at"].as_f64().unwrap_or(0.0);
    let transcript = std::fs::read_to_string(dir.join("transcripts.txt")).unwrap_or_default();
    let transcript_lines = transcript
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.contains("[paused]"))
        .count();
    let mut tracks = Map::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut files: Vec<(String, u64)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let audio =
                    name.starts_with("audio") && (name.ends_with(".ogg") || name.ends_with(".wav"));
                let len = e.metadata().ok().filter(|m| m.is_file())?.len();
                audio.then_some((name, len))
            })
            .collect();
        files.sort();
        for (name, len) in files {
            tracks.insert(name, json!(len));
        }
    }
    let recap_present = std::fs::read_to_string(dir.join("recap.md"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let last_activity = if ended > 0.0 { ended } else { started };
    json!({
        "id": id,
        "started_at": rfc3339(started),
        "age_secs": age_secs(last_activity, now),
        "duration_secs": meta["duration_secs"].as_f64(),
        "recording": dir.join(".recording").exists(),
        "chunk_count": meta["chunk_count"].as_u64(),
        "transcript_nonempty": transcript_lines > 0,
        "transcript_lines": transcript_lines,
        "audio_tracks_bytes": Value::Object(tracks),
        "recap_present": recap_present,
        "log_notes": meeting_log_notes(log, &id),
    })
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
            // Unset is the default and means "not chosen yet": the device
            // is picked at runtime, consent is asked at launch.
            if matches!(
                s.key,
                "selected_device" | "analytics_consent" | "crash_reports_consent"
            ) {
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
    fn every_user_setting_in_config_is_in_the_schema() {
        let lib = include_str!("lib.rs");
        let start = lib.find("pub struct AppConfig {").expect("AppConfig");
        let body = &lib[start..start + lib[start..].find("\n}").expect("end")];
        let keys: Vec<String> = settings()
            .iter()
            .map(|s| s.key.to_string())
            .chain(
                crate::setup_prefs::app_prefs()
                    .iter()
                    .map(|p| p.key.to_string()),
            )
            .collect();
        for line in body.lines() {
            let Some(field) = line
                .trim()
                .strip_prefix("pub ")
                .and_then(|r| r.split_once(':'))
                .map(|(f, _)| f)
            else {
                continue;
            };
            assert!(
                keys.iter().any(|k| k == field) || NOT_SETTINGS.contains(&field),
                "AppConfig.{field} is neither in the setup schema nor in NOT_SETTINGS"
            );
        }
    }

    #[test]
    fn text_number_list_and_rules_are_checked() {
        assert!(validate_patch(&json!({ "prompt": "Konrad, Dimmy" })).is_ok());
        let long = "x".repeat(4001);
        assert_eq!(
            validate_patch(&json!({ "prompt": long })).unwrap_err()[0].problem,
            "out_of_range"
        );
        assert!(validate_patch(&json!({ "input_gain": 0.7 })).is_ok());
        assert!(validate_patch(&json!({ "input_gain": 2.5 })).is_err());
        assert!(validate_patch(&json!({ "user_dict": ["Dimmy", "Temera"] })).is_ok());
        assert!(validate_patch(&json!({ "user_dict": "Dimmy" })).is_err());
        assert!(validate_patch(&json!({ "user_dict": [1] })).is_err());
        let rule = json!([{ "match_pattern": "code.exe", "match_type": "process_name", "llm_style": "formal" }]);
        assert_eq!(validate_patch(&json!({ "app_rules": rule })), Ok(()));
        assert!(validate_patch(&json!({ "app_rules": [{ "match_pattern": 1 }] })).is_err());
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
        assert_eq!(
            sc["settings"].as_array().unwrap().len(),
            all.len() + crate::setup_prefs::app_prefs().len()
        );
    }

    #[test]
    fn schema_never_offers_a_secret() {
        // A switch such as llm_use_same_key cannot hold a secret.
        for s in settings().into_iter().filter(|s| s.kind != Kind::Bool) {
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

    #[test]
    fn mac_bundle_is_derived_from_the_running_binary() {
        let exe = Path::new("/Applications/Dimmy Agent.app/Contents/MacOS/Dimmy Agent");
        let app = mac_app_bundle(exe).expect("bundle");
        assert_eq!(app, Path::new("/Applications/Dimmy Agent.app"));
        assert_eq!(
            mac_info_plist_domain(&app),
            Path::new("/Applications/Dimmy Agent.app/Contents/Info")
        );
        assert!(mac_app_bundle(Path::new("/usr/local/bin/dimmy-setup")).is_none());
        assert!(mac_app_bundle(Path::new("/x/Dimmy.app/Contents/Resources/tool")).is_none());
        assert!(mac_app_bundle(Path::new("/x/Dimmy/Contents/MacOS/Dimmy")).is_none());
    }

    #[test]
    fn quit_script_targets_the_bundle_id() {
        assert!(valid_bundle_id("com.dimmy.agent"));
        assert!(valid_bundle_id("com.dimmy.app-staging"));
        assert!(!valid_bundle_id(""));
        assert!(!valid_bundle_id("Dimmy"));
        assert!(!valid_bundle_id("com.x\" to quit\ntell app \"Finder"));
        assert_eq!(
            mac_quit_script("com.dimmy.agent"),
            "tell application id \"com.dimmy.agent\" to quit"
        );
    }

    #[test]
    fn no_microphone_is_a_warning() {
        let w = check_warnings(&[]);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["code"], "no_microphone");
        assert!(check_warnings(&["MacBook Pro Microphone".into()]).is_empty());
    }

    #[test]
    fn every_mac_permission_has_a_settings_pane() {
        for p in PERMISSIONS {
            let url = permission_settings_url(p, "macos").expect(p);
            assert!(url.starts_with("x-apple.systempreferences:"));
        }
        assert_eq!(
            permission_settings_url("microphone", "windows"),
            Some("ms-settings:privacy-microphone")
        );
        assert_eq!(permission_settings_url("accessibility", "windows"), None);
        assert_eq!(permission_settings_url("camera", "macos"), None);
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn permissions_file_is_read_back_with_its_age() {
        let file = r#"{"updated_at":"2026-10-16T10:00:00Z","platform":"macos",
            "microphone":"granted","accessibility":"denied",
            "input_monitoring":"granted","system_audio":"bogus"}"#;
        let v = permissions_report(Some(file), at("2026-10-16T10:01:30Z"));
        assert_eq!(v["known"], true);
        assert_eq!(v["stale_secs"], 90);
        assert_eq!(v["permissions"]["microphone"], "granted");
        assert_eq!(v["permissions"]["accessibility"], "denied");
        assert_eq!(v["permissions"]["system_audio"], "unknown");
        assert_eq!(v["required_granted"], false);

        let ok = file.replace("\"denied\"", "\"granted\"");
        let v = permissions_report(Some(&ok), at("2026-10-16T10:00:00Z"));
        assert_eq!(v["required_granted"], true);
        assert_eq!(v["stale_secs"], 0);
    }

    #[test]
    fn missing_or_broken_permissions_file_says_so() {
        let now = at("2026-10-16T10:00:00Z");
        let v = permissions_report(None, now);
        assert_eq!(v["known"], false);
        assert_eq!(v["fix"], "dimmy-setup app start");
        assert_eq!(v["permissions"]["microphone"], "unknown");
        let v = permissions_report(Some("{not json"), now);
        assert_eq!(v["known"], false);
        assert_eq!(v["required_granted"], false);
    }

    #[test]
    fn windows_mic_switches() {
        assert_eq!(windows_mic_state(None, None, None), "granted");
        assert_eq!(
            windows_mic_state(Some("Allow"), Some("Allow"), Some("Allow")),
            "granted"
        );
        assert_eq!(
            windows_mic_state(Some("Allow"), Some("Allow"), Some("Deny")),
            "denied"
        );
        assert_eq!(windows_mic_state(Some("Deny"), None, None), "denied");
        assert_eq!(windows_mic_state(None, Some("Prompt"), None), "unknown");
    }

    #[test]
    fn dictation_summary_carries_no_text() {
        let t = crate::history::Transcript {
            id: 1,
            text: "ciao a tutti".into(),
            language: "it".into(),
            timestamp: 1_000.0,
            duration: 2.5,
            word_count: 3,
            enhanced_text: None,
            audio_path: None,
            app_process_name: None,
            app_bundle_id: None,
            llm_style: None,
            llm_translate_to: None,
            size_bytes: 0,
            word_timestamps: None,
        };
        let v = dictation_summary(&t, 1_060.0);
        assert_eq!(v["age_secs"], 60);
        assert_eq!(v["chars"], 12);
        assert_eq!(v["words"], 3);
        assert_eq!(v["ok"], true);
        assert!(!v.to_string().contains("ciao"));
    }

    fn meeting_fixture(root: &Path, id: &str, meta: Value) -> PathBuf {
        let d = root.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("meta.json"), meta.to_string()).unwrap();
        d
    }

    #[test]
    fn latest_meeting_is_the_newest_start() {
        let root = temp_dir("meetings-order");
        meeting_fixture(&root, "old", json!({ "started_at": 100.0 }));
        meeting_fixture(&root, "new", json!({ "started_at": 200.0 }));
        std::fs::write(root.join("stray.txt"), "x").unwrap();
        let got = latest_meeting_dir(&root).unwrap();
        assert_eq!(got.file_name().unwrap(), "new");
        assert!(latest_meeting_dir(&root.join("missing")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_test_meeting_looks_failed() {
        // The 2026-10 Mac run: 2.8 s, no microphone, empty transcript, no recap.
        let root = temp_dir("meeting-failed");
        let d = meeting_fixture(
            &root,
            "abc123",
            json!({ "started_at": 1_000.0, "ended_at": 1_002.8, "duration_secs": 2.8, "chunk_count": 0 }),
        );
        std::fs::write(d.join("transcripts.txt"), "").unwrap();
        std::fs::write(d.join("audio_mic.ogg"), vec![0u8; 4096]).unwrap();
        let log = "x\n[Meeting] started id=abc123 dir=\"C:\\\\Users\\\\bob\\\\m\"\n\
                   [Meeting] WARN no input device\nok line\n\
                   [Meeting] started id=other\n[Meeting] WARN later meeting\n";
        let v = meeting_summary(&d, 1_010.0, log);
        assert_eq!(v["id"], "abc123");
        assert_eq!(v["duration_secs"], 2.8);
        assert_eq!(v["age_secs"], 7);
        assert_eq!(v["transcript_nonempty"], false);
        assert_eq!(v["recap_present"], false);
        assert_eq!(v["recording"], false);
        assert_eq!(v["audio_tracks_bytes"]["audio_mic.ogg"], 4096);
        let notes = v["log_notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].as_str().unwrap().contains("no input device"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_good_meeting_looks_good() {
        let root = temp_dir("meeting-good");
        let d = meeting_fixture(
            &root,
            "m1",
            json!({ "started_at": 1_000.0, "duration_secs": 600.0 }),
        );
        std::fs::write(
            d.join("transcripts.txt"),
            "[00:00:01] [mic] hello\n[paused]\n",
        )
        .unwrap();
        std::fs::write(d.join("recap.md"), "# Weekly\n- done\n").unwrap();
        std::fs::write(d.join(".recording"), "1").unwrap();
        let v = meeting_summary(&d, 2_000.0, "");
        assert_eq!(v["transcript_nonempty"], true);
        assert_eq!(v["transcript_lines"], 1);
        assert_eq!(v["recap_present"], true);
        assert_eq!(v["recording"], true);
        assert_eq!(v["started_at"], "1970-01-01T00:16:40+00:00");
        let _ = std::fs::remove_dir_all(&root);
    }
}

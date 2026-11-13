//! The one way a handled failure reaches Sentry.
//!
//! Most of Dimmy's failures never panic: they are caught, logged to the local
//! `dimmy.log`, and turned into a return code or a toast. Before this module
//! only three of them (dictation STT, dictation LLM, recap) reached Sentry,
//! and those grouped by message text and stack, so one failure could split
//! into several issues ("HTTP 1" was three).
//!
//! A report is a stable `(subsystem, kind, engine)` triple. The triple is the
//! Sentry fingerprint, so each distinct failure is exactly one issue, and an
//! agent can read what broke from the tags alone. `detail` is optional and
//! goes through the same prose/secret/path scrub as every other event.
//!
//! Privacy: `kind` and `engine` are categorical (an error bucket, a provider
//! or engine name, a model kind). Never pass a device name, a path, a URL or
//! anything the user typed or said. Only users who said yes to crash reports
//! send anything; the consent check is in `sentry_pipeline`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Where a failure happened. Stable names: they are Sentry tags and part of
/// the fingerprint, so renaming one splits every issue that uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    /// Dictation speech-to-text (cloud or local).
    Stt,
    /// Loading or running a local engine (whisper, parakeet, qwen, LLM...).
    LocalEngine,
    /// Model and engine downloads.
    Download,
    /// Dictation post-processing.
    Llm,
    /// Meeting recap and other raw LLM calls.
    Recap,
    /// Command mode (transform the selection).
    Command,
    /// Meeting capture, transcription thread, finalize, re-transcribe.
    Meeting,
    /// Audio devices and streams.
    Audio,
    /// Echo cancellation.
    Aec,
    /// File load (drop / picker).
    FileLoad,
    /// History store and saved audio.
    History,
    /// The encrypted key store.
    Keystore,
    /// config.json and OS settings such as autostart.
    Config,
    /// Licence activation and refresh.
    License,
    /// Notion, Confluence, Telegram, calendar, Claude Desktop.
    Integration,
    /// The global shortcut hook.
    Hotkey,
    /// The previous launch ended in a native abort.
    Native,
    /// An exception in the Windows or macOS host UI.
    Host,
}

impl Subsystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::LocalEngine => "local_engine",
            Self::Download => "download",
            Self::Llm => "llm",
            Self::Recap => "recap",
            Self::Command => "command",
            Self::Meeting => "meeting",
            Self::Audio => "audio",
            Self::Aec => "aec",
            Self::FileLoad => "file_load",
            Self::History => "history",
            Self::Keystore => "keystore",
            Self::Config => "config",
            Self::License => "license",
            Self::Integration => "integration",
            Self::Hotkey => "hotkey",
            Self::Native => "native",
            Self::Host => "host",
        }
    }
}

/// Failures the user's network, account or machine cause, not our code.
/// They still reach Sentry (a spike is worth knowing), as warnings, so an
/// alert or an agent can take only the errors.
const ENVIRONMENTAL: &[&str] = &[
    "network",
    "timeout",
    "auth",
    "rate_limit",
    "too_large",
    "not_logged_in",
    "not_installed",
    "offline",
    "cancelled",
];

pub fn is_environmental(kind: &str) -> bool {
    ENVIRONMENTAL.contains(&kind)
}

/// The Sentry fingerprint and the issue title. Identifier text only, so the
/// title survives the prose filter and two reports of one failure always
/// land in one issue.
pub fn fingerprint(subsystem: Subsystem, kind: &str, engine: &str) -> Vec<String> {
    assert!(!kind.is_empty(), "a report needs a kind");
    let mut parts = vec![subsystem.as_str().to_string(), slug(kind)];
    if !engine.is_empty() {
        parts.push(slug(engine));
    }
    parts
}

pub fn title(subsystem: Subsystem, kind: &str, engine: &str) -> String {
    fingerprint(subsystem, kind, engine).join("/")
}

/// Keep a categorical value categorical: anything outside `[a-z0-9_.-]`
/// becomes `_`, and it is capped. A caller that passes free text by mistake
/// sends a mangled slug, not the text.
pub fn slug(s: &str) -> String {
    const MAX: usize = 48;
    let out: String = s
        .trim()
        .chars()
        .take(MAX)
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// A failure that repeats (a meeting chunk every 30 s, a stream error per
/// callback) must not become hundreds of events from one user. The first
/// few per process are enough to see it, count users and get a stack.
pub const MAX_PER_FINGERPRINT: u32 = 3;

pub struct Throttle {
    seen: HashMap<String, u32>,
}

impl Throttle {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    pub fn allow(&mut self, key: &str) -> bool {
        let n = self.seen.entry(key.to_string()).or_insert(0);
        *n += 1;
        *n <= MAX_PER_FINGERPRINT
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

static THROTTLE: Mutex<Option<Throttle>> = Mutex::new(None);

pub(crate) fn allowed(key: &str) -> bool {
    // A poisoned lock means a panic elsewhere already reached Sentry; losing
    // the throttle is better than losing the report.
    let mut guard = match THROTTLE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.get_or_insert_with(Throttle::new).allow(key)
}

/// Report a handled failure. Best-effort and silent: never blocks for long,
/// never panics, does nothing without consent or a compiled DSN.
pub fn report(subsystem: Subsystem, kind: &str, engine: &str, detail: &str) {
    if !super::is_crash_enabled() || !super::has_compiled_dsn() {
        return;
    }
    let fp = fingerprint(subsystem, kind, engine);
    if !allowed(&fp.join("/")) {
        return;
    }
    super::sentry_pipeline::capture_failure(
        subsystem.as_str(),
        &fp,
        is_environmental(kind),
        detail,
    );
}

/// What a failure-carrying analytics event should also send to Sentry.
///
/// These events already reach PostHog as counts. Sentry is where an agent
/// looks for something to fix, and until this mirror none of them were
/// there: a licence refresh that failed for every user, a download that
/// never finished, a CLI that exited non-zero (the call site sees only
/// "request failed", so the CLI event is the one that names the engine).
///
/// Left out because their call sites in `ffi.rs` already report the same
/// failure with more detail, and a second report is a second issue:
/// `TranscriptionFailed`, `LlmFailed` and `MeetingRecapCompleted` (the host
/// sends it after `dimmy_llm_call_raw` reported; it was RUST-1H). What only
/// the recap event saw (no key, no local model) stays in PostHog.
pub fn failure_of(event: &super::Event) -> Option<(Subsystem, String, &'static str)> {
    use super::Event as E;
    let r = |s: Subsystem, kind: &str, engine: &'static str| Some((s, kind.to_string(), engine));
    match event {
        E::ModelDownloadCompleted {
            kind,
            success: false,
        } => r(Subsystem::Download, "failed", kind),
        E::LicenseActivationFailed { error_category, .. } => {
            r(Subsystem::License, error_category, "activation")
        }
        E::LicenseRefreshFailed { error_category, .. } => {
            r(Subsystem::License, error_category, "refresh")
        }
        E::MeetingStopTimeout => r(Subsystem::Meeting, "stop_timeout", ""),
        E::MeetingTranscriptionBehind { engine, .. } => {
            r(Subsystem::Meeting, "transcription_behind", engine)
        }
        E::MeetingTranscriptionUnavailable { engine, reason } => {
            r(Subsystem::Meeting, reason, engine)
        }
        E::MeetingCaptureRatio { ratio_bucket } if *ratio_bucket != "ge_95" => {
            r(Subsystem::Meeting, "capture_ratio_low", ratio_bucket)
        }
        E::DictationCaptureRatio {
            ratio_bucket,
            source,
        } if *ratio_bucket != "ge_95" => r(Subsystem::Audio, "capture_ratio_low", source),
        E::LoopbackRateMismatch { measured, .. } => {
            r(Subsystem::Audio, "loopback_rate_mismatch", measured)
        }
        E::FileLoadCompleted { success: false, .. } => r(Subsystem::FileLoad, "failed", ""),
        E::NotionRecapSent { ok: false, .. } => r(Subsystem::Integration, "send_failed", "notion"),
        E::ErrorGpuCrash { backend, .. } => r(Subsystem::LocalEngine, "gpu_crash", backend),
        E::ClaudeCodeInvocation {
            success: false,
            error_category,
            ..
        } => r(Subsystem::Llm, error_category, "claude_code"),
        E::CodexInvocation {
            success: false,
            error_category,
            ..
        } => r(Subsystem::Llm, error_category, "codex"),
        E::KimiCliInvocation {
            success: false,
            error_category,
            ..
        } => r(Subsystem::Llm, error_category, "kimi_cli"),
        E::GeminiCliInvocation {
            success: false,
            error_category,
            ..
        } => r(Subsystem::Llm, error_category, "gemini_cli"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::Event;

    #[test]
    fn one_failure_is_one_fingerprint() {
        assert_eq!(
            fingerprint(Subsystem::Meeting, "stop_timeout", ""),
            vec!["meeting", "stop_timeout"]
        );
        assert_eq!(
            title(Subsystem::LocalEngine, "model_load", "parakeet"),
            "local_engine/model_load/parakeet"
        );
    }

    #[test]
    fn free_text_cannot_ride_in_a_categorical_field() {
        let s = slug("Microfono (Realtek) C:\\Users\\mario");
        assert!(
            !s.contains(' ') && !s.contains('\\') && !s.contains('('),
            "{s}"
        );
        assert!(s.len() <= 48);
        assert_eq!(slug(""), "unknown");
        assert_eq!(slug("Rate_Limit"), "rate_limit");
    }

    #[test]
    fn the_title_survives_the_prose_filter() {
        let t = title(Subsystem::Integration, "send_failed", "notion");
        assert!(t.chars().all(|c| !c.is_whitespace()), "{t}");
    }

    #[test]
    fn a_repeating_failure_is_capped_per_process() {
        let mut t = Throttle::new();
        for _ in 0..MAX_PER_FINGERPRINT {
            assert!(t.allow("meeting/chunk_stt_failed/whisper"));
        }
        assert!(!t.allow("meeting/chunk_stt_failed/whisper"));
        assert!(
            t.allow("meeting/chunk_stt_failed/parakeet"),
            "keys are separate"
        );
    }

    #[test]
    fn environment_failures_are_warnings_and_bugs_are_errors() {
        assert!(is_environmental("network"));
        assert!(is_environmental("rate_limit"));
        assert!(!is_environmental("model_load"));
        assert!(!is_environmental("unknown"));
        assert!(!is_environmental("cli_exit"));
    }

    #[test]
    fn failure_events_are_mirrored_and_successes_are_not() {
        let fail = Event::LicenseRefreshFailed {
            error_category: "server_5xx",
        };
        assert_eq!(
            failure_of(&fail),
            Some((Subsystem::License, "server_5xx".to_string(), "refresh"))
        );
        assert_eq!(
            failure_of(&Event::ModelDownloadCompleted {
                kind: "parakeet",
                success: false
            }),
            Some((Subsystem::Download, "failed".to_string(), "parakeet"))
        );
        assert_eq!(
            failure_of(&Event::ModelDownloadCompleted {
                kind: "parakeet",
                success: true
            }),
            None
        );
        assert_eq!(
            failure_of(&Event::MeetingCaptureRatio {
                ratio_bucket: "ge_95"
            }),
            None
        );
        assert!(failure_of(&Event::MeetingCaptureRatio {
            ratio_bucket: "lt_50"
        })
        .is_some());
    }

    #[test]
    fn dictation_failures_are_not_mirrored_twice() {
        // Their call sites report with the scrubbed message.
        let e = Event::TranscriptionFailed {
            mode: "cloud",
            provider: "groq",
            error_category: "auth",
        };
        assert_eq!(failure_of(&e), None);
    }

    #[test]
    fn the_host_recap_event_is_not_a_second_report() {
        // `dimmy_llm_call_raw` already reported the failure; mirroring the
        // host's event made a second issue with kind "unknown" (RUST-1H).
        let recap = Event::MeetingRecapCompleted {
            provider: "anthropic",
            recap_model_bucket: "opus",
            processing_ms_bucket: "lt_500",
            success: false,
            error_category: String::new(),
        };
        assert_eq!(failure_of(&recap), None);
    }
}

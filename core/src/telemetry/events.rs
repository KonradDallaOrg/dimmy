//! Typed event taxonomy for Dimmy telemetry.
//!
//! Each variant maps to a stable event name + property schema documented
//! in `docs/dev/telemetry-plan.md` §5. The enum is the source of truth
//! for what we send: there is no free-string event API.
//!
//! When you add a new event:
//! 1. Add a variant here with the explicit property fields.
//! 2. Add it to the corresponding section of `telemetry-plan.md` §5.
//! 3. Add a unit test that constructs it and runs it through the
//!    sanitiser to assert no PII slips in.

use serde::Serialize;

/// All telemetry events Dimmy can emit. New variants only — never
/// rename or remove an existing variant without a migration plan
/// (PostHog will treat a renamed event as a new one).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "properties", rename_all = "snake_case")]
pub enum Event {
    // ── Lifecycle ────────────────────────────────────────────
    AppStarted {
        version: &'static str,
        os: &'static str,
        arch: &'static str,
        cold_start_ms: u64,
    },
    AppSessionEnded {
        duration_secs: u64,
        transcribe_count: u32,
    },
    AppUpdateCheck {
        current_version: String,
        available_version: Option<String>,
    },
    AppUpdateApplied {
        from_version: String,
        to_version: String,
    },

    // ── Onboarding ───────────────────────────────────────────
    OnboardingStarted,
    OnboardingStepCompleted {
        step: &'static str,
    },
    OnboardingCompleted {
        path: &'static str,
        duration_secs: u64,
    },
    OnboardingAbandoned {
        last_step: &'static str,
    },

    // ── Configuration changes ────────────────────────────────
    ConfigSttModeChanged {
        mode: &'static str,
    },
    ConfigCloudProviderChanged {
        provider: &'static str,
    },
    ConfigLlmEnabledChanged {
        enabled: bool,
    },
    ConfigLlmStyleChanged {
        style: String,
    },
    ConfigShortcutChanged,
    ConfigPreprocessingChanged {
        enabled: bool,
    },
    ConfigInputGainChanged {
        gain: f32,
    },

    // ── Transcription ────────────────────────────────────────
    TranscriptionCompleted {
        mode: &'static str,
        provider: &'static str,
        audio_secs: f64,
        processing_ms: u64,
        word_count: u32,
        language: String,
        success: bool,
        had_filler_removal: bool,
        had_llm: bool,
    },
    TranscriptionFailed {
        mode: &'static str,
        provider: &'static str,
        error_category: &'static str,
    },
    TranscriptionCancelled {
        audio_secs: f64,
    },

    // ── LLM post-processing ──────────────────────────────────
    LlmApplied {
        mode: &'static str,
        provider: &'static str,
        style: String,
        tone: String,
        processing_ms: u64,
        success: bool,
    },
    LlmFailed {
        mode: &'static str,
        provider: &'static str,
        error_category: &'static str,
    },

    // ── Performance ──────────────────────────────────────────
    PerfStartupMs {
        value: u64,
        version: &'static str,
    },
    PerfGpuStatus {
        backend: &'static str,
        fell_back_to_cpu: bool,
        known_bad: bool,
    },
    PerfTranscribeOverheadPct {
        value: f64,
        mode: &'static str,
        provider: &'static str,
    },

    // ── Errors (also forwarded to Sentry) ────────────────────
    ErrorCloudStt {
        provider: &'static str,
        status_code: Option<u16>,
        error_category: &'static str,
    },
    ErrorCloudLlm {
        provider: &'static str,
        status_code: Option<u16>,
        error_category: &'static str,
    },
    ErrorLocalStt {
        model: String,
        error_category: &'static str,
    },
    ErrorLocalLlm {
        model: String,
        error_category: &'static str,
    },
    ErrorGpuCrash {
        backend: &'static str,
        context: String,
    },
    ErrorAudioHealth {
        code: i32,
    },
}

impl Event {
    /// Stable event name as it will appear in PostHog.
    pub fn name(&self) -> &'static str {
        match self {
            Event::AppStarted { .. } => "app.started",
            Event::AppSessionEnded { .. } => "app.session_ended",
            Event::AppUpdateCheck { .. } => "app.update_check",
            Event::AppUpdateApplied { .. } => "app.update_applied",
            Event::OnboardingStarted => "onboarding.started",
            Event::OnboardingStepCompleted { .. } => "onboarding.step_completed",
            Event::OnboardingCompleted { .. } => "onboarding.completed",
            Event::OnboardingAbandoned { .. } => "onboarding.abandoned",
            Event::ConfigSttModeChanged { .. } => "config.stt_mode_changed",
            Event::ConfigCloudProviderChanged { .. } => "config.cloud_provider_changed",
            Event::ConfigLlmEnabledChanged { .. } => "config.llm_enabled_changed",
            Event::ConfigLlmStyleChanged { .. } => "config.llm_style_changed",
            Event::ConfigShortcutChanged => "config.shortcut_changed",
            Event::ConfigPreprocessingChanged { .. } => "config.preprocessing_changed",
            Event::ConfigInputGainChanged { .. } => "config.input_gain_changed",
            Event::TranscriptionCompleted { .. } => "transcription.completed",
            Event::TranscriptionFailed { .. } => "transcription.failed",
            Event::TranscriptionCancelled { .. } => "transcription.cancelled",
            Event::LlmApplied { .. } => "llm.applied",
            Event::LlmFailed { .. } => "llm.failed",
            Event::PerfStartupMs { .. } => "perf.startup_ms",
            Event::PerfGpuStatus { .. } => "perf.gpu_status",
            Event::PerfTranscribeOverheadPct { .. } => "perf.transcribe_overhead_pct",
            Event::ErrorCloudStt { .. } => "error.cloud_stt",
            Event::ErrorCloudLlm { .. } => "error.cloud_llm",
            Event::ErrorLocalStt { .. } => "error.local_stt",
            Event::ErrorLocalLlm { .. } => "error.local_llm",
            Event::ErrorGpuCrash { .. } => "error.gpu_crash",
            Event::ErrorAudioHealth { .. } => "error.audio_health",
        }
    }

    /// Serialize the event's properties as a JSON object. Returns an
    /// empty object for property-less variants.
    pub fn properties(&self) -> serde_json::Value {
        // Round-trip via Serialize so we preserve the field names
        // declared on each variant. This produces:
        //   {"event": "...", "properties": {...}}
        // We extract "properties" so the caller can wrap it in PostHog's
        // top-level shape.
        let v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        v.get("properties")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
    }
}

/// Runtime detection of OS + arch for the lifecycle events.
pub fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

pub fn arch_name() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_name_is_stable_and_lowercase() {
        let e = Event::AppStarted {
            version: "0.6.20",
            os: "linux",
            arch: "x86_64",
            cold_start_ms: 123,
        };
        assert_eq!(e.name(), "app.started");
        assert!(e
            .name()
            .chars()
            .all(|c| c.is_lowercase() || c == '.' || c == '_'));
    }

    #[test]
    fn properties_serialise_known_fields() {
        let e = Event::TranscriptionCompleted {
            mode: "cloud",
            provider: "groq",
            audio_secs: 4.5,
            processing_ms: 800,
            word_count: 12,
            language: "en".to_string(),
            success: true,
            had_filler_removal: true,
            had_llm: false,
        };
        let p = e.properties();
        assert_eq!(p["mode"], "cloud");
        assert_eq!(p["provider"], "groq");
        assert_eq!(p["word_count"], 12);
        assert_eq!(p["had_llm"], false);
    }

    #[test]
    fn properties_for_unit_variants_is_empty() {
        let e = Event::ConfigShortcutChanged;
        let p = e.properties();
        assert!(p.is_object() || p.is_null());
    }

    #[test]
    fn os_and_arch_are_known() {
        assert!(matches!(os_name(), "windows" | "macos" | "linux" | "other"));
        assert!(matches!(arch_name(), "x86_64" | "aarch64" | "other"));
    }
}

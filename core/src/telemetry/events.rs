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
    /// Emitted when the user flips the "Launch at login" toggle in
    /// Settings. The autostart toggle is a real cross-platform OS
    /// integration, so success here is non-trivial; we track only
    /// successful flips (the Rust core returns -1 from the FFI on
    /// OS-level failures, in which case the C# UI does NOT flip its
    /// `IsOn` state and no event is emitted).
    ConfigAutostartChanged {
        enabled: bool,
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

    // ── Feature usage (engagement signals) ───────────────────
    /// Emitted when the global hotkey starts a recording. Pairs with
    /// the existing `transcription.*` events to derive the
    /// hotkey-vs-button ratio (= recordings without a corresponding
    /// `feature.hotkey_triggered` originated from a UI button).
    FeatureHotkeyTriggered,
    /// Emitted when the user successfully writes an API key for any
    /// provider. Carries `scope` (stt|llm) and `provider` (stable
    /// enum tag — never the key value). Used to measure the
    /// "configured a real provider" activation step independently of
    /// whether they then ran a transcription.
    FeatureApiKeySet {
        scope: &'static str,
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

    // ── Licensing ────────────────────────────────────────────
    // Privacy hard rule (CLAUDE.md): NEVER send `email`, `email_hash`,
    // `license_id`, `device_id`, `device_label`, `token`, magic links
    // (the URL contains the activation code which is one-shot but
    // observably distinguishes users). Tier names + categorical error
    // buckets + counts are OK. The categorical sets are documented in
    // docs/dev/telemetry-implementation.md.
    /// User started or completed an activation. Fired from the dimmy://
    /// scheme handler AND from the manual paste-code path.
    LicenseActivated {
        /// "trial" | "monthly" | "annual" | "lifetime" — comes from the verified token
        /// after redeem so it's always categorical, never user-supplied.
        tier: &'static str,
    },
    /// Activation request failed (network, server, signature). The
    /// `error_category` is bucketed to a fixed enum — never the raw
    /// reqwest::Error message which can leak URLs.
    LicenseActivationFailed {
        /// "network" | "server_4xx" | "server_5xx" | "verify" | "disk" | "unknown"
        error_category: &'static str,
    },
    /// `/api/refresh` succeeded, last_online_check bumped.
    LicenseRefreshed {
        tier: &'static str,
    },
    LicenseRefreshFailed {
        error_category: &'static str,
    },
    /// A scope check at a feature gate returned false (= user hit a
    /// paywall). Lets us see which capabilities matter most for upsell.
    LicenseScopeDenied {
        /// "managed_stt" | "managed_llm" | "auto_update" | "history_sync" | "premium_styles"
        scope: &'static str,
    },
    /// Self sign-out or admin device-deactivate.
    LicenseDeviceDeactivated {
        /// `true` if the calling device deactivated itself, `false` if
        /// it deactivated another device under the same license.
        is_self: bool,
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
            Event::ConfigAutostartChanged { .. } => "config.autostart_changed",
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
            Event::FeatureHotkeyTriggered => "feature.hotkey_triggered",
            Event::FeatureApiKeySet { .. } => "feature.api_key_set",
            Event::LicenseActivated { .. } => "license.activated",
            Event::LicenseActivationFailed { .. } => "license.activation_failed",
            Event::LicenseRefreshed { .. } => "license.refreshed",
            Event::LicenseRefreshFailed { .. } => "license.refresh_failed",
            Event::LicenseScopeDenied { .. } => "license.scope_denied",
            Event::LicenseDeviceDeactivated { .. } => "license.device_deactivated",
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

    /// Every license event MUST carry only categorical data — verify by
    /// scanning the serialised properties for any field that smells like
    /// a user identifier. This catches drift if someone adds a field
    /// like `license_id` or `email_hash` later without thinking.
    #[test]
    fn license_events_carry_no_user_identifiers() {
        let events = vec![
            Event::LicenseActivated { tier: "trial" },
            Event::LicenseActivationFailed {
                error_category: "network",
            },
            Event::LicenseRefreshed { tier: "annual" },
            Event::LicenseRefreshFailed {
                error_category: "server_5xx",
            },
            Event::LicenseScopeDenied {
                scope: "managed_stt",
            },
            Event::LicenseDeviceDeactivated { is_self: true },
        ];
        let banned_keys = [
            "email",
            "email_hash",
            "eh",
            "license_id",
            "lid",
            "device_id",
            "did",
            "device_label",
            "label",
            "token",
            "magic_link",
            "code",
            "ip",
            "hostname",
            "username",
        ];
        for e in events {
            let p = e.properties();
            let p_obj = p
                .as_object()
                .expect("license event must serialise as object");
            for k in p_obj.keys() {
                assert!(
                    !banned_keys.contains(&k.as_str()),
                    "license event '{}' leaks PII via property '{}'",
                    e.name(),
                    k
                );
            }
        }
    }

    #[test]
    fn license_event_names_are_dotted_lowercase() {
        let names = [
            Event::LicenseActivated { tier: "trial" }.name(),
            Event::LicenseActivationFailed {
                error_category: "x",
            }
            .name(),
            Event::LicenseRefreshed { tier: "trial" }.name(),
            Event::LicenseRefreshFailed {
                error_category: "x",
            }
            .name(),
            Event::LicenseScopeDenied {
                scope: "managed_stt",
            }
            .name(),
            Event::LicenseDeviceDeactivated { is_self: false }.name(),
        ];
        for n in names {
            assert!(
                n.starts_with("license."),
                "license event name must start with 'license.': {}",
                n
            );
            assert!(
                n.chars().all(|c| c.is_lowercase() || c == '.' || c == '_'),
                "license event name must be lowercase + dot/underscore: {}",
                n
            );
        }
    }
}

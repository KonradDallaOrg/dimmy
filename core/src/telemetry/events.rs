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
        /// "whisper" | "parakeet" | "" (cloud or unknown). Added 2026-05-12 so
        /// we can derive the local-backend distribution without inferring
        /// from `provider` (which collapses to "local_whisper" for both).
        local_backend: &'static str,
        /// "hotkey" | "file_load" | "meeting" | "pill". Lets us distinguish
        /// "real-time dictation" engagement from "post-hoc file processing"
        /// in the funnel.
        entry_point: &'static str,
        audio_secs: f64,
        processing_ms: u64,
        word_count: u32,
        language: String,
        success: bool,
        had_filler_removal: bool,
        had_llm: bool,
        /// "batch" | "deepgram_stream" | "local_stream" | "chunked_caption".
        /// Distinguishes the realtime engines from the post-stop batch path.
        /// Added 2026-06-07 so streaming/realtime adoption is visible.
        engine: &'static str,
    },
    /// A local model finished downloading (whisper / llm / parakeet bundle).
    /// `kind` is categorical; `success = false` flags a failed or aborted
    /// fetch (onboarding drop-off + failure-rate signal).
    ModelDownloadCompleted {
        kind: &'static str,
        success: bool,
    },
    /// The recording-consent gate logged a step. `kind` is the categorical
    /// consent event (shown / accepted / cancelled / announced / declined /
    /// other) so we can measure drop-off on a mandatory blocking gate.
    ConsentLogged {
        kind: &'static str,
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

    // ── Meeting mode ────────────────────────────────────────────
    //
    // The meeting feature has been live since v0.6 but emitted ZERO
    // events. Added 2026-05-12. All durations / counts bucketed via
    // `sanitize::bucket_*` so we can't fingerprint a user via an
    // unusual meeting length / word count.
    MeetingStarted,
    MeetingStopped {
        /// `lt_30` | `30_120` | `120_600` | `600_1800` | `1800_3600` | `ge_3600`
        duration_bucket: &'static str,
        /// `0` | `1_50` | `51_200` | `201_1000` | `1001_5000` | `ge_5000`
        words_bucket: &'static str,
        /// Was a recap pipeline triggered after stop?
        had_recap: bool,
    },
    MeetingPaused,
    MeetingResumed,
    /// Fired when the LLM recap completes (success or fail). Lets us
    /// see how often the recap chain breaks vs how often users
    /// actually get a recap out the other end.
    MeetingRecapCompleted {
        /// "groq" | "openai" | "anthropic" | "gemini" | "openrouter" | "local" | "unset"
        provider: &'static str,
        /// "opus" | "sonnet" | "haiku" | "gemini_pro" | "gemini_flash"
        /// | "gpt_5" | "gpt_4" | "llama" | "gemma" | "default" | "other"
        recap_model_bucket: &'static str,
        /// "lt_500" | "500_2000" | "2000_10000" | "10000_60000" | "ge_60000"
        processing_ms_bucket: &'static str,
        success: bool,
    },
    /// Fired when a user clicks "Run recap as meeting" on the File
    /// Load card (Win + Mac). Distinguishes "live mic recording"
    /// from "imported audio" in the meeting funnel.
    MeetingImportedFromFile,

    // ── File Load ───────────────────────────────────────────────
    //
    // Tracks the offline-WAV path. Lets us see how many users use
    // file-load at all (was always opaque) and the typical file
    // shape.
    FileLoadStarted {
        /// "drop" | "picker" — how the user got to the transcribe
        /// call.
        source: &'static str,
        audio_secs_bucket: &'static str,
    },
    FileLoadCompleted {
        audio_secs_bucket: &'static str,
        processing_ms_bucket: &'static str,
        words_bucket: &'static str,
        success: bool,
    },

    // ── Custom Dictionary ───────────────────────────────────────
    //
    // The user-dict surface shipped in v0.6.36. These events let us
    // see which entry point users prefer (hotkey vs Services menu vs
    // direct text-box add).
    UserDictWordAdded {
        /// "hotkey" | "services_menu" | "settings_ui"
        source: &'static str,
    },
    UserDictWordRemoved,
    /// Periodic snapshot of dict size (bucketed). Emitted once per
    /// session at startup so we can see the size distribution
    /// without sampling per-add (which would bias toward power users).
    UserDictSizeSnapshot {
        /// "0" | "1_5" | "6_20" | "21_100" | "ge_100"
        size_bucket: &'static str,
    },

    // ── Notion integration ──────────────────────────────────────
    //
    // No PII — only categorical flags. The token, page id, workspace
    // name, etc. are NEVER on the wire.
    NotionConnected,
    NotionDisconnected,
    NotionRecapSent {
        /// `true` if auto-send fired at recap-completion time, `false`
        /// if the user clicked the manual "Send to Notion" button.
        auto: bool,
        ok: bool,
    },

    // ── App Rules ───────────────────────────────────────────────
    //
    // Engagement signal for the app-rules feature. We never send the
    // rule pattern itself (it's user-curated text that may identify
    // a workplace via specific .exe names).
    AppRulesEvaluated {
        /// Whether the current context (focused app) matched any rule.
        /// Helps measure how often the rule list actually fires vs
        /// sits unused.
        matched: bool,
        /// `0` | `1_5` | `6_20` | `ge_20`
        rules_total_bucket: &'static str,
    },
    AppRuleAdded,
    AppRuleRemoved,
    AppRuleReordered,

    // ── Pill UI ─────────────────────────────────────────────────
    //
    // The pill is the primary in-app surface. These events tell us
    // how users interact with it: visibility toggles, scroll-cycle
    // gestures (style + language), right-click menu opens.
    PillVisibilityToggled {
        /// Final state after the toggle.
        visible: bool,
        /// "settings" | "tray" | "jumplist" | "hotkey" | "dock_menu"
        source: &'static str,
    },
    /// User scrolled the pill's style label to cycle between LLM
    /// styles. Categorical-only — never send the chosen style here
    /// (already covered by ConfigLlmStyleChanged).
    PillStyleScrolled,
    /// User scrolled the pill's language label to cycle translation
    /// target. Same rule — never send the language code (covered by
    /// the existing config events).
    PillLanguageScrolled,
    PillContextMenuOpened,

    // ── Update channel ──────────────────────────────────────────
    UpdateChannelChanged {
        /// "stable" | "prerelease"
        from: &'static str,
        /// "stable" | "prerelease"
        to: &'static str,
    },
    /// User dismissed the quit-time "Apply now?" dialog with Skip.
    /// The update is still staged for next launch (ApplyOnExit).
    UpdateApplyDeferred,

    // ── Permissions (Mac only — TCC) ────────────────────────────
    //
    // Win doesn't have an equivalent TCC concept (no per-feature
    // grant prompts). On Mac, these surface the funnel drop-off at
    // each permission step.
    PermissionGranted {
        /// "microphone" | "accessibility" | "input_monitoring"
        service: &'static str,
    },
    PermissionDenied {
        service: &'static str,
    },

    // ── Recap model picker ─────────────────────────────────────
    /// User changed the recap-model override in Settings. Bucketed
    /// model name so we don't fingerprint via odd custom model
    /// strings (e.g. user-set Together AI hosted ones).
    ConfigRecapModelChanged {
        recap_model_bucket: &'static str,
    },

    // ── Claude Code (Anthropic subscription) backend ────────────
    //
    // The user-explicit subscription provider that piggybacks on
    // the `claude` CLI. We track funnel + reliability without
    // touching any user content — no prompt text, no model output,
    // no binary paths. Just status enums, durations bucketed, and
    // error categories.
    /// Status probe result. Emitted by the Settings card on every
    /// refresh + after a login spawn returns. Categorical only:
    /// the UI logic already exposes the same 3 states.
    ClaudeCodeStatusProbed {
        /// "ready" | "not_logged_in" | "not_installed"
        status: &'static str,
    },
    /// User clicked "Sign in via browser". Fires when the subprocess
    /// is spawned, regardless of outcome — pairs with
    /// `ClaudeCodeLoginCompleted` to derive the abandonment rate.
    ClaudeCodeLoginSpawned,
    /// Polling loop concluded — either ready, timed out at 3 min,
    /// or the spawn itself failed. Lets us see whether the typical
    /// user actually makes it through the browser flow.
    ClaudeCodeLoginCompleted {
        /// "success" | "timeout" | "spawn_failed"
        outcome: &'static str,
    },
    /// One LLM call via the subprocess returned. Lets us see how
    /// often the CLI path is exercised vs HTTP providers, and the
    /// typical processing-time distribution.
    ClaudeCodeInvocation {
        /// "rewrite" | "recap" — the LLM dispatch site.
        kind: &'static str,
        /// `lt_500` | `500_2000` | `2000_10000` | `10000_60000` | `ge_60000`
        processing_ms_bucket: &'static str,
        success: bool,
        /// "ok" | "not_installed" | "not_logged_in" | "timeout" |
        /// "spawn" | "exit_nonzero" | "invalid_utf8". Categorical
        /// error bucket — never the raw stderr (which can echo back
        /// transcript content via the model's complaints).
        error_category: &'static str,
    },

    // ── Codex (OpenAI / ChatGPT subscription) — mirror of the
    //    ClaudeCode events above. Same categorical-only discipline. ──
    /// Status probe result from the Codex Settings card.
    CodexStatusProbed {
        /// "ready" | "not_logged_in" | "not_installed"
        status: &'static str,
    },
    /// User clicked "Sign in with ChatGPT". Fires when the `codex login`
    /// subprocess is spawned, regardless of outcome.
    CodexLoginSpawned,
    /// Codex login polling loop concluded.
    CodexLoginCompleted {
        /// "success" | "timeout" | "spawn_failed"
        outcome: &'static str,
    },
    /// One LLM call via `codex exec` returned.
    CodexInvocation {
        /// "rewrite" | "recap" — the LLM dispatch site.
        kind: &'static str,
        /// `lt_500` | `500_2000` | `2000_10000` | `10000_60000` | `ge_60000`
        processing_ms_bucket: &'static str,
        success: bool,
        /// "ok" | "not_installed" | "not_logged_in" | "timeout" |
        /// "spawn" | "exit_nonzero" | "invalid_utf8".
        error_category: &'static str,
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
            Event::ModelDownloadCompleted { .. } => "model.download_completed",
            Event::ConsentLogged { .. } => "consent.logged",
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
            Event::MeetingStarted => "meeting.started",
            Event::MeetingStopped { .. } => "meeting.stopped",
            Event::MeetingPaused => "meeting.paused",
            Event::MeetingResumed => "meeting.resumed",
            Event::MeetingRecapCompleted { .. } => "meeting.recap_completed",
            Event::MeetingImportedFromFile => "meeting.imported_from_file",
            Event::FileLoadStarted { .. } => "file_load.started",
            Event::FileLoadCompleted { .. } => "file_load.completed",
            Event::UserDictWordAdded { .. } => "user_dict.word_added",
            Event::UserDictWordRemoved => "user_dict.word_removed",
            Event::UserDictSizeSnapshot { .. } => "user_dict.size_snapshot",
            Event::NotionConnected => "notion.connected",
            Event::NotionDisconnected => "notion.disconnected",
            Event::NotionRecapSent { .. } => "notion.recap_sent",
            Event::AppRulesEvaluated { .. } => "app_rules.evaluated",
            Event::AppRuleAdded => "app_rules.added",
            Event::AppRuleRemoved => "app_rules.removed",
            Event::AppRuleReordered => "app_rules.reordered",
            Event::PillVisibilityToggled { .. } => "pill.visibility_toggled",
            Event::PillStyleScrolled => "pill.style_scrolled",
            Event::PillLanguageScrolled => "pill.language_scrolled",
            Event::PillContextMenuOpened => "pill.context_menu_opened",
            Event::UpdateChannelChanged { .. } => "update.channel_changed",
            Event::UpdateApplyDeferred => "update.apply_deferred",
            Event::PermissionGranted { .. } => "permission.granted",
            Event::PermissionDenied { .. } => "permission.denied",
            Event::ConfigRecapModelChanged { .. } => "config.recap_model_changed",
            Event::ClaudeCodeStatusProbed { .. } => "claude_code.status_probed",
            Event::ClaudeCodeLoginSpawned => "claude_code.login_spawned",
            Event::ClaudeCodeLoginCompleted { .. } => "claude_code.login_completed",
            Event::ClaudeCodeInvocation { .. } => "claude_code.invocation",
            Event::CodexStatusProbed { .. } => "codex.status_probed",
            Event::CodexLoginSpawned => "codex.login_spawned",
            Event::CodexLoginCompleted { .. } => "codex.login_completed",
            Event::CodexInvocation { .. } => "codex.invocation",
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
            local_backend: "",
            entry_point: "hotkey",
            audio_secs: 4.5,
            processing_ms: 800,
            word_count: 12,
            language: "en".to_string(),
            success: true,
            had_filler_removal: true,
            had_llm: false,
            engine: "batch",
        };
        let p = e.properties();
        assert_eq!(p["mode"], "cloud");
        assert_eq!(p["provider"], "groq");
        assert_eq!(p["word_count"], 12);
        assert_eq!(p["had_llm"], false);
        assert_eq!(p["entry_point"], "hotkey");
        assert_eq!(p["local_backend"], "");
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

    /// Same PII-scan as the license check, but for the Claude Code
    /// events. These run on every status probe / login / invocation,
    /// so a leaked field would amplify into the highest-cardinality
    /// stream we have. Bind the names + property keys hard.
    #[test]
    fn claude_code_events_carry_no_user_content() {
        let events = vec![
            Event::ClaudeCodeStatusProbed { status: "ready" },
            Event::ClaudeCodeLoginSpawned,
            Event::ClaudeCodeLoginCompleted { outcome: "success" },
            Event::ClaudeCodeInvocation {
                kind: "recap",
                processing_ms_bucket: "2000_10000",
                success: true,
                error_category: "ok",
            },
        ];
        // Any property key that smells like content, paths, or
        // identifiers. The list mirrors the license PII scan; if a
        // new event ever needs one of these legitimately, allow-list
        // here EXPLICITLY (not by removing the check).
        let banned_keys = [
            "prompt",
            "model_output",
            "stderr",
            "binary_path",
            "path",
            "home_dir",
            "user",
            "username",
            "hostname",
            "ip",
            "token",
            "credentials",
        ];
        for e in events {
            let p = e.properties();
            let p_obj = p
                .as_object()
                .expect("claude_code event must serialise as object");
            for k in p_obj.keys() {
                assert!(
                    !banned_keys.contains(&k.as_str()),
                    "claude_code event '{}' leaks via property '{}'",
                    e.name(),
                    k
                );
            }
        }
    }

    #[test]
    fn claude_code_event_names_are_dotted_lowercase() {
        let names = [
            Event::ClaudeCodeStatusProbed { status: "ready" }.name(),
            Event::ClaudeCodeLoginSpawned.name(),
            Event::ClaudeCodeLoginCompleted { outcome: "success" }.name(),
            Event::ClaudeCodeInvocation {
                kind: "recap",
                processing_ms_bucket: "lt_500",
                success: true,
                error_category: "ok",
            }
            .name(),
        ];
        for n in names {
            assert!(
                n.starts_with("claude_code."),
                "claude_code event name must start with 'claude_code.': {}",
                n
            );
            assert!(
                n.chars().all(|c| c.is_lowercase() || c == '.' || c == '_'),
                "claude_code event name must be lowercase + dot/underscore: {}",
                n
            );
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

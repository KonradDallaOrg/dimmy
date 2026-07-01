//! Sanitisation helpers for telemetry payloads.
//!
//! Every property that goes into a telemetry event passes through one
//! of these helpers. This is the single layer that prevents PII leaks.
//! Add a test in `tests/telemetry_sanitize.rs` for every new helper.

/// Map a provider URL to a stable short name. Never returns the URL.
///
/// Used to record "which cloud provider was hit" without ever sending
/// the URL itself (custom routes can leak tenant IDs or proxies).
pub fn provider_from_url(url: &str) -> &'static str {
    if url.contains("api.groq.com") {
        "groq"
    } else if url.contains("api.openai.com") {
        "openai"
    } else if url.contains("api.anthropic.com") {
        "anthropic"
    } else if url.contains("api.deepgram.com") {
        "deepgram"
    } else if url.contains("generativelanguage.googleapis.com") {
        "gemini"
    } else if url.is_empty() {
        "unset"
    } else {
        "custom"
    }
}

/// Bucket an error into a small set of stable categories.
///
/// `status` is the HTTP status if available (cloud calls), `None` for
/// local errors. The error message itself is never returned — it can
/// contain user-quoted strings, paths, or fragments of credentials.
pub fn error_category(message: &str, status: Option<u16>) -> &'static str {
    if let Some(s) = status {
        return match s {
            401 | 403 => "auth",
            404 => "not_found",
            429 => "rate_limit",
            500..=599 => "server_error",
            _ => "client_error",
        };
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        "timeout"
    } else if lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("network")
        || lower.contains("unreachable")
    {
        "network"
    } else if lower.contains("model load")
        || lower.contains("ggml")
        || lower.contains("whisper_init")
    {
        "model_load"
    } else if lower.contains("permission") || lower.contains("access") {
        "permission"
    } else {
        "unknown"
    }
}

/// Replace any user-identifying segment of a path with placeholders.
///
/// Returns a string that is safe to include in telemetry but still
/// useful for debugging (e.g. the model filename is preserved).
///
/// Example: `/Users/mario/Library/Application Support/dimmy/models/ggml-base.bin`
///       -> `<HOME>/dimmy/models/ggml-base.bin`
pub fn scrub_path(path: &str) -> String {
    let p = std::path::Path::new(path);

    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            let rest = &path[home_str.len()..];
            let rest = rest.trim_start_matches(['/', '\\']);
            return format!(
                "<HOME>/{}",
                rest.replace('\\', "/")
                    .split('/')
                    .filter(|seg| !seg.is_empty())
                    .skip_while(|seg| matches!(
                        *seg,
                        "Library"
                            | "Application Support"
                            | ".config"
                            | "AppData"
                            | "Roaming"
                            | "Local"
                    ))
                    .collect::<Vec<_>>()
                    .join("/")
            );
        }
    }

    // Fallback: keep the last two segments only (typically `dir/file.ext`)
    let segs: Vec<&std::ffi::OsStr> = p.iter().collect();
    let n = segs.len();
    if n >= 2 {
        format!(
            "<…>/{}/{}",
            segs[n - 2].to_string_lossy(),
            segs[n - 1].to_string_lossy()
        )
    } else {
        "<scrubbed>".to_string()
    }
}

/// Round a floating-point gain value to 0.1 precision so we don't
/// fingerprint users by an unusual decimal.
pub fn round_gain(gain: f32) -> f32 {
    (gain * 10.0).round() / 10.0
}

// ── Bucketing helpers ──────────────────────────────────────────────
//
// Used by telemetry events that want a categorical signal instead of
// a precise count (which could fingerprint individual users:
// "this is the only user with 17 app rules"). All bucket fns return a
// stable `&'static str` so PostHog stores them as categorical and we
// keep the property cardinality bounded.

/// Audio duration buckets. Designed for both single-recording
/// dictation (sub-minute typical) and long meetings (~hours).
pub fn bucket_audio_secs(secs: f64) -> &'static str {
    match secs {
        s if s < 30.0 => "lt_30",
        s if s < 120.0 => "30_120",
        s if s < 600.0 => "120_600",
        s if s < 1800.0 => "600_1800",
        s if s < 3600.0 => "1800_3600",
        _ => "ge_3600",
    }
}

/// Wall-clock processing time buckets (STT or LLM call).
/// Capture-ratio buckets: captured audio seconds / elapsed recording
/// seconds. `ge_95` is healthy (Mic-mode dictation should land here);
/// anything below `85_95` means the capture path silently dropped audio.
pub fn bucket_capture_ratio(ratio: f64) -> &'static str {
    match ratio {
        r if r < 0.50 => "lt_50",
        r if r < 0.85 => "50_85",
        r if r < 0.95 => "85_95",
        _ => "ge_95",
    }
}

pub fn bucket_processing_ms(ms: u64) -> &'static str {
    match ms {
        m if m < 500 => "lt_500",
        m if m < 2_000 => "500_2000",
        m if m < 10_000 => "2000_10000",
        m if m < 60_000 => "10000_60000",
        _ => "ge_60000",
    }
}

/// Word count buckets. Covers dictation snippets (~10 words) up to
/// meeting transcripts (~10k words).
pub fn bucket_word_count(n: u32) -> &'static str {
    match n {
        0 => "0",
        1..=50 => "1_50",
        51..=200 => "51_200",
        201..=1000 => "201_1000",
        1001..=5000 => "1001_5000",
        _ => "ge_5000",
    }
}

/// User-dictionary size buckets. Most users will have < 20; this
/// captures the "power user" tail without leaking the exact count.
pub fn bucket_dict_size(n: usize) -> &'static str {
    match n {
        0 => "0",
        1..=5 => "1_5",
        6..=20 => "6_20",
        21..=100 => "21_100",
        _ => "ge_100",
    }
}

/// App-rules count buckets. Same shape as dict_size — most users
/// will have the seed defaults (~10-20).
pub fn bucket_app_rules(n: usize) -> &'static str {
    match n {
        0 => "0",
        1..=5 => "1_5",
        6..=20 => "6_20",
        _ => "ge_20",
    }
}

/// LLM/recap model bucket. Strips precise version suffixes so we
/// don't fingerprint via odd model picks ("claude-opus-4-7" → "opus",
/// "gpt-5-mini-2024-07-18" → "gpt"). Used for `recap_model_bucket`
/// on MeetingRecapCompleted so the user's exact model id (which can
/// be an unusual custom string) doesn't leak.
pub fn bucket_recap_model(model: &str) -> &'static str {
    let lower = model.to_ascii_lowercase();
    if lower.contains("opus") {
        "opus"
    } else if lower.contains("sonnet") {
        "sonnet"
    } else if lower.contains("haiku") {
        "haiku"
    } else if lower.contains("gemini-2.5-pro") || lower.contains("gemini-3-pro") {
        "gemini_pro"
    } else if lower.contains("gemini-2.5-flash") || lower.contains("gemini-3-flash") {
        "gemini_flash"
    } else if lower.contains("gpt-5") {
        "gpt_5"
    } else if lower.contains("gpt-4") {
        "gpt_4"
    } else if lower.contains("llama") {
        "llama"
    } else if lower.contains("gemma") {
        "gemma"
    } else if lower.is_empty() {
        "default"
    } else {
        "other"
    }
}

/// True if the string looks like it might contain a secret. Used as
/// a defensive last-ditch check before forwarding any payload.
pub fn looks_like_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    s.starts_with("sk-")
        || s.starts_with("phc_")
        || s.starts_with("phx_")
        || s.starts_with("sntrys_")
        || s.starts_with("gsk_")
        || s.starts_with("Bearer ")
        || lower.contains("api_key=")
        || lower.contains("api-key:")
        || lower.contains("authorization:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_from_url_maps_known_hosts() {
        assert_eq!(
            provider_from_url("https://api.groq.com/openai/v1/audio/transcriptions"),
            "groq"
        );
        assert_eq!(
            provider_from_url("https://api.openai.com/v1/audio/transcriptions"),
            "openai"
        );
        assert_eq!(
            provider_from_url("https://api.anthropic.com/v1/messages"),
            "anthropic"
        );
        assert_eq!(provider_from_url(""), "unset");
        assert_eq!(
            provider_from_url("https://my-corporate-proxy.local"),
            "custom"
        );
    }

    #[test]
    fn provider_from_url_never_returns_input_url() {
        let urls = [
            "https://api.groq.com/v1?api_key=gsk_super_secret",
            "https://my-tenant-x123.proxied.example.com/v1",
        ];
        for url in urls {
            let cat = provider_from_url(url);
            assert!(!cat.contains(url), "leaked URL: {} -> {}", url, cat);
            assert!(!cat.contains("gsk"), "leaked secret prefix: {}", cat);
            assert!(!cat.contains("tenant"), "leaked tenant id: {}", cat);
        }
    }

    #[test]
    fn error_category_uses_status_first() {
        assert_eq!(error_category("anything", Some(401)), "auth");
        assert_eq!(error_category("anything", Some(429)), "rate_limit");
        assert_eq!(error_category("anything", Some(503)), "server_error");
    }

    #[test]
    fn error_category_falls_back_to_message_keywords() {
        assert_eq!(error_category("connection refused", None), "network");
        assert_eq!(error_category("Operation timed out", None), "timeout");
        assert_eq!(error_category("ggml: failed to load", None), "model_load");
        assert_eq!(
            error_category("EACCES: permission denied", None),
            "permission"
        );
        assert_eq!(error_category("something weird happened", None), "unknown");
    }

    #[test]
    fn scrub_path_replaces_home() {
        if let Some(home) = dirs::home_dir() {
            let p = home.join("dimmy").join("models").join("ggml-base.bin");
            let scrubbed = scrub_path(&p.to_string_lossy());
            assert!(scrubbed.starts_with("<HOME>/"));
            assert!(scrubbed.ends_with("ggml-base.bin"));
            assert!(!scrubbed.contains(home.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn round_gain_buckets_to_tenth() {
        assert_eq!(round_gain(1.234), 1.2);
        assert_eq!(round_gain(0.05), 0.1);
        assert_eq!(round_gain(2.0), 2.0);
    }

    #[test]
    fn looks_like_secret_catches_common_prefixes() {
        assert!(looks_like_secret("sk-proj-abc123"));
        assert!(looks_like_secret("phc_owaPfYy"));
        assert!(looks_like_secret("gsk_GroqKey"));
        assert!(looks_like_secret("Bearer eyJhbGciOi"));
        assert!(!looks_like_secret("hello world"));
        assert!(!looks_like_secret("user said: ask not"));
    }

    // ── Bucket helper tests ────────────────────────────────────
    //
    // Pin the bucket boundaries so a future refactor doesn't silently
    // shift the categorical buckets that PostHog dashboards already
    // depend on (the dashboards filter by these stable string values).

    #[test]
    fn bucket_audio_secs_boundaries() {
        assert_eq!(bucket_audio_secs(0.0), "lt_30");
        assert_eq!(bucket_audio_secs(29.999), "lt_30");
        assert_eq!(bucket_audio_secs(30.0), "30_120");
        assert_eq!(bucket_audio_secs(119.9), "30_120");
        assert_eq!(bucket_audio_secs(120.0), "120_600");
        assert_eq!(bucket_audio_secs(600.0), "600_1800");
        assert_eq!(bucket_audio_secs(1800.0), "1800_3600");
        assert_eq!(bucket_audio_secs(3600.0), "ge_3600");
        assert_eq!(bucket_audio_secs(99_999.0), "ge_3600");
    }

    #[test]
    fn bucket_processing_ms_boundaries() {
        assert_eq!(bucket_processing_ms(0), "lt_500");
        assert_eq!(bucket_processing_ms(499), "lt_500");
        assert_eq!(bucket_processing_ms(500), "500_2000");
        assert_eq!(bucket_processing_ms(2000), "2000_10000");
        assert_eq!(bucket_processing_ms(10_000), "10000_60000");
        assert_eq!(bucket_processing_ms(60_000), "ge_60000");
    }

    #[test]
    fn bucket_word_count_boundaries() {
        assert_eq!(bucket_word_count(0), "0");
        assert_eq!(bucket_word_count(1), "1_50");
        assert_eq!(bucket_word_count(50), "1_50");
        assert_eq!(bucket_word_count(51), "51_200");
        assert_eq!(bucket_word_count(200), "51_200");
        assert_eq!(bucket_word_count(201), "201_1000");
        assert_eq!(bucket_word_count(1000), "201_1000");
        assert_eq!(bucket_word_count(1001), "1001_5000");
        assert_eq!(bucket_word_count(5001), "ge_5000");
    }

    #[test]
    fn bucket_dict_size_boundaries() {
        assert_eq!(bucket_dict_size(0), "0");
        assert_eq!(bucket_dict_size(1), "1_5");
        assert_eq!(bucket_dict_size(5), "1_5");
        assert_eq!(bucket_dict_size(6), "6_20");
        assert_eq!(bucket_dict_size(20), "6_20");
        assert_eq!(bucket_dict_size(21), "21_100");
        assert_eq!(bucket_dict_size(100), "21_100");
        assert_eq!(bucket_dict_size(101), "ge_100");
    }

    #[test]
    fn bucket_app_rules_boundaries() {
        assert_eq!(bucket_app_rules(0), "0");
        assert_eq!(bucket_app_rules(5), "1_5");
        assert_eq!(bucket_app_rules(20), "6_20");
        assert_eq!(bucket_app_rules(21), "ge_20");
    }

    #[test]
    fn bucket_recap_model_known_families() {
        assert_eq!(bucket_recap_model("claude-opus-4-7"), "opus");
        assert_eq!(bucket_recap_model("claude-sonnet-4-6"), "sonnet");
        assert_eq!(bucket_recap_model("claude-haiku-4-5-20251001"), "haiku");
        assert_eq!(bucket_recap_model("gemini-2.5-pro"), "gemini_pro");
        assert_eq!(bucket_recap_model("gemini-2.5-flash"), "gemini_flash");
        assert_eq!(bucket_recap_model("gpt-5-mini-2024-07-18"), "gpt_5");
        assert_eq!(bucket_recap_model("gpt-4o-mini"), "gpt_4");
        assert_eq!(bucket_recap_model("llama-3.1-70b"), "llama");
        assert_eq!(bucket_recap_model("gemma-3-12b-it"), "gemma");
        assert_eq!(bucket_recap_model(""), "default");
        // Custom user-set model strings always fall to "other" so we
        // never fingerprint a workplace via a specific Together-AI
        // hosted model name.
        assert_eq!(bucket_recap_model("acme-corp/internal-model-v3"), "other");
    }

    /// Hard rule: the bucket label set is part of the PostHog
    /// dashboard contract. If a refactor accidentally returns a
    /// non-static string (e.g. via `format!`) the categorical type
    /// breaks and dashboards stop working. This compile-time test
    /// pins the return type as `&'static str`.
    #[test]
    fn bucket_helpers_return_static_str() {
        let _: &'static str = bucket_audio_secs(0.0);
        let _: &'static str = bucket_processing_ms(0);
        let _: &'static str = bucket_word_count(0);
        let _: &'static str = bucket_dict_size(0);
        let _: &'static str = bucket_app_rules(0);
        let _: &'static str = bucket_recap_model("");
    }
}

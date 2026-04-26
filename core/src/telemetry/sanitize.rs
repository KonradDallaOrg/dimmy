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
}

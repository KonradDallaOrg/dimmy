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
            // Groq answers 413 for BOTH a genuinely oversized request and
            // a per-minute token allowance that is already spent, and
            // telling the two apart would mean reading the response body
            // — which echoes the prompt back, i.e. the transcript. Its
            // own category, so the hint can cover both honestly without
            // anyone having to look. See `error.rs` on why bodies never
            // leave the machine.
            413 => "too_large",
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

/// Replace every user-directory NAME inside a free-text message with
/// `<USER>`, leaving the rest of the path intact.
///
/// `scrub_path` above handles a string that IS a path. This one handles
/// the far more common case: a path EMBEDDED in a sentence, which is what
/// every `format!("… {}", path.display())` error produces. Those strings
/// reach Sentry through `capture_error` and `before_send`, and until
/// 2026-09-02 they arrived intact — a real user's Windows account name
/// sat in an issue TITLE for 59 events:
/// `local model: model file not found: C:\Users\<name>\AppData\…`.
///
/// The rule is STRUCTURAL, not identity-based, and that is the point.
/// Matching `dirs::home_dir()` only redacts the home of the process that
/// happens to be reporting, compares case-sensitively (so it misses
/// `c:\users\…` against a `C:\Users\…` home), and says nothing about a
/// path belonging to somebody else. Recognising the SHAPE of a user
/// directory — `/Users/x`, `\Users\x`, `/home/x` — covers all three and
/// keeps working on a machine whose home sits somewhere unusual.
///
/// Everything except the name survives, so the message stays debuggable:
/// `C:\Users\<USER>\AppData\Roaming\dimmy\models\ggml-large-v3-q5_0.bin`.
pub fn scrub_user_paths(text: &str) -> String {
    // Matched case-insensitively: Windows paths round-trip through APIs
    // that change the case of both the drive letter and `Users`.
    const MARKERS: [&str; 2] = ["users", "home"];

    // `to_ascii_lowercase` remaps only A-Z, one byte in one byte out, so
    // `lower` and `text` share byte offsets and can be indexed together.
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < text.len() {
        // A marker counts only when it is a whole path segment: preceded
        // by a separator and followed by one. Without that, "chrome/" and
        // "homepage/" would both look like a home directory.
        let marker = if i > 0 && is_path_sep_byte(bytes[i - 1]) {
            MARKERS.iter().find(|m| {
                lower[i..].starts_with(**m)
                    && bytes.get(i + m.len()).is_some_and(|b| is_path_sep_byte(*b))
            })
        } else {
            None
        };

        let Some(marker) = marker else {
            let step = text[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            out.push_str(&text[i..i + step]);
            i += step;
            continue;
        };

        // Keep the marker and its separator verbatim (original case),
        // then swallow the single segment that follows: the account name.
        let name_start = i + marker.len() + 1;
        out.push_str(&text[i..name_start]);
        let name_end = text[name_start..]
            .find(|c: char| matches!(c, '/' | '\\') || c.is_whitespace())
            .map(|off| name_start + off)
            .unwrap_or(text.len());
        if name_end > name_start {
            out.push_str("<USER>");
        }
        i = name_end;
    }

    out
}

fn is_path_sep_byte(b: u8) -> bool {
    b == b'/' || b == b'\\'
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
///
/// The key-prefix checks match ANYWHERE in the string, per token. They
/// used to be `starts_with` against the whole input, which made them
/// close to useless at the two places that matter most: `client.rs`
/// hands this an entire serialised JSON document (it begins with `{`, so
/// no prefix could ever match, and the "last-ditch grep" was really
/// checking three of its nine patterns), and an error message embeds a
/// key mid-sentence rather than starting with one.
///
/// Per token, not `contains`, because a bare `contains("sk-")` fires on
/// ordinary words — "task-force" contains it — and a filter that redacts
/// innocent messages gets removed by the next person who trips over it.
pub fn looks_like_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.contains("api_key=")
        || lower.contains("api-key:")
        || lower.contains("authorization:")
        // Keeps its space, so it cannot survive tokenisation below.
        || lower.contains("bearer ")
    {
        return true;
    }
    s.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | ',' | ';' | ':' | '=' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
    })
    .any(token_looks_like_key)
}

/// A token is key-shaped when it carries a known vendor prefix followed
/// by enough payload to be an actual credential. The length floor keeps
/// a bare `sk-` or a truncated fragment from tripping the filter.
fn token_looks_like_key(token: &str) -> bool {
    const PREFIXES: [&str; 5] = ["sk-", "phc_", "phx_", "sntrys_", "gsk_"];
    const MIN_PAYLOAD: usize = 6;
    PREFIXES
        .iter()
        .any(|p| token.starts_with(p) && token.len() >= p.len() + MIN_PAYLOAD)
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

    /// The exact string that leaked. Sentry issue RUST-B carried a real
    /// user's Windows account name in its TITLE for 59 events.
    #[test]
    fn scrub_user_paths_redacts_the_string_that_actually_leaked() {
        let leaked = r"local model: model file not found: C:\Users\gregr\AppData\Roaming\dimmy\models\ggml-large-v3-q5_0.bin";
        let safe = scrub_user_paths(leaked);
        assert!(!safe.contains("gregr"), "account name survived: {safe}");
        assert_eq!(
            safe,
            r"local model: model file not found: C:\Users\<USER>\AppData\Roaming\dimmy\models\ggml-large-v3-q5_0.bin"
        );
    }

    #[test]
    fn scrub_user_paths_covers_every_platform_shape() {
        assert_eq!(
            scrub_user_paths("/Users/mario/Library/Application Support/dimmy/x.bin"),
            "/Users/<USER>/Library/Application Support/dimmy/x.bin"
        );
        assert_eq!(
            scrub_user_paths("/home/mario/.config/dimmy/config.json"),
            "/home/<USER>/.config/dimmy/config.json"
        );
        // Case-insensitive: the identity-based filter this replaced
        // compared with `starts_with` and missed exactly this.
        assert_eq!(
            scrub_user_paths(r"c:\users\GREGR\dimmy"),
            r"c:\users\<USER>\dimmy"
        );
    }

    #[test]
    fn scrub_user_paths_leaves_everything_that_is_not_an_account_name() {
        // A marker inside a word is not a path segment.
        assert_eq!(scrub_user_paths("chrome/tabs"), "chrome/tabs");
        assert_eq!(
            scrub_user_paths("see the homepage/index"),
            "see the homepage/index"
        );
        // Nothing to redact: unchanged, byte for byte.
        assert_eq!(scrub_user_paths("HTTP 413"), "HTTP 413");
        assert_eq!(scrub_user_paths(""), "");
        // A relative path is not somebody's home.
        assert_eq!(scrub_user_paths("users/list.json"), "users/list.json");
        // Trailing separator: no name follows, so nothing is invented.
        assert_eq!(scrub_user_paths("/home/"), "/home/");
    }

    #[test]
    fn scrub_user_paths_handles_several_paths_and_multibyte_text() {
        assert_eq!(
            scrub_user_paths(r"copy C:\Users\a\x to /home/b/y"),
            r"copy C:\Users\<USER>\x to /home/<USER>/y"
        );
        // Byte-indexed scanning must never split a UTF-8 character.
        let s = "però C:\\Users\\josé\\modèles — è finito";
        let out = scrub_user_paths(s);
        assert_eq!(out, "però C:\\Users\\<USER>\\modèles — è finito");
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

    /// The prefix checks used to be `starts_with` against the WHOLE
    /// input, so they never fired on the two shapes that actually carry
    /// a key: a serialised JSON payload (starts with `{`) and a key
    /// embedded mid-sentence.
    #[test]
    fn looks_like_secret_finds_keys_that_are_not_at_position_zero() {
        assert!(looks_like_secret("token sk-proj-abc123def456ghi789"));
        assert!(looks_like_secret(
            r#"{"event":"llm.failed","properties":{"k":"gsk_abcdefghij"}}"#
        ));
        assert!(looks_like_secret("failed to open (phc_owaPfYyvAinx)"));
    }

    /// A filter that redacts innocent messages gets deleted by the next
    /// person who trips over it, so the prefixes match per token.
    #[test]
    fn looks_like_secret_does_not_fire_on_ordinary_words() {
        assert!(!looks_like_secret("the task-force reviewed it"));
        assert!(!looks_like_secret("whisper-large-v3-turbo loaded"));
        assert!(!looks_like_secret("risk-free, ask-and-answer"));
        // Prefix with nothing behind it is not a credential.
        assert!(!looks_like_secret("sk-"));
        assert!(!looks_like_secret("gsk_ab"));
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

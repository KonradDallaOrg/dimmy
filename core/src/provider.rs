/// Provider detection from API URLs.
///
/// Replaces runtime string matching (`url.contains("groq.com")`) with an
/// exhaustive enum. Adding a provider forces handling in all `match` arms.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Groq,
    OpenAI,
    OpenRouter,
    Gemini,
    Deepgram,
    Anthropic,
    Custom,
    Local,
}

impl Provider {
    /// Detect provider from an API URL.
    pub fn from_url(url: &str) -> Self {
        // Default to Groq if URL is empty (e.g. first-launch with no config)
        if url.is_empty() {
            return Self::Groq;
        }
        if url.contains("groq.com") {
            Self::Groq
        } else if url.contains("openai.com") {
            Self::OpenAI
        } else if url.contains("openrouter.ai") {
            Self::OpenRouter
        } else if url.contains("googleapis.com") {
            Self::Gemini
        } else if url.contains("deepgram.com") {
            Self::Deepgram
        } else if url.contains("anthropic.com") {
            Self::Anthropic
        } else {
            Self::Custom
        }
    }

    /// Keyring entry name suffix (e.g. "groq", "openai").
    /// Used with a prefix like "api-key" or "llm-key" → "api-key-groq".
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Groq => "groq",
            Self::OpenAI => "openai",
            Self::OpenRouter => "openrouter",
            Self::Gemini => "gemini",
            Self::Deepgram => "deepgram",
            Self::Anthropic => "anthropic",
            Self::Custom => "custom",
            Self::Local => "local",
        }
    }

    /// Maximum file size in bytes this provider accepts for STT upload.
    /// Used to decide whether to chunk a long recording before sending.
    /// Returns a conservative 25MB default for unknown/custom providers.
    pub fn max_file_bytes(&self) -> usize {
        let limit = match self {
            Self::Deepgram => 2 * 1024 * 1024 * 1024, // 2 GB
            Self::Gemini => 20 * 1024 * 1024,         // 20 MB (inline_data limit)
            Self::Local => usize::MAX,                // In-memory, no upload limit
            _ => 25 * 1024 * 1024,                    // 25 MB (Groq, OpenAI, OpenRouter, etc.)
        };
        // Limit must be positive — a zero limit would prevent all uploads
        assert!(limit > 0, "max_file_bytes produced zero for {:?}", self);
        limit
    }

    /// Whether this provider uses the Anthropic Messages API format.
    pub fn is_anthropic(&self) -> bool {
        *self == Self::Anthropic
    }

    /// Whether this provider uses the Deepgram raw-body format.
    pub fn is_deepgram(&self) -> bool {
        *self == Self::Deepgram
    }

    /// Whether this provider uses the Gemini generateContent format.
    pub fn is_gemini(&self) -> bool {
        *self == Self::Gemini
    }

    /// Validate a URL for use as an API endpoint.
    /// Rejects: non-HTTPS (except localhost), dangerous schemes, null bytes,
    /// CRLF injection, credentials in URL, unparseable URLs.
    pub fn validate_url(url: &str) -> Result<(), String> {
        if url.is_empty() {
            return Err("URL is empty".into());
        }
        // Reject null bytes and CRLF (header injection)
        if url.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n') {
            return Err("URL contains forbidden bytes (null/CR/LF)".into());
        }
        let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;
        // Only allow http and https schemes
        match parsed.scheme() {
            "https" => {}
            "http" => {
                let host = parsed.host_str().unwrap_or("");
                if host != "localhost" && host != "127.0.0.1" && host != "::1" && host != "[::1]" {
                    return Err(format!(
                        "HTTP not allowed for remote host '{}' (use HTTPS)",
                        host
                    ));
                }
            }
            scheme => return Err(format!("Scheme '{}' not allowed (use HTTPS)", scheme)),
        }
        // Reject credentials in URL (could leak in logs/errors)
        if parsed.username() != "" || parsed.password().is_some() {
            return Err("URL must not contain credentials (user:pass@)".into());
        }
        Ok(())
    }

    /// Scrub an API key from a text string (e.g., error body).
    /// Replaces the full key and any prefix >=8 chars with [REDACTED].
    pub fn scrub_api_key(text: &str, key: &str) -> String {
        if key.len() < 4 {
            return text.to_string();
        }
        // Replace full key
        let mut result = text.replace(key, "[REDACTED]");
        // Also scrub any prefix of 8+ chars (partial key exposure)
        if key.len() >= 8 {
            let prefix = &key[..8];
            result = result.replace(prefix, "[REDACTED]");
        }
        result
    }

    /// Whether the URL uses HTTPS (or is localhost, which is exempt).
    pub fn is_secure_url(url: &str) -> bool {
        // URL must not be empty — caller should validate before reaching here
        assert!(!url.is_empty(), "is_secure_url called with empty URL");
        if let Ok(parsed) = url::Url::parse(url) {
            if parsed.scheme() == "http" {
                let host = parsed.host_str().unwrap_or("");
                // host_str() returns "::1" for http://[::1]:port
                return host == "localhost"
                    || host == "127.0.0.1"
                    || host == "::1"
                    || host == "[::1]";
            }
        }
        true // HTTPS or unparseable (will fail at request time)
    }
}

// ── Keyring scoping ──────────────────────────────────────────────────

/// Keyring entry scope — prevents mixing up STT and LLM keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyringScope {
    /// Transcription / STT API key
    Stt(Provider),
    /// LLM post-processing API key
    Llm(Provider),
}

impl KeyringScope {
    /// Keyring entry name (e.g. "api-key-groq", "llm-key-anthropic").
    pub fn entry_name(&self) -> String {
        let name = match self {
            Self::Stt(p) => format!("api-key-{}", p.as_str()),
            Self::Llm(p) => format!("llm-key-{}", p.as_str()),
        };
        // Entry name must be non-empty and contain a dash separator
        assert!(!name.is_empty(), "entry_name produced empty string");
        assert!(
            name.contains('-'),
            "entry_name missing dash separator: {}",
            name
        );
        name
    }

    /// The provider for this scope.
    pub fn provider(&self) -> Provider {
        match self {
            Self::Stt(p) | Self::Llm(p) => *p,
        }
    }
}

impl std::fmt::Display for KeyringScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.entry_name())
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_groq() {
        assert_eq!(
            Provider::from_url("https://api.groq.com/openai/v1/audio/transcriptions"),
            Provider::Groq
        );
    }

    #[test]
    fn detect_openai() {
        assert_eq!(
            Provider::from_url("https://api.openai.com/v1/audio/transcriptions"),
            Provider::OpenAI
        );
    }

    #[test]
    fn detect_openrouter() {
        assert_eq!(
            Provider::from_url("https://openrouter.ai/api/v1/chat/completions"),
            Provider::OpenRouter
        );
    }

    #[test]
    fn detect_gemini() {
        assert_eq!(
            Provider::from_url("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"),
            Provider::Gemini
        );
    }

    #[test]
    fn detect_deepgram() {
        assert_eq!(
            Provider::from_url("https://api.deepgram.com/v1/listen?model=nova-3"),
            Provider::Deepgram
        );
    }

    #[test]
    fn detect_anthropic() {
        assert_eq!(
            Provider::from_url("https://api.anthropic.com/v1/messages"),
            Provider::Anthropic
        );
    }

    #[test]
    fn detect_custom() {
        assert_eq!(
            Provider::from_url("https://my-custom-server.com/v1/transcriptions"),
            Provider::Custom
        );
    }

    #[test]
    fn as_str_matches_legacy() {
        // These strings must match what was hardcoded in has_key_for_provider calls
        assert_eq!(Provider::Groq.as_str(), "groq");
        assert_eq!(Provider::OpenAI.as_str(), "openai");
        assert_eq!(Provider::OpenRouter.as_str(), "openrouter");
        assert_eq!(Provider::Gemini.as_str(), "gemini");
        assert_eq!(Provider::Deepgram.as_str(), "deepgram");
        assert_eq!(Provider::Anthropic.as_str(), "anthropic");
        assert_eq!(Provider::Custom.as_str(), "custom");
    }

    #[test]
    fn secure_url_https() {
        assert!(Provider::is_secure_url("https://api.groq.com/v1"));
    }

    #[test]
    fn secure_url_localhost() {
        assert!(Provider::is_secure_url("http://localhost:8080/v1"));
        assert!(Provider::is_secure_url("http://127.0.0.1:8080/v1"));
        assert!(Provider::is_secure_url("http://[::1]:8080/v1"));
    }

    #[test]
    fn insecure_url_rejected() {
        assert!(!Provider::is_secure_url("http://api.groq.com/v1"));
    }

    #[test]
    fn keyring_scope_stt() {
        assert_eq!(
            KeyringScope::Stt(Provider::Groq).entry_name(),
            "api-key-groq"
        );
        assert_eq!(
            KeyringScope::Stt(Provider::OpenAI).entry_name(),
            "api-key-openai"
        );
    }

    #[test]
    fn keyring_scope_llm() {
        assert_eq!(
            KeyringScope::Llm(Provider::Anthropic).entry_name(),
            "llm-key-anthropic"
        );
        assert_eq!(
            KeyringScope::Llm(Provider::OpenRouter).entry_name(),
            "llm-key-openrouter"
        );
    }

    #[test]
    fn keyring_scope_provider() {
        assert_eq!(KeyringScope::Stt(Provider::Groq).provider(), Provider::Groq);
        assert_eq!(
            KeyringScope::Llm(Provider::Anthropic).provider(),
            Provider::Anthropic
        );
    }

    #[test]
    fn serde_roundtrip() {
        let provider = Provider::Groq;
        let json = serde_json::to_string(&provider).unwrap();
        assert_eq!(json, "\"groq\"");
        let back: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(back, provider);
    }

    // ── URL validation tests (TDD: these define the security contract) ──

    #[test]
    fn validate_url_accepts_valid_https() {
        assert!(Provider::validate_url("https://api.groq.com/v1/audio").is_ok());
        assert!(Provider::validate_url("https://api.openai.com/v1/audio").is_ok());
    }

    #[test]
    fn validate_url_accepts_localhost_http() {
        assert!(Provider::validate_url("http://localhost:8080/v1").is_ok());
        assert!(Provider::validate_url("http://127.0.0.1:5000/v1").is_ok());
        assert!(Provider::validate_url("http://[::1]:8080/v1").is_ok());
    }

    #[test]
    fn validate_url_rejects_insecure_http() {
        assert!(Provider::validate_url("http://api.groq.com/v1").is_err());
        assert!(Provider::validate_url("http://evil.com/steal").is_err());
    }

    #[test]
    fn validate_url_rejects_dangerous_schemes() {
        assert!(Provider::validate_url("javascript:alert(1)").is_err());
        assert!(Provider::validate_url("file:///etc/passwd").is_err());
        assert!(Provider::validate_url("ftp://files.com/data").is_err());
        assert!(Provider::validate_url("data:text/html,<h1>hi</h1>").is_err());
    }

    #[test]
    fn validate_url_rejects_null_bytes() {
        assert!(Provider::validate_url("https://api.groq.com/v1\0injected").is_err());
    }

    #[test]
    fn validate_url_rejects_crlf_injection() {
        assert!(Provider::validate_url("https://api.groq.com/v1\r\nX-Injected: true").is_err());
    }

    #[test]
    fn validate_url_rejects_empty_and_garbage() {
        assert!(Provider::validate_url("").is_err());
        assert!(Provider::validate_url("not a url at all").is_err());
        assert!(Provider::validate_url("://missing-scheme").is_err());
    }

    #[test]
    fn validate_url_rejects_credentials_in_url() {
        // user:password@ in URL could leak credentials in logs
        assert!(Provider::validate_url("https://user:pass@api.groq.com/v1").is_err());
    }

    // ── API key scrubbing tests ──

    #[test]
    fn scrub_key_removes_key_from_text() {
        let key = "sk-proj-abc123xyz";
        let body = format!("Error: invalid key {}", key);
        let scrubbed = Provider::scrub_api_key(&body, key);
        assert!(
            !scrubbed.contains(key),
            "Key should be scrubbed: {}",
            scrubbed
        );
        assert!(scrubbed.contains("[REDACTED]"));
    }

    #[test]
    fn scrub_key_noop_when_key_absent() {
        let body = "Error: rate limit exceeded";
        let scrubbed = Provider::scrub_api_key(body, "sk-secret-key");
        assert_eq!(scrubbed, body);
    }

    #[test]
    fn scrub_key_handles_empty_key() {
        let body = "Error: something";
        let scrubbed = Provider::scrub_api_key(body, "");
        assert_eq!(scrubbed, body);
    }

    #[test]
    fn scrub_key_handles_partial_key_prefix() {
        // Even if only first 8 chars of key appear, should scrub
        let key = "sk-proj-abc123xyz789longkey";
        let body = "Auth: sk-proj-abc123xyz789longkey failed";
        let scrubbed = Provider::scrub_api_key(&body, key);
        assert!(
            !scrubbed.contains("sk-proj-abc"),
            "Partial key leaked: {}",
            scrubbed
        );
    }

    // ── Provider file size limits (TDD: these define the upload contract) ──

    #[test]
    fn provider_file_limits_known_providers() {
        assert_eq!(Provider::Groq.max_file_bytes(), 25 * 1024 * 1024);
        assert_eq!(Provider::OpenAI.max_file_bytes(), 25 * 1024 * 1024);
        assert_eq!(Provider::OpenRouter.max_file_bytes(), 25 * 1024 * 1024);
        assert_eq!(Provider::Deepgram.max_file_bytes(), 2 * 1024 * 1024 * 1024);
        assert_eq!(Provider::Gemini.max_file_bytes(), 20 * 1024 * 1024);
        assert_eq!(Provider::Anthropic.max_file_bytes(), 25 * 1024 * 1024);
        assert_eq!(Provider::Custom.max_file_bytes(), 25 * 1024 * 1024);
    }

    #[test]
    fn provider_file_limits_are_positive() {
        // Every provider must have a positive, non-zero limit
        for provider in [
            Provider::Groq,
            Provider::OpenAI,
            Provider::OpenRouter,
            Provider::Gemini,
            Provider::Deepgram,
            Provider::Anthropic,
            Provider::Custom,
        ] {
            assert!(
                provider.max_file_bytes() > 0,
                "{:?} has zero file limit",
                provider
            );
        }
    }

    #[test]
    fn deepgram_limit_much_larger_than_others() {
        // Deepgram's 2GB limit must be significantly larger than the 25MB default
        assert!(
            Provider::Deepgram.max_file_bytes() > Provider::Groq.max_file_bytes() * 10,
            "Deepgram limit should be >> Groq limit"
        );
    }

    #[test]
    fn local_provider_basics() {
        assert_eq!(Provider::Local.as_str(), "local");
        // Local provider has no URL — from_url should never return Local
        assert_ne!(Provider::from_url("http://localhost:8080"), Provider::Local);
        // Local provider has no max file size limit (processed in-memory)
        assert_eq!(Provider::Local.max_file_bytes(), usize::MAX);
    }
}

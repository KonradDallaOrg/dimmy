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
}

impl Provider {
    /// Detect provider from an API URL.
    pub fn from_url(url: &str) -> Self {
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
        }
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

    /// Whether the URL uses HTTPS (or is localhost, which is exempt).
    pub fn is_secure_url(url: &str) -> bool {
        if let Ok(parsed) = url::Url::parse(url) {
            if parsed.scheme() == "http" {
                let host = parsed.host_str().unwrap_or("");
                // host_str() returns "::1" for http://[::1]:port
                return host == "localhost" || host == "127.0.0.1" || host == "::1"
                    || host == "[::1]";
            }
        }
        true // HTTPS or unparseable (will fail at request time)
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
    fn serde_roundtrip() {
        let provider = Provider::Groq;
        let json = serde_json::to_string(&provider).unwrap();
        assert_eq!(json, "\"groq\"");
        let back: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(back, provider);
    }
}

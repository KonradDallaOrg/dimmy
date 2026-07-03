//! Typed error hierarchy for Dimmy.
//!
//! Replaces String-based errors with structured types that can be matched,
//! composed with `?`, and automatically serialized for FFI consumers.

use serde::Serialize;

/// Top-level error type for all Dimmy operations.
#[derive(Debug)]
pub enum DimmyError {
    Audio(AudioError),
    Transcribe(TranscribeError),
    Llm(LlmError),
    Config(String),
    InvalidState(String),
    Platform(String),
}

#[derive(Debug)]
pub enum AudioError {
    NoDevice,
    DeviceNotFound(String),
    Capture(String),
    Encode(String),
}

#[derive(Debug)]
pub enum TranscribeError {
    NoApiKey(String),
    Api { status: u16, body: String },
    Empty,
    Network(String),
    InsecureUrl(String),
    LocalModel(String),
}

#[derive(Debug)]
pub enum LlmError {
    Api {
        status: u16,
        body: String,
    },
    Network(String),
    NoApiKey(String),
    LocalModel(String),
    /// The model declined the request (Anthropic `stop_reason: "refusal"`,
    /// Fable 5+). Not an infrastructure failure — retrying the same input
    /// won't help, rewording it may.
    Refusal,
}

// ── Display implementations ────────────────────────────────────────

impl std::fmt::Display for DimmyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Audio(e) => write!(f, "audio: {}", e),
            Self::Transcribe(e) => write!(f, "transcribe: {}", e),
            Self::Llm(e) => write!(f, "llm: {}", e),
            Self::Config(msg) => write!(f, "config: {}", msg),
            Self::InvalidState(msg) => write!(f, "invalid state: {}", msg),
            Self::Platform(msg) => write!(f, "platform: {}", msg),
        }
    }
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no input device available"),
            Self::DeviceNotFound(name) => write!(f, "device '{}' not found", name),
            Self::Capture(msg) => write!(f, "capture failed: {}", msg),
            Self::Encode(msg) => write!(f, "encoding failed: {}", msg),
        }
    }
}

impl std::fmt::Display for TranscribeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoApiKey(provider) => write!(f, "no API key configured for {}", provider),
            // SECURITY: API error response bodies often echo back the
            // request (= the transcript / prompt = user content). The
            // first 200 chars of an OpenAI 400 response, for example,
            // includes a JSON-quoted prompt fragment. Display MUST NOT
            // leak that body downstream — `capture_error` -> Sentry
            // would ship it across the wire. The body still lands in
            // the local `dimmy.log` via a separate `log()` call site
            // for offline debugging. Burned 2026-05-12: a Sentry panic
            // surfaced part of a transcribed chat in the message.
            Self::Api { status, .. } => write!(f, "HTTP {}", status),
            Self::Empty => write!(f, "empty transcription"),
            Self::Network(msg) => write!(f, "request failed: {}", msg),
            Self::InsecureUrl(url) => {
                write!(f, "refusing HTTP (HTTPS required): {}", url)
            }
            Self::LocalModel(msg) => write!(f, "local model: {}", msg),
        }
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // SECURITY: see TranscribeError::Api — same redaction rule.
            // LLM API error bodies are even worse than STT bodies: they
            // contain the FULL prompt (= transcript + system prompt),
            // because the LLM dispatch path inlines the whole chat
            // payload. Stripping the body in Display is the choke point
            // that keeps it out of telemetry.
            Self::Api { status, .. } => write!(f, "HTTP {}", status),
            Self::Network(msg) => write!(f, "request failed: {}", msg),
            Self::NoApiKey(provider) => write!(f, "no API key for LLM provider {}", provider),
            Self::LocalModel(msg) => write!(f, "local LLM model: {}", msg),
            Self::Refusal => write!(f, "the model declined this request (safety refusal)"),
        }
    }
}

impl std::error::Error for DimmyError {}
impl std::error::Error for AudioError {}
impl std::error::Error for TranscribeError {}
impl std::error::Error for LlmError {}

// ── From conversions ───────────────────────────────────────────────

impl From<AudioError> for DimmyError {
    fn from(e: AudioError) -> Self {
        Self::Audio(e)
    }
}

impl From<TranscribeError> for DimmyError {
    fn from(e: TranscribeError) -> Self {
        Self::Transcribe(e)
    }
}

impl From<LlmError> for DimmyError {
    fn from(e: LlmError) -> Self {
        Self::Llm(e)
    }
}

// ── Serialize needed for JSON error payloads over FFI ───────────────

impl Serialize for DimmyError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

// ── Convenience: convert Box<dyn Error> to our typed errors ────────

impl From<Box<dyn std::error::Error>> for TranscribeError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Self::Network(e.to_string())
    }
}

impl From<Box<dyn std::error::Error>> for LlmError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Self::Network(e.to_string())
    }
}

impl From<Box<dyn std::error::Error>> for AudioError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Self::Encode(e.to_string())
    }
}

// hound errors → AudioError::Encode
impl From<hound::Error> for AudioError {
    fn from(e: hound::Error) -> Self {
        Self::Encode(e.to_string())
    }
}

// String → TranscribeError::Network (for misc string errors)
impl From<String> for TranscribeError {
    fn from(e: String) -> Self {
        Self::Network(e)
    }
}

// reqwest errors → Network
impl From<reqwest::Error> for TranscribeError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

// serde_json errors → Network (response parsing failures)
impl From<serde_json::Error> for TranscribeError {
    fn from(e: serde_json::Error) -> Self {
        Self::Network(format!("JSON parse error: {}", e))
    }
}

impl From<serde_json::Error> for LlmError {
    fn from(e: serde_json::Error) -> Self {
        Self::Network(format!("JSON parse error: {}", e))
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimmy_error_display() {
        let e = DimmyError::Audio(AudioError::NoDevice);
        assert_eq!(e.to_string(), "audio: no input device available");
    }

    #[test]
    fn transcribe_error_display_strips_body() {
        // Privacy hard-rule: Display MUST NOT include the body (it
        // can echo back transcript content from the upstream API).
        // The body still lives on the struct field so caller can
        // log it locally if needed — just never via Display.
        let e = TranscribeError::Api {
            status: 401,
            body: "Unauthorized — your transcript said xyz".into(),
        };
        assert_eq!(e.to_string(), "HTTP 401");
        assert!(!e.to_string().contains("transcript"));
        assert!(!e.to_string().contains("xyz"));
    }

    #[test]
    fn llm_error_display_strips_body() {
        // Same privacy contract as TranscribeError::Api — LLM API
        // error bodies often echo the prompt, which contains the
        // transcript. Display must redact.
        let e = LlmError::Api {
            status: 429,
            body: "Rate limited — prompt was 'meeting transcript: ...'".into(),
        };
        assert_eq!(e.to_string(), "HTTP 429");
        assert!(!e.to_string().contains("prompt"));
        assert!(!e.to_string().contains("transcript"));
    }

    #[test]
    fn llm_error_display() {
        let e = LlmError::NoApiKey("groq".into());
        assert_eq!(e.to_string(), "no API key for LLM provider groq");
    }

    #[test]
    fn llm_local_model_error_display() {
        let e = LlmError::LocalModel("model not found".into());
        assert!(
            e.to_string().contains("local LLM model"),
            "LlmError::LocalModel display must contain 'local LLM model': {}",
            e
        );
    }

    #[test]
    fn from_audio_to_dimmy() {
        let e: DimmyError = AudioError::NoDevice.into();
        assert!(matches!(e, DimmyError::Audio(AudioError::NoDevice)));
    }

    #[test]
    fn serialize_for_ffi() {
        let e = DimmyError::InvalidState("not recording".into());
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, "\"invalid state: not recording\"");
    }
}

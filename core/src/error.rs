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
    Api { status: u16, body: String },
    Network(String),
    NoApiKey(String),
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
            Self::Api { status, body } => write!(f, "HTTP {}: {}", status, body),
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
            Self::Api { status, body } => write!(f, "HTTP {}: {}", status, body),
            Self::Network(msg) => write!(f, "request failed: {}", msg),
            Self::NoApiKey(provider) => write!(f, "no API key for LLM provider {}", provider),
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
    fn transcribe_error_display() {
        let e = TranscribeError::Api {
            status: 401,
            body: "Unauthorized".into(),
        };
        assert_eq!(e.to_string(), "HTTP 401: Unauthorized");
    }

    #[test]
    fn llm_error_display() {
        let e = LlmError::NoApiKey("groq".into());
        assert_eq!(e.to_string(), "no API key for LLM provider groq");
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

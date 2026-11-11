//! Whether a meeting's transcriber is producing text, as one explicit state.
//!
//! Before this, a window that failed to transcribe became an empty string,
//! exactly like a window of silence. A meeting whose engine never loaded
//! (Nemotron refused for memory, 2026-11-09) or whose key was wrong left a
//! folder identical to a silent one, and the host blamed the VAD.
//!
//! Owned by the transcription thread (`meeting::stt_thread_loop`); the
//! capture worker never touches it (THE AUDIO RULE). Pure: the caller turns
//! the returned [`Notice`] into an event.

/// Why the transcriber produces no text. The code is what hosts map to a
/// message; it never carries user content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NotEnoughMemory,
    ModelMissing,
    NoApiKey,
    Auth,
    Network,
    RateLimit,
    EngineError,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::NotEnoughMemory => "not_enough_memory",
            Reason::ModelMissing => "model_missing",
            Reason::NoApiKey => "no_api_key",
            Reason::Auth => "auth",
            Reason::Network => "network",
            Reason::RateLimit => "rate_limit",
            Reason::EngineError => "engine_error",
        }
    }

    /// From an engine's error text. Unknown text is an engine error: the
    /// point is that it is a failure, not silence.
    pub fn classify(message: &str) -> Reason {
        let lower = message.to_ascii_lowercase();
        if lower.contains("not enough free memory") || lower.contains("out of memory") {
            Reason::NotEnoughMemory
        } else if lower.contains("not downloaded")
            || lower.contains("no usable")
            || lower.contains("model not found")
            || lower.contains("bundle missing")
        {
            Reason::ModelMissing
        } else if lower.contains("missing key") || lower.contains("no api key") {
            Reason::NoApiKey
        } else if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized") {
            Reason::Auth
        } else if lower.contains("429") || lower.contains("rate limit") {
            Reason::RateLimit
        } else if lower.contains("error sending request") {
            Reason::Network
        } else {
            match crate::telemetry::sanitize::error_category(message, None) {
                "network" | "timeout" => Reason::Network,
                _ => Reason::EngineError,
            }
        }
    }
}

/// What one transcription window produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Text,
    Silent,
    Failed(Reason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Live,
    Unavailable(Reason),
}

/// A transition the host must hear about. One per transition, never repeated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Unavailable(Reason),
    Recovered,
}

/// How the meeting's transcript ended up, written into `meta.json` so a
/// failed meeting can never again read as a silent one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// At least one window produced text.
    Ok,
    /// Windows were transcribed and every one was silence.
    Silent,
    /// No window produced text and at least one failed.
    Failed(Reason),
    /// No window reached the transcriber at all (a meeting of a second).
    Empty,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Silent => "silent",
            Status::Failed(_) => "failed",
            Status::Empty => "empty",
        }
    }

    pub fn reason(self) -> Option<Reason> {
        match self {
            Status::Failed(r) => Some(r),
            _ => None,
        }
    }
}

/// Failed windows in a row before the user is told. One failure can be a
/// passing network blip; two (30 s of a default meeting) is a pattern.
const FAILURES_BEFORE_UNAVAILABLE: u32 = 2;

#[derive(Debug)]
pub struct TranscriberHealth {
    health: Health,
    failures_in_a_row: u32,
    last_failure: Option<Reason>,
    /// A streaming engine (Nemotron) can close its last lines only at stop.
    text_at_finish: bool,
    pub windows_text: u32,
    pub windows_silent: u32,
    pub windows_failed: u32,
}

impl Default for TranscriberHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriberHealth {
    pub fn new() -> Self {
        Self {
            health: Health::Live,
            failures_in_a_row: 0,
            last_failure: None,
            text_at_finish: false,
            windows_text: 0,
            windows_silent: 0,
            windows_failed: 0,
        }
    }

    pub fn health(&self) -> Health {
        self.health
    }

    /// The engine could not start at all: no window will ever produce text.
    pub fn refused(&mut self, reason: Reason) -> Option<Notice> {
        self.last_failure = Some(reason);
        self.become_unavailable(reason)
    }

    pub fn observe(&mut self, window: Window) -> Option<Notice> {
        let before = self.windows_text + self.windows_silent + self.windows_failed;
        let notice = match window {
            Window::Text => {
                self.windows_text += 1;
                self.failures_in_a_row = 0;
                match self.health {
                    Health::Unavailable(_) => {
                        self.health = Health::Live;
                        Some(Notice::Recovered)
                    }
                    Health::Live => None,
                }
            }
            Window::Silent => {
                // Silence says nothing about the engine either way.
                self.windows_silent += 1;
                None
            }
            Window::Failed(reason) => {
                self.windows_failed += 1;
                self.failures_in_a_row += 1;
                self.last_failure = Some(reason);
                if self.failures_in_a_row >= FAILURES_BEFORE_UNAVAILABLE {
                    self.become_unavailable(reason)
                } else {
                    None
                }
            }
        };
        assert_eq!(
            self.windows_text + self.windows_silent + self.windows_failed,
            before + 1,
            "every window is counted exactly once"
        );
        notice
    }

    fn become_unavailable(&mut self, reason: Reason) -> Option<Notice> {
        match self.health {
            Health::Unavailable(_) => None,
            Health::Live => {
                self.health = Health::Unavailable(reason);
                Some(Notice::Unavailable(reason))
            }
        }
    }

    /// The transcript file has lines once the transcriber finished.
    pub fn finished_with_text(&mut self) {
        self.text_at_finish = true;
    }

    pub fn status(&self) -> Status {
        if self.windows_text > 0 || self.text_at_finish {
            Status::Ok
        } else if let Some(r) = self.last_failure {
            Status::Failed(r)
        } else if self.windows_silent > 0 {
            Status::Silent
        } else {
            Status::Empty
        }
    }

    /// The `transcript` object of `meta.json`.
    pub fn to_meta(&self, engine: &str) -> serde_json::Value {
        let status = self.status();
        serde_json::json!({
            "status": status.code(),
            "engine": engine,
            "reason": status.reason().map(Reason::code),
            "windows_text": self.windows_text,
            "windows_silent": self.windows_silent,
            "windows_failed": self.windows_failed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_failure_is_not_announced() {
        let mut h = TranscriberHealth::new();
        assert_eq!(h.observe(Window::Failed(Reason::Network)), None);
        assert_eq!(h.health(), Health::Live);
        assert_eq!(h.observe(Window::Text), None);
        assert_eq!(h.status(), Status::Ok);
    }

    #[test]
    fn two_failures_in_a_row_announce_once() {
        let mut h = TranscriberHealth::new();
        assert_eq!(h.observe(Window::Failed(Reason::Auth)), None);
        assert_eq!(
            h.observe(Window::Failed(Reason::Auth)),
            Some(Notice::Unavailable(Reason::Auth))
        );
        assert_eq!(h.observe(Window::Failed(Reason::Auth)), None, "said once");
        assert_eq!(h.health(), Health::Unavailable(Reason::Auth));
    }

    #[test]
    fn silence_between_failures_keeps_the_streak() {
        let mut h = TranscriberHealth::new();
        h.observe(Window::Failed(Reason::Network));
        h.observe(Window::Silent);
        assert_eq!(
            h.observe(Window::Failed(Reason::Network)),
            Some(Notice::Unavailable(Reason::Network))
        );
    }

    #[test]
    fn text_after_unavailable_recovers_once() {
        let mut h = TranscriberHealth::new();
        h.observe(Window::Failed(Reason::Network));
        h.observe(Window::Failed(Reason::Network));
        assert_eq!(h.observe(Window::Text), Some(Notice::Recovered));
        assert_eq!(h.observe(Window::Text), None);
        assert_eq!(h.health(), Health::Live);
        // And it can fail again later, and be told again.
        h.observe(Window::Failed(Reason::Network));
        assert_eq!(
            h.observe(Window::Failed(Reason::Network)),
            Some(Notice::Unavailable(Reason::Network))
        );
    }

    #[test]
    fn an_engine_that_never_loaded_is_failed_not_silent() {
        // 2026-11-09: Nemotron refused for memory, every track None, 20 min
        // of audio and 0 bytes of text.
        let mut h = TranscriberHealth::new();
        assert_eq!(
            h.refused(Reason::NotEnoughMemory),
            Some(Notice::Unavailable(Reason::NotEnoughMemory))
        );
        assert_eq!(h.status(), Status::Failed(Reason::NotEnoughMemory));
        let meta = h.to_meta("nemotron");
        assert_eq!(meta["status"], "failed");
        assert_eq!(meta["reason"], "not_enough_memory");
    }

    #[test]
    fn status_tells_failure_silence_and_nothing_apart() {
        let mut silent = TranscriberHealth::new();
        silent.observe(Window::Silent);
        assert_eq!(silent.status(), Status::Silent);
        assert_eq!(silent.to_meta("whisper")["reason"], serde_json::Value::Null);

        let mut failed = TranscriberHealth::new();
        failed.observe(Window::Silent);
        failed.observe(Window::Failed(Reason::ModelMissing));
        assert_eq!(failed.status(), Status::Failed(Reason::ModelMissing));

        assert_eq!(TranscriberHealth::new().status(), Status::Empty);
    }

    #[test]
    fn lines_closed_only_at_stop_count_as_text() {
        let mut h = TranscriberHealth::new();
        h.observe(Window::Silent);
        h.finished_with_text();
        assert_eq!(h.status(), Status::Ok);
    }

    #[test]
    fn classify_maps_the_engine_messages_we_know() {
        let cases = [
            (
                "not enough free memory to load Nemotron (4700 MB needed)",
                Reason::NotEnoughMemory,
            ),
            ("Nemotron is not downloaded", Reason::ModelMissing),
            (
                "local STT skipped: no usable .bin model",
                Reason::ModelMissing,
            ),
            ("missing key or runtime", Reason::NoApiKey),
            ("HTTP 401 Unauthorized", Reason::Auth),
            ("HTTP 429 Too Many Requests", Reason::RateLimit),
            ("error sending request: connection refused", Reason::Network),
            ("operation timed out", Reason::Network),
            (
                "request failed: error sending request for url (https://x/)",
                Reason::Network,
            ),
            ("onnx session run failed", Reason::EngineError),
        ];
        for (message, want) in cases {
            assert_eq!(Reason::classify(message), want, "{message}");
        }
    }
}

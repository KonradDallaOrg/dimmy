//! Bridge between Rust AppState and GTK main loop.
//!
//! AppEvent enum carries typed events from background threads (tokio)
//! to the GTK main loop via glib::Sender.

/// Events sent from background threads to the GTK main loop.
#[derive(Debug, Clone)]
pub enum AppEvent {
    RecordingStarted,
    RecordingStopped,
    AmplitudeUpdate(f32),
    TranscriptionProgress { current: u32, total: u32 },
    TranscriptionComplete(String),
    LlmComplete(String),
    Error(String),
    StyleChanged(String),
    ToneChanged(String),
}

/// Create a glib channel pair for AppEvents.
pub fn create_event_channel() -> (
    gtk4::glib::Sender<AppEvent>,
    gtk4::glib::Receiver<AppEvent>,
) {
    gtk4::glib::MainContext::channel(gtk4::glib::Priority::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_event_is_send_and_clone() {
        fn assert_send<T: Send>() {}
        fn assert_clone<T: Clone>() {}
        assert_send::<AppEvent>();
        assert_clone::<AppEvent>();
    }

    #[test]
    fn app_event_debug_format() {
        let event = AppEvent::TranscriptionComplete("hello".to_string());
        let debug = format!("{:?}", event);
        assert!(debug.contains("hello"));
    }

    #[test]
    fn app_event_amplitude_range() {
        let event = AppEvent::AmplitudeUpdate(0.5);
        match event {
            AppEvent::AmplitudeUpdate(v) => {
                assert!(v >= 0.0 && v <= 1.0);
            }
            _ => panic!("wrong variant"),
        }
    }
}

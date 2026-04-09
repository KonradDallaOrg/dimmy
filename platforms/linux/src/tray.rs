//! System tray via StatusNotifierItem (D-Bus).
//!
//! Uses ksni crate. Works on:
//! - GNOME: requires AppIndicator extension (pre-installed on Ubuntu 24.04+)
//! - KDE: native StatusNotifierItem support
//! - XFCE/Cinnamon: native support

use std::sync::{Arc, Mutex};

/// Current visual state of the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Recording,
    Transcribing,
    Done,
}

struct DimmyTray {
    state: Arc<Mutex<TrayState>>,
    language: Arc<Mutex<String>>,
    llm_style: Arc<Mutex<String>>,
    shortcut: Arc<Mutex<String>>,
}

impl ksni::Tray for DimmyTray {
    fn id(&self) -> String {
        "dimmy".to_string()
    }

    fn title(&self) -> String {
        "Dimmy".to_string()
    }

    fn icon_name(&self) -> String {
        match *self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            TrayState::Idle => "audio-input-microphone-symbolic".to_string(),
            TrayState::Recording => "media-record-symbolic".to_string(),
            TrayState::Transcribing => "emblem-synchronizing-symbolic".to_string(),
            TrayState::Done => "emblem-ok-symbolic".to_string(),
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let state_label = match *self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            TrayState::Idle => "Ready",
            TrayState::Recording => "Recording...",
            TrayState::Transcribing => "Transcribing...",
            TrayState::Done => "Done",
        };
        ksni::ToolTip {
            title: format!("Dimmy \u{2014} {}", state_label),
            description: String::new(),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let state_label = match *self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            TrayState::Idle => "\u{25cf} Ready",
            TrayState::Recording => "\u{25cf} Recording...",
            TrayState::Transcribing => "\u{25cf} Transcribing...",
            TrayState::Done => "\u{25cf} Done",
        };
        let lang = self
            .language
            .lock()
            .map(|l| l.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        let style = self
            .llm_style
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        let shortcut = self
            .shortcut
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());

        vec![
            // Status (read-only)
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: state_label.to_string(),
                enabled: false,
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            // Info items (read-only)
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: format!(
                    "Language: {}",
                    if lang.is_empty() { "auto" } else { &lang }
                ),
                enabled: false,
                ..Default::default()
            }),
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: format!(
                    "Style: {}",
                    if style.is_empty() || style == "off" {
                        "off"
                    } else {
                        &style
                    }
                ),
                enabled: false,
                ..Default::default()
            }),
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: format!("Shortcut: {}", shortcut),
                enabled: false,
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            // Actions
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Show/Hide Pill".to_string(),
                activate: Box::new(|_| {
                    dimmy_lib::log("Tray: Show/Hide Pill clicked");
                    // TODO: Toggle pill visibility via GTK signal
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Settings\u{2026}".to_string(),
                activate: Box::new(|_| {
                    dimmy_lib::log("Tray: Settings clicked");
                    // TODO: Open settings window
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit Dimmy".to_string(),
                activate: Box::new(|_| {
                    dimmy_lib::log("Tray: Quit clicked");
                    std::process::exit(0);
                }),
                ..Default::default()
            }),
        ]
    }
}

/// Handle returned by [`start_tray`]. Keeps the tray alive as long as it exists.
pub struct TrayHandle {
    _handle: ksni::Handle<DimmyTray>,
    state: Arc<Mutex<TrayState>>,
}

impl TrayHandle {
    /// Update the tray icon/tooltip state. The icon will refresh on next
    /// menu interaction (ksni limitation — no push-based icon refresh).
    pub fn update_state(&self, new_state: TrayState) {
        assert!(
            [
                TrayState::Idle,
                TrayState::Recording,
                TrayState::Transcribing,
                TrayState::Done
            ]
            .contains(&new_state),
            "TrayState must be a known variant"
        );
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = new_state;
    }
}

/// Start the system tray (StatusNotifierItem via D-Bus).
///
/// Returns `Some(TrayHandle)` on success, `None` if D-Bus is unavailable
/// (e.g., headless environment, missing SNI host). The app continues without
/// a tray in that case.
///
/// Must be called after GTK initialization — ksni spawns its own thread.
pub fn start_tray(app_state: &Arc<dimmy_lib::AppState>) -> Option<TrayHandle> {
    let tray_state = Arc::new(Mutex::new(TrayState::Idle));

    let language = Arc::new(Mutex::new(
        app_state
            .language
            .lock()
            .map(|l| l.clone())
            .unwrap_or_else(|e| e.into_inner().clone()),
    ));
    let llm_style = Arc::new(Mutex::new(
        app_state
            .llm_style
            .lock()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|_| "off".to_string()),
    ));
    let shortcut = Arc::new(Mutex::new(
        app_state
            .shortcut
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone()),
    ));

    let tray = DimmyTray {
        state: tray_state.clone(),
        language,
        llm_style,
        shortcut,
    };

    let service = ksni::TrayService::new(tray);
    let handle = service.handle();
    service.spawn();
    dimmy_lib::log("System tray started (StatusNotifierItem)");
    Some(TrayHandle {
        _handle: handle,
        state: tray_state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_state_equality() {
        assert_eq!(TrayState::Idle, TrayState::Idle);
        assert_ne!(TrayState::Idle, TrayState::Recording);
        assert_ne!(TrayState::Recording, TrayState::Transcribing);
        assert_ne!(TrayState::Transcribing, TrayState::Done);
    }

    #[test]
    fn tray_state_is_debug() {
        let s = format!("{:?}", TrayState::Recording);
        assert!(s.contains("Recording"));
        let s2 = format!("{:?}", TrayState::Idle);
        assert!(s2.contains("Idle"));
    }

    #[test]
    fn tray_state_is_copy() {
        let a = TrayState::Done;
        let b = a; // Copy
        assert_eq!(a, b);
    }
}

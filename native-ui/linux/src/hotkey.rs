//! Global hotkeys for Linux.
//!
//! Wayland: xdg-desktop-portal GlobalShortcuts (ashpd)
//! X11: XGrabKey via x11rb (when x11-fallback feature enabled)

use dimmy_lib::log;

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

/// Detect which hotkey backend to use based on display server.
pub fn detect_hotkey_backend() -> &'static str {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => {
            log("Hotkey backend: xdg-desktop-portal (Wayland)");
            "portal"
        }
        _ => {
            log("Hotkey backend: XGrabKey (X11)");
            "x11"
        }
    }
}

/// Register a global hotkey. Events are sent via the glib Sender.
/// Returns Ok(()) if registration succeeds, Err with message if not.
///
/// NOTE: This is a stub. Full implementation requires a Linux desktop
/// environment for testing portal/X11 interaction.
pub fn register_hotkey(
    _shortcut: &str,
    _sender: gtk4::glib::Sender<HotkeyEvent>,
) -> Result<(), String> {
    let backend = detect_hotkey_backend();
    log(&format!(
        "register_hotkey: backend={}, stub — not yet implemented",
        backend
    ));
    Ok(())
}

/// Unregister the currently active global hotkey.
pub fn unregister_hotkey() {
    log("unregister_hotkey: stub — not yet implemented");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_backend_returns_valid() {
        let backend = detect_hotkey_backend();
        assert!(backend == "portal" || backend == "x11");
    }

    #[test]
    fn hotkey_event_is_clone_and_debug() {
        let event = HotkeyEvent::Pressed;
        let cloned = event.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("Pressed"));
    }
}

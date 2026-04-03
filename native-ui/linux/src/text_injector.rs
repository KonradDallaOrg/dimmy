//! Text injection: copy to clipboard + simulate Ctrl+V.
//!
//! Wayland: wtype (primary) or ydotool (fallback)
//! X11: xdotool
//! Last resort: clipboard-only (user pastes manually)

use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PasteMethod {
    Wtype,
    Ydotool,
    Xdotool,
    ClipboardOnly,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplayServer {
    Wayland,
    X11,
    Unknown,
}

pub fn detect_display_server() -> DisplayServer {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => DisplayServer::Wayland,
        Ok("x11") => DisplayServer::X11,
        _ => {
            if std::env::var("WAYLAND_DISPLAY").is_ok() {
                DisplayServer::Wayland
            } else {
                DisplayServer::X11
            }
        }
    }
}

fn tool_available(name: &str) -> bool {
    assert!(!name.is_empty(), "tool_available: name must not be empty");
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn detect_paste_method(display: DisplayServer) -> PasteMethod {
    match display {
        DisplayServer::Wayland => {
            if tool_available("wtype") {
                PasteMethod::Wtype
            } else if tool_available("ydotool") {
                PasteMethod::Ydotool
            } else {
                PasteMethod::ClipboardOnly
            }
        }
        DisplayServer::X11 | DisplayServer::Unknown => {
            if tool_available("xdotool") {
                PasteMethod::Xdotool
            } else {
                PasteMethod::ClipboardOnly
            }
        }
    }
}

/// Copy text to clipboard and optionally simulate Ctrl+V.
pub fn inject_text(text: &str, method: PasteMethod) -> Result<(), String> {
    assert!(!text.is_empty(), "inject_text: text must not be empty");

    // Step 1: Copy to clipboard
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("Clipboard: {}", e))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("Clipboard set: {}", e))?;

    // Step 2: Simulate Ctrl+V (if not clipboard-only)
    match method {
        PasteMethod::Wtype => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let status = Command::new("wtype")
                .args(["-M", "ctrl", "-P", "v", "-m", "ctrl", "-p", "v"])
                .status()
                .map_err(|e| format!("wtype: {}", e))?;
            if !status.success() {
                return Err("wtype failed".into());
            }
        }
        PasteMethod::Ydotool => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let status = Command::new("ydotool")
                .args(["key", "29:1", "47:1", "47:0", "29:0"])
                .status()
                .map_err(|e| format!("ydotool: {}", e))?;
            if !status.success() {
                return Err("ydotool failed".into());
            }
        }
        PasteMethod::Xdotool => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let status = Command::new("xdotool")
                .args(["key", "--clearmodifiers", "ctrl+v"])
                .status()
                .map_err(|e| format!("xdotool: {}", e))?;
            if !status.success() {
                return Err("xdotool failed".into());
            }
        }
        PasteMethod::ClipboardOnly => {
            // Text already in clipboard — user pastes manually
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_display_server_from_env() {
        let ds = detect_display_server();
        assert!(matches!(
            ds,
            DisplayServer::Wayland | DisplayServer::X11 | DisplayServer::Unknown
        ));
    }

    #[test]
    fn detect_paste_method_clipboard_fallback() {
        let method = detect_paste_method(DisplayServer::Unknown);
        assert!(matches!(
            method,
            PasteMethod::Xdotool | PasteMethod::ClipboardOnly
        ));
    }

    #[test]
    fn tool_available_returns_bool() {
        assert!(tool_available("ls"));
        assert!(!tool_available("definitely_not_a_real_tool_xyz"));
    }

    #[test]
    fn paste_method_display_is_debug() {
        let method = PasteMethod::ClipboardOnly;
        let debug = format!("{:?}", method);
        assert!(debug.contains("ClipboardOnly"));
    }

    #[test]
    fn display_server_variants_are_eq() {
        assert_eq!(DisplayServer::Wayland, DisplayServer::Wayland);
        assert_ne!(DisplayServer::Wayland, DisplayServer::X11);
    }

    #[test]
    #[should_panic(expected = "text must not be empty")]
    fn inject_text_rejects_empty() {
        let _ = inject_text("", PasteMethod::ClipboardOnly);
    }

    #[test]
    #[should_panic(expected = "name must not be empty")]
    fn tool_available_rejects_empty() {
        tool_available("");
    }
}

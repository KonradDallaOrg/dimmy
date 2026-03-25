//! Permissions tab — Linux-specific: mic access, text injection tools, autostart.

use std::cell::Cell;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

fn tool_available(name: &str) -> bool {
    assert!(!name.is_empty(), "tool_available: name must not be empty");
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn status_icon(available: bool) -> &'static str {
    if available { "emblem-ok-symbolic" } else { "dialog-warning-symbolic" }
}

fn status_text(available: bool) -> &'static str {
    if available { "Available" } else { "Not found" }
}

fn detect_display_server() -> &'static str {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => "Wayland",
        Ok("x11") => "X11",
        _ => {
            if std::env::var("WAYLAND_DISPLAY").is_ok() {
                "Wayland"
            } else {
                "X11"
            }
        }
    }
}

fn xdg_autostart_exists() -> bool {
    // Use XDG_CONFIG_HOME or ~/.config as fallback
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(home).join(".config")
        });
    config_dir.join("autostart").join("dimmy.desktop").exists()
}

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Permissions")
        .icon_name("dialog-password-symbolic")
        .build();

    // ── Microphone group ─────────────────────────────────────────────
    let mic_group = adw::PreferencesGroup::builder()
        .title("Microphone")
        .build();

    let devices = dimmy_lib::audio::list_input_devices();
    let mic_available = !devices.is_empty();

    let mic_icon = gtk4::Image::from_icon_name(status_icon(mic_available));
    let mic_subtitle = if mic_available {
        format!("{} device(s) found", devices.len())
    } else {
        "No audio input devices detected".to_string()
    };
    let mic_row = adw::ActionRow::builder()
        .title("Microphone Access")
        .subtitle(&mic_subtitle)
        .build();
    mic_row.add_prefix(&mic_icon);
    mic_group.add(&mic_row);

    // List devices if any
    for dev in &devices {
        let dev_row = adw::ActionRow::builder()
            .title(dev.as_str())
            .build();
        let check = gtk4::Image::from_icon_name("audio-input-microphone-symbolic");
        dev_row.add_prefix(&check);
        mic_group.add(&dev_row);
    }

    page.add(&mic_group);

    // ── Text Injection group ─────────────────────────────────────────
    let inject_group = adw::PreferencesGroup::builder()
        .title("Text Injection Tools")
        .description(&format!("Display server: {}", detect_display_server()))
        .build();

    let display = detect_display_server();

    // wtype (Wayland)
    let wtype_ok = tool_available("wtype");
    let wtype_row = adw::ActionRow::builder()
        .title("wtype")
        .subtitle(if display == "Wayland" { "Primary — Wayland text injection" } else { "Wayland only (not needed on X11)" })
        .build();
    let wtype_icon = gtk4::Image::from_icon_name(status_icon(wtype_ok));
    let wtype_label = gtk4::Label::new(Some(status_text(wtype_ok)));
    wtype_row.add_prefix(&wtype_icon);
    wtype_row.add_suffix(&wtype_label);
    inject_group.add(&wtype_row);

    // ydotool (Wayland fallback)
    let ydotool_ok = tool_available("ydotool");
    let ydotool_row = adw::ActionRow::builder()
        .title("ydotool")
        .subtitle("Wayland fallback — requires ydotoold daemon")
        .build();
    let ydotool_icon = gtk4::Image::from_icon_name(status_icon(ydotool_ok));
    let ydotool_label = gtk4::Label::new(Some(status_text(ydotool_ok)));
    ydotool_row.add_prefix(&ydotool_icon);
    ydotool_row.add_suffix(&ydotool_label);
    inject_group.add(&ydotool_row);

    // xdotool (X11)
    let xdotool_ok = tool_available("xdotool");
    let xdotool_row = adw::ActionRow::builder()
        .title("xdotool")
        .subtitle(if display == "X11" { "Primary — X11 text injection" } else { "X11 only (not needed on Wayland)" })
        .build();
    let xdotool_icon = gtk4::Image::from_icon_name(status_icon(xdotool_ok));
    let xdotool_label = gtk4::Label::new(Some(status_text(xdotool_ok)));
    xdotool_row.add_prefix(&xdotool_icon);
    xdotool_row.add_suffix(&xdotool_label);
    inject_group.add(&xdotool_row);

    // Install hints
    if display == "Wayland" && !wtype_ok && !ydotool_ok {
        let hint_row = adw::ActionRow::builder()
            .title("Install text injection")
            .subtitle("sudo apt install wtype  (or: sudo apt install ydotool)")
            .build();
        let warn_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        hint_row.add_prefix(&warn_icon);
        inject_group.add(&hint_row);
    } else if display == "X11" && !xdotool_ok {
        let hint_row = adw::ActionRow::builder()
            .title("Install text injection")
            .subtitle("sudo apt install xdotool")
            .build();
        let warn_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        hint_row.add_prefix(&warn_icon);
        inject_group.add(&hint_row);
    }

    page.add(&inject_group);

    // ── Autostart group ──────────────────────────────────────────────
    let autostart_group = adw::PreferencesGroup::builder()
        .title("Autostart")
        .build();

    let autostart_ok = xdg_autostart_exists();
    let autostart_row = adw::ActionRow::builder()
        .title("XDG Autostart")
        .subtitle(if autostart_ok {
            "dimmy.desktop found in ~/.config/autostart/"
        } else {
            "No autostart entry — Dimmy won't start at login"
        })
        .build();
    let autostart_icon = gtk4::Image::from_icon_name(status_icon(autostart_ok));
    autostart_row.add_prefix(&autostart_icon);
    autostart_group.add(&autostart_row);

    page.add(&autostart_group);

    // ── System Settings button ───────────────────────────────────────
    let actions_group = adw::PreferencesGroup::new();

    let sys_btn = gtk4::Button::builder()
        .label("Open System Settings")
        .css_classes(vec!["flat".to_string()])
        .build();
    sys_btn.connect_clicked(move |_| {
        log("Opening system settings...");
        let _ = Command::new("xdg-open")
            .arg("gnome-control-center")
            .spawn()
            .or_else(|_| Command::new("gnome-control-center").spawn())
            .or_else(|_| Command::new("xdg-open").arg("x-settings://").spawn());
    });

    let sys_row = adw::ActionRow::builder()
        .title("System Settings")
        .subtitle("Open GNOME/system sound or privacy settings")
        .activatable_widget(&sys_btn)
        .build();
    sys_row.add_suffix(&sys_btn);
    actions_group.add(&sys_row);

    page.add(&actions_group);

    // Suppress unused warning — app_state not directly needed here
    // but we take it for consistency with other tabs
    let _ = app_state;

    page
}

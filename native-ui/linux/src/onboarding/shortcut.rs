//! Onboarding Step 2: Shortcut setup.

use gtk4::prelude::*;
use std::sync::Arc;

pub fn create_page(app_state: &Arc<dimmy_lib::AppState>) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page.set_halign(gtk4::Align::Center);
    page.set_valign(gtk4::Align::Center);
    page.set_margin_start(40);
    page.set_margin_end(40);

    let title = gtk4::Label::new(Some("Your Shortcut"));
    title.add_css_class("title-2");
    page.append(&title);

    let subtitle = gtk4::Label::new(Some("Hold to dictate, release to paste"));
    subtitle.add_css_class("dim-label");
    page.append(&subtitle);

    // Current shortcut display
    let current = app_state
        .shortcut
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|e| e.into_inner().clone());

    let shortcut_btn = gtk4::Button::with_label(&current);
    shortcut_btn.add_css_class("title-3");
    shortcut_btn.set_halign(gtk4::Align::Center);
    shortcut_btn.set_margin_top(12);
    // TODO: Click to record shortcut (requires portal on Wayland)
    page.append(&shortcut_btn);

    // Preset buttons
    let presets_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    presets_box.set_halign(gtk4::Align::Center);
    presets_box.set_margin_top(12);

    let state_ref = app_state.clone();
    for (label, value) in &[
        ("Ctrl+Alt", "ctrl+alt"),
        ("Ctrl+Shift", "ctrl+shift"),
        ("Alt+Shift", "alt+shift"),
        ("Super", "win"),
    ] {
        let btn = gtk4::Button::with_label(label);
        btn.add_css_class("flat");
        let val = value.to_string();
        let state = state_ref.clone();
        let shortcut_ref = shortcut_btn.clone();
        btn.connect_clicked(move |_| {
            *state
                .shortcut
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = val.clone();
            shortcut_ref.set_label(&val);
            dimmy_lib::log(&format!("Shortcut preset selected: {}", val));
        });
        presets_box.append(&btn);
    }
    page.append(&presets_box);

    // Mode picker
    let mode_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    mode_box.set_margin_top(20);

    let mode_label = gtk4::Label::new(Some("Mode"));
    mode_label.add_css_class("heading");
    mode_label.set_halign(gtk4::Align::Start);
    mode_box.append(&mode_label);

    let current_mode = app_state
        .shortcut_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|e| e.into_inner().clone());

    let ptt_btn =
        gtk4::CheckButton::with_label("Push-to-Talk \u{2014} hold to record, release to paste");
    let toggle_btn =
        gtk4::CheckButton::with_label("Toggle \u{2014} tap to start, tap to stop");
    toggle_btn.set_group(Some(&ptt_btn));

    if current_mode == "toggle" {
        toggle_btn.set_active(true);
    } else {
        ptt_btn.set_active(true);
    }

    let state_for_ptt = app_state.clone();
    ptt_btn.connect_toggled(move |btn| {
        if btn.is_active() {
            *state_for_ptt
                .shortcut_mode
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = "hold".to_string();
        }
    });

    let state_for_toggle = app_state.clone();
    toggle_btn.connect_toggled(move |btn| {
        if btn.is_active() {
            *state_for_toggle
                .shortcut_mode
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = "toggle".to_string();
        }
    });

    mode_box.append(&ptt_btn);
    mode_box.append(&toggle_btn);
    page.append(&mode_box);

    page
}

//! Shortcut settings tab — hotkey configuration and mode.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

const SHORTCUT_PRESETS: &[(&str, &str)] = &[
    ("Ctrl+Alt", "ctrl+alt"),
    ("Ctrl+Shift", "ctrl+shift"),
    ("Alt+Shift", "alt+shift"),
    ("Super", "super"),
];

const MODES: &[(&str, &str)] = &[
    ("Push-to-Talk", "hold"),
    ("Toggle", "toggle"),
];

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Shortcut")
        .icon_name("input-keyboard-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Keyboard Shortcut")
        .description("Configure the global hotkey to start/stop recording")
        .build();

    // Current shortcut display
    let current_shortcut = app_state.shortcut.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let shortcut_label = gtk4::Label::builder()
        .label(&current_shortcut)
        .css_classes(vec!["title-2".to_string()])
        .margin_top(12)
        .margin_bottom(12)
        .build();

    let shortcut_row = adw::ActionRow::builder()
        .title("Current Shortcut")
        .build();
    shortcut_row.add_suffix(&shortcut_label);
    group.add(&shortcut_row);

    // Preset buttons
    let preset_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::Center)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    preset_box.add_css_class("linked");

    for (display_name, value) in SHORTCUT_PRESETS {
        let btn = gtk4::Button::builder()
            .label(*display_name)
            .build();

        let state_ref = app_state.clone();
        let label_ref = shortcut_label.clone();
        let val = value.to_string();
        btn.connect_clicked(move |_| {
            *state_ref.shortcut.lock().unwrap_or_else(|e| e.into_inner()) = val.clone();
            label_ref.set_label(&val);
            log(&format!("Shortcut preset: {}", val));
        });

        preset_box.append(&btn);
    }

    let preset_row = adw::ActionRow::builder()
        .title("Presets")
        .build();
    preset_row.add_suffix(&preset_box);
    group.add(&preset_row);

    // Mode
    let mode_items: Vec<&str> = MODES.iter().map(|(name, _)| *name).collect();
    let mode_model = gtk4::StringList::new(&mode_items);
    let mode_row = adw::ComboRow::builder()
        .title("Mode")
        .subtitle("How the shortcut activates recording")
        .model(&mode_model)
        .build();

    let current_mode = app_state.shortcut_mode.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mode_idx = MODES.iter().position(|(_, val)| *val == current_mode).unwrap_or(0);
    mode_row.set_selected(mode_idx as u32);

    let state_mode = app_state.clone();
    mode_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < MODES.len() {
            let val = MODES[idx].1.to_string();
            *state_mode.shortcut_mode.lock().unwrap_or_else(|e| e.into_inner()) = val.clone();
            log(&format!("Shortcut mode: {}", val));
        }
    });
    group.add(&mode_row);

    page.add(&group);
    page
}

//! Debug tab (Advanced only) — diagnostic tools and state inspection.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Debug")
        .icon_name("applications-engineering-symbolic")
        .build();

    // ── Simulation group ─────────────────────────────────────────────
    let sim_group = adw::PreferencesGroup::builder()
        .title("Simulation")
        .build();

    let sim_btn = gtk4::Button::builder()
        .label("Simulate Recording")
        .css_classes(vec!["flat".to_string()])
        .build();
    sim_btn.connect_clicked(move |_| {
        log("TODO: Simulate recording (needs pill window integration)");
    });

    let sim_row = adw::ActionRow::builder()
        .title("Simulate Recording")
        .subtitle("Test the recording flow without a real microphone")
        .activatable_widget(&sim_btn)
        .build();
    sim_row.add_suffix(&sim_btn);
    sim_group.add(&sim_row);

    page.add(&sim_group);

    // ── Debug flags group ────────────────────────────────────────────
    let flags_group = adw::PreferencesGroup::builder()
        .title("Debug Flags")
        .build();

    // Audio debug
    let audio_debug_row = adw::SwitchRow::builder()
        .title("Audio Debug")
        .subtitle("Save raw audio captures to disk for inspection")
        .build();
    audio_debug_row.set_active(*app_state.audio_debug_enabled.lock().unwrap_or_else(|e| e.into_inner()));

    let state_ad = app_state.clone();
    audio_debug_row.connect_active_notify(move |row| {
        *state_ad.audio_debug_enabled.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("Audio debug: {}", row.is_active()));
    });
    flags_group.add(&audio_debug_row);

    // LLM log
    let llm_log_row = adw::SwitchRow::builder()
        .title("LLM Log")
        .subtitle("Log LLM requests and responses")
        .build();
    llm_log_row.set_active(*app_state.llm_log_enabled.lock().unwrap_or_else(|e| e.into_inner()));

    let state_ll = app_state.clone();
    llm_log_row.connect_active_notify(move |row| {
        *state_ll.llm_log_enabled.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("LLM log: {}", row.is_active()));
    });
    flags_group.add(&llm_log_row);

    page.add(&flags_group);

    // ── Audio health group ───────────────────────────────────────────
    let health_group = adw::PreferencesGroup::builder()
        .title("Audio Health")
        .build();

    let health_label = gtk4::Label::builder()
        .label("Not checked")
        .xalign(0.0)
        .wrap(true)
        .build();

    let health_btn = gtk4::Button::builder()
        .label("Check Audio Health")
        .css_classes(vec!["flat".to_string()])
        .build();

    let state_h = app_state.clone();
    let label_ref = health_label.clone();
    health_btn.connect_clicked(move |_| {
        let devices = dimmy_lib::audio::list_input_devices();
        let recording = *state_h.recording.lock().unwrap_or_else(|e| e.into_inner());
        let selected = state_h.selected_device.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let sample_rate = *state_h.audio_sample_rate.lock().unwrap_or_else(|e| e.into_inner());
        let buffer_len = state_h.audio_buffer.lock().unwrap_or_else(|e| e.into_inner()).len();

        let report = format!(
            "Devices: {} found\nSelected: {}\nSample rate: {} Hz\nRecording: {}\nBuffer: {} samples",
            devices.len(),
            selected.as_deref().unwrap_or("Default"),
            sample_rate,
            recording,
            buffer_len,
        );
        label_ref.set_label(&report);
        log(&format!("Audio health check:\n{}", report));
    });

    let health_row = adw::ActionRow::builder()
        .title("Audio Health Check")
        .subtitle("Inspect audio subsystem state")
        .activatable_widget(&health_btn)
        .build();
    health_row.add_suffix(&health_btn);
    health_group.add(&health_row);

    let result_row = adw::ActionRow::builder()
        .title("Result")
        .build();
    result_row.add_suffix(&health_label);
    health_group.add(&result_row);

    page.add(&health_group);

    // ── Current state group ──────────────────────────────────────────
    let state_group = adw::PreferencesGroup::builder()
        .title("Current State")
        .build();

    let api_url = app_state.api_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let api_model = app_state.api_model.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let language = app_state.language.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let shortcut = app_state.shortcut.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let shortcut_mode = app_state.shortcut_mode.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let llm_url = app_state.llm_api_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let llm_model = app_state.llm_api_model.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let llm_style = app_state.llm_style.lock().unwrap_or_else(|e| e.into_inner()).as_str();
    let has_key = app_state.api_key.lock().unwrap_or_else(|e| e.into_inner()).is_some();

    let state_items: &[(&str, String)] = &[
        ("STT URL", api_url),
        ("STT Model", api_model),
        ("Language", if language.is_empty() { "auto".to_string() } else { language }),
        ("Shortcut", format!("{} ({})", shortcut, shortcut_mode)),
        ("Has API Key", format!("{}", has_key)),
        ("LLM URL", llm_url),
        ("LLM Model", llm_model),
        ("LLM Style", llm_style.to_string()),
    ];

    for (title, value) in state_items {
        let row = adw::ActionRow::builder()
            .title(*title)
            .build();
        let label = gtk4::Label::builder()
            .label(value.as_str())
            .ellipsize(gtk4::pango::EllipsizeMode::Middle)
            .max_width_chars(40)
            .build();
        row.add_suffix(&label);
        state_group.add(&row);
    }

    page.add(&state_group);
    page
}

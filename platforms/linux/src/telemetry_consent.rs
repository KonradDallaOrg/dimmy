//! Anonymous statistics + crash reports consent.
//!
//! The answer lives in `analytics_consent` / `crash_reports_consent`
//! (`None` = not asked yet). Both pipelines stay off until a saved "yes".
//! Windows and macOS persist it through `dimmy_telemetry_set_enabled`; this
//! host holds the `AppState` directly and saves the config the same way
//! `terms.rs` does.

use std::sync::Arc;

use dimmy_lib::AppState;
use gtk4::prelude::*;
use libadwaita as adw;

pub const TITLE: &str = "Help improve Dimmy";
pub const BODY: &str = "Can Dimmy send anonymous usage statistics and crash reports? \
They tell us which features are used and what breaks. Never your audio, your text, \
your prompts, file names or keys. You can change this any time in Settings, Privacy.";

/// True when the user has not answered yet.
pub fn needs_asking(state: &AppState) -> bool {
    state
        .analytics_consent
        .lock()
        .map(|c| c.is_none())
        .unwrap_or(false)
}

/// Record one answer for both pipelines, apply it and persist it.
pub fn answer(state: &AppState, yes: bool) {
    if let Ok(mut c) = state.analytics_consent.lock() {
        *c = Some(yes);
    }
    if let Ok(mut c) = state.crash_reports_consent.lock() {
        *c = Some(yes);
    }
    dimmy_lib::telemetry::set_enabled(yes);
    dimmy_lib::telemetry::set_crash_enabled(yes);
    if let Ok(cfg) = dimmy_lib::snapshot_config(state) {
        dimmy_lib::save_config_file(&cfg);
    }
    dimmy_lib::log(&format!("[Consent] statistics answered: {}", yes));
}

/// Asked once per launch until answered. Closing it leaves the answer
/// unrecorded, so it comes back next launch.
pub fn show_dialog(app: &adw::Application, state: &Arc<AppState>) {
    let window = gtk4::Window::builder()
        .application(app)
        .title(TITLE)
        .default_width(440)
        .resizable(false)
        .build();

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let title = gtk4::Label::new(Some(TITLE));
    title.add_css_class("title-3");
    title.set_xalign(0.0);
    content.append(&title);

    let body = gtk4::Label::new(Some(BODY));
    body.set_wrap(true);
    body.set_xalign(0.0);
    content.append(&body);

    // Equal weight on purpose: neither answer is styled as the default.
    let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    buttons.set_homogeneous(true);
    buttons.set_margin_top(8);
    let yes_btn = gtk4::Button::with_label("Yes, send them");
    let no_btn = gtk4::Button::with_label("No, thanks");
    buttons.append(&yes_btn);
    buttons.append(&no_btn);
    content.append(&buttons);

    for (btn, yes) in [(&yes_btn, true), (&no_btn, false)] {
        let window = window.clone();
        let state = state.clone();
        btn.connect_clicked(move |_| {
            answer(&state, yes);
            window.close();
        });
    }

    window.set_child(Some(&content));
    window.present();
}

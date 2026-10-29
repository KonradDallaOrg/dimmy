//! Terms of Service / EULA acceptance (clickwrap).
//!
//! The version lives in the core (`dimmy_lib::TERMS_VERSION`) and the answer
//! in `terms_accepted_version`. Windows and macOS write it through
//! `dimmy_set_config_json`; this host holds the `AppState` directly and saves
//! the config the same way the onboarding wizard does.

use std::sync::Arc;

use dimmy_lib::AppState;
use gtk4::prelude::*;
use libadwaita as adw;

pub const TERMS_URL: &str = "https://dimmy.app/terms/";
pub const CHECK_LABEL: &str = "I accept the Terms of Service and End-User Licence Agreement";

/// True when the user already accepted the CURRENT terms version.
pub fn is_accepted(state: &AppState) -> bool {
    state
        .terms_accepted_version
        .lock()
        .map(|v| v.as_str() == dimmy_lib::TERMS_VERSION)
        .unwrap_or(false)
}

/// Record the acceptance and persist it.
pub fn accept(state: &AppState) {
    if let Ok(mut v) = state.terms_accepted_version.lock() {
        *v = dimmy_lib::TERMS_VERSION.to_string();
    }
    if let Ok(cfg) = dimmy_lib::snapshot_config(state) {
        dimmy_lib::save_config_file(&cfg);
    }
    dimmy_lib::log("[Terms] accepted");
}

/// Existing installs: asked once per launch until the current terms are
/// accepted. Never blocks anything; closing it leaves the answer unrecorded.
pub fn show_launch_dialog(app: &adw::Application, state: &Arc<AppState>) {
    let window = gtk4::Window::builder()
        .application(app)
        .title("Dimmy terms")
        .default_width(440)
        .resizable(false)
        .build();

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let intro = gtk4::Label::new(Some(
        "Dimmy has Terms of Service and an End-User Licence Agreement. Please read and accept them.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    content.append(&intro);

    let link = gtk4::LinkButton::with_label(TERMS_URL, "Read the terms");
    link.set_halign(gtk4::Align::Start);
    content.append(&link);

    let check = gtk4::CheckButton::with_label(CHECK_LABEL);
    content.append(&check);

    let accept_btn = gtk4::Button::with_label("Accept");
    accept_btn.add_css_class("suggested-action");
    accept_btn.set_halign(gtk4::Align::End);
    accept_btn.set_sensitive(false);
    content.append(&accept_btn);

    {
        let accept_btn = accept_btn.clone();
        check.connect_toggled(move |c| accept_btn.set_sensitive(c.is_active()));
    }
    {
        let window = window.clone();
        let state = state.clone();
        accept_btn.connect_clicked(move |_| {
            accept(&state);
            window.close();
        });
    }

    window.set_child(Some(&content));
    window.present();
}

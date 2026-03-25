//! Settings window — AdwPreferencesWindow with 8 tabs and Advanced toggle.

mod about;
mod debug;
mod general;
mod output;
mod overlay;
mod permissions;
mod shortcut;
mod stats;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

/// Build and present the settings window.
///
/// The window contains 8 preference pages (tabs). Stats and Debug are only
/// visible when the Advanced toggle is active. Each page reads/writes directly
/// to `AppState` fields. The Save button snapshots the config and writes it
/// to disk.
pub fn show_settings_window(app: &adw::Application, app_state: &Arc<AppState>) {
    let show_advanced = Rc::new(Cell::new(false));

    let window = adw::PreferencesWindow::builder()
        .title("Dimmy Settings")
        .default_width(720)
        .default_height(560)
        .search_enabled(true)
        .build();

    if let Some(gtk_app) = app.downcast_ref::<gtk4::Application>() {
        window.set_application(Some(gtk_app));
    }

    // ── Tab pages ────────────────────────────────────────────────────
    let general_page = general::create_page(app_state, &show_advanced);
    let shortcut_page = shortcut::create_page(app_state, &show_advanced);
    let output_page = output::create_page(app_state, &show_advanced);
    let overlay_page = overlay::create_page(app_state, &show_advanced);
    let permissions_page = permissions::create_page(app_state, &show_advanced);
    let stats_page = stats::create_page(app_state, &show_advanced);
    let debug_page = debug::create_page(app_state, &show_advanced);
    let about_page = about::create_page(app_state, &show_advanced);

    window.add(&general_page);
    window.add(&shortcut_page);
    window.add(&output_page);
    window.add(&overlay_page);
    window.add(&permissions_page);
    window.add(&stats_page);
    window.add(&debug_page);
    window.add(&about_page);

    // Advanced-only tabs start hidden
    stats_page.set_visible(false);
    debug_page.set_visible(false);

    // ── Advanced toggle (header bar button) ──────────────────────────
    let advanced_button = gtk4::ToggleButton::builder()
        .label("Advanced")
        .css_classes(vec!["flat".to_string()])
        .build();

    let stats_page_ref = stats_page.clone();
    let debug_page_ref = debug_page.clone();
    let show_advanced_ref = show_advanced.clone();
    advanced_button.connect_toggled(move |btn| {
        let active = btn.is_active();
        show_advanced_ref.set(active);
        stats_page_ref.set_visible(active);
        debug_page_ref.set_visible(active);
        log(&format!("Advanced mode: {}", active));
    });

    // Add to header bar
    let header = window.first_child()
        .and_then(|c| c.first_child())
        .and_then(|c| c.downcast::<adw::HeaderBar>().ok());
    if let Some(hb) = header {
        hb.pack_end(&advanced_button);
    } else {
        // Fallback: just log, the toggle will not be visible but pages still work
        log("WARN: could not find header bar for Advanced toggle");
    }

    // ── Save button in header bar ────────────────────────────────────
    let save_button = gtk4::Button::builder()
        .label("Save")
        .css_classes(vec!["suggested-action".to_string()])
        .build();

    let state_for_save = app_state.clone();
    save_button.connect_clicked(move |_btn| {
        match dimmy_lib::snapshot_config(&state_for_save) {
            Ok(cfg) => {
                dimmy_lib::save_config_file(&cfg);
                log("Settings saved to disk");
            }
            Err(e) => {
                log(&format!("ERROR saving settings: {}", e));
            }
        }
    });

    // Try to add save button to header bar too
    let header2 = window.first_child()
        .and_then(|c| c.first_child())
        .and_then(|c| c.downcast::<adw::HeaderBar>().ok());
    if let Some(hb) = header2 {
        hb.pack_start(&save_button);
    }

    window.present();
    log("Settings window opened");
}

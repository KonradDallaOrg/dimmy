//! About tab — app info, version, links.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

pub fn create_page(_app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("About")
        .icon_name("help-about-symbolic")
        .build();

    // ── App identity group ───────────────────────────────────────────
    let identity_group = adw::PreferencesGroup::new();

    // App icon placeholder
    let icon = gtk4::Image::builder()
        .icon_name("audio-input-microphone-symbolic")
        .pixel_size(64)
        .margin_top(24)
        .margin_bottom(8)
        .halign(gtk4::Align::Center)
        .build();

    let title_label = gtk4::Label::builder()
        .label("Dimmy")
        .css_classes(vec!["title-1".to_string()])
        .halign(gtk4::Align::Center)
        .build();

    let version_label = gtk4::Label::builder()
        .label(&format!("Version {}", env!("CARGO_PKG_VERSION")))
        .css_classes(vec!["dim-label".to_string()])
        .halign(gtk4::Align::Center)
        .build();

    let tagline_label = gtk4::Label::builder()
        .label("Voice dictation that stays out of your way")
        .css_classes(vec!["dim-label".to_string()])
        .halign(gtk4::Align::Center)
        .margin_bottom(16)
        .build();

    let header_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(4)
        .halign(gtk4::Align::Center)
        .build();
    header_box.append(&icon);
    header_box.append(&title_label);
    header_box.append(&version_label);
    header_box.append(&tagline_label);

    // Use a gtk4::ListBoxRow to embed the header box in the group
    let header_row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&header_box)
        .build();
    identity_group.add(&header_row);

    page.add(&identity_group);

    // ── Actions group ────────────────────────────────────────────────
    let actions_group = adw::PreferencesGroup::builder()
        .title("Updates")
        .build();

    let update_btn = gtk4::Button::builder()
        .label("Check for Updates")
        .css_classes(vec!["flat".to_string()])
        .build();
    update_btn.connect_clicked(move |_| {
        log("TODO: Check for updates (not yet implemented)");
    });

    let update_row = adw::ActionRow::builder()
        .title("Check for Updates")
        .subtitle("Look for a newer version of Dimmy")
        .activatable_widget(&update_btn)
        .build();
    update_row.add_suffix(&update_btn);
    actions_group.add(&update_row);

    page.add(&actions_group);

    // ── Links group ──────────────────────────────────────────────────
    let links_group = adw::PreferencesGroup::builder()
        .title("Links")
        .build();

    let github_row = adw::ActionRow::builder()
        .title("GitHub Repository")
        .subtitle("https://github.com/konradcr/pai-voice")
        .build();
    let github_btn = gtk4::Button::builder()
        .label("Open")
        .css_classes(vec!["flat".to_string()])
        .valign(gtk4::Align::Center)
        .build();
    github_btn.connect_clicked(move |_| {
        let _ = std::process::Command::new("xdg-open")
            .arg("https://github.com/konradcr/pai-voice")
            .spawn();
    });
    github_row.add_suffix(&github_btn);
    links_group.add(&github_row);

    let issues_row = adw::ActionRow::builder()
        .title("Report a Bug")
        .subtitle("https://github.com/konradcr/pai-voice/issues")
        .build();
    let issues_btn = gtk4::Button::builder()
        .label("Open")
        .css_classes(vec!["flat".to_string()])
        .valign(gtk4::Align::Center)
        .build();
    issues_btn.connect_clicked(move |_| {
        let _ = std::process::Command::new("xdg-open")
            .arg("https://github.com/konradcr/pai-voice/issues")
            .spawn();
    });
    issues_row.add_suffix(&issues_btn);
    links_group.add(&issues_row);

    page.add(&links_group);

    // ── Footer ───────────────────────────────────────────────────────
    let footer_group = adw::PreferencesGroup::new();
    let footer_label = gtk4::Label::builder()
        .label("Made with irony")
        .css_classes(vec!["dim-label".to_string()])
        .halign(gtk4::Align::Center)
        .margin_top(16)
        .margin_bottom(16)
        .build();
    let footer_row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&footer_label)
        .build();
    footer_group.add(&footer_row);

    page.add(&footer_group);

    page
}

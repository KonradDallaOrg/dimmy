//! Onboarding Step 3: Try it live.

use gtk4::prelude::*;
use std::sync::Arc;

pub fn create_page(app_state: &Arc<dimmy_lib::AppState>) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page.set_halign(gtk4::Align::Center);
    page.set_valign(gtk4::Align::Center);
    page.set_margin_start(40);
    page.set_margin_end(40);

    let title = gtk4::Label::new(Some("Try it!"));
    title.add_css_class("title-2");
    page.append(&title);

    let shortcut = app_state
        .shortcut
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|e| e.into_inner().clone());

    let instruction = gtk4::Label::new(Some(&format!(
        "Hold {} and say something",
        shortcut.to_uppercase()
    )));
    instruction.set_wrap(true);
    instruction.set_justify(gtk4::Justification::Center);
    page.append(&instruction);

    let hint = gtk4::Label::new(Some(
        "Look at the pill overlay \u{2014} it will animate while you speak",
    ));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_margin_top(4);
    page.append(&hint);

    // Transcript display
    let transcript_frame = gtk4::Frame::new(None);
    transcript_frame.set_margin_top(16);

    let transcript_label = gtk4::Label::new(Some("Your transcription will appear here..."));
    transcript_label.set_wrap(true);
    transcript_label.set_selectable(true);
    transcript_label.set_margin_top(12);
    transcript_label.set_margin_bottom(12);
    transcript_label.set_margin_start(12);
    transcript_label.set_margin_end(12);
    transcript_label.add_css_class("dim-label");
    transcript_frame.set_child(Some(&transcript_label));

    page.append(&transcript_frame);

    // Success state (hidden initially)
    let success_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    success_box.set_visible(false);
    success_box.set_halign(gtk4::Align::Center);

    let check = gtk4::Image::from_icon_name("emblem-ok-symbolic");
    check.set_pixel_size(48);
    check.add_css_class("success");
    success_box.append(&check);

    let success_label = gtk4::Label::new(Some("You're all set!"));
    success_label.add_css_class("title-3");
    success_box.append(&success_label);

    let success_hint = gtk4::Label::new(Some(&format!(
        "Dimmy lives in your system tray.\nHold {} anywhere to dictate.",
        shortcut.to_uppercase()
    )));
    success_hint.set_wrap(true);
    success_hint.set_justify(gtk4::Justification::Center);
    success_box.append(&success_hint);

    page.append(&success_box);

    page
}

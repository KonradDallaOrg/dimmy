//! Onboarding Step 1: Welcome page.

use gtk4::prelude::*;

pub fn create_page() -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page.set_halign(gtk4::Align::Center);
    page.set_valign(gtk4::Align::Center);
    page.set_margin_start(40);
    page.set_margin_end(40);

    // Icon
    let icon = gtk4::Image::from_icon_name("audio-input-microphone-symbolic");
    icon.set_pixel_size(64);
    icon.add_css_class("accent");
    page.append(&icon);

    // Title
    let title = gtk4::Label::new(Some("Dimmy"));
    title.add_css_class("title-1");
    page.append(&title);

    // Tagline
    let tagline = gtk4::Label::new(Some("Voice dictation that stays out of your way"));
    tagline.add_css_class("dim-label");
    page.append(&tagline);

    // Description
    let desc = gtk4::Label::new(Some(
        "Hold a shortcut, speak, release.\nYour words appear wherever you're typing.",
    ));
    desc.set_justify(gtk4::Justification::Center);
    desc.set_wrap(true);
    desc.set_margin_top(8);
    page.append(&desc);

    page
}

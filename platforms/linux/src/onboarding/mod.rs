//! Onboarding wizard — 3-step AdwCarousel flow.

mod shortcut;
mod tryit;
mod welcome;

use adw::prelude::*;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;

pub fn show_onboarding(app: &adw::Application, app_state: &Arc<dimmy_lib::AppState>) {
    let window = adw::Window::builder()
        .application(app)
        .title("Welcome to Dimmy")
        .default_width(520)
        .default_height(440)
        .resizable(false)
        .modal(true)
        .build();

    let main_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    let carousel = adw::Carousel::new();
    carousel.set_allow_long_swipes(false);
    carousel.set_allow_scroll_wheel(false);
    carousel.set_hexpand(true);
    carousel.set_vexpand(true);

    // Progress dots
    let dots = adw::CarouselIndicatorDots::new();
    dots.set_carousel(Some(&carousel));

    // Pages
    let (welcome_page, terms_check) = welcome::create_page(app_state);
    let shortcut_page = shortcut::create_page(app_state);
    let tryit_page = tryit::create_page(app_state);

    carousel.append(&welcome_page);
    carousel.append(&shortcut_page);
    carousel.append(&tryit_page);

    main_box.append(&carousel);
    main_box.append(&dots);

    // Navigation buttons
    let nav_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    nav_box.set_halign(gtk4::Align::Center);
    nav_box.set_margin_top(12);
    nav_box.set_margin_bottom(16);

    let back_btn = gtk4::Button::with_label("Back");
    back_btn.set_visible(false);

    let next_btn = gtk4::Button::with_label("Get Started");
    next_btn.add_css_class("suggested-action");
    next_btn.set_sensitive(terms_check.is_active());

    nav_box.append(&back_btn);
    nav_box.append(&next_btn);
    main_box.append(&nav_box);

    window.set_content(Some(&main_box));

    // Navigation logic — update button labels on page change
    carousel.connect_position_notify(glib::clone!(
        #[weak]
        back_btn,
        #[weak]
        next_btn,
        #[weak]
        terms_check,
        move |carousel| {
            let pos = carousel.position().round() as u32;
            back_btn.set_visible(pos > 0);
            next_btn.set_sensitive(pos > 0 || terms_check.is_active());
            match pos {
                0 => next_btn.set_label("Get Started"),
                1 => next_btn.set_label("Next"),
                2 => next_btn.set_label("Start Using Dimmy"),
                _ => {}
            }
        }
    ));

    // The box only gates the Welcome page.
    {
        let carousel = carousel.clone();
        let next_btn = next_btn.clone();
        terms_check.connect_toggled(move |c| {
            if carousel.position().round() as u32 == 0 {
                next_btn.set_sensitive(c.is_active());
            }
        });
    }

    let carousel_for_next = carousel.clone();
    let window_for_next = window.clone();
    let state_for_next = app_state.clone();
    let terms_for_next = terms_check.clone();
    let app_for_next = app.clone();

    next_btn.connect_clicked(move |_| {
        let pos = carousel_for_next.position().round() as u32;
        if pos == 0 {
            if !terms_for_next.is_active() {
                return;
            }
            crate::terms::accept(&state_for_next);
        }
        let n_pages = carousel_for_next.n_pages();
        if pos < n_pages - 1 {
            let next_page = carousel_for_next.nth_page(pos + 1);
            carousel_for_next.scroll_to(&next_page, true);
        } else {
            // Finish onboarding
            if let Ok(cfg) = dimmy_lib::snapshot_config(&state_for_next) {
                dimmy_lib::save_config_file(&cfg);
            }
            let _ = dimmy_lib::mark_onboarding_done();
            dimmy_lib::log("Onboarding completed");
            // Presented before the wizard closes so the app keeps a window.
            if crate::telemetry_consent::needs_asking(&state_for_next) {
                crate::telemetry_consent::show_dialog(&app_for_next, &state_for_next);
            }
            window_for_next.close();
        }
    });

    let carousel_for_back = carousel.clone();

    back_btn.connect_clicked(move |_| {
        let pos = carousel_for_back.position().round() as u32;
        if pos > 0 {
            let prev_page = carousel_for_back.nth_page(pos - 1);
            carousel_for_back.scroll_to(&prev_page, true);
        }
    });

    window.present();
}

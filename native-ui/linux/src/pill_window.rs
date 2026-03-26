//! Floating pill overlay window.
//!
//! Uses gtk4-layer-shell on Wayland for always-on-top positioning.
//! Falls back to GtkWindow type hints on X11.

use gtk4::prelude::*;
use gtk4::glib;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::waveform::{self, WaveformStyle};

// ── State machine ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PillState {
    Idle,
    Recording,
    Transcribing,
    Processing,
    Completing,
    Error,
}

// ── Border colors ──────────────────────────────────────────

pub struct BorderColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl BorderColor {
    pub fn from_style(style: &str) -> Option<Self> {
        match style {
            "Blue" => Some(Self {
                r: 0.220,
                g: 0.741,
                b: 0.973,
            }), // #38bdf8
            "Green" => Some(Self {
                r: 0.290,
                g: 0.855,
                b: 0.498,
            }), // #4ade80
            "Purple" => Some(Self {
                r: 0.655,
                g: 0.545,
                b: 0.980,
            }), // #a78bfa
            "Orange" => Some(Self {
                r: 0.984,
                g: 0.573,
                b: 0.235,
            }), // #fb923c
            "None" => Some(Self {
                r: 0.235,
                g: 0.235,
                b: 0.235,
            }), // #3c3c3c
            _ => None, // "Rainbow" handled separately
        }
    }
}

// ── LLM style colors (for dot indicator) ───────────────────

pub fn llm_style_color(style: &str) -> (f64, f64, f64) {
    match style {
        "off" => (0.255, 0.690, 0.694),            // #41B0B1
        "correct" => (0.176, 0.831, 0.749),        // #2dd4bf
        "summarize" => (0.984, 0.749, 0.141),      // #fbbf24
        "elaborate" => (0.290, 0.855, 0.498),      // #4ade80
        "comprehensible" => (0.220, 0.741, 0.973), // #38bdf8
        "professional" => (0.957, 0.447, 0.714),   // #f472b6
        "prompt" => (0.655, 0.545, 0.980),         // #a78bfa
        "genz" => (0.910, 0.475, 0.976),           // #e879f9
        "boomer" => (0.976, 0.451, 0.086),         // #f97316
        "emoji" => (0.980, 0.800, 0.082),          // #facc15
        "acronyms" => (0.133, 0.827, 0.933),       // #22d3ee
        "imbruttito" => (0.937, 0.267, 0.267),     // #ef4444
        "custom" => (0.984, 0.573, 0.235),         // #fb923c
        _ => (0.255, 0.690, 0.694),                // default teal
    }
}

// ── Pill window builder ────────────────────────────────────

/// Create the pill overlay window.
/// Returns the window and a state-update closure.
pub fn create_pill_window(
    app: &libadwaita::Application,
    app_state: &Arc<dimmy_lib::AppState>,
) -> (gtk4::Window, Rc<dyn Fn(PillState)>) {
    let window = gtk4::Window::builder()
        .application(app)
        .title("Dimmy Pill")
        .default_width(240)
        .default_height(60)
        .decorated(false)
        .resizable(false)
        .build();

    // Try layer-shell for Wayland (if compiled with feature)
    #[cfg(feature = "layer-shell")]
    setup_layer_shell(&window);
    #[cfg(not(feature = "layer-shell"))]
    {
        dimmy_lib::log("Layer shell not compiled in — using X11 window hints");
        window.set_deletable(false);
    }

    // Make transparent
    let css = gtk4::CssProvider::new();
    css.load_from_data("window { background: transparent; }");
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("No display"),
        &css,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Main container
    let overlay_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    overlay_box.set_halign(gtk4::Align::Center);
    overlay_box.set_valign(gtk4::Align::Center);

    // State label (changes per state)
    let state_label = gtk4::Label::new(Some("\u{25cf}")); // ●
    state_label.add_css_class("pill-label");

    // Timer label (recording only)
    let timer_label = gtk4::Label::new(None);
    timer_label.set_visible(false);

    // Waveform (recording only)
    let waveform_style = app_state
        .waveform_style
        .lock()
        .map(|s| WaveformStyle::from_str(&s))
        .unwrap_or(WaveformStyle::Bars);
    let (waveform_area, amplitude) = waveform::create_waveform_widget(waveform_style, 60, 24);
    waveform_area.set_visible(false);

    overlay_box.append(&state_label);
    overlay_box.append(&waveform_area);
    overlay_box.append(&timer_label);

    // Drawing area for border (background layer)
    let border_area = gtk4::DrawingArea::new();
    border_area.set_hexpand(true);
    border_area.set_vexpand(true);

    let border_style = app_state
        .border_style
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| "Rainbow".to_string());

    let rainbow_angle = Rc::new(Cell::new(0.0f64));
    let pill_state = Rc::new(Cell::new(PillState::Idle));

    let angle_clone = rainbow_angle.clone();
    let style_clone = border_style.clone();
    let state_clone = pill_state.clone();

    border_area.set_draw_func(move |_area, cr, w, h| {
        let radius = (h as f64).min(w as f64) / 2.0;
        let state = state_clone.get();
        let w = w as f64;
        let h = h as f64;

        // Determine border color
        let (r, g, b) = match state {
            PillState::Transcribing => (0.220, 0.741, 0.973), // blue
            PillState::Processing => (0.655, 0.545, 0.980),   // purple
            PillState::Completing => (0.290, 0.855, 0.498),   // green
            PillState::Error => (0.937, 0.267, 0.267),        // red
            _ => {
                if let Some(bc) = BorderColor::from_style(&style_clone) {
                    (bc.r, bc.g, bc.b)
                } else {
                    // Rainbow: use rotating hue
                    let angle = angle_clone.get();
                    let hue = (angle % 360.0) / 360.0;
                    hsv_to_rgb(hue, 0.8, 1.0)
                }
            }
        };

        // Draw rounded rectangle border
        cr.set_source_rgba(r, g, b, 0.9);
        rounded_rect(cr, 0.0, 0.0, w, h, radius);
        let _ = cr.fill();

        // Inner dark fill
        cr.set_source_rgba(0.1, 0.1, 0.1, 0.95); // #1a1a1a
        rounded_rect(cr, 2.0, 2.0, w - 4.0, h - 4.0, radius - 2.0);
        let _ = cr.fill();
    });

    // Stack: border behind content
    let stack_overlay = gtk4::Overlay::new();
    stack_overlay.set_child(Some(&border_area));
    stack_overlay.add_overlay(&overlay_box);
    window.set_child(Some(&stack_overlay));

    // Rainbow animation timer (30Hz)
    let angle_for_timer = rainbow_angle.clone();
    let area_for_timer = border_area.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
        let angle = angle_for_timer.get();
        angle_for_timer.set(angle + 4.32); // 360deg / 2.5s at 30Hz ~ 4.32deg/frame
        area_for_timer.queue_draw();
        glib::ControlFlow::Continue
    });

    // Amplitude polling timer (12Hz)
    let waveform_for_timer = waveform_area.clone();
    let state_for_amp = pill_state.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(83), move || {
        if state_for_amp.get() == PillState::Recording {
            waveform_for_timer.queue_draw();
        }
        glib::ControlFlow::Continue
    });

    // Recording timer (1Hz)
    let recording_start = Rc::new(Cell::new(std::time::Instant::now()));
    let timer_label_clone = timer_label.clone();
    let state_for_timer = pill_state.clone();
    let start_clone = recording_start.clone();
    glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
        if state_for_timer.get() == PillState::Recording {
            let elapsed = start_clone.get().elapsed();
            let mins = elapsed.as_secs() / 60;
            let secs = elapsed.as_secs() % 60;
            timer_label_clone.set_text(&format!("{:02}:{:02}", mins, secs));
        }
        glib::ControlFlow::Continue
    });

    // Drag gesture
    let drag = gtk4::GestureDrag::new();
    drag.connect_drag_update(move |_gesture, offset_x, offset_y| {
        // Layer shell windows can't be positioned via GTK API on Wayland
        // but this works on X11
        dimmy_lib::log(&format!(
            "Pill drag: offset ({}, {})",
            offset_x, offset_y
        ));
    });
    window.add_controller(drag);

    // Context menu (right-click)
    let click = gtk4::GestureClick::builder().button(3).build(); // right click
    let win_for_menu = window.clone();
    click.connect_pressed(move |_gesture, _n_press, x, y| {
        let menu = gtk4::gio::Menu::new();
        menu.append(Some("Settings..."), Some("app.show-settings"));
        menu.append(Some("Hide"), Some("app.hide-pill"));
        let popover = gtk4::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&win_for_menu);
        popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
            x as i32, y as i32, 1, 1,
        )));
        popover.popup();
    });
    window.add_controller(click);

    // State update closure
    let state_ref = pill_state.clone();
    let label_ref = state_label;
    let waveform_ref = waveform_area;
    let timer_ref = timer_label;
    let start_ref = recording_start;
    let amp_ref = amplitude;
    let border_ref = border_area;

    let update_state: Rc<dyn Fn(PillState)> = Rc::new(move |new_state: PillState| {
        state_ref.set(new_state);
        border_ref.queue_draw();

        match new_state {
            PillState::Idle => {
                label_ref.set_text("\u{25cf}"); // ●
                waveform_ref.set_visible(false);
                timer_ref.set_visible(false);
            }
            PillState::Recording => {
                label_ref.set_text("");
                waveform_ref.set_visible(true);
                timer_ref.set_visible(true);
                timer_ref.set_text("00:00");
                start_ref.set(std::time::Instant::now());
                amp_ref.set(0.0);
            }
            PillState::Transcribing => {
                label_ref.set_text("Transcribing...");
                waveform_ref.set_visible(false);
                timer_ref.set_visible(false);
            }
            PillState::Processing => {
                label_ref.set_text("Processing...");
                waveform_ref.set_visible(false);
                timer_ref.set_visible(false);
            }
            PillState::Completing => {
                label_ref.set_text("\u{2713}"); // checkmark
                waveform_ref.set_visible(false);
                timer_ref.set_visible(false);
                // Auto-dismiss after 1.2s
                let state_copy = state_ref.clone();
                let label_copy = label_ref.clone();
                glib::timeout_add_local_once(
                    std::time::Duration::from_millis(1200),
                    move || {
                        if state_copy.get() == PillState::Completing {
                            state_copy.set(PillState::Idle);
                            label_copy.set_text("\u{25cf}"); // ●
                        }
                    },
                );
            }
            PillState::Error => {
                label_ref.set_text("\u{26a0} Error"); // warning sign
                waveform_ref.set_visible(false);
                timer_ref.set_visible(false);
                // Auto-dismiss after 3s
                let state_copy = state_ref.clone();
                let label_copy = label_ref.clone();
                glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                    if state_copy.get() == PillState::Error {
                        state_copy.set(PillState::Idle);
                        label_copy.set_text("\u{25cf}"); // ●
                    }
                });
            }
        }
    });

    (window, update_state)
}

// ── Layer shell setup ──────────────────────────────────────

#[cfg(feature = "layer-shell")]
fn setup_layer_shell(window: &gtk4::Window) {
    if !gtk4_layer_shell::is_supported() {
        dimmy_lib::log("Layer shell not supported — using X11 window hints");
        window.set_deletable(false);
        return;
    }

    gtk4_layer_shell::init_for_window(window);
    gtk4_layer_shell::set_layer(window, gtk4_layer_shell::Layer::Overlay);
    gtk4_layer_shell::set_exclusive_zone(window, -1);
    gtk4_layer_shell::set_keyboard_mode(window, gtk4_layer_shell::KeyboardMode::None);

    gtk4_layer_shell::set_anchor(window, gtk4_layer_shell::Edge::Bottom, true);
    gtk4_layer_shell::set_anchor(window, gtk4_layer_shell::Edge::Right, true);
    gtk4_layer_shell::set_margin(window, gtk4_layer_shell::Edge::Bottom, 40);
    gtk4_layer_shell::set_margin(window, gtk4_layer_shell::Edge::Right, 40);

    dimmy_lib::log("Layer shell initialized — pill on overlay layer");
}

// ── Drawing helpers ────────────────────────────────────────

fn rounded_rect(cr: &gtk4::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(
        x + w - r,
        y + r,
        r,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    cr.arc(
        x + w - r,
        y + h - r,
        r,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        3.0 * std::f64::consts::FRAC_PI_2,
    );
    cr.close_path();
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let i = (h * 6.0).floor() as u32;
    let f = h * 6.0 - i as f64;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_color_from_known_styles() {
        assert!(BorderColor::from_style("Blue").is_some());
        assert!(BorderColor::from_style("Green").is_some());
        assert!(BorderColor::from_style("Purple").is_some());
        assert!(BorderColor::from_style("Orange").is_some());
        assert!(BorderColor::from_style("None").is_some());
        assert!(BorderColor::from_style("Rainbow").is_none());
        assert!(BorderColor::from_style("invalid").is_none());
    }

    #[test]
    fn llm_style_colors_are_valid_rgb() {
        for style in &[
            "off",
            "correct",
            "summarize",
            "elaborate",
            "comprehensible",
            "professional",
            "prompt",
            "genz",
            "boomer",
            "emoji",
            "acronyms",
            "imbruttito",
            "custom",
        ] {
            let (r, g, b) = llm_style_color(style);
            assert!(r >= 0.0 && r <= 1.0, "{} r={}", style, r);
            assert!(g >= 0.0 && g <= 1.0, "{} g={}", style, g);
            assert!(b >= 0.0 && b <= 1.0, "{} b={}", style, b);
        }
    }

    #[test]
    fn hsv_to_rgb_red() {
        let (r, g, b) = hsv_to_rgb(0.0, 1.0, 1.0);
        assert!((r - 1.0).abs() < 0.01);
        assert!(g < 0.01);
        assert!(b < 0.01);
    }

    #[test]
    fn hsv_to_rgb_green() {
        let (r, g, b) = hsv_to_rgb(1.0 / 3.0, 1.0, 1.0);
        assert!(r < 0.01);
        assert!((g - 1.0).abs() < 0.01);
        assert!(b < 0.01);
    }

    #[test]
    fn hsv_to_rgb_blue() {
        let (r, g, b) = hsv_to_rgb(2.0 / 3.0, 1.0, 1.0);
        assert!(r < 0.01);
        assert!(g < 0.01);
        assert!((b - 1.0).abs() < 0.01);
    }

    #[test]
    fn pill_state_equality() {
        assert_eq!(PillState::Idle, PillState::Idle);
        assert_ne!(PillState::Idle, PillState::Recording);
    }
}

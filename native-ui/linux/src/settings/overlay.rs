//! Overlay settings tab — appearance, position, border, waveform.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

const POSITIONS: &[(&str, &str)] = &[
    ("Top Right", "top-right"),
    ("Top Left", "top-left"),
    ("Bottom Right", "bottom-right"),
    ("Bottom Left", "bottom-left"),
    ("Bottom Center", "bottom-center"),
    ("Top Center", "top-center"),
];

const BORDER_STYLES: &[(&str, &str)] = &[
    ("Rainbow", "rainbow"),
    ("Blue", "blue"),
    ("Green", "green"),
    ("Purple", "purple"),
    ("Orange", "orange"),
    ("None", "none"),
];

const WAVEFORM_STYLES: &[(&str, &str)] = &[
    ("Bars", "bars"),
    ("Bars Center", "bars-center"),
    ("Bars Round", "bars-round"),
    ("Line", "line"),
    ("Dots", "dots"),
];

const IDLE_OPACITIES: &[(&str, f64)] = &[
    ("Nearly Invisible", 0.1),
    ("Subtle", 0.3),
    ("Visible", 0.6),
];

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Overlay")
        .icon_name("view-reveal-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Overlay Appearance")
        .description("Configure the recording overlay pill")
        .build();

    // Show overlay (placeholder — actual visibility managed by pill window)
    let show_row = adw::SwitchRow::builder()
        .title("Show Overlay")
        .subtitle("Display the recording pill overlay")
        .active(true)
        .build();
    show_row.connect_active_notify(move |row| {
        log(&format!("Show overlay: {}", row.is_active()));
    });
    group.add(&show_row);

    // Idle opacity
    let opacity_items: Vec<&str> = IDLE_OPACITIES.iter().map(|(name, _)| *name).collect();
    let opacity_model = gtk4::StringList::new(&opacity_items);
    let opacity_row = adw::ComboRow::builder()
        .title("Idle Opacity")
        .subtitle("Overlay transparency when not recording")
        .model(&opacity_model)
        .build();
    opacity_row.set_selected(1); // default: Subtle
    opacity_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < IDLE_OPACITIES.len() {
            log(&format!("Idle opacity: {} ({})", IDLE_OPACITIES[idx].0, IDLE_OPACITIES[idx].1));
        }
    });
    group.add(&opacity_row);

    // Position
    let pos_items: Vec<&str> = POSITIONS.iter().map(|(name, _)| *name).collect();
    let pos_model = gtk4::StringList::new(&pos_items);
    let position_row = adw::ComboRow::builder()
        .title("Position")
        .subtitle("Where the overlay appears on screen")
        .model(&pos_model)
        .build();

    let current_pos = app_state.overlay_position.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let pos_idx = POSITIONS.iter().position(|(_, val)| *val == current_pos).unwrap_or(0);
    position_row.set_selected(pos_idx as u32);

    let state_pos = app_state.clone();
    position_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < POSITIONS.len() {
            let val = POSITIONS[idx].1.to_string();
            *state_pos.overlay_position.lock().unwrap_or_else(|e| e.into_inner()) = val.clone();
            log(&format!("Overlay position: {}", val));
        }
    });
    group.add(&position_row);

    // Reset position
    let reset_btn = gtk4::Button::builder()
        .label("Reset Position")
        .css_classes(vec!["flat".to_string()])
        .build();

    let state_reset = app_state.clone();
    reset_btn.connect_clicked(move |_| {
        *state_reset.window_anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
        log("Overlay position reset");
    });

    let reset_row = adw::ActionRow::builder()
        .title("Reset Position")
        .subtitle("Reset overlay to default screen position")
        .activatable_widget(&reset_btn)
        .build();
    reset_row.add_suffix(&reset_btn);
    group.add(&reset_row);

    // Border style
    let border_items: Vec<&str> = BORDER_STYLES.iter().map(|(name, _)| *name).collect();
    let border_model = gtk4::StringList::new(&border_items);
    let border_row = adw::ComboRow::builder()
        .title("Border Style")
        .subtitle("Overlay border color during recording")
        .model(&border_model)
        .build();

    let current_border = app_state.border_style.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let border_idx = BORDER_STYLES.iter().position(|(_, val)| *val == current_border).unwrap_or(0);
    border_row.set_selected(border_idx as u32);

    let state_border = app_state.clone();
    border_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < BORDER_STYLES.len() {
            let val = BORDER_STYLES[idx].1.to_string();
            *state_border.border_style.lock().unwrap_or_else(|e| e.into_inner()) = val.clone();
            log(&format!("Border style: {}", val));
        }
    });
    group.add(&border_row);

    // Waveform style
    let wave_items: Vec<&str> = WAVEFORM_STYLES.iter().map(|(name, _)| *name).collect();
    let wave_model = gtk4::StringList::new(&wave_items);
    let wave_row = adw::ComboRow::builder()
        .title("Waveform Style")
        .subtitle("Audio visualization in the overlay")
        .model(&wave_model)
        .build();

    let current_wave = app_state.waveform_style.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let wave_idx = WAVEFORM_STYLES.iter().position(|(_, val)| *val == current_wave).unwrap_or(0);
    wave_row.set_selected(wave_idx as u32);

    let state_wave = app_state.clone();
    wave_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < WAVEFORM_STYLES.len() {
            let val = WAVEFORM_STYLES[idx].1.to_string();
            *state_wave.waveform_style.lock().unwrap_or_else(|e| e.into_inner()) = val.clone();
            log(&format!("Waveform style: {}", val));
        }
    });
    group.add(&wave_row);

    page.add(&group);
    page
}

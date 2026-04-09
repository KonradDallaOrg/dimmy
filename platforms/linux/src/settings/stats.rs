//! Stats tab (Advanced only) — word count, speaking time, time saved.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::AppState;
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

/// Format seconds into human-readable time string.
fn format_time(secs: f64) -> String {
    assert!(secs.is_finite(), "format_time: secs must be finite, got {}", secs);
    assert!(secs >= 0.0, "format_time: secs must be non-negative, got {}", secs);

    let total_secs = secs as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let remaining = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m {}s", minutes, remaining)
    }
}

/// Calculate time saved: words * (1/40 - 1/150) * 60 seconds.
///
/// Typing speed ~40 WPM, speaking speed ~150 WPM.
/// The difference is the time saved per word in minutes, converted to seconds.
fn time_saved_secs(words: u64) -> f64 {
    let words_f = words as f64;
    let saved = words_f * (1.0 / 40.0 - 1.0 / 150.0) * 60.0;
    assert!(saved.is_finite(), "time_saved must be finite");
    assert!(saved >= 0.0, "time_saved must be non-negative");
    saved
}

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Stats")
        .icon_name("utilities-system-monitor-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Usage Statistics")
        .description("Lifetime dictation metrics")
        .build();

    // Total words
    let total_words = *app_state.stats_total_words.lock().unwrap_or_else(|e| e.into_inner());
    let words_row = adw::ActionRow::builder()
        .title("Total Words Dictated")
        .build();
    let words_label = gtk4::Label::new(Some(&format!("{}", total_words)));
    words_label.add_css_class("title-3");
    words_row.add_suffix(&words_label);
    group.add(&words_row);

    // Total speaking time
    let total_secs = *app_state.stats_total_speaking_secs.lock().unwrap_or_else(|e| e.into_inner());
    let time_row = adw::ActionRow::builder()
        .title("Total Speaking Time")
        .build();
    let time_label = gtk4::Label::new(Some(&format_time(total_secs)));
    time_label.add_css_class("title-3");
    time_row.add_suffix(&time_label);
    group.add(&time_row);

    // Time saved
    let saved = time_saved_secs(total_words);
    let saved_row = adw::ActionRow::builder()
        .title("Time Saved")
        .subtitle("vs typing at 40 WPM")
        .build();
    let saved_label = gtk4::Label::new(Some(&format_time(saved)));
    saved_label.add_css_class("title-3");
    saved_row.add_suffix(&saved_label);
    group.add(&saved_row);

    page.add(&group);
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_time_zero() {
        assert_eq!(format_time(0.0), "0m 0s");
    }

    #[test]
    fn format_time_minutes() {
        assert_eq!(format_time(125.0), "2m 5s");
    }

    #[test]
    fn format_time_hours() {
        assert_eq!(format_time(3661.0), "1h 1m");
    }

    #[test]
    fn time_saved_zero_words() {
        assert_eq!(time_saved_secs(0), 0.0);
    }

    #[test]
    fn time_saved_positive() {
        let saved = time_saved_secs(1000);
        assert!(saved > 0.0);
        // 1000 * (1/40 - 1/150) * 60 = 1000 * 0.01833... * 60 = 1100
        assert!((saved - 1100.0).abs() < 1.0);
    }
}

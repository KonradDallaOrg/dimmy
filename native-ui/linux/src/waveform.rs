//! Waveform visualization widget for the pill overlay.
//! Custom gtk4::DrawingArea with 5 animation styles matching Windows/macOS.

use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaveformStyle {
    Bars,
    BarsCenter,
    BarsRound,
    Line,
    Dots,
}

impl WaveformStyle {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Bars Center" => Self::BarsCenter,
            "Bars Round" => Self::BarsRound,
            "Line" => Self::Line,
            "Dots" => Self::Dots,
            _ => Self::Bars,
        }
    }
}

const BAR_WEIGHTS: [f64; 7] = [0.3, 0.5, 0.7, 1.0, 0.7, 0.5, 0.3];
const DOT_WEIGHTS: [f64; 5] = [0.4, 0.7, 1.0, 0.7, 0.4];
const BAR_MIN_HEIGHT: f64 = 3.0;
const BAR_MAX_HEIGHT: f64 = 16.0;
const DOT_MIN_SIZE: f64 = 3.0;
const DOT_MAX_SIZE: f64 = 10.0;
const BAR_WIDTH: f64 = 3.0;
const BAR_ROUND_WIDTH: f64 = 4.0;
const BAR_SPACING: f64 = 2.0;
const COLOR_WHITE: (f64, f64, f64, f64) = (1.0, 1.0, 1.0, 0.9); // #E6FFFFFF

/// Create a waveform DrawingArea widget.
/// Returns (widget, amplitude_setter) — call amplitude_setter(value) to update.
pub fn create_waveform_widget(
    style: WaveformStyle,
    width: i32,
    height: i32,
) -> (gtk4::DrawingArea, Rc<Cell<f32>>) {
    let amplitude = Rc::new(Cell::new(0.0f32));
    let jitter_seed = Rc::new(Cell::new(0u32));

    let area = gtk4::DrawingArea::new();
    area.set_size_request(width, height);
    area.set_hexpand(false);
    area.set_vexpand(false);

    let amp_clone = amplitude.clone();
    let seed_clone = jitter_seed.clone();

    area.set_draw_func(move |_area, cr, w, h| {
        let amp = amp_clone.get() as f64;
        let seed = seed_clone.get();
        // Increment seed for next frame's jitter
        seed_clone.set(seed.wrapping_add(1));

        cr.set_source_rgba(COLOR_WHITE.0, COLOR_WHITE.1, COLOR_WHITE.2, COLOR_WHITE.3);

        match style {
            WaveformStyle::Bars => draw_bars(cr, w as f64, h as f64, amp, seed, false, false),
            WaveformStyle::BarsCenter => draw_bars(cr, w as f64, h as f64, amp, seed, true, false),
            WaveformStyle::BarsRound => draw_bars(cr, w as f64, h as f64, amp, seed, false, true),
            WaveformStyle::Line => draw_line(cr, w as f64, h as f64, amp, seed),
            WaveformStyle::Dots => draw_dots(cr, w as f64, h as f64, amp, seed),
        }
    });

    (area, amplitude)
}

/// Simple pseudo-random jitter from seed (deterministic per-frame).
fn jitter(seed: u32, index: usize) -> f64 {
    let hash = seed
        .wrapping_mul(2_654_435_761)
        .wrapping_add(index as u32 * 1_234_567);
    let normalized = (hash % 1000) as f64 / 1000.0; // 0.0..1.0
    0.8 + normalized * 0.4 // 0.8..1.2 (±20%)
}

fn draw_bars(
    cr: &gtk4::cairo::Context,
    w: f64,
    h: f64,
    amp: f64,
    seed: u32,
    center: bool,
    round: bool,
) {
    let bar_w = if round { BAR_ROUND_WIDTH } else { BAR_WIDTH };
    let count = BAR_WEIGHTS.len();
    let total_w = count as f64 * bar_w + (count - 1) as f64 * BAR_SPACING;
    let start_x = (w - total_w) / 2.0;

    for (i, &weight) in BAR_WEIGHTS.iter().enumerate() {
        let bar_h = BAR_MIN_HEIGHT + (BAR_MAX_HEIGHT - BAR_MIN_HEIGHT) * amp * weight * jitter(seed, i);
        let bar_h = bar_h.clamp(BAR_MIN_HEIGHT, BAR_MAX_HEIGHT);
        let x = start_x + i as f64 * (bar_w + BAR_SPACING);

        let y = if center {
            (h - bar_h) / 2.0
        } else {
            h - bar_h
        };

        if round {
            let radius = bar_w / 2.0;
            // Rounded rectangle via arcs
            cr.new_sub_path();
            cr.arc(x + radius, y + radius, radius, std::f64::consts::PI, 0.0);
            cr.arc(
                x + radius,
                y + bar_h - radius,
                radius,
                0.0,
                std::f64::consts::PI,
            );
            cr.close_path();
            let _ = cr.fill();
        } else {
            cr.rectangle(x, y, bar_w, bar_h);
            let _ = cr.fill();
        }
    }
}

fn draw_line(cr: &gtk4::cairo::Context, w: f64, h: f64, amp: f64, seed: u32) {
    let count = BAR_WEIGHTS.len();
    let spacing = w / (count + 1) as f64;

    cr.set_line_width(2.0);
    cr.set_line_cap(gtk4::cairo::LineCap::Round);
    cr.set_line_join(gtk4::cairo::LineJoin::Round);

    for (i, &weight) in BAR_WEIGHTS.iter().enumerate() {
        let x = spacing * (i + 1) as f64;
        let displacement = (BAR_MAX_HEIGHT / 2.0) * amp * weight * jitter(seed, i);
        let y = h / 2.0 - displacement;

        if i == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();
}

fn draw_dots(cr: &gtk4::cairo::Context, w: f64, h: f64, amp: f64, seed: u32) {
    let count = DOT_WEIGHTS.len();
    let spacing = w / (count + 1) as f64;

    for (i, &weight) in DOT_WEIGHTS.iter().enumerate() {
        let x = spacing * (i + 1) as f64;
        let y = h / 2.0;
        let size =
            DOT_MIN_SIZE + (DOT_MAX_SIZE - DOT_MIN_SIZE) * amp * weight * jitter(seed, i);
        let size = size.clamp(DOT_MIN_SIZE, DOT_MAX_SIZE);
        let radius = size / 2.0;

        cr.new_sub_path();
        cr.arc(x, y, radius, 0.0, 2.0 * std::f64::consts::PI);
        let _ = cr.fill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_style_from_str_roundtrip() {
        assert_eq!(WaveformStyle::from_str("Bars"), WaveformStyle::Bars);
        assert_eq!(
            WaveformStyle::from_str("Bars Center"),
            WaveformStyle::BarsCenter
        );
        assert_eq!(
            WaveformStyle::from_str("Bars Round"),
            WaveformStyle::BarsRound
        );
        assert_eq!(WaveformStyle::from_str("Line"), WaveformStyle::Line);
        assert_eq!(WaveformStyle::from_str("Dots"), WaveformStyle::Dots);
        assert_eq!(WaveformStyle::from_str("unknown"), WaveformStyle::Bars);
    }

    #[test]
    fn jitter_is_deterministic() {
        let j1 = jitter(42, 0);
        let j2 = jitter(42, 0);
        assert!((j1 - j2).abs() < f64::EPSILON);
    }

    #[test]
    fn jitter_range_is_valid() {
        for seed in 0..100 {
            for idx in 0..7 {
                let j = jitter(seed, idx);
                assert!(
                    j >= 0.8 && j <= 1.2,
                    "jitter({}, {}) = {} out of range",
                    seed,
                    idx,
                    j
                );
            }
        }
    }

    #[test]
    fn bar_weights_are_symmetric() {
        assert_eq!(BAR_WEIGHTS[0], BAR_WEIGHTS[6]);
        assert_eq!(BAR_WEIGHTS[1], BAR_WEIGHTS[5]);
        assert_eq!(BAR_WEIGHTS[2], BAR_WEIGHTS[4]);
    }

    #[test]
    fn dot_weights_are_symmetric() {
        assert_eq!(DOT_WEIGHTS[0], DOT_WEIGHTS[4]);
        assert_eq!(DOT_WEIGHTS[1], DOT_WEIGHTS[3]);
    }
}

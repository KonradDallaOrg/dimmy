//! Property-based tests for the audio preprocess pipeline.
//!
//! These guard the invariants that are easy to violate during DSP edits and
//! that have shipped as regressions in the past:
//! - AUDIO-001: feeding zero-amplitude samples to dagc (AGC) produced
//!   permanent NaN corruption. Documented in docs/dev/known-bugs.md.
//! - General: any frame must remain finite and clamped to [-1.0, 1.0]
//!   before reaching whisper / the cloud uploader.
//!
//! Run with: `cargo test --test preprocess_properties`

use dimmy_lib::preprocess::process_buffer;
use proptest::prelude::*;

/// Sample rates we expect to see in the wild. 48000 enables the VAD path,
/// the others fall through to the highpass+AGC-only path. Both branches
/// must satisfy the same output invariants.
const SAMPLE_RATES: &[u32] = &[16_000, 44_100, 48_000];

/// Assert the universal output contract: every sample is finite and within
/// the [-1.0, 1.0] envelope. Any violation here is shipping-blocking — it
/// either crashes whisper or distorts the cloud-uploaded WAV.
fn assert_output_contract(out: &[f32]) {
    for (i, &s) in out.iter().enumerate() {
        assert!(
            s.is_finite(),
            "non-finite sample at index {}: {} (NaN={}, Inf={})",
            i,
            s,
            s.is_nan(),
            s.is_infinite()
        );
        assert!(
            (-1.0..=1.0).contains(&s),
            "sample at index {} outside [-1.0, 1.0]: {}",
            i,
            s
        );
    }
}

proptest! {
    /// Arbitrary in-range input must never produce NaN/Inf or out-of-envelope output.
    /// Length cap of 96000 ≈ 2s at 48kHz — large enough to flush the 480-sample
    /// VAD frame buffer, small enough to keep the test loop fast.
    #[test]
    fn finite_input_produces_finite_clamped_output(
        samples in proptest::collection::vec(-2.0_f32..2.0_f32, 0..96_000),
        sr_idx in 0usize..SAMPLE_RATES.len(),
    ) {
        let sr = SAMPLE_RATES[sr_idx];
        let out = process_buffer(&samples, sr);
        assert_output_contract(&out);
    }

    /// AUDIO-001 guard: an all-zero buffer must not trigger NaN propagation
    /// in the AGC stage. Length is randomized to cover both "shorter than
    /// one VAD frame" and "many frames" code paths.
    #[test]
    fn all_zero_input_does_not_produce_nan(
        len in 0usize..96_000,
        sr_idx in 0usize..SAMPLE_RATES.len(),
    ) {
        let sr = SAMPLE_RATES[sr_idx];
        let samples = vec![0.0_f32; len];
        let out = process_buffer(&samples, sr);
        assert_output_contract(&out);
    }

    /// Hot signal (constant amplitude near the rails) must be clamped, not
    /// allowed to amplify past 1.0 by AGC overshoot.
    #[test]
    fn near_rail_input_stays_clamped(
        amplitude in 0.5_f32..0.99_f32,
        len in 480usize..48_000,
        sr_idx in 0usize..SAMPLE_RATES.len(),
    ) {
        let sr = SAMPLE_RATES[sr_idx];
        // Slow sinusoid at 100 Hz so the highpass (80 Hz cutoff) doesn't kill it.
        let samples: Vec<f32> = (0..len)
            .map(|i| (i as f32 * 100.0 * std::f32::consts::TAU / sr as f32).sin() * amplitude)
            .collect();
        let out = process_buffer(&samples, sr);
        assert_output_contract(&out);
    }
}

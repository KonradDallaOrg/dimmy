//! Neural-network noise suppressor for the mic capture path.
//!
//! Plugged upstream of AEC3 in the `dimmy-aec` worker: each mic frame
//! (480 samples @ 48 kHz mono) goes through NN inference first, then
//! feeds AEC3 as the capture signal. NN handles steady-state noise
//! (fan, HVAC, traffic, breath, keyboard typing); AEC handles the
//! speaker→mic acoustic loop. Stacked together they approximate what
//! Krisp / NVIDIA Maxine / Zoom Studio Quality offer commercially.
//!
//! ## MVP backend: nnnoiseless (RNNoise port)
//!
//! `nnnoiseless` is already a direct dependency, ships its model
//! embedded in the binary (~85 KB), runs pure-Rust with no ONNX
//! runtime, and produces ~1 ms inference per frame on a typical
//! laptop CPU. It's not state-of-the-art (DeepFilterNet3 is) but it
//! is robust, has been in production use for years, and gets us a
//! working denoise stage **with zero installer / bundle changes**.
//! Upgrading to DFN3 later is mechanical — swap the inner state +
//! the per-frame call, the agreed-upon API surface here is the same.
//!
//! ## Frame format quirk
//!
//! nnnoiseless was ported from RNNoise's C implementation and kept
//! its quirk of using `f32` samples in the **i16 range**
//! (`[-32768.0, 32767.0]`), NOT the standard audio engineer
//! `[-1.0, 1.0]`. We convert in/out inside `process_frame` so callers
//! can stay in normalised-float land.
//!
//! ## Frame size + sample rate
//!
//! 480 samples @ 48 kHz = 10 ms. Exactly matches the AEC3 frame
//! cadence (`aec.rs` FRAME_SAMPLES) and `MEETING_CANONICAL_RATE` in
//! `audio.rs`, so the chain mic_callback → resample to 48 k → DFN →
//! AEC3 needs zero internal resampling.
//!
//! ## Config gate
//!
//! `try_init()` consults the runtime config field `denoise_enabled`
//! (default true). When the user disables it via Settings, AEC falls
//! through to the AEC3-only path without re-spawning anything; the
//! processor itself is only created when AEC starts.

use std::sync::atomic::{AtomicBool, Ordering};

/// Runtime toggle for the denoiser. Defaults to true so the first
/// AEC start always creates a processor; the host UI flips this via
/// `dimmy_set_denoise_enabled()` when the user touches the Settings
/// switch. Storing it as a free static (not an AppState field) keeps
/// the dfn module independent of the FFI state lifecycle — the
/// global init order doesn't matter for a simple atomic.
pub static DENOISE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Stateful neural-network denoiser. One `DenoiseState` per instance.
/// Keep it local to whichever worker drives it — the RNN state
/// inside doesn't try to be Sync.
pub struct DfnProcessor {
    state: Box<nnnoiseless::DenoiseState<'static>>,
}

impl DfnProcessor {
    /// Try to construct a denoiser, honouring the runtime config
    /// toggle. Returns `None` when the user has disabled denoise OR
    /// the global state hasn't initialised yet (very early startup —
    /// caller falls through to no-op, which matches the behaviour
    /// during `dimmy_init` before the config is loaded).
    pub fn try_init() -> Option<Self> {
        if !current_denoise_enabled() {
            crate::log("[Denoise] disabled by config — AEC mic path runs DFN-bypass");
            return None;
        }
        crate::log(
            "[Denoise] nnnoiseless active on mic capture (480-sample frames @ 48 kHz, ~1 ms/frame)",
        );
        Some(Self {
            state: nnnoiseless::DenoiseState::new(),
        })
    }

    /// Process exactly one frame. `src` and `dest` must both be 480
    /// samples in normalised `[-1.0, 1.0]` float. The function scales
    /// to the i16 range internally for nnnoiseless and back on output.
    pub fn process_frame(&mut self, src: &[f32], dest: &mut [f32]) {
        assert_eq!(src.len(), Self::FRAME_SIZE);
        assert_eq!(dest.len(), Self::FRAME_SIZE);
        // Scale to i16 range (nnnoiseless quirk). Clamp BEFORE scaling
        // so we don't push the model outside its training range.
        let mut scaled_in = [0.0f32; Self::FRAME_SIZE];
        for (i, &s) in src.iter().enumerate() {
            scaled_in[i] = s.clamp(-1.0, 1.0) * 32768.0;
        }
        let mut scaled_out = [0.0f32; Self::FRAME_SIZE];
        // process_frame returns a VAD probability we currently
        // ignore — could feed a future "speech-only segments" path.
        let _vad = self.state.process_frame(&mut scaled_out, &scaled_in);
        // Scale back to [-1, 1] and clamp as a safety net. NaN/Inf
        // would corrupt the WAV (and AEC asserts no-NaN downstream).
        for (i, &s) in scaled_out.iter().enumerate() {
            let v = s / 32768.0;
            dest[i] = if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
    }

    pub const FRAME_SIZE: usize = 480;
}

fn current_denoise_enabled() -> bool {
    DENOISE_ENABLED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: feed a sine wave + noise frame, verify the
    /// denoiser produces a finite output of the right length.
    #[test]
    fn process_frame_produces_finite_output_in_range() {
        let mut p = DfnProcessor {
            state: nnnoiseless::DenoiseState::new(),
        };
        let mut input = [0.0f32; DfnProcessor::FRAME_SIZE];
        for (i, s) in input.iter_mut().enumerate() {
            let t = i as f32 / 48000.0;
            let sine = (440.0 * 2.0 * std::f32::consts::PI * t).sin() * 0.5;
            let noise = (i as f32 * 0.13).sin() * 0.1;
            *s = (sine + noise).clamp(-1.0, 1.0);
        }
        let mut output = [0.0f32; DfnProcessor::FRAME_SIZE];
        p.process_frame(&input, &mut output);
        for &s in output.iter() {
            assert!(s.is_finite(), "denoiser produced NaN/Inf: {}", s);
            assert!(
                (-1.0..=1.0).contains(&s),
                "denoiser sample out of [-1, 1] range: {}",
                s
            );
        }
    }

    /// Verify the denoiser is genuinely a stateful processor: the
    /// first frame primes the RNN with a fade-in, so feeding the
    /// same input twice in a row gives slightly different outputs.
    #[test]
    fn denoiser_is_stateful_across_frames() {
        let mut p = DfnProcessor {
            state: nnnoiseless::DenoiseState::new(),
        };
        let mut input = [0.0f32; DfnProcessor::FRAME_SIZE];
        for (i, s) in input.iter_mut().enumerate() {
            *s = ((i as f32) * 0.05).sin() * 0.3;
        }
        let mut out_a = [0.0f32; DfnProcessor::FRAME_SIZE];
        let mut out_b = [0.0f32; DfnProcessor::FRAME_SIZE];
        p.process_frame(&input, &mut out_a);
        p.process_frame(&input, &mut out_b);
        let differ = out_a
            .iter()
            .zip(out_b.iter())
            .any(|(a, b)| (a - b).abs() > 1e-6);
        assert!(
            differ,
            "denoiser appears stateless — RNN state didn't evolve"
        );
    }
}

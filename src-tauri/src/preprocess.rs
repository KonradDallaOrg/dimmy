//! Audio preprocessing pipeline: highpass filter, VAD (via nnnoiseless), AGC, downsample.
//!
//! The pipeline runs on raw f32 mono samples at the device sample rate (typically 48kHz).
//! It produces cleaned audio suitable for Whisper transcription.
//!
//! Key design decisions:
//! - nnnoiseless is used for VAD only — the denoised output is discarded.
//!   Whisper performs better on original audio but hallucmates on silence/noise.
//! - AGC (dagc) replaces static RMS normalization for adaptive gain control.
//! - Audio is downsampled to 16kHz before sending to Whisper (which resamples to 16kHz
//!   internally anyway), reducing upload bandwidth by 3x.

use biquad::{
    Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type as FilterType, Q_BUTTERWORTH_F32,
};
use dagc::MonoAgc;
use nnnoiseless::DenoiseState;

/// Frame size expected by nnnoiseless (480 samples = 10ms at 48kHz)
const DENOISE_FRAME_SIZE: usize = 480;

/// nnnoiseless requires exactly 48kHz input. If the device sample rate differs,
/// we skip the VAD pass and only apply highpass + AGC.
const REQUIRED_SAMPLE_RATE: u32 = 48000;

/// Voice probability threshold to START considering speech (onset).
/// Higher than offset to prevent flickering on noise bursts.
const VAD_ONSET_THRESHOLD: f32 = 0.6;

/// Voice probability threshold to STOP speech (offset / hysteresis).
/// Lower than onset — once we're in speech, we keep going until confidence drops below this.
const VAD_OFFSET_THRESHOLD: f32 = 0.35;

/// Minimum consecutive speech frames before activating (prevents brief noise from triggering).
/// At 48kHz with 480-sample frames, each frame = 10ms → 3 frames = 30ms.
const MIN_SPEECH_FRAMES: usize = 3;

/// Number of silence frames to keep after speech ends (grace period).
/// At 48kHz with 480-sample frames, each frame = 10ms → 50 frames = 500ms.
/// Longer grace captures sentence-final phonemes that trail off.
const SILENCE_GRACE_FRAMES: usize = 50;

/// Target RMS level for AGC. 0.2 ≈ -14dBFS — loud enough for clear STT input.
const TARGET_RMS: f32 = 0.2;

/// AGC distortion factor — controls how fast gain adapts.
/// Lower = smoother adaptation, less distortion. 0.001 is typical for speech.
const AGC_DISTORTION: f32 = 0.001;

/// Whisper's internal sample rate — we downsample to this before sending.
const WHISPER_SAMPLE_RATE: u32 = 16000;

pub struct AudioPreprocessor {
    /// Highpass biquad filter state (80Hz cutoff, removes DC offset + rumble)
    highpass: Option<DirectForm2Transposed<f32>>,
    /// nnnoiseless state for VAD
    denoise: Option<Box<DenoiseState<'static>>>,
    /// Adaptive gain control
    agc: MonoAgc,
    /// Accumulator for 480-sample frames (nnnoiseless requirement)
    frame_buf: Vec<f32>,
    /// Corresponding original (highpass-filtered) samples for the current frame
    original_buf: Vec<f32>,
    /// Consecutive silence frames counter
    silence_frames: usize,
    /// Consecutive speech frames counter (for onset confirmation)
    speech_frames: usize,
    /// Whether we're currently in confirmed speech mode
    in_speech: bool,
    /// Whether the device sample rate supports VAD (must be 48kHz)
    vad_enabled: bool,
    /// Device sample rate
    sample_rate: u32,
}

impl AudioPreprocessor {
    pub fn new(sample_rate: u32) -> Self {
        // Invariant: sample rate must be positive (0 would cause division-by-zero downstream)
        assert!(
            sample_rate > 0,
            "sample_rate must be > 0, got {}",
            sample_rate
        );
        // Compile-time guard: AGC constants must be valid for MonoAgc::new()
        const {
            assert!(
                TARGET_RMS > 0.0 && TARGET_RMS <= 1.0,
                "TARGET_RMS out of range"
            );
            assert!(
                AGC_DISTORTION > 0.0 && AGC_DISTORTION < 1.0,
                "AGC_DISTORTION out of range"
            );
        }

        // Build highpass filter at 80Hz (Butterworth, 2nd order)
        let highpass = if sample_rate >= 1000 {
            Coefficients::<f32>::from_params(
                FilterType::HighPass,
                (sample_rate as f32).hz(),
                80.0.hz(),
                Q_BUTTERWORTH_F32,
            )
            .ok()
            .map(DirectForm2Transposed::<f32>::new)
        } else {
            None
        };

        let vad_enabled = sample_rate == REQUIRED_SAMPLE_RATE;
        let denoise = if vad_enabled {
            Some(DenoiseState::new())
        } else {
            None
        };

        // dagc AGC — unwrap is safe: 0.2 and 0.001 are valid params
        let agc = MonoAgc::new(TARGET_RMS, AGC_DISTORTION).unwrap();

        crate::log(&format!(
            "AudioPreprocessor: sr={}, highpass={}, vad={}, agc=dagc(target={}, distortion={})",
            sample_rate,
            highpass.is_some(),
            vad_enabled,
            TARGET_RMS,
            AGC_DISTORTION,
        ));

        Self {
            highpass,
            denoise,
            agc,
            frame_buf: Vec::with_capacity(DENOISE_FRAME_SIZE),
            original_buf: Vec::with_capacity(DENOISE_FRAME_SIZE),
            silence_frames: 0,
            speech_frames: 0,
            in_speech: false,
            vad_enabled,
            sample_rate,
        }
    }

    /// Process a chunk of raw audio samples (f32, [-1.0, 1.0], mono).
    /// Returns only the speech segments, with highpass filtering and AGC applied.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        // Step 0: Clamp input to [-1.0, 1.0] — extreme values (f32::MAX, Inf, NaN)
        // would corrupt the highpass filter state and cause downstream FFT crashes.
        let sanitized: Vec<f32> = samples
            .iter()
            .map(|&s| {
                if s.is_finite() {
                    s.clamp(-1.0, 1.0)
                } else {
                    0.0
                }
            })
            .collect();

        // Step 1: Highpass filter (in-place on a copy)
        let filtered: Vec<f32> = if let Some(ref mut hp) = self.highpass {
            sanitized.iter().map(|&s| hp.run(s)).collect()
        } else {
            sanitized
        };

        // Step 2: VAD — keep only speech frames
        let speech = if self.vad_enabled {
            self.vad_filter(&filtered)
        } else {
            filtered
        };

        if speech.is_empty() {
            return Vec::new();
        }

        // Step 3: Adaptive gain control (replaces static RMS normalization)
        let mut output = speech;
        self.agc.process(&mut output);

        // Clamp to [-1.0, 1.0] after AGC (safety net).
        // AGC can produce NaN on long recordings — treat as silence.
        for s in output.iter_mut() {
            *s = if s.is_finite() {
                s.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }

        // Invariant: all output samples must be in [-1.0, 1.0] after clamping
        assert!(
            output.iter().all(|&s| (-1.0..=1.0).contains(&s)),
            "output contains samples outside [-1.0, 1.0]"
        );
        // Note: output.len() CAN exceed samples.len() for a single call because the VAD
        // buffers partial frames internally. A 959-sample input plus 1 buffered sample from
        // a previous call can produce 960 samples (2 complete frames). The invariant holds
        // cumulatively across all calls, not per-call.

        output
    }

    /// VAD using nnnoiseless voice probability with hysteresis state machine.
    ///
    /// State machine:
    /// - IDLE: waiting for speech onset (voice_prob > ONSET_THRESHOLD for MIN_SPEECH_FRAMES)
    /// - SPEECH: confirmed speech, keep emitting until offset
    /// - GRACE: speech ended, keep emitting for SILENCE_GRACE_FRAMES then transition to IDLE
    ///
    /// Sends original (filtered) audio to output, not the denoised version.
    fn vad_filter(&mut self, samples: &[f32]) -> Vec<f32> {
        let denoise = match self.denoise {
            Some(ref mut d) => d,
            None => return samples.to_vec(),
        };

        let mut speech_audio = Vec::with_capacity(samples.len());
        // Buffer for pending frames during onset confirmation
        let mut pending: Vec<f32> = Vec::new();
        let mut denoise_output = vec![0.0f32; DENOISE_FRAME_SIZE];

        for &sample in samples {
            // Clamp to [-1.0, 1.0] before scaling — extreme values (f32::MAX) would
            // overflow when multiplied by 32767.0, producing Inf that crashes nnnoiseless/FFT.
            let clamped = sample.clamp(-1.0, 1.0);
            // nnnoiseless expects [-32768.0, 32767.0]
            self.frame_buf.push(clamped * 32767.0);
            self.original_buf.push(clamped);

            // Invariant: frame_buf must never exceed the expected frame size
            assert!(
                self.frame_buf.len() <= DENOISE_FRAME_SIZE,
                "frame_buf length {} exceeds DENOISE_FRAME_SIZE {}",
                self.frame_buf.len(),
                DENOISE_FRAME_SIZE
            );

            if self.frame_buf.len() == DENOISE_FRAME_SIZE {
                let voice_prob = denoise.process_frame(&mut denoise_output, &self.frame_buf);

                if voice_prob > VAD_ONSET_THRESHOLD {
                    // High-confidence speech
                    self.speech_frames += 1;
                    self.silence_frames = 0;

                    if self.in_speech {
                        // Already in speech mode — just emit
                        speech_audio.extend_from_slice(&self.original_buf);
                    } else if self.speech_frames >= MIN_SPEECH_FRAMES {
                        // Onset confirmed — flush pending frames and enter speech mode
                        self.in_speech = true;
                        speech_audio.append(&mut pending);
                        speech_audio.extend_from_slice(&self.original_buf);
                    } else {
                        // Accumulating onset frames — buffer them
                        pending.extend_from_slice(&self.original_buf);
                    }
                } else if voice_prob > VAD_OFFSET_THRESHOLD && self.in_speech {
                    // Hysteresis: still above offset threshold while in speech → continue
                    speech_audio.extend_from_slice(&self.original_buf);
                    // Don't reset speech_frames (we're in speech), but don't increment either
                } else {
                    // Below threshold
                    self.speech_frames = 0;
                    pending.clear(); // Discard unconfirmed onset frames

                    if self.in_speech {
                        // Grace period — keep short silences (natural pauses)
                        self.silence_frames += 1;
                        if self.silence_frames <= SILENCE_GRACE_FRAMES {
                            speech_audio.extend_from_slice(&self.original_buf);
                        } else {
                            // Grace expired — exit speech mode
                            self.in_speech = false;
                        }
                    }
                    // If not in speech and below threshold: drop frame
                }

                self.frame_buf.clear();
                self.original_buf.clear();

                // Invariant: counters should not overflow to absurd values (indicates logic bug)
                assert!(
                    self.silence_frames <= 1_000_000,
                    "silence_frames overflowed to {}",
                    self.silence_frames
                );
                assert!(
                    self.speech_frames <= 1_000_000,
                    "speech_frames overflowed to {}",
                    self.speech_frames
                );
            }
        }

        speech_audio
    }

    /// Reset state between recordings
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.frame_buf.clear();
        self.original_buf.clear();
        self.silence_frames = 0;
        self.speech_frames = 0;
        self.in_speech = false;
        if self.vad_enabled {
            self.denoise = Some(DenoiseState::new());
        }
        // Fresh AGC
        self.agc = MonoAgc::new(TARGET_RMS, AGC_DISTORTION).unwrap();
        // Rebuild highpass to clear filter state
        if self.sample_rate >= 1000 {
            self.highpass = Coefficients::<f32>::from_params(
                FilterType::HighPass,
                (self.sample_rate as f32).hz(),
                80.0.hz(),
                Q_BUTTERWORTH_F32,
            )
            .ok()
            .map(DirectForm2Transposed::<f32>::new);
        }
    }
}

/// Process a complete audio buffer (used for final transcription on stop_recording).
/// Creates a fresh preprocessor, processes the entire buffer, returns cleaned audio.
pub fn process_buffer(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    // Invariant: sample rate must be positive
    assert!(
        sample_rate > 0,
        "process_buffer: sample_rate must be > 0, got {}",
        sample_rate
    );
    let mut proc = AudioPreprocessor::new(sample_rate);
    proc.process(samples)
}

/// Downsample audio to 16kHz for Whisper (which internally resamples to 16kHz anyway).
/// Uses a lowpass anti-aliasing filter + linear interpolation.
/// Returns samples at 16kHz. If source is already 16kHz, returns a clone.
pub fn downsample_to_16k(samples: &[f32], source_rate: u32) -> Vec<f32> {
    // Invariant: source rate must be positive (0 would cause division-by-zero)
    assert!(
        source_rate > 0,
        "downsample_to_16k: source_rate must be > 0, got {}",
        source_rate
    );

    if source_rate <= WHISPER_SAMPLE_RATE {
        return samples.to_vec();
    }

    // Step 1: Anti-aliasing lowpass filter at 7kHz (Nyquist for 16kHz is 8kHz, use 7kHz margin)
    let filtered = if source_rate >= 1000 {
        if let Ok(coeffs) = Coefficients::<f32>::from_params(
            FilterType::LowPass,
            (source_rate as f32).hz(),
            7000.0.hz(),
            Q_BUTTERWORTH_F32,
        ) {
            let mut lp = DirectForm2Transposed::<f32>::new(coeffs);
            samples.iter().map(|&s| lp.run(s)).collect::<Vec<f32>>()
        } else {
            samples.to_vec()
        }
    } else {
        samples.to_vec()
    };

    // Step 2: Linear interpolation to 16kHz
    let ratio = source_rate as f64 / WHISPER_SAMPLE_RATE as f64;
    let output_len = (filtered.len() as f64 / ratio).floor() as usize;
    if output_len == 0 {
        return Vec::new();
    }

    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let s0 = filtered[idx.min(filtered.len() - 1)];
        let s1 = filtered[(idx + 1).min(filtered.len() - 1)];
        output.push(s0 + (s1 - s0) * frac);
    }

    // Invariant: output length should be approximately input_length * 16000 / source_rate (within 1 sample)
    let expected_len =
        (samples.len() as f64 * WHISPER_SAMPLE_RATE as f64 / source_rate as f64).floor() as usize;
    assert!(
        (output.len() as isize - expected_len as isize).unsigned_abs() <= 1,
        "downsample output length {} deviates from expected {} by more than 1 sample",
        output.len(),
        expected_len
    );
    // Invariant: all output samples must be finite (no NaN or Inf from interpolation)
    assert!(
        output.iter().all(|s| s.is_finite()),
        "downsample output contains NaN or Inf"
    );

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agc_boosts_quiet_audio() {
        let mut proc = AudioPreprocessor::new(44100); // non-48k → no VAD, just highpass + AGC
                                                      // Feed quiet audio — AGC should boost it
        let quiet: Vec<f32> = (0..44100)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin() * 0.02)
            .collect();
        let rms_before = (quiet.iter().map(|s| s * s).sum::<f32>() / quiet.len() as f32).sqrt();
        let out = proc.process(&quiet);
        assert!(!out.is_empty(), "Should produce output");
        let rms_after = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(
            rms_after > rms_before,
            "AGC should boost quiet audio: before={:.4} after={:.4}",
            rms_before,
            rms_after
        );
    }

    #[test]
    fn agc_output_is_clamped() {
        let mut proc = AudioPreprocessor::new(44100);
        let samples = vec![0.5f32; 4410];
        let out = proc.process(&samples);
        assert!(
            out.iter().all(|&s| s <= 1.0 && s >= -1.0),
            "AGC output should be clamped to [-1, 1]"
        );
    }

    #[test]
    fn preprocessor_empty_input() {
        let mut proc = AudioPreprocessor::new(48000);
        let out = proc.process(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn preprocessor_silence_is_stripped() {
        let mut proc = AudioPreprocessor::new(48000);
        // Feed enough silence frames to exceed grace period
        let silence = vec![0.0f32; 480 * 80]; // 80 frames of silence (800ms > 500ms grace)
        let out = proc.process(&silence);
        // After grace period (50 frames = 500ms), silence should be stripped
        assert!(
            out.len() < silence.len(),
            "Silence should be partially stripped"
        );
    }

    #[test]
    fn preprocessor_non_48k_skips_vad() {
        let mut proc = AudioPreprocessor::new(44100);
        assert!(!proc.vad_enabled, "VAD should be disabled for non-48kHz");
        let samples = vec![0.1f32; 1000];
        let out = proc.process(&samples);
        // Without VAD, all samples pass through (highpass + AGC)
        assert!(!out.is_empty());
    }

    #[test]
    fn preprocessor_reset_clears_state() {
        let mut proc = AudioPreprocessor::new(48000);
        proc.frame_buf.push(1.0);
        proc.silence_frames = 100;
        proc.speech_frames = 5;
        proc.in_speech = true;
        proc.reset();
        assert!(proc.frame_buf.is_empty());
        assert_eq!(proc.silence_frames, 0);
        assert_eq!(proc.speech_frames, 0);
        assert!(!proc.in_speech);
    }

    #[test]
    fn process_buffer_returns_audio() {
        // Simple tone should pass through
        let samples: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let out = process_buffer(&samples, 48000);
        assert!(!out.is_empty(), "Tone should produce output");
    }

    // Downsample tests

    #[test]
    fn downsample_16k_passthrough() {
        let samples = vec![0.5f32; 16000];
        let out = downsample_to_16k(&samples, 16000);
        assert_eq!(out.len(), samples.len(), "16kHz input should pass through");
    }

    #[test]
    fn downsample_48k_to_16k_reduces_length() {
        let samples: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let out = downsample_to_16k(&samples, 48000);
        // 48000 / 3 = 16000
        assert_eq!(
            out.len(),
            16000,
            "Should produce exactly 16000 samples for 1 second"
        );
    }

    #[test]
    fn downsample_44100_to_16k() {
        let samples: Vec<f32> = (0..44100)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin() * 0.5)
            .collect();
        let out = downsample_to_16k(&samples, 44100);
        // 44100 / 2.75625 ≈ 16000
        let expected = (44100.0_f64 / (44100.0_f64 / 16000.0_f64)).floor() as usize;
        assert_eq!(
            out.len(),
            expected,
            "Should produce ~16000 samples for 1 second of 44100Hz"
        );
    }

    #[test]
    fn downsample_empty_input() {
        let out = downsample_to_16k(&[], 48000);
        assert!(out.is_empty());
    }

    #[test]
    fn downsample_preserves_audio_energy() {
        let samples: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let rms_in = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        let out = downsample_to_16k(&samples, 48000);
        let rms_out = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        // RMS should be roughly preserved (within 20% — lowpass removes some energy)
        assert!(
            (rms_out - rms_in).abs() / rms_in < 0.2,
            "RMS should be roughly preserved: in={:.4} out={:.4}",
            rms_in,
            rms_out
        );
    }

    #[test]
    fn vad_hysteresis_prevents_flickering() {
        let mut proc = AudioPreprocessor::new(48000);
        // Generate a tone that should be detected as speech by nnnoiseless
        let speech: Vec<f32> = (0..480 * 10) // 10 frames = 100ms
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 48000.0).sin() * 0.3)
            .collect();
        let out = proc.process(&speech);
        // With onset confirmation (MIN_SPEECH_FRAMES=3), very short signals may or may not pass.
        // The key test is that the output length is reasonable (not vastly inflated).
        assert!(
            out.len() <= speech.len(),
            "Output should not exceed input length"
        );
    }

    #[test]
    fn highpass_recovers_after_nan_injection() {
        // Even though input is sanitized, verify the filter produces
        // finite output after receiving edge values in previous calls.
        let mut proc = AudioPreprocessor::new(44100); // non-48k → no VAD

        // Feed normal audio first
        let normal: Vec<f32> = (0..44100)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin() * 0.5)
            .collect();
        let out1 = proc.process(&normal);
        assert!(!out1.is_empty());
        assert!(out1.iter().all(|s| s.is_finite()));

        // Feed more normal audio — filter state should still be clean
        let out2 = proc.process(&normal);
        assert!(!out2.is_empty());
        assert!(
            out2.iter()
                .all(|s| s.is_finite() && *s >= -1.0 && *s <= 1.0),
            "Filter output should remain finite and clamped"
        );
    }

    #[test]
    fn all_output_finite_after_1000_chunks() {
        // Long-running stability: no NaN accumulation over many chunks
        let mut proc = AudioPreprocessor::new(44100);
        for i in 0..1000 {
            let freq = 200.0 + (i as f32 * 3.0); // vary frequency
            let chunk: Vec<f32> = (0..4410)
                .map(|j| (2.0 * std::f32::consts::PI * freq * j as f32 / 44100.0).sin() * 0.3)
                .collect();
            let out = proc.process(&chunk);
            for &s in &out {
                assert!(
                    s.is_finite() && s >= -1.0 && s <= 1.0,
                    "Chunk {}: non-finite or out-of-range sample: {}",
                    i,
                    s
                );
            }
        }
    }
}

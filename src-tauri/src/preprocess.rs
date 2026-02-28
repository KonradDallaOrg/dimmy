//! Audio preprocessing pipeline: highpass filter, VAD (via nnnoiseless), RMS normalization.
//!
//! The pipeline runs on raw f32 mono samples at the device sample rate (typically 48kHz).
//! It produces cleaned audio suitable for Whisper transcription.
//!
//! Key design decision: we use nnnoiseless for VAD (voice activity detection) only.
//! The *denoised* output is intentionally discarded — Whisper performs better on original
//! audio, but hallucmates on silence/noise segments. VAD eliminates those segments.

use biquad::{
    Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type as FilterType, Q_BUTTERWORTH_F32,
};
use nnnoiseless::DenoiseState;

/// Frame size expected by nnnoiseless (480 samples = 10ms at 48kHz)
const DENOISE_FRAME_SIZE: usize = 480;

/// nnnoiseless requires exactly 48kHz input. If the device sample rate differs,
/// we skip the VAD pass and only apply highpass + normalization.
const REQUIRED_SAMPLE_RATE: u32 = 48000;

/// Voice probability threshold — frames below this are considered non-speech.
const VAD_THRESHOLD: f32 = 0.5;

/// Number of silence frames to keep after speech ends (grace period).
/// At 48kHz with 480-sample frames, each frame = 10ms → 30 frames = 300ms.
const SILENCE_GRACE_FRAMES: usize = 30;

/// Target RMS level for normalization. 0.2 ≈ -14dBFS — loud enough for clear STT input.
const TARGET_RMS: f32 = 0.2;

/// Maximum gain applied during normalization (prevents amplifying noise in quiet segments).
const MAX_GAIN: f32 = 10.0;

pub struct AudioPreprocessor {
    /// Highpass biquad filter state (80Hz cutoff, removes DC offset + rumble)
    highpass: Option<DirectForm2Transposed<f32>>,
    /// nnnoiseless state for VAD
    denoise: Option<Box<DenoiseState<'static>>>,
    /// Accumulator for 480-sample frames (nnnoiseless requirement)
    frame_buf: Vec<f32>,
    /// Corresponding original (highpass-filtered) samples for the current frame
    original_buf: Vec<f32>,
    /// Consecutive silence frames counter
    silence_frames: usize,
    /// Whether the device sample rate supports VAD (must be 48kHz)
    vad_enabled: bool,
    /// Device sample rate
    #[allow(dead_code)]
    sample_rate: u32,
}

impl AudioPreprocessor {
    pub fn new(sample_rate: u32) -> Self {
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

        crate::log(&format!(
            "AudioPreprocessor: sr={}, highpass={}, vad={}",
            sample_rate,
            highpass.is_some(),
            vad_enabled
        ));

        Self {
            highpass,
            denoise,
            frame_buf: Vec::with_capacity(DENOISE_FRAME_SIZE),
            original_buf: Vec::with_capacity(DENOISE_FRAME_SIZE),
            silence_frames: 0,
            vad_enabled,
            sample_rate,
        }
    }

    /// Process a chunk of raw audio samples (f32, [-1.0, 1.0], mono).
    /// Returns only the speech segments, with highpass filtering and RMS normalization applied.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        // Step 1: Highpass filter (in-place on a copy)
        let filtered: Vec<f32> = if let Some(ref mut hp) = self.highpass {
            samples.iter().map(|&s| hp.run(s)).collect()
        } else {
            samples.to_vec()
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

        // Step 3: RMS normalization
        let mut output = speech;
        normalize_rms(&mut output, TARGET_RMS);

        output
    }

    /// VAD using nnnoiseless voice probability.
    /// Sends original (filtered) audio to output, not the denoised version.
    fn vad_filter(&mut self, samples: &[f32]) -> Vec<f32> {
        let denoise = match self.denoise {
            Some(ref mut d) => d,
            None => return samples.to_vec(),
        };

        let mut speech_audio = Vec::with_capacity(samples.len());
        let mut denoise_output = vec![0.0f32; DENOISE_FRAME_SIZE];

        for &sample in samples {
            // nnnoiseless expects [-32768.0, 32767.0]
            self.frame_buf.push(sample * 32767.0);
            self.original_buf.push(sample);

            if self.frame_buf.len() == DENOISE_FRAME_SIZE {
                let voice_prob = denoise.process_frame(&mut denoise_output, &self.frame_buf);

                if voice_prob > VAD_THRESHOLD {
                    // Speech detected — push original audio (not denoised)
                    speech_audio.extend_from_slice(&self.original_buf);
                    self.silence_frames = 0;
                } else {
                    self.silence_frames += 1;
                    if self.silence_frames <= SILENCE_GRACE_FRAMES {
                        // Keep short silences (natural pauses between words)
                        speech_audio.extend_from_slice(&self.original_buf);
                    }
                    // Longer silence: drop frames → prevents Whisper hallucinations
                }

                self.frame_buf.clear();
                self.original_buf.clear();
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
        if self.vad_enabled {
            self.denoise = Some(DenoiseState::new());
        }
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

/// Normalize samples to a target RMS level, with gain capping to avoid amplifying noise.
fn normalize_rms(samples: &mut [f32], target_rms: f32) {
    if samples.is_empty() {
        return;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms > 1e-6 {
        let gain = (target_rms / rms).min(MAX_GAIN);
        for s in samples.iter_mut() {
            *s = (*s * gain).clamp(-1.0, 1.0);
        }
    }
}

/// Process a complete audio buffer (used for final transcription on stop_recording).
/// Creates a fresh preprocessor, processes the entire buffer, returns cleaned audio.
pub fn process_buffer(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    let mut proc = AudioPreprocessor::new(sample_rate);
    proc.process(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rms_silence() {
        let mut buf = vec![0.0f32; 100];
        normalize_rms(&mut buf, 0.1);
        assert!(buf.iter().all(|&s| s == 0.0), "Silence should stay silent");
    }

    #[test]
    fn normalize_rms_boosts_quiet() {
        let mut buf = vec![0.05f32; 480];
        let rms_before = (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt();
        normalize_rms(&mut buf, TARGET_RMS);
        let rms_after = (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt();
        assert!(rms_after > rms_before, "Should boost quiet audio");
        assert!(
            (rms_after - TARGET_RMS).abs() < 0.01,
            "Should be near target RMS"
        );
    }

    #[test]
    fn normalize_rms_caps_gain() {
        let mut buf = vec![0.001f32; 480];
        normalize_rms(&mut buf, TARGET_RMS);
        // gain would be 200x but capped at MAX_GAIN (10x), so result ≈ 0.01
        let rms = (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt();
        assert!(rms < TARGET_RMS, "Gain should be capped, rms={}", rms);
    }

    #[test]
    fn normalize_rms_clamps() {
        let mut buf = vec![0.5f32; 480];
        normalize_rms(&mut buf, 1.0);
        assert!(
            buf.iter().all(|&s| s <= 1.0 && s >= -1.0),
            "Should clamp to [-1, 1]"
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
        let silence = vec![0.0f32; 480 * 50]; // 50 frames of silence
        let out = proc.process(&silence);
        // After grace period, silence should be stripped
        // Grace = 30 frames * 480 samples = 14400 samples
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
        // Without VAD, all samples pass through (highpass + normalize)
        assert!(!out.is_empty());
    }

    #[test]
    fn preprocessor_reset_clears_state() {
        let mut proc = AudioPreprocessor::new(48000);
        proc.frame_buf.push(1.0);
        proc.silence_frames = 100;
        proc.reset();
        assert!(proc.frame_buf.is_empty());
        assert_eq!(proc.silence_frames, 0);
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
}

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
const VAD_ONSET_THRESHOLD: f32 = 0.5;

/// Voice probability threshold to STOP speech (offset / hysteresis).
/// Lower than onset — once we're in speech, we keep going until confidence drops below this.
const VAD_OFFSET_THRESHOLD: f32 = 0.3;

/// Minimum consecutive speech frames before activating (prevents brief noise from triggering).
/// At 48kHz with 480-sample frames, each frame = 10ms → 3 frames = 30ms.
const MIN_SPEECH_FRAMES: usize = 3;

/// Number of silence frames to keep after speech ends (grace period).
/// At 48kHz with 480-sample frames, each frame = 10ms → 300 frames = 3s.
/// Natural speech pauses can be 500ms–2s; generous grace prevents premature cutoff.
const SILENCE_GRACE_FRAMES: usize = 300;

/// RMS energy floor: frames above this are "clearly audible" and should not be
/// dropped even if nnnoiseless voice probability is low. Prevents the VAD from
/// killing loud speech when the RNN model drifts after long recordings.
const ENERGY_FLOOR: f32 = 0.015;

/// Target RMS level for AGC. 0.05 ≈ -26 dBFS.
///
/// Speech carries a large peak-to-RMS ratio: measured over 38 real dictation
/// captures from this project's `audio_debug` (2026-07-28 → 2026-08-04), the
/// median crest factor is **19.6 dB**. An RMS target therefore implies a peak
/// target 19.6 dB above it. The previous 0.2 (-14 dBFS) implied peaks at
/// **+5.6 dBFS** — above full scale — so every single one of those 38 captures
/// came out hard-clipped at exactly 1.000. -26 dBFS puts the peaks near
/// -6 dBFS, comfortably inside the rail, and whisper normalises internally
/// anyway so the absolute level costs nothing.
const TARGET_RMS: f32 = 0.05;

/// True-peak ceiling applied after the AGC, -1 dBFS.
///
/// The gain stage targets RMS, so on unusually dynamic speech it can still ask
/// for peaks above full scale even with a conservative target. Clamping those
/// (what used to happen) is hard clipping: it squares off the waveform and
/// sprays broadband harmonics across the spectrum whisper builds its mel
/// features from. Scaling the whole buffer by one factor instead changes the
/// level but not the shape, so it is inaudible to the model. This makes
/// clipping impossible by construction rather than unlikely.
const PEAK_CEILING: f32 = 0.89;

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
    /// Whether speech has been confirmed at least once in this session.
    /// After first speech, onset is easier (lower threshold) to handle
    /// nnnoiseless RNN drift on long recordings.
    has_spoken: bool,
    /// Whether the device sample rate supports VAD (must be 48kHz)
    vad_enabled: bool,
    /// Device sample rate
    sample_rate: u32,
    /// Whether the AGC stage runs. False for the realtime chunk workers:
    /// dagc is adaptive, so a per-chunk instance would settle on a different
    /// gain for each window and adjacent chunks would come out at different
    /// levels; and it produces permanent NaN on all-silence input (AUDIO-001),
    /// which is exactly what an idle chunk is.
    apply_agc: bool,
    /// Whether a frame must ALSO carry speech-level energy to count as speech.
    ///
    /// nnnoiseless scores voice likelihood from spectral shape, not level, so a
    /// keyboard click or a breath at -60 dBFS can score above the onset
    /// threshold and open a speech window. On the batch path that is harmless
    /// (the recording really does contain speech somewhere). On a realtime
    /// chunk of an idle mic it is the whole problem: the window survives with
    /// ~1 s of clicks, whisper is handed a second of unintelligible noise, and
    /// it emits a training-set sign-off ("Grazie", "Thank you"). Measured
    /// 2026-07-31 on a real meeting: mic track median level 0.00028, 50x below
    /// ENERGY_FLOOR, yet every 15 s chunk produced a phantom "Grazie".
    require_frame_energy: bool,
}

impl AudioPreprocessor {
    /// Full pipeline: highpass + VAD + AGC. Logs a one-line banner.
    pub fn new(sample_rate: u32) -> Self {
        let proc = Self::build(sample_rate, true);
        crate::log(&format!(
            "AudioPreprocessor: sr={}, highpass={}, vad={}, agc=dagc(target={}, distortion={})",
            sample_rate,
            proc.highpass.is_some(),
            proc.vad_enabled,
            TARGET_RMS,
            AGC_DISTORTION,
        ));
        proc
    }

    /// Highpass + VAD, no AGC. For the realtime chunk workers, which build one
    /// per chunk (every 3 s in dictation, every 15 s per track in a meeting) —
    /// hence no banner, it would drown the log.
    pub fn new_vad_trim(sample_rate: u32) -> Self {
        let mut proc = Self::build(sample_rate, false);
        proc.require_frame_energy = true;
        proc
    }

    fn build(sample_rate: u32, apply_agc: bool) -> Self {
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

        Self {
            highpass,
            denoise,
            agc,
            frame_buf: Vec::with_capacity(DENOISE_FRAME_SIZE),
            original_buf: Vec::with_capacity(DENOISE_FRAME_SIZE),
            silence_frames: 0,
            speech_frames: 0,
            in_speech: false,
            has_spoken: false,
            vad_enabled,
            sample_rate,
            apply_agc,
            require_frame_energy: false,
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

        // Step 3: Adaptive gain control (replaces static RMS normalization).
        // Skipped on the VAD-trim path — see the `apply_agc` field.
        let mut output = speech;
        if self.apply_agc {
            self.agc.process(&mut output);
            // The gain stage is the only thing that can push samples past the
            // rail, so the ceiling belongs here and not on the VAD-trim path,
            // which never applies gain and must hand back untouched levels.
            limit_peak(&mut output);
        }

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

    /// Compute RMS energy of a frame of original (non-scaled) samples.
    fn frame_rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        (sum_sq / samples.len() as f32).sqrt()
    }

    /// VAD using nnnoiseless voice probability with hysteresis state machine
    /// and energy-based fallback.
    ///
    /// State machine:
    /// - IDLE: waiting for speech onset (voice_prob > ONSET_THRESHOLD for MIN_SPEECH_FRAMES)
    /// - SPEECH: confirmed speech, keep emitting until offset
    /// - GRACE: speech ended, keep emitting for SILENCE_GRACE_FRAMES then transition to IDLE
    ///
    /// Energy fallback: if a frame has RMS > ENERGY_FLOOR and speech was previously
    /// confirmed (has_spoken), treat it as speech regardless of voice_prob. This prevents
    /// nnnoiseless RNN drift from killing loud, clear speech on long recordings.
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
                let rms = Self::frame_rms(&self.original_buf);

                // Energy-based override: loud frame + prior speech → treat as speech.
                // This catches cases where nnnoiseless RNN drifts on long recordings
                // and stops detecting legitimate speech.
                let energy_override = self.has_spoken && rms > ENERGY_FLOOR;

                // Effective onset: after first speech, use offset threshold for easier
                // re-entry (speech momentum — person is still talking).
                let effective_onset = if self.has_spoken {
                    VAD_OFFSET_THRESHOLD
                } else {
                    VAD_ONSET_THRESHOLD
                };

                // On the chunk path, spectral likeness alone is not enough:
                // the frame must also carry speech-level energy. Without this
                // a click at -60 dBFS opens a speech window on an idle mic.
                let voice_like = voice_prob > effective_onset || energy_override;
                let is_speech = if self.require_frame_energy {
                    voice_like && rms > ENERGY_FLOOR
                } else {
                    voice_like
                };

                if is_speech {
                    // Speech detected (by probability or energy)
                    self.speech_frames += 1;
                    self.silence_frames = 0;

                    if self.in_speech {
                        // Already in speech mode — just emit
                        speech_audio.extend_from_slice(&self.original_buf);
                    } else if self.speech_frames >= MIN_SPEECH_FRAMES {
                        // Onset confirmed — flush pending frames and enter speech mode
                        self.in_speech = true;
                        self.has_spoken = true;
                        speech_audio.append(&mut pending);
                        speech_audio.extend_from_slice(&self.original_buf);
                    } else {
                        // Accumulating onset frames — buffer them
                        pending.extend_from_slice(&self.original_buf);
                    }
                } else if voice_prob > VAD_OFFSET_THRESHOLD && self.in_speech {
                    // Hysteresis: still above offset threshold while in speech → continue.
                    // Only emit if frame has energy — silence frames must NEVER reach AGC
                    // because dagc produces NaN on zero-amplitude input, permanently
                    // corrupting all subsequent output.
                    if rms > ENERGY_FLOOR {
                        speech_audio.extend_from_slice(&self.original_buf);
                    }
                    // Don't reset speech_frames (we're in speech), but don't increment either
                } else {
                    // Below threshold
                    self.speech_frames = 0;
                    pending.clear(); // Discard unconfirmed onset frames

                    if self.in_speech {
                        // Grace period — keep in_speech=true to prevent premature exit
                        // during natural pauses. Do NOT emit silence frames: dagc
                        // (AGC) produces NaN on zero-amplitude input, permanently
                        // corrupting all subsequent output. The grace period only
                        // delays the in_speech→false transition; it does not need
                        // to output audio.
                        self.silence_frames += 1;
                        if self.silence_frames > SILENCE_GRACE_FRAMES {
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

    /// Flush any residual samples remaining in the VAD frame buffer.
    /// When recording stops mid-speech, up to 479 samples (~10ms at 48kHz) may
    /// be sitting in `frame_buf` waiting to complete a 480-sample frame. If we're
    /// currently in speech mode, these samples represent the tail end of the user's
    /// utterance and must not be silently discarded.
    ///
    /// Returns the residual samples (with AGC applied) if in speech, empty otherwise.
    pub fn flush(&mut self) -> Vec<f32> {
        if !self.in_speech || self.original_buf.is_empty() {
            self.frame_buf.clear();
            self.original_buf.clear();
            return Vec::new();
        }

        // Emit the residual original samples (already highpass-filtered)
        let mut tail = std::mem::take(&mut self.original_buf);
        self.frame_buf.clear();

        // Apply AGC to maintain consistent volume with the rest of the output
        if self.apply_agc {
            self.agc.process(&mut tail);
            limit_peak(&mut tail);
        }

        // Clamp after AGC (same safety net as process())
        for s in tail.iter_mut() {
            *s = if s.is_finite() {
                s.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }

        tail
    }

    /// Reset state between recordings
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.frame_buf.clear();
        self.original_buf.clear();
        self.silence_frames = 0;
        self.speech_frames = 0;
        self.in_speech = false;
        self.has_spoken = false;
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

/// Scale `samples` down uniformly if any peak exceeds [`PEAK_CEILING`].
///
/// Uniform scaling preserves waveform shape, so unlike clipping it introduces
/// no harmonics — the model sees the same sound, quieter. A no-op when the
/// buffer already fits inside the ceiling, which is the common case once the
/// AGC target leaves headroom. Non-finite samples are ignored by the peak
/// search (`f32::max` propagates the operand, not the NaN) and are zeroed by
/// the caller's clamp immediately afterwards.
fn limit_peak(samples: &mut [f32]) {
    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if !peak.is_finite() || peak <= PEAK_CEILING {
        return;
    }
    let gain = PEAK_CEILING / peak;
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

/// File-load preprocess: highpass only, NO VAD, NO AGC.
///
/// The full `process_buffer` pipeline is tuned for live mic capture where
/// the level is unpredictable, so AGC normalises to target=0.2. On a long
/// recorded file (e.g. a 90-min meeting WAV) this AGC pass is actively
/// destructive: any natural pause in the recording has near-zero samples,
/// dagc produces NaN on those, and although the post-AGC clamp turns NaN
/// into 0, the AGC's *internal state* is now corrupted and outputs NaN
/// for every subsequent sample. End result: 97% of a 95-min file becomes
/// silent zeros, Parakeet correctly emits empty for every chunk after the
/// first NaN burst, and the user sees only the first ~2 minutes of
/// transcript. CLAUDE.md AUDIO-001 / known-bugs.md.
///
/// File-load audio doesn't need AGC — it's already at a recorded level.
/// Highpass at 80 Hz removes mic rumble without touching the level
/// envelope. VAD is also disabled because the chunker downstream
/// (`split_at_silence`) already handles silence boundaries.
pub fn process_buffer_for_file_load(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    assert!(
        sample_rate > 0,
        "process_buffer_for_file_load: sample_rate must be > 0, got {}",
        sample_rate
    );
    if samples.is_empty() {
        return Vec::new();
    }

    // Sanitise: any non-finite input becomes 0 (defensive — file loaders
    // shouldn't emit NaN/Inf but cheap to enforce). Clamp finite values
    // to [-1, 1] before filtering so the highpass biquad never sees
    // extreme magnitudes that could push it into a numerically unstable
    // regime over a long stream.
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

    // 80 Hz Butterworth highpass — same coefficients as the live path.
    if sample_rate < 1000 {
        return sanitized;
    }
    let coeffs = match Coefficients::<f32>::from_params(
        FilterType::HighPass,
        (sample_rate as f32).hz(),
        80.0.hz(),
        Q_BUTTERWORTH_F32,
    ) {
        Ok(c) => c,
        Err(_) => return sanitized,
    };
    let mut hp = DirectForm2Transposed::<f32>::new(coeffs);

    let mut out: Vec<f32> = sanitized
        .into_iter()
        .map(|s| {
            let v = hp.run(s);
            if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        })
        .collect();

    // Post-condition: same length, all finite, in range.
    assert_eq!(
        out.len(),
        samples.len(),
        "process_buffer_for_file_load preserves sample count"
    );
    debug_assert!(out.iter().all(|s| s.is_finite()));
    debug_assert!(out.iter().all(|&s| (-1.0..=1.0).contains(&s)));
    // Touch via mut to avoid "useless mut" lint when assertions are off.
    out.shrink_to_fit();
    out
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
    let mut output = proc.process(samples);
    // Flush residual samples from the VAD frame buffer — up to 479 samples (~10ms)
    // that were buffered waiting for a complete 480-sample frame. Without this,
    // the very tail of speech is silently discarded.
    output.extend(proc.flush());
    output
}

/// Which preprocessing path a captured dictation buffer takes before STT.
///
/// Single source of truth for the route-aware decision made at
/// `dimmy_stop_recording`. Extracted as a pure function so the mapping is
/// unit-testable and can't silently drift (it did, twice — see BUG B in
/// known-bugs.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreprocessRoute {
    /// User disabled preprocessing — pass samples through untouched.
    Raw,
    /// Local STT (whisper / parakeet): full VAD + AGC. They hallucinate on
    /// long silence and want normalized levels, so the pipeline HELPS.
    Full,
    /// Cloud STT (Groq / OpenAI / Deepgram): 80 Hz highpass only. They run
    /// their own VAD + normalization server-side, so ours is redundant and
    /// can only degrade (proven 2026-07-01: quiet mic → VAD trims speech →
    /// dagc amplifies noise to clipping → a 45 s dictation became "Ah!").
    HighpassOnly,
}

/// Pure route decision for a dictation buffer. `stt_mode` is the config
/// string; anything that isn't exactly `"local"` is treated as cloud — the
/// conservative default, because the highpass-only path can never make audio
/// worse than raw.
///
/// This reproduces the exact inline mapping that shipped in
/// `dimmy_stop_recording` on 2026-07-01; extracting it changes no behaviour.
pub fn preprocess_route(preprocessing_enabled: bool, stt_mode: &str) -> PreprocessRoute {
    if !preprocessing_enabled {
        PreprocessRoute::Raw
    } else if stt_mode == "local" {
        PreprocessRoute::Full
    } else {
        PreprocessRoute::HighpassOnly
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    // Treat any non-finite sample as 0 so a stray NaN/Inf in the input can't
    // poison the RMS (which would make the make-it-worse comparison behave
    // unpredictably). f64 accumulation keeps precision over long buffers.
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let s = if s.is_finite() { s as f64 } else { 0.0 };
            s * s
        })
        .sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

/// Retained-sample fraction below which the full pipeline is considered to
/// have collapsed the recording. Deliberately EXTREME (5 %): a normal VAD
/// trim keeps ~40-60 %, so this only trips on near-total loss.
const COLLAPSE_RETENTION_FLOOR: f32 = 0.05;
/// Output-vs-input RMS ratio below which the output is considered
/// effectively silent relative to a speech-level input.
const COLLAPSE_RMS_FLOOR: f32 = 0.05;

/// Did the full preprocessing pipeline make a clearly-speech input
/// *catastrophically* worse? The user's rule: preprocessing must HELP, never
/// make audio worse than raw. This is the detector for the LOCAL make-it-worse
/// fallback.
///
/// Returns true ONLY on near-total collapse of a speech-level input:
/// - the input is clearly speech (`rms(input) > ENERGY_FLOOR`), AND
/// - the output is empty, OR retained < 5 % of the samples, OR its RMS is
///   below 5 % of the input's (essentially silent).
///
/// It is intentionally conservative so it can NEVER fire on a healthy 40-60 %
/// VAD trim — i.e. it does not alter the behaviour validated in real
/// dictations; it is a floor, not a quality knob. If the input has no clear
/// speech energy there is nothing to protect, so it returns false.
pub fn preprocess_made_it_worse(input: &[f32], output: &[f32]) -> bool {
    if input.is_empty() {
        return false;
    }
    let input_rms = rms(input);
    if input_rms <= ENERGY_FLOOR {
        return false;
    }
    // f64 division so the fraction stays exact even for multi-minute buffers
    // (>2^24 samples exceeds f32 integer precision).
    let retained = output.len() as f64 / input.len() as f64;
    output.is_empty()
        || retained < COLLAPSE_RETENTION_FLOOR as f64
        || rms(output) < input_rms * COLLAPSE_RMS_FLOOR
}

/// Full pipeline with a make-it-worse safety net for the LOCAL path.
///
/// Runs the full VAD + AGC pipeline (`process_buffer`); if that
/// catastrophically collapses a speech-level input (see
/// `preprocess_made_it_worse`), falls back to the highpass-only path — the
/// same safe path cloud + file-load use. This guarantees the pipeline never
/// ships audio worse than doing nothing, even for an input we never
/// anticipated. On healthy audio the full output is returned unchanged.
pub fn process_buffer_guarded(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    assert!(
        sample_rate > 0,
        "process_buffer_guarded: sample_rate must be > 0, got {}",
        sample_rate
    );
    if samples.is_empty() {
        return Vec::new();
    }

    let full = process_buffer(samples, sample_rate);
    if preprocess_made_it_worse(samples, &full) {
        crate::log(
            "[Preprocess] WARN full pipeline collapsed a speech-level input — \
             falling back to highpass-only (make-it-worse guard)",
        );
        let fallback = process_buffer_for_file_load(samples, sample_rate);
        // Postcondition (house rule: assert in prod): the fallback preserves
        // the input (highpass keeps sample count + level), so it can never
        // itself be degenerate. Only runs on the rare fallback path.
        assert!(
            !preprocess_made_it_worse(samples, &fallback),
            "highpass fallback is itself degenerate — the make-it-worse guard has no safe path"
        );
        return fallback;
    }
    full
}

/// Fraction of 10 ms frames that must clear `ENERGY_FLOOR` before a chunk
/// whose VAD output collapsed is handed back untrimmed.
///
/// Measured 2026-07-31 on a real meeting: the offending mic track had ~4 % of
/// frames above the floor (keyboard clicks, breath), while genuine speech
/// retains 40-60 %. The two populations are far apart, so 10 % sits in the gap
/// with room on both sides.
const CHUNK_SUSTAINED_ENERGY_FRACTION: f32 = 0.10;

/// Below this much retained speech a chunk is not worth a model call: no word
/// fits in it, and whisper pads any input to a full 30 s encoder window, so a
/// sliver costs a complete pass to return nothing.
const CHUNK_MIN_SPEECH_MS: usize = 200;

/// Fraction of 10 ms frames whose RMS clears `ENERGY_FLOOR`. Distinguishes
/// sustained speech energy from isolated transients, which a whole-window RMS
/// cannot: a single loud click lifts the window average without producing a
/// single audible frame of speech.
fn sustained_energy_fraction(samples: &[f32], sample_rate: u32) -> f32 {
    assert!(
        sample_rate > 0,
        "sustained_energy_fraction: sample_rate must be > 0"
    );
    let frame = (sample_rate as usize / 100).max(1);
    if samples.len() < frame {
        return 0.0;
    }
    let frames = samples.len() / frame;
    let loud = (0..frames)
        .filter(|i| rms(&samples[i * frame..(i + 1) * frame]) > ENERGY_FLOOR)
        .count();
    loud as f32 / frames as f32
}

/// Chunk gate for the realtime workers: decide whether a window is worth a
/// model call, and hand it over WHOLE. Never trims, never applies AGC.
///
/// The chunked-dictation worker (`chunked_stt.rs`) and the meeting chunk
/// worker (`meeting.rs`) feed whisper short windows *while* capture is still
/// running. Handing it raw idle audio makes it emit the sign-off phrases from
/// its training data ("thank you", "grazie per la visione"), which is what the
/// 2026-07-31 work set out to stop.
///
/// That work removed the silence *inside* every window. It is the right cure
/// for an idle mic and the wrong one for speech: on real dictation the VAD
/// kept a fraction of each 3 s window, whisper received sub-second fragments
/// with no context, and hallucinated just as badly in the other direction —
/// Italian speech came back as invented Spanish, and a 9 s dictation as
/// "Yeah. Yeah. Yeah." (measured 2026-08-05). One hallucination class was
/// traded for another.
///
/// So the VAD is used as a GATE, not a scalpel. It answers one question —
/// *is there speech in this window?* — and the window then goes to the model
/// untouched, pauses and all, which is exactly what the batch path does and
/// what whisper needs for punctuation and segmentation.
///
/// The anti-hallucination guarantee is not weakened by this, it is
/// strengthened: a window without speech never reaches the model at all,
/// where before it could still arrive as a second of trimmed clicks.
pub fn process_chunk_vad_only(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    assert!(
        sample_rate > 0,
        "process_chunk_vad_only: sample_rate must be > 0, got {}",
        sample_rate
    );
    if samples.is_empty() {
        return Vec::new();
    }
    // No VAD below 48 kHz (nnnoiseless requires it) — nothing to gate on, so
    // pass the window through rather than dropping audio we cannot judge.
    if sample_rate != REQUIRED_SAMPLE_RATE {
        return samples.to_vec();
    }

    // How much of this window does the VAD consider speech? `new_vad_trim`
    // requires each frame to clear ENERGY_FLOOR as well as look like speech,
    // so quiet clicks and breath — which score high on spectral shape alone —
    // do not count. The OUTPUT LENGTH is the signal; the output itself is
    // discarded.
    let mut proc = AudioPreprocessor::new_vad_trim(sample_rate);
    let mut speech = proc.process(samples);
    speech.extend(proc.flush());
    let speech_ms = (speech.len() * 1000) / sample_rate as usize;

    if speech_ms >= CHUNK_MIN_SPEECH_MS {
        return samples.to_vec();
    }

    // The VAD found little or nothing. Two very different causes: a quiet
    // room (drop it), or the VAD failing on speech that is plainly there
    // (never lose the user's words). Sustained frame energy tells them apart —
    // a whole-window RMS cannot, because one loud click lifts the average
    // without producing a single audible frame of speech.
    let sustained = sustained_energy_fraction(samples, sample_rate);
    if sustained >= CHUNK_SUSTAINED_ENERGY_FRACTION {
        crate::log(&format!(
            "[Preprocess] VAD found only {speech_ms} ms of speech but {:.0}% sustained energy — passing the window through",
            sustained * 100.0
        ));
        return samples.to_vec();
    }
    Vec::new()
}

/// Downsample audio to 16kHz for Whisper (which internally resamples to 16kHz anyway).
/// Uses a lowpass anti-aliasing filter + linear interpolation.
/// Returns samples at 16kHz. If source is already 16kHz, returns a clone.
pub fn downsample_to_16k(samples: &[f32], source_rate: u32) -> Vec<f32> {
    downsample_to(samples, source_rate, WHISPER_SAMPLE_RATE)
}

/// Downsample to an arbitrary `target_rate`.
///
/// Generalisation of [`downsample_to_16k`], which is now a thin wrapper: every
/// STT route in this codebase wanted 16 kHz until OpenAI's Realtime API turned
/// up demanding exactly 24 kHz (see `openai_stream::REQUIRED_INPUT_RATE`).
/// Rather than a second copy of the same DSP, the rate is a parameter.
///
/// Returns a clone when the source is already at or below the target — this
/// only ever downsamples, it will not invent bandwidth that is not there.
pub fn downsample_to(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    // Invariant: rates must be positive (0 would cause division-by-zero)
    assert!(
        source_rate > 0,
        "downsample_to: source_rate must be > 0, got {}",
        source_rate
    );
    assert!(
        target_rate > 0,
        "downsample_to: target_rate must be > 0, got {}",
        target_rate
    );

    if source_rate <= target_rate {
        return samples.to_vec();
    }

    // Step 1: anti-aliasing lowpass a little under the target's Nyquist
    // (7/8 of it, i.e. 7 kHz for a 16 kHz target, 10.5 kHz for 24 kHz) so the
    // filter's transition band lands below the fold-over point.
    let cutoff = target_rate as f32 * 0.4375;
    let filtered = if source_rate >= 1000 {
        if let Ok(coeffs) = Coefficients::<f32>::from_params(
            FilterType::LowPass,
            (source_rate as f32).hz(),
            cutoff.hz(),
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

    // Step 2: linear interpolation to the target rate
    let ratio = source_rate as f64 / target_rate as f64;
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

    // Invariant: output length ≈ input_length * target_rate / source_rate (within 1 sample)
    let expected_len =
        (samples.len() as f64 * target_rate as f64 / source_rate as f64).floor() as usize;
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

    /// Speech-shaped signal with a REALISTIC crest factor.
    ///
    /// `generate_speech_like` sits around 8 dB peak-to-RMS, which is far below
    /// real speech and hides every headroom bug. Measured on 38 real dictation
    /// sessions from this project's own `audio_debug` captures (2026-07-28 →
    /// 2026-08-04): median crest factor **19.6 dB**, median RMS 0.019
    /// (-34.4 dBFS). This builds a signal in that band — loud vowels, quiet
    /// consonants, short plosive transients, inter-word gaps — and rescales it
    /// to the requested RMS.
    fn speech_with_real_crest(sample_rate: u32, duration_secs: f32, target_rms: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * duration_secs) as usize;
        let sr = sample_rate as f32;
        // Per-syllable gains: vowels are ~20x louder than the quiet consonants
        // between them. This spread is what produces the real crest factor.
        let syllable_gain = [1.0f32, 0.22, 0.6, 0.05, 0.85, 0.12, 0.45, 0.08];
        let mut rng_state: u32 = 1234;
        let mut out: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                // 5 syllables/second, with a 25 % silent gap between words.
                let syl = (t * 5.0) as usize;
                let phase = (t * 5.0).fract();
                let gain = if phase > 0.75 {
                    0.01 // inter-word gap, not digital silence
                } else {
                    syllable_gain[syl % syllable_gain.len()]
                };
                rng_state ^= rng_state << 13;
                rng_state ^= rng_state >> 17;
                rng_state ^= rng_state << 5;
                let noise = (rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let f0 = 2.0 * std::f32::consts::PI * 150.0 * t;
                let voiced = f0.sin() * 0.6 + (2.0 * f0).sin() * 0.25 + (3.0 * f0).sin() * 0.15;
                // Plosive: a 4 ms full-scale transient at each syllable onset.
                let plosive = if phase < 0.02 { noise * 1.0 } else { 0.0 };
                (voiced * 0.8 + noise * 0.2) * gain + plosive * gain
            })
            .collect();
        let cur = rms(&out);
        assert!(cur > 0.0, "fixture generated silence");
        let scale = target_rms / cur;
        for s in out.iter_mut() {
            *s *= scale;
        }
        out
    }

    /// The clipping bug, reproduced from field data (2026-08-04).
    ///
    /// `TARGET_RMS` is 0.2 (-14 dBFS). Real speech carries ~19.6 dB of crest
    /// factor, so normalising its RMS to -14 dBFS puts the peaks at +5.6 dBFS —
    /// i.e. 5.6 dB ABOVE full scale. The post-AGC clamp then hard-clips them
    /// into a square wave, which is what whisper actually receives.
    ///
    /// This is not an edge case: all 38 preprocessing-ON sessions in
    /// `audio_debug` peaked at exactly 1.000 with a median 3.14 % of samples
    /// pinned to full scale. Clipping is GUARANTEED by the constant, not
    /// triggered by unlucky input.
    ///
    /// 44.1 kHz keeps the VAD out of the picture (nnnoiseless needs exactly
    /// 48 kHz) so this pins the gain stage alone — which is correct here: the
    /// field data clears the VAD, whose retention was a healthy 67 % median.
    #[test]
    fn agc_must_not_clip_real_speech_levels() {
        let sr = 44_100u32;
        let input = speech_with_real_crest(sr, 4.0, 0.019);

        let in_rms = rms(&input);
        let in_peak = input.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        let crest_db = 20.0 * (in_peak / in_rms).log10();
        assert!(
            (15.0..=24.0).contains(&crest_db),
            "fixture must sit in the measured 15-24 dB crest band, got {:.1} dB",
            crest_db
        );
        assert!(in_peak < 1.0, "fixture must not clip before preprocessing");

        let out = process_buffer(&input, sr);
        assert!(!out.is_empty(), "pipeline dropped everything");

        let clipped = out.iter().filter(|s| s.abs() >= 0.999).count();
        let clipped_pct = 100.0 * clipped as f32 / out.len() as f32;
        assert!(
            clipped_pct < 0.1,
            "preprocessing hard-clipped {:.2} % of a {:.1} dB-crest speech input \
             (in: rms={:.5} peak={:.3} -> out: rms={:.5} peak={:.3}). \
             Hard clipping is broadband distortion; it destroys the mel features \
             whisper transcribes from.",
            clipped_pct,
            crest_db,
            in_rms,
            in_peak,
            rms(&out),
            out.iter().fold(0.0f32, |m, &s| m.max(s.abs())),
        );
    }

    #[test]
    fn limit_peak_is_transparent_and_bounded() {
        // Uniform scaling is the whole point: it must change the LEVEL and
        // leave the SHAPE alone, otherwise it would be a distortion stage
        // rather than a safety rail.
        let mut loud: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin() * 2.5).collect();
        let before = loud.clone();
        limit_peak(&mut loud);

        let peak = loud.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            (peak - PEAK_CEILING).abs() < 1e-4,
            "peak must land exactly on the ceiling, got {peak}"
        );
        // Shape check: every sample kept the same ratio to its neighbour.
        let ratio = loud[10] / before[10];
        for (a, b) in loud.iter().zip(before.iter()) {
            if b.abs() > 1e-6 {
                assert!(
                    ((a / b) - ratio).abs() < 1e-4,
                    "scaling must be uniform — waveform shape changed"
                );
            }
        }
    }

    #[test]
    fn limit_peak_leaves_quiet_audio_untouched() {
        let mut quiet: Vec<f32> = (0..500).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
        let before = quiet.clone();
        limit_peak(&mut quiet);
        assert_eq!(
            quiet, before,
            "audio inside the ceiling must not be touched"
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
        let silence = vec![0.0f32; 480 * 400]; // 400 frames of silence (4s > 3s grace)
        let out = proc.process(&silence);
        // After grace period (300 frames = 3s), silence should be stripped
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
        proc.has_spoken = true;
        proc.reset();
        assert!(proc.frame_buf.is_empty());
        assert_eq!(proc.silence_frames, 0);
        assert_eq!(proc.speech_frames, 0);
        assert!(!proc.in_speech);
        assert!(!proc.has_spoken);
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

    /// Generate a speech-like signal: broadband noise shaped by amplitude envelope.
    /// More realistic than a pure tone for VAD testing.
    fn generate_speech_like(sample_rate: u32, duration_secs: f32, amplitude: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * duration_secs) as usize;
        let mut rng_state: u32 = 42;
        (0..n)
            .map(|i| {
                // Simple pseudo-random noise (xorshift32)
                rng_state ^= rng_state << 13;
                rng_state ^= rng_state >> 17;
                rng_state ^= rng_state << 5;
                let noise = (rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                // Modulate with low-frequency envelope (simulates speech rhythm)
                let envelope =
                    (2.0 * std::f32::consts::PI * 3.0 * i as f32 / sample_rate as f32).sin() * 0.3
                        + 0.7;
                // Mix with tonal component (vocal fundamental ~150Hz)
                let tone =
                    (2.0 * std::f32::consts::PI * 150.0 * i as f32 / sample_rate as f32).sin();
                (noise * 0.3 + tone * 0.7) * envelope * amplitude
            })
            .collect()
    }

    #[test]
    fn speech_pause_speech_preserves_all_segments() {
        // Simulate: 3s speech → 1s silence → 3s speech → 0.5s silence → 3s speech
        // All speech segments should be preserved (grace period = 3s covers 1s pause).
        let sr = 48000u32;
        let speech1 = generate_speech_like(sr, 3.0, 0.3);
        let silence1 = vec![0.0f32; sr as usize]; // 1s silence
        let speech2 = generate_speech_like(sr, 3.0, 0.3);
        let silence2 = vec![0.0f32; sr as usize / 2]; // 0.5s silence
        let speech3 = generate_speech_like(sr, 3.0, 0.3);

        let total_speech_samples = speech1.len() + speech2.len() + speech3.len();

        let mut full_audio = Vec::new();
        full_audio.extend_from_slice(&speech1);
        full_audio.extend_from_slice(&silence1);
        full_audio.extend_from_slice(&speech2);
        full_audio.extend_from_slice(&silence2);
        full_audio.extend_from_slice(&speech3);

        let out = process_buffer(&full_audio, sr);

        // Output should contain at least 50% of total speech samples.
        // (VAD may trim edges, but should NOT drop entire segments.)
        assert!(
            out.len() > total_speech_samples / 2,
            "VAD dropped too much speech: output {} samples vs {} total speech samples \
             ({:.0}% preserved). Speech-pause-speech pattern should preserve most audio.",
            out.len(),
            total_speech_samples,
            out.len() as f64 / total_speech_samples as f64 * 100.0
        );
    }

    #[test]
    fn dagc_produces_nan_after_silence() {
        // Documents the dagc library bug: feeding zero samples corrupts state permanently.
        // This is the root cause of the "first recording after app launch" bug where
        // speech after a 5s+ pause gets killed.
        let mut agc = MonoAgc::new(TARGET_RMS, AGC_DISTORTION).unwrap();

        // Feed 1s of speech-level audio
        let mut speech: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 48000.0).sin() * 0.3)
            .collect();
        agc.process(&mut speech);

        // Feed 3s of silence (grace period duration)
        let mut silence = vec![0.0f32; 48000 * 3];
        agc.process(&mut silence);

        // Feed 1s of speech again — dagc will produce ALL NaN
        let mut speech2: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 48000.0).sin() * 0.3)
            .collect();
        agc.process(&mut speech2);

        let nan_count = speech2.iter().filter(|s| !s.is_finite()).count();
        assert!(
            nan_count == speech2.len(),
            "Expected ALL NaN from dagc after silence (got {}/{}). \
             If dagc fixed this upstream, we can simplify our AGC reset logic.",
            nan_count,
            speech2.len()
        );
    }

    #[test]
    fn agc_reset_recovers_after_silence() {
        // After resetting AGC (as we do when grace expires), speech is preserved.
        let mut agc = MonoAgc::new(TARGET_RMS, AGC_DISTORTION).unwrap();

        // Feed 1s speech → 3s silence (simulates grace period)
        let mut speech: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 48000.0).sin() * 0.3)
            .collect();
        agc.process(&mut speech);
        let mut silence = vec![0.0f32; 48000 * 3];
        agc.process(&mut silence);

        // Reset AGC (this is what our fix does)
        agc = MonoAgc::new(TARGET_RMS, AGC_DISTORTION).unwrap();

        // Feed 1s of speech — should work fine now
        let mut speech2: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 48000.0).sin() * 0.3)
            .collect();
        agc.process(&mut speech2);

        let nan_count = speech2.iter().filter(|s| !s.is_finite()).count();
        let rms = (speech2.iter().map(|s| s * s).sum::<f32>() / speech2.len() as f32).sqrt();
        assert_eq!(nan_count, 0, "AGC should produce no NaN after reset");
        assert!(
            rms > 0.01,
            "AGC should produce audible output after reset: RMS={:.6}",
            rms
        );
    }

    #[test]
    fn speech_long_pause_speech_preserves_second_segment() {
        // Simulate: 10s speech → 5s silence (exceeds 3s grace!) → 10s speech
        // The VAD must re-enter speech mode after the grace period expires.
        // This is the exact pattern that caused the user's bug: first recording
        // after app launch, speech → 5s pause → speech killed.
        let sr = 48000u32;
        let speech1 = generate_speech_like(sr, 10.0, 0.3);
        let silence = vec![0.0f32; sr as usize * 5]; // 5s silence (> 3s grace)
        let speech2 = generate_speech_like(sr, 10.0, 0.3);

        let mut full_audio = Vec::new();
        full_audio.extend_from_slice(&speech1);
        full_audio.extend_from_slice(&silence);
        full_audio.extend_from_slice(&speech2);

        let out = process_buffer(&full_audio, sr);

        // Both speech segments should be preserved.
        // speech1 (10s) + speech2 (10s) = 20s = 960000 samples.
        // Output should have at least 50% of total speech (onset trimming is OK, dropping isn't).
        let total_speech = speech1.len() + speech2.len();
        assert!(
            out.len() > total_speech / 2,
            "VAD killed speech after 5s pause: output {:.1}s vs {:.1}s total speech ({:.0}% preserved). \
             Speech after a long pause must NOT be dropped.",
            out.len() as f64 / sr as f64,
            total_speech as f64 / sr as f64,
            out.len() as f64 / total_speech as f64 * 100.0
        );
    }

    #[test]
    fn no_nan_in_output_after_silence_gap() {
        // REGRESSION: dagc produces NaN on zero-amplitude input. If silence
        // frames reach AGC (e.g., from VAD grace period), the NaN propagates
        // to all subsequent samples, which get clamped to 0.0 — killing speech.
        //
        // This test feeds process_buffer a single buffer (like stop_recording does)
        // with speech → 7s silence → speech, and verifies no NaN/zero sections
        // in the output.
        let sr = 48000u32;
        let speech1 = generate_speech_like(sr, 15.0, 0.3);
        let silence = vec![0.0f32; sr as usize * 7]; // 7s silence (>> 3s grace)
        let speech2 = generate_speech_like(sr, 15.0, 0.3);

        let mut full = Vec::new();
        full.extend_from_slice(&speech1);
        full.extend_from_slice(&silence);
        full.extend_from_slice(&speech2);

        let out = process_buffer(&full, sr);

        // All output samples must be finite (no NaN from dagc)
        let nan_count = out.iter().filter(|s| !s.is_finite()).count();
        assert_eq!(
            nan_count, 0,
            "process_buffer produced {} NaN samples — dagc silence corruption leaked through",
            nan_count
        );

        // Check for long runs of zero (NaN clamped to 0.0).
        // A run of > 1s of consecutive zeros means speech was killed.
        let max_zero_run = {
            let mut max_run = 0usize;
            let mut current_run = 0usize;
            for &s in &out {
                if s.abs() < 1e-10 {
                    current_run += 1;
                    max_run = max_run.max(current_run);
                } else {
                    current_run = 0;
                }
            }
            max_run
        };
        let one_second = sr as usize;
        assert!(
            max_zero_run < one_second,
            "Output has {:.1}s of consecutive zeros (NaN→0.0 corruption). \
             Max allowed: 1.0s. Silence must not reach AGC.",
            max_zero_run as f64 / sr as f64
        );
    }

    #[test]
    fn output_no_nan_with_multiple_silence_gaps() {
        // Multiple silence gaps: speech → 5s silence → speech → 8s silence → speech
        // Each gap exceeds grace. All speech segments must survive.
        let sr = 48000u32;
        let mut full = Vec::new();
        full.extend_from_slice(&generate_speech_like(sr, 10.0, 0.3));
        full.extend_from_slice(&vec![0.0f32; sr as usize * 5]);
        full.extend_from_slice(&generate_speech_like(sr, 10.0, 0.3));
        full.extend_from_slice(&vec![0.0f32; sr as usize * 8]);
        full.extend_from_slice(&generate_speech_like(sr, 10.0, 0.3));

        let out = process_buffer(&full, sr);

        // No NaN
        assert!(
            out.iter().all(|s| s.is_finite()),
            "NaN in output with multiple silence gaps"
        );

        // Total speech is 30s. Output should preserve at least 40%.
        let min = sr as usize * 30 * 40 / 100;
        assert!(
            out.len() > min,
            "Multiple gaps killed speech: {:.1}s output vs 30s speech ({:.0}%)",
            out.len() as f64 / sr as f64,
            out.len() as f64 / (sr as f64 * 30.0) * 100.0
        );
    }

    #[test]
    fn long_recording_preserves_speech() {
        // Simulate a 30-second continuous speech recording.
        // The VAD must NOT kill speech after the first few seconds.
        let sr = 48000u32;
        let speech = generate_speech_like(sr, 30.0, 0.3);
        let input_len = speech.len();

        let out = process_buffer(&speech, sr);

        // At minimum, 40% of input should survive (VAD strips some noise-like frames,
        // but should NOT truncate to just the first few seconds).
        let min_expected = input_len * 40 / 100;
        assert!(
            out.len() > min_expected,
            "VAD killed most of a 30s recording: output {} samples ({:.1}s) vs input {} ({:.1}s). \
             Minimum expected: {} samples ({:.1}s)",
            out.len(),
            out.len() as f64 / sr as f64,
            input_len,
            input_len as f64 / sr as f64,
            min_expected,
            min_expected as f64 / sr as f64,
        );
    }

    #[test]
    fn energy_floor_prevents_dropping_loud_speech() {
        // Generate audio that is clearly audible (high RMS) — even if nnnoiseless
        // doesn't classify it as speech, the energy floor should keep it.
        let sr = 48000u32;
        // First: some "speech" to set has_spoken = true
        let speech = generate_speech_like(sr, 1.0, 0.3);
        // Then: loud broadband noise (simulates speech that nnnoiseless might not detect)
        let loud: Vec<f32> = (0..sr as usize * 5)
            .map(|i| {
                let mut rng = (i as u32).wrapping_mul(2654435761);
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                ((rng as f32 / u32::MAX as f32) * 2.0 - 1.0) * 0.15 // RMS ~0.087 >> ENERGY_FLOOR
            })
            .collect();

        let mut full = Vec::new();
        full.extend_from_slice(&speech);
        full.extend_from_slice(&loud);

        let out = process_buffer(&full, sr);

        // The loud section should be mostly preserved due to energy floor
        let min_expected = loud.len() / 2;
        assert!(
            out.len() > min_expected,
            "Energy floor failed: only {} samples output from {} input. \
             Loud audio should not be dropped.",
            out.len(),
            full.len()
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
    fn flush_emits_residual_samples_during_speech() {
        // When recording stops mid-speech, up to 479 samples may be in frame_buf.
        // flush() must emit them to avoid losing the tail of the utterance.
        let mut proc = AudioPreprocessor::new(48000);

        // Feed speech to enter speech mode (need enough frames for onset confirmation)
        let speech = generate_speech_like(48000, 1.0, 0.3);
        let out = proc.process(&speech);
        assert!(!out.is_empty(), "Should detect speech");
        assert!(proc.in_speech, "Should be in speech mode");

        // Now feed a partial frame (less than 480 samples) — simulates recording stop
        let partial: Vec<f32> = (0..200)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 48000.0).sin() * 0.3)
            .collect();
        let _ = proc.process(&partial);

        // frame_buf should have residual samples (partial frame didn't complete)
        assert!(
            !proc.original_buf.is_empty(),
            "Should have residual samples in buffer"
        );

        // flush() should emit them
        let flushed = proc.flush();
        assert!(
            !flushed.is_empty(),
            "flush() should emit residual samples during speech"
        );
        assert!(
            flushed
                .iter()
                .all(|s| s.is_finite() && *s >= -1.0 && *s <= 1.0),
            "Flushed samples must be finite and clamped"
        );
    }

    #[test]
    fn flush_emits_nothing_when_not_in_speech() {
        let mut proc = AudioPreprocessor::new(48000);
        // Feed silence — should NOT enter speech mode
        let silence = vec![0.0f32; 480 * 5];
        let _ = proc.process(&silence);
        assert!(!proc.in_speech);
        let flushed = proc.flush();
        assert!(
            flushed.is_empty(),
            "flush() should emit nothing outside speech"
        );
    }

    #[test]
    fn process_buffer_includes_residual_tail() {
        // process_buffer() should include flushed residual samples.
        // Feed speech that doesn't end on a 480-sample frame boundary.
        let sr = 48000u32;
        let speech = generate_speech_like(sr, 2.0, 0.3);
        // Add a partial frame (100 extra samples, not aligned to 480)
        let extra: Vec<f32> = (0..100)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / sr as f32).sin() * 0.3)
            .collect();
        let mut full = speech;
        full.extend_from_slice(&extra);

        let out = process_buffer(&full, sr);
        // Output should be non-empty and include the tail
        assert!(!out.is_empty(), "process_buffer should produce output");
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

    // ── process_buffer_for_file_load ─────────────────────────────────
    // Regression coverage for the 2026-05-08 file-load AGC NaN bug
    // (commit 0ed682b). The live-recording pipeline (process_buffer)
    // walks dagc over the entire input — on a long pre-recorded WAV
    // dagc emits NaN on natural silence stretches, the post-AGC
    // clamp turns those NaN into 0, and from that point all
    // subsequent samples are zero. process_buffer_for_file_load is
    // highpass-only to skip that booby trap.

    #[test]
    fn file_load_empty_input_returns_empty() {
        let out = process_buffer_for_file_load(&[], 16_000);
        assert!(out.is_empty());
    }

    #[test]
    fn file_load_preserves_sample_count() {
        // Input is 1 second of speech-shaped audio; output must have
        // EXACTLY the same length — no VAD trimming, no AGC
        // expansion, no chunk-boundary loss.
        let sr = 16_000u32;
        let input = generate_speech_like(sr, 1.0, 0.3);
        let n_in = input.len();
        let out = process_buffer_for_file_load(&input, sr);
        assert_eq!(
            out.len(),
            n_in,
            "file-load preprocess must preserve sample count"
        );
    }

    #[test]
    fn file_load_replaces_nan_inf_with_zero() {
        let mut input = vec![0.5f32; 1000];
        input[100] = f32::NAN;
        input[200] = f32::INFINITY;
        input[300] = f32::NEG_INFINITY;
        let out = process_buffer_for_file_load(&input, 16_000);
        assert_eq!(out.len(), input.len());
        // Output is finite everywhere.
        assert!(
            out.iter().all(|s| s.is_finite()),
            "file-load output must be all-finite"
        );
        // Output is in [-1, 1] everywhere.
        assert!(
            out.iter().all(|&s| (-1.0..=1.0).contains(&s)),
            "file-load output must be clamped to [-1, 1]"
        );
    }

    #[test]
    fn file_load_clamps_extreme_values() {
        let input = vec![10.0f32, -10.0, 1.5, -1.5, 0.5, -0.5];
        let out = process_buffer_for_file_load(&input, 16_000);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|&s| (-1.0..=1.0).contains(&s)));
    }

    #[test]
    fn file_load_handles_zero_sample_rate() {
        // Defensive: assert was added because divide-by-zero hit at
        // a previous call site. Without an assert / sane fallback the
        // function would create a NaN biquad and corrupt the output.
        let result = std::panic::catch_unwind(|| process_buffer_for_file_load(&[0.5f32; 100], 0));
        assert!(result.is_err(), "sr=0 must panic via assert");
    }

    #[test]
    fn file_load_long_silence_does_not_corrupt_subsequent_audio() {
        // The exact bug AGC introduced: long silence stretch in the
        // middle of the file would corrupt all samples AFTER it.
        // file-load preprocess must NOT do this — silence is just
        // silence, audio after it is unchanged.
        let sr = 16_000u32;
        let speech_a = generate_speech_like(sr, 0.5, 0.3);
        let silence = vec![0.0f32; sr as usize * 5]; // 5 s silence
        let speech_b = generate_speech_like(sr, 0.5, 0.3);

        let mut input = Vec::new();
        input.extend_from_slice(&speech_a);
        input.extend_from_slice(&silence);
        input.extend_from_slice(&speech_b);

        let out = process_buffer_for_file_load(&input, sr);
        assert_eq!(out.len(), input.len());

        // Tail (post-silence) speech must NOT be all zeros.
        let tail_start = speech_a.len() + silence.len();
        let tail = &out[tail_start..];
        let tail_peak = tail.iter().fold(0f32, |m, &s| m.max(s.abs()));
        assert!(
            tail_peak > 0.05,
            "post-silence audio must be preserved, peak={:.4}",
            tail_peak
        );

        // No NaN/Inf anywhere.
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn file_load_skips_highpass_for_low_sample_rates() {
        // Input below 1 kHz is below our biquad's Nyquist coherence;
        // function returns sanitized passthrough.
        let input = vec![0.5f32, -0.5, 0.5, -0.5];
        let out = process_buffer_for_file_load(&input, 500);
        assert_eq!(out.len(), input.len());
        // Should be byte-identical (no HP applied).
        for (a, b) in input.iter().zip(out.iter()) {
            assert_eq!(*a, *b);
        }
    }

    #[test]
    fn file_load_works_at_common_sample_rates() {
        for &sr in &[8_000u32, 16_000, 22_050, 44_100, 48_000, 96_000] {
            let input = generate_speech_like(sr, 0.2, 0.3);
            let out = process_buffer_for_file_load(&input, sr);
            assert_eq!(out.len(), input.len(), "sr={} sample count drift", sr);
            assert!(
                out.iter().all(|s| s.is_finite()),
                "sr={} produced non-finite samples",
                sr
            );
        }
    }

    // ---- Route-aware preprocessing (§1) -----------------------------------

    #[test]
    fn preprocess_route_maps_cloud_local_disabled() {
        use PreprocessRoute::*;
        // Disabled → Raw regardless of mode.
        assert_eq!(preprocess_route(false, "local"), Raw);
        assert_eq!(preprocess_route(false, "cloud"), Raw);
        // Enabled + local → Full.
        assert_eq!(preprocess_route(true, "local"), Full);
        // Enabled + cloud (or anything not "local") → HighpassOnly.
        assert_eq!(preprocess_route(true, "cloud"), HighpassOnly);
        assert_eq!(preprocess_route(true, ""), HighpassOnly);
        assert_eq!(preprocess_route(true, "groq"), HighpassOnly);
    }

    // ---- Make-it-worse fallback (§2) --------------------------------------

    #[test]
    fn made_it_worse_false_on_healthy_vad_trim() {
        // Healthy speech in, a normal ~50% VAD trim out at a healthy level:
        // this is the everyday case and must NEVER be flagged as degenerate,
        // otherwise the guard would alter validated dictations.
        let input = generate_speech_like(48_000, 4.0, 0.3);
        let output = generate_speech_like(48_000, 2.0, 0.2); // half the samples, healthy RMS
        assert!(rms(&input) > ENERGY_FLOOR);
        assert!(!preprocess_made_it_worse(&input, &output));
    }

    #[test]
    fn made_it_worse_true_on_empty_output() {
        let input = generate_speech_like(48_000, 4.0, 0.3);
        assert!(preprocess_made_it_worse(&input, &[]));
    }

    #[test]
    fn made_it_worse_true_on_near_total_sample_loss() {
        // 4.0 s in (192 000 samples), 0.1 s out (4 800 samples) = 2.5 % —
        // below the 5 % COLLAPSE_RETENTION_FLOOR, so the guard must trip.
        let input = generate_speech_like(48_000, 4.0, 0.3);
        let output = generate_speech_like(48_000, 0.1, 0.3);
        assert!(preprocess_made_it_worse(&input, &output));
    }

    #[test]
    fn made_it_worse_true_on_near_silent_output() {
        let input = generate_speech_like(48_000, 4.0, 0.3);
        // Same length, but essentially silent (rms << 5% of input rms).
        let output = vec![0.0001f32; input.len()];
        assert!(preprocess_made_it_worse(&input, &output));
    }

    #[test]
    fn made_it_worse_false_when_input_has_no_speech() {
        // Silence in → nothing to protect, never a "make it worse" case even
        // if the output is empty.
        let input = vec![0.0f32; 48_000];
        assert!(!preprocess_made_it_worse(&input, &[]));
    }

    #[test]
    fn made_it_worse_false_on_empty_input() {
        assert!(!preprocess_made_it_worse(&[], &[]));
    }

    #[test]
    fn guarded_matches_full_on_healthy_speech() {
        // On healthy speech the guard must NOT trip: guarded output is
        // byte-identical to the plain full pipeline. This is the invariant
        // that keeps validated dictations unchanged.
        let input = generate_speech_like(48_000, 6.0, 0.3);
        let full = process_buffer(&input, 48_000);
        let guarded = process_buffer_guarded(&input, 48_000);
        assert_eq!(
            guarded.len(),
            full.len(),
            "guard altered a healthy recording"
        );
        assert_eq!(guarded, full);
    }

    #[test]
    fn guarded_output_is_never_degenerate() {
        // Whatever the input, the guarded output is never itself flagged as a
        // make-it-worse collapse (either full was fine, or we fell back to
        // highpass which preserves the input).
        for (dur, amp) in &[(6.0f32, 0.3f32), (2.0, 0.05), (10.0, 0.2)] {
            let input = generate_speech_like(48_000, *dur, *amp);
            let guarded = process_buffer_guarded(&input, 48_000);
            assert!(
                !preprocess_made_it_worse(&input, &guarded),
                "guarded output degenerate for dur={dur} amp={amp}"
            );
            assert!(guarded.iter().all(|s| s.is_finite()));
        }
    }

    // ---- AUDIO-001 belt-and-suspenders (§4) -------------------------------

    #[test]
    fn full_pipeline_on_exact_zeros_produces_no_nan() {
        // Feeding pure silence must never yield NaN (dagc-on-zero is the
        // AUDIO-001 root cause). Empty output is fine; NaN is not.
        let zeros = vec![0.0f32; 48_000 * 2];
        let out = process_buffer(&zeros, 48_000);
        assert!(
            out.iter().all(|s| s.is_finite()),
            "process_buffer emitted non-finite samples on pure silence"
        );
    }

    #[test]
    fn full_pipeline_on_near_zero_produces_no_nan() {
        // Near-zero (denormal-ish) input is the other dagc danger zone.
        let tiny = vec![1e-9f32; 48_000 * 2];
        let out = process_buffer(&tiny, 48_000);
        assert!(
            out.iter().all(|s| s.is_finite()),
            "process_buffer emitted non-finite samples on near-zero input"
        );
    }

    // ── Chunk VAD trim: realtime chunked dictation + meeting chunks ──────
    // These paths used to hand whisper the raw buffer, silence included,
    // which is the input it hallucinates "thank you" / "grazie" on.

    #[test]
    fn chunk_vad_only_drops_an_all_silence_window() {
        // The reason the function exists: an idle window must never reach
        // the model.
        let sr = 48_000u32;
        let silence = vec![0.0f32; (sr as f32 * 3.5) as usize];
        let out = process_chunk_vad_only(&silence, sr);
        assert!(
            out.is_empty(),
            "a silent window must collapse to empty, kept {} samples",
            out.len()
        );
    }

    #[test]
    fn chunk_vad_only_keeps_speech() {
        let sr = 48_000u32;
        let speech = generate_speech_like(sr, 3.5, 0.3);
        let out = process_chunk_vad_only(&speech, sr);
        assert!(!out.is_empty(), "a speech window must reach the model");
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn chunk_vad_only_hands_a_speech_window_over_untouched() {
        // The gate decides WHETHER the model is called, never what it hears.
        //
        // Trimming inside the window is what broke local dictation on
        // 2026-08-05: whisper got sub-second fragments stripped of their
        // pauses, lost the context it needs for punctuation and segmentation,
        // and returned invented Spanish for Italian speech. The pauses are
        // signal, not waste.
        let sr = 48_000u32;
        let speech = generate_speech_like(sr, 3.5, 0.3);
        let out = process_chunk_vad_only(&speech, sr);
        assert_eq!(
            out.len(),
            speech.len(),
            "a window that passes the gate must go to the model whole"
        );
        assert_eq!(out, speech, "the gate must not modify a single sample");
    }

    #[test]
    fn chunk_vad_only_keeps_the_pauses_around_speech() {
        // 1 s silence + 2 s speech + 4 s silence: exactly the shape the old
        // trim shrank to a fragment. The window carries speech, so it goes to
        // the model as-is — a natural pause is what tells whisper a sentence
        // ended.
        let sr = 48_000u32;
        let mut window = vec![0.0f32; sr as usize];
        window.extend(generate_speech_like(sr, 2.0, 0.3));
        window.extend(vec![0.0f32; sr as usize * 4]);
        let out = process_chunk_vad_only(&window, sr);
        assert_eq!(
            out.len(),
            window.len(),
            "the window holds speech, so it must survive intact"
        );
    }

    #[test]
    fn chunk_vad_only_does_not_apply_agc() {
        // AGC must stay off here: one instance per chunk would settle on a
        // different gain per window and adjacent chunks would come out at
        // different levels. A quiet window must stay quiet.
        let sr = 48_000u32;
        let quiet = generate_speech_like(sr, 3.0, 0.05);
        let out = process_chunk_vad_only(&quiet, sr);
        assert!(!out.is_empty());
        let out_rms = rms(&out);
        assert!(
            out_rms < TARGET_RMS * 0.5,
            "output RMS {} looks normalised toward the AGC target {}",
            out_rms,
            TARGET_RMS
        );
    }

    #[test]
    fn chunk_collapse_guard_drops_transients_but_keeps_sustained_energy() {
        // The 2026-07-31 regression: a mostly-silent 15 s mic window with a
        // couple of loud clicks lifted the WINDOW rms above the floor, the
        // guard handed whisper the untrimmed window, and out came "Grazie.".
        // Isolated transients must now collapse to nothing.
        let sr = 48_000u32;
        let mut clicky = vec![0.0f32; sr as usize * 3];
        for c in 0..3 {
            let at = (c + 1) * sr as usize * 3 / 4;
            for k in 0..(sr as usize / 200) {
                clicky[at + k] = if k % 2 == 0 { 0.6 } else { -0.6 };
            }
        }
        assert!(
            rms(&clicky) > ENERGY_FLOOR,
            "fixture must trip the old guard: window rms above the floor"
        );
        assert!(
            sustained_energy_fraction(&clicky, sr) < CHUNK_SUSTAINED_ENERGY_FRACTION,
            "fixture must be transient-shaped, not sustained"
        );
        assert!(
            process_chunk_vad_only(&clicky, sr).is_empty(),
            "a window of isolated clicks must never reach the model"
        );

        // The other side of the split: sustained speech-level energy still
        // gets handed back rather than silently dropped.
        let speech = generate_speech_like(sr, 3.0, 0.3);
        assert!(
            sustained_energy_fraction(&speech, sr) >= CHUNK_SUSTAINED_ENERGY_FRACTION,
            "real speech must read as sustained"
        );
        assert!(!process_chunk_vad_only(&speech, sr).is_empty());
    }

    #[test]
    fn chunk_vad_only_drops_a_window_holding_only_a_sliver_of_speech() {
        // 0.1 s of speech in an otherwise silent window. Whisper pads any
        // input to a full 30 s encoder window, so this costs a whole pass to
        // return nothing — and on 2026-07-31 it returned a phantom sign-off
        // instead. The gate keeps the model out of it entirely.
        let sr = 48_000u32;
        let mut window = generate_speech_like(sr, 0.1, 0.3);
        window.extend(vec![0.0f32; sr as usize * 3]);
        let out = process_chunk_vad_only(&window, sr);
        assert!(
            out.is_empty(),
            "under {}ms of speech must not open the gate, kept {} samples",
            CHUNK_MIN_SPEECH_MS,
            out.len()
        );
    }

    #[test]
    fn chunk_vad_only_never_empties_a_loud_window() {
        // Safety invariant for the user: a window with clear energy in it
        // must never come back empty, whatever the VAD makes of it. Either
        // the VAD keeps it, or the make-it-worse guard hands the raw window
        // back. A pure tone is the awkward case — loud, but not speech.
        let sr = 48_000u32;
        let tone: Vec<f32> = (0..(sr as usize * 3))
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin() * 0.4)
            .collect();
        let out = process_chunk_vad_only(&tone, sr);
        assert!(!out.is_empty(), "a loud window must never be dropped");
    }

    #[test]
    fn chunk_vad_only_passes_through_below_48k() {
        // nnnoiseless needs 48 kHz. Below it there is nothing to trim, so
        // the window must come back byte-identical rather than picking up a
        // highpass the caller never asked for.
        let sr = 16_000u32;
        let speech = generate_speech_like(sr, 1.0, 0.3);
        let out = process_chunk_vad_only(&speech, sr);
        assert_eq!(out, speech, "below 48 kHz the window must be untouched");
    }

    #[test]
    fn chunk_vad_only_empty_input_is_empty() {
        assert!(process_chunk_vad_only(&[], 48_000).is_empty());
    }

    #[test]
    fn chunk_vad_only_drops_speech_shaped_noise_below_the_energy_floor() {
        // The real-world failure, 2026-07-31: an idle meeting mic whose median
        // level was 0.00028 still produced a phantom "Grazie" every 15 s.
        // nnnoiseless scores voice likelihood from spectral shape, so quiet
        // clicks and breath look like speech to it. On the chunk path a frame
        // must ALSO clear ENERGY_FLOOR, so a whole window of speech-SHAPED but
        // inaudible audio must collapse to nothing and never reach the model.
        let sr = 48_000u32;
        let whisper_quiet = generate_speech_like(sr, 3.5, 0.003);
        assert!(
            rms(&whisper_quiet) < ENERGY_FLOOR,
            "test fixture must sit below the energy floor"
        );
        let out = process_chunk_vad_only(&whisper_quiet, sr);
        assert!(
            out.is_empty(),
            "speech-shaped noise under the energy floor must not reach the model, kept {} samples",
            out.len()
        );
    }
}

//! Silero VAD: the speech/no-speech decision for the realtime chunk gate.
//!
//! # Why this exists
//!
//! The gate answers one question — is there speech in this window? — and
//! wrongness costs asymmetrically: a false drop loses the user's words, a
//! false pass hands whisper silence and it invents a training-set sign-off
//! ("Grazie", "Thank you everyone").
//!
//! Until now the answer came from nnnoiseless (RNNoise) voice probability AND
//! a per-frame absolute energy floor. The energy term was a crutch: RNNoise is
//! *trained* to be indifferent to level (it keeps the energy cepstral
//! coefficient and skips cepstral mean normalisation), so a keyboard click at
//! -60 dBFS opens a speech window. The crutch is the fragile part — its
//! threshold has to span sessions, and in the real corpus the gap between the
//! noisiest room and the quietest speech is only ~6 dB. It had to be moved
//! once already on 2026-08-05.
//!
//! # The measurement that decided it
//!
//! 300 windows over 12 real meeting mic tracks, 272 scored (`vad_ab` bin,
//! ground truth = whisper on the untrimmed window):
//!
//! |                     | words lost | hallucinations passed |
//! |---|---|---|
//! | RNNoise + energy    | 0          | 62                    |
//! | Silero              | 0          | **19**                |
//!
//! All 47 speech windows survived BOTH gates, so this costs nothing in
//! recall. Of 40 disagreements, 39 were Silero correctly rejecting silence we
//! let through. An earlier run over dictations had favoured the old gate, but
//! that corpus held only 4 silence windows: the right measurement, the wrong
//! material.
//!
//! Cost: 150 ms median per 3 s window against the old gate's 16 ms. Both are
//! noise against a 3000 ms budget.
//!
//! # Shipping and degradation
//!
//! The model is 885 KB and rides INSIDE the signed installer / DMG, next to
//! the executable. No runtime download: at 0.8% of the installer it is not
//! worth a background thread, a not-yet-available state and a network failure
//! mode. (The whisper models are fetched because they are 0.5-1.1 GB; copying
//! that pattern here was a reflex, and the wrong one.)
//!
//! When the file is absent anyway — a build that did not bundle it, an
//! unreadable copy, or no `local-stt` — every entry point returns `None` and
//! the caller keeps the RNNoise+energy path. The gate always works.

/// Filename in the shared model directory.
pub const MODEL_FILE: &str = "ggml-silero-v6.2.0.bin";

/// Upstream source, kept for provenance: this is where `core/assets/` got the
/// bundled copy from. Nothing downloads at runtime.
pub const MODEL_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin";

/// Silero's own default, and the value faster-whisper and whisper.cpp both
/// ship. Deliberately NOT tuned by feel: it is a probability on a
/// level-robust score, which is exactly the property the old absolute energy
/// floor lacked. Retune only against a measurement.
const THRESHOLD: f32 = 0.5;

/// Sample rate the model expects. Callers must downsample first.
pub const REQUIRED_RATE: u32 = 16_000;

/// Where the model actually is.
///
/// SHIPPED first: at 885 KB it rides inside the signed installer / DMG next to
/// the executable, so it is present on first run, works offline, and carries
/// the installer's signature. That is the whole reason there is no download
/// path here — the whisper models are fetched because they are 0.5-1.1 GB, and
/// copying that pattern for a file smaller than an icon would have bought a
/// background thread, a not-yet-available state and a network failure mode for
/// nothing.
///
/// The model directory is still checked as a fallback so a developer (or a
/// user with an older install) can drop the file in by hand.
pub fn model_path() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let shipped = dir.join(MODEL_FILE);
            if shipped.is_file() {
                return shipped;
            }
        }
    }
    crate::local_stt::model_path(MODEL_FILE)
}

/// Is the model on disk? False in a build that did not bundle it, which is a
/// supported state: the caller keeps the RNNoise+energy fallback.
pub fn model_present() -> bool {
    model_path().is_file()
}

#[cfg(feature = "local-stt")]
mod backend {
    use std::sync::Mutex;
    use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

    /// Loaded once and reused: construction reads and initialises the model,
    /// which is pure overhead per window. `WhisperVadContext` is Send + Sync
    /// upstream but `detect_speech` takes `&mut self`, so the Mutex is what
    /// makes reuse sound rather than an optimisation.
    static CTX: Mutex<Option<WhisperVadContext>> = Mutex::new(None);

    /// True when the window holds speech, `None` when the model is unavailable
    /// and the caller should fall back.
    pub fn speech_present(pcm16k: &[f32]) -> Option<bool> {
        if pcm16k.is_empty() {
            return Some(false);
        }
        let path = super::model_path();
        if !path.is_file() {
            return None;
        }

        let mut guard = CTX.lock().ok()?;
        if guard.is_none() {
            // CPU by default: measured 150 ms per 3 s window, and the GPU is
            // already busy with whisper on the very next call.
            let params = WhisperVadContextParams::new();
            match WhisperVadContext::new(path.to_string_lossy().as_ref(), params) {
                Ok(c) => {
                    crate::log("[Silero] VAD model loaded");
                    *guard = Some(c);
                }
                Err(e) => {
                    crate::log(&format!(
                        "[Silero] load failed ({e:?}) — falling back to the RNNoise gate"
                    ));
                    return None;
                }
            }
        }
        let ctx = guard.as_mut()?;

        let mut params = WhisperVadParams::new();
        params.set_threshold(super::THRESHOLD);
        match ctx.segments_from_samples(params, pcm16k) {
            Ok(segs) => Some(segs.num_segments() > 0),
            Err(e) => {
                crate::log(&format!("[Silero] detect failed ({e:?}) — falling back"));
                None
            }
        }
    }

    /// Drop the cached context (model swap / shutdown).
    pub fn clear() {
        if let Ok(mut g) = CTX.lock() {
            if g.is_some() {
                crate::log("[Silero] clearing VAD context");
            }
            *g = None;
        }
    }
}

#[cfg(not(feature = "local-stt"))]
mod backend {
    /// No whisper-rs in this build, so no Silero. The caller keeps the
    /// RNNoise+energy gate.
    pub fn speech_present(_pcm16k: &[f32]) -> Option<bool> {
        None
    }
    pub fn clear() {}
}

/// Does this 16 kHz window hold speech?
///
/// `None` means "no answer available" — model missing, failed to load, or a
/// build without `local-stt` — and the caller MUST fall back rather than
/// treat it as silence. Getting that wrong would drop every window on a fresh
/// install.
pub fn speech_present(pcm16k: &[f32]) -> Option<bool> {
    backend::speech_present(pcm16k)
}

/// Drop the cached context.
pub fn clear() {
    backend::clear()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_path_prefers_the_shipped_copy_then_the_model_dir() {
        // The bundled file rides next to the executable inside the signed
        // installer, so it must win: a stale hand-dropped copy in the model
        // directory must never shadow what we shipped and tested.
        let p = model_path();
        assert!(
            p.ends_with(MODEL_FILE),
            "path must end with the filename, got {}",
            p.display()
        );
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.to_path_buf()));
        let shipped_here = exe_dir.as_ref().map(|d| d.join(MODEL_FILE));
        match shipped_here {
            Some(s) if s.is_file() => assert_eq!(p, s, "shipped copy must win"),
            _ => assert_eq!(
                p,
                crate::local_stt::model_path(MODEL_FILE),
                "with nothing shipped, fall back to the model directory"
            ),
        }
    }

    #[test]
    fn empty_window_is_not_speech() {
        // Cheap and unambiguous: no samples cannot hold speech, and answering
        // None here would push an empty window onto the fallback for nothing.
        assert_eq!(speech_present(&[]), Some(false));
    }

    #[test]
    fn missing_model_yields_no_answer_not_a_false_negative() {
        // The distinction is load-bearing. If an absent model read as "no
        // speech", a fresh install would drop every window and transcribe
        // nothing at all.
        if model_present() {
            return; // machine has the model; covered by the harness instead
        }
        assert_eq!(
            speech_present(&vec![0.1f32; 16_000]),
            None,
            "an unavailable model must return None so the caller falls back"
        );
    }

    #[test]
    fn threshold_is_the_upstream_default() {
        // Guards against someone "tuning" it by feel. It is a probability on
        // a level-robust score, not an energy floor: the value only moves
        // against a measurement.
        assert!((THRESHOLD - 0.5).abs() < f32::EPSILON);
    }
}

//! Qwen3-ASR on the Apple Neural Engine, through FluidAudio's CoreML
//! pipeline -- the same library that runs Parakeet here.
//!
//! `qwen_asr`'s llama.cpp path runs Qwen on the GPU, the one the window
//! server draws with, which is what makes a long local meeting slow the whole
//! Mac. This one encodes and decodes on the ANE. FluidAudio publishes the
//! 0.6B only, in f32 and int8. It needs macOS 15 (stateful CoreML decoder),
//! checked at runtime because the Mac app still targets 14.
//!
//! Every function exists on every platform so `qwen_asr` routes without a
//! `cfg`; where it cannot run, `supported()` is false and the catalog hides it.

use crate::error::TranscribeError;

#[cfg(all(
    target_os = "macos",
    target_arch = "aarch64",
    feature = "local-stt-parakeet-fluid"
))]
mod inference {
    use super::TranscribeError;
    use fluidaudio_rs::FluidAudio;
    use std::sync::Mutex;

    /// The loaded variant (`true` = int8) and its engine. Held for the
    /// process: loading compiles for the ANE, which is the whole cost.
    static ENGINE: Mutex<Option<(bool, FluidAudio)>> = Mutex::new(None);

    pub fn supported() -> bool {
        FluidAudio::qwen3_supported()
    }

    pub fn models_present(int8: bool) -> bool {
        supported() && FluidAudio::qwen3_models_exist(int8)
    }

    pub fn download(int8: bool, progress: &(dyn Fn(f64) + Sync)) -> Result<(), TranscribeError> {
        FluidAudio::qwen3_download(int8, progress)
            .map_err(|e| TranscribeError::LocalModel(format!("qwen3 download: {e}")))
    }

    pub fn transcribe(
        pcm_16k: &[f32],
        int8: bool,
        language: Option<&str>,
    ) -> Result<String, TranscribeError> {
        let mut guard = ENGINE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.as_ref().is_none_or(|(loaded, _)| *loaded != int8) {
            // Drop the other variant first: never two sets of weights at once.
            *guard = None;
            let fa = FluidAudio::new()
                .map_err(|e| TranscribeError::LocalModel(format!("fluid new: {e}")))?;
            fa.init_qwen3(int8)
                .map_err(|e| TranscribeError::LocalModel(format!("qwen3 init: {e}")))?;
            *guard = Some((int8, fa));
        }
        let (_, fa) = guard.as_ref().expect("variant loaded above");
        fa.transcribe_qwen3(pcm_16k, language)
            .map_err(|e| TranscribeError::LocalModel(format!("qwen3 transcribe: {e}")))
    }
}

#[cfg(not(all(
    target_os = "macos",
    target_arch = "aarch64",
    feature = "local-stt-parakeet-fluid"
)))]
mod inference {
    use super::TranscribeError;

    pub fn supported() -> bool {
        false
    }

    pub fn models_present(_int8: bool) -> bool {
        false
    }

    pub fn download(_int8: bool, _progress: &(dyn Fn(f64) + Sync)) -> Result<(), TranscribeError> {
        Err(unavailable())
    }

    pub fn transcribe(
        _pcm_16k: &[f32],
        _int8: bool,
        _language: Option<&str>,
    ) -> Result<String, TranscribeError> {
        Err(unavailable())
    }

    fn unavailable() -> TranscribeError {
        TranscribeError::LocalModel(
            "Qwen3-ASR on the Neural Engine needs Apple Silicon and the local-stt-parakeet-fluid feature"
                .to_string(),
        )
    }
}

/// Whether this build, on this OS, can run it.
pub fn supported() -> bool {
    inference::supported()
}

/// Whether the variant's models are already in FluidAudio's cache.
pub fn models_present(int8: bool) -> bool {
    inference::models_present(int8)
}

/// Download one variant (blocking), reporting the completed fraction 0..1.
pub fn download(int8: bool, progress: &(dyn Fn(f64) + Sync)) -> Result<(), TranscribeError> {
    inference::download(int8, progress)
}

/// Transcribe one 16 kHz mono window, loading the variant on first use.
///
/// `language` is an ISO code, or empty for auto-detect. Naming it matters
/// here more than it looks: FluidAudio decides the language per call, so an
/// unanchored meeting drifts from window to window -- see
/// `qwen_asr::transcribe_instruction` for the measurement.
pub fn transcribe(pcm_16k: &[f32], int8: bool, language: &str) -> Result<String, TranscribeError> {
    assert!(
        pcm_16k.iter().all(|s| s.is_finite()),
        "qwen_fluid::transcribe: pcm_16k must be all-finite"
    );
    if pcm_16k.is_empty() {
        return Ok(String::new());
    }
    inference::transcribe(pcm_16k, int8, (!language.is_empty()).then_some(language))
}

#[cfg(test)]
mod tests {
    /// An empty language means Auto-detect, and FluidAudio spells that
    /// `None` -- passing `Some("")` would have it look up a language named
    /// "" and it decides per call, which is how a 22-minute Italian meeting
    /// came back with its middle third in Spanish (measured 2026-09-12).
    #[test]
    fn empty_language_becomes_auto_not_a_language_named_nothing() {
        assert_eq!((!"".is_empty()).then_some(""), None);
        assert_eq!((!"it".is_empty()).then_some("it"), Some("it"));
    }

    /// Where it cannot run, nothing about it is offered: the catalog asks
    /// `supported()` before listing the variants, so a stub that answered
    /// `true` would put an undownloadable entry in the user's picker.
    #[test]
    fn unsupported_platforms_offer_nothing() {
        if !cfg!(all(
            target_os = "macos",
            target_arch = "aarch64",
            feature = "local-stt-parakeet-fluid"
        )) {
            assert!(!super::supported());
            assert!(!super::models_present(true));
        }
    }

    #[test]
    fn an_empty_window_is_empty_text_not_a_model_load() {
        // Only reachable where the engine is compiled out or absent: the
        // guard runs before any FluidAudio call, so it holds either way.
        assert_eq!(super::transcribe(&[], true, "it").unwrap(), "");
    }
}

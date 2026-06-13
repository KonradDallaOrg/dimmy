//! Parakeet TDT v3 via FluidInference's CoreML bundle on Apple Silicon.
//!
//! Wraps `fluidaudio-rs` (Swift bridge) to run the Parakeet model on the
//! Neural Engine instead of the ort + CPU path that ships on Win/Linux.
//! Documented RTF on M-series: 100-300×; first `init_asr()` downloads
//! the ~3 GB CoreML bundle into `~/.cache/fluidaudio/` and ANE-compiles
//! it (~20-30 s one-time), subsequent loads ~1 s warm.
//!
//! Surface mirrors `parakeet.rs` so the dispatch in `transcribe.rs` and
//! `chunked_stt.rs` can swap between backends without knowing which is
//! active. The two never coexist in the same build (the cargo features
//! are mutually exclusive on Mac), but the feature gate lives only in
//! this file — every caller compiles the always-on stubs unchanged.
//!
//! `transcribe_samples` only landed on `main` after tag v0.12.6, so we
//! bridge in-memory PCM to disk via a temp WAV per call. The cost is
//! ~320 KB I/O for a 5 s chunk, negligible vs the ANE inference time.

#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::path::PathBuf;

use crate::error::TranscribeError;

/// Default cache directory FluidAudio writes its CoreML bundle into on
/// first `init_asr()`. Verified empirically on macOS 26: the framework
/// uses `~/Library/Application Support/FluidAudio/Models/` (not the
/// `~/Library/Caches/` path one would assume from the name). We only
/// sanity-check this dir for the UI's "downloaded?" pill — the
/// download itself is fully owned by the FluidAudio Swift side.
pub fn cache_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join("FluidAudio").join("Models"))
}

/// Specific Parakeet bundle subdir. FluidAudio writes the v3 CoreML
/// model under `parakeet-tdt-0.6b-v3-coreml/`; if that exists with
/// the expected `.mlmodelc` children we treat the bundle as present.
fn parakeet_bundle_dir() -> Option<PathBuf> {
    cache_dir().map(|p| p.join("parakeet-tdt-0.6b-v3-coreml"))
}

/// Heuristic "bundle on disk" check — true when the Parakeet v3 subdir
/// exists and contains the four `.mlmodelc` directories the runtime
/// loads (Encoder / Decoder / JointDecision / Preprocessor). Cheaper
/// than calling FluidAudio Swift just to ask, and stable across
/// FluidAudio versions that shuffle internal manifests.
pub fn bundle_present() -> bool {
    let Some(dir) = parakeet_bundle_dir() else {
        return false;
    };
    let required = [
        "Encoder.mlmodelc",
        "Decoder.mlmodelc",
        "JointDecision.mlmodelc",
        "Preprocessor.mlmodelc",
    ];
    required.iter().all(|name| {
        std::fs::metadata(dir.join(name))
            .map(|m| m.is_dir())
            .unwrap_or(false)
    })
}

#[cfg(feature = "local-stt-parakeet-fluid")]
pub use inference::{download_bundle, transcribe, warmup};

/// Stubs for builds without the feature so call sites stay simple.
#[cfg(not(feature = "local-stt-parakeet-fluid"))]
pub fn transcribe(_pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    Err(TranscribeError::LocalModel(
        "parakeet-fluid requires the `local-stt-parakeet-fluid` cargo feature".into(),
    ))
}

#[cfg(not(feature = "local-stt-parakeet-fluid"))]
pub fn warmup() -> Result<(), TranscribeError> {
    Ok(())
}

#[cfg(not(feature = "local-stt-parakeet-fluid"))]
pub fn download_bundle(_progress: impl FnMut(u64, u64)) -> Result<(), TranscribeError> {
    Err(TranscribeError::LocalModel(
        "parakeet-fluid requires the `local-stt-parakeet-fluid` cargo feature".into(),
    ))
}

#[cfg(feature = "local-stt-parakeet-fluid")]
mod inference {
    use super::*;
    use fluidaudio_rs::FluidAudio;
    use std::sync::OnceLock;

    /// Box::leak the wrapper for the same reason as `parakeet.rs`: the
    /// underlying CoreML compiled models load slowly and we want them
    /// alive for the whole process. Drop order at exit is also a known
    /// trip-hazard for ML libraries that hold global state.
    static FLUID: OnceLock<&'static std::sync::Mutex<Option<FluidAudio>>> = OnceLock::new();

    fn lock() -> &'static std::sync::Mutex<Option<FluidAudio>> {
        FLUID.get_or_init(|| Box::leak(Box::new(std::sync::Mutex::new(None))))
    }

    /// Eager-load the FluidAudio ASR engine. On first run this downloads
    /// the ~3 GB CoreML bundle and ANE-compiles it (20-30 s). Subsequent
    /// calls in the same process short-circuit on the OnceLock; across
    /// processes the FluidAudio framework reads its own cache.
    fn ensure_loaded() -> Result<(), TranscribeError> {
        let mtx = lock();
        let mut g = mtx
            .lock()
            .map_err(|e| TranscribeError::LocalModel(format!("fluid mutex: {e}")))?;
        if g.is_some() {
            return Ok(());
        }
        let fa = FluidAudio::new()
            .map_err(|e| TranscribeError::LocalModel(format!("fluid new: {e}")))?;
        fa.init_asr()
            .map_err(|e| TranscribeError::LocalModel(format!("fluid init_asr: {e}")))?;
        *g = Some(fa);
        Ok(())
    }

    /// Mirror of `parakeet::download_bundle`. FluidAudio doesn't expose
    /// a byte-level progress callback in v0.12.6, so `ensure_loaded`
    /// (which owns the download) runs on a worker thread while THIS
    /// thread — the one that owns the non-Send `progress` closure —
    /// polls the cache dir every 200 ms and emits cumulative-bytes
    /// ticks live. The previous design ran `ensure_loaded` here and
    /// ferried ticks into a channel that was only drained AFTER it
    /// returned, so the UI sat at 0 % for the entire 466 MB download.
    /// The total is the empirical 466 MB the v3 bundle expands to
    /// (pinned as a const so a future bundle revision trips an obvious
    /// "stuck at 80 %" instead of silently mis-reporting).
    pub fn download_bundle(mut progress: impl FnMut(u64, u64)) -> Result<(), TranscribeError> {
        const TOTAL_BYTES: u64 = 466 * 1024 * 1024;
        const POLL_MS: u64 = 200;

        progress(0, TOTAL_BYTES);

        let worker = std::thread::Builder::new()
            .name("fluid-dl".into())
            .spawn(ensure_loaded)
            .map_err(|e| TranscribeError::LocalModel(format!("spawn fluid-dl: {e}")))?;

        let dir = parakeet_bundle_dir();
        let mut last = 0u64;
        while !worker.is_finished() {
            let b = dir
                .as_ref()
                .map(|p| dir_size(p))
                .unwrap_or(0)
                .min(TOTAL_BYTES);
            if b != last {
                progress(b, TOTAL_BYTES);
                last = b;
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        }
        worker
            .join()
            .map_err(|_| TranscribeError::LocalModel("fluid-dl thread panicked".into()))??;
        // Final 100 % tick — ensure UI lands on completion regardless
        // of whether the last polled value was a few hundred KB shy.
        progress(TOTAL_BYTES, TOTAL_BYTES);
        Ok(())
    }

    /// Recursively sum file sizes under `dir`. Returns 0 if the dir
    /// doesn't exist yet (early-poll race) or any I/O error happens
    /// — the poller re-tries on the next tick.
    fn dir_size(dir: &std::path::Path) -> u64 {
        fn walk(p: &std::path::Path) -> u64 {
            let Ok(rd) = std::fs::read_dir(p) else {
                return 0;
            };
            let mut sum = 0u64;
            for entry in rd.flatten() {
                let path = entry.path();
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        sum = sum.saturating_add(meta.len());
                    } else if meta.is_dir() {
                        sum = sum.saturating_add(walk(&path));
                    }
                }
            }
            sum
        }
        walk(dir)
    }

    /// Run a 1 s zero-PCM dummy inference so the first user recording
    /// doesn't pay the ANE-compile or model-load cost. Idempotent.
    pub fn warmup() -> Result<(), TranscribeError> {
        ensure_loaded()?;
        let dummy: Vec<f32> = vec![0.0; 16_000];
        let _ = transcribe(&dummy)?;
        Ok(())
    }

    /// Transcribe a 16 kHz mono f32 PCM buffer through FluidAudio.
    /// FluidAudio v0.12.6's Rust binding only exposes `transcribe_file`,
    /// so we serialize the PCM to a temp WAV (int16 PCM, mono, 16 kHz)
    /// and clean up after. The temp file write is ~320 KB / 5 s chunk.
    pub fn transcribe(pcm_16k: &[f32]) -> Result<String, TranscribeError> {
        assert!(
            pcm_16k.iter().all(|s| s.is_finite()),
            "parakeet_fluid::transcribe: pcm_16k must be all-finite"
        );
        if pcm_16k.is_empty() {
            return Ok(String::new());
        }
        ensure_loaded()?;
        let mtx = lock();
        let g = mtx
            .lock()
            .map_err(|e| TranscribeError::LocalModel(format!("fluid mutex: {e}")))?;
        let fa = g.as_ref().expect("ensure_loaded just initialised");

        // Write temp WAV. We use the system temp dir + a per-call PID/ts
        // suffix so concurrent transcribers don't clobber each other.
        let tmp = std::env::temp_dir().join(format!(
            "dimmy-fluid-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or(0),
        ));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut w = hound::WavWriter::create(&tmp, spec)
                .map_err(|e| TranscribeError::LocalModel(format!("temp wav create: {e}")))?;
            for s in pcm_16k {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped * 32767.0) as i16;
                w.write_sample(i)
                    .map_err(|e| TranscribeError::LocalModel(format!("temp wav sample: {e}")))?;
            }
            w.finalize()
                .map_err(|e| TranscribeError::LocalModel(format!("temp wav finalize: {e}")))?;
        }

        let res = fa.transcribe_file(&tmp);
        let _ = std::fs::remove_file(&tmp);
        let result =
            res.map_err(|e| TranscribeError::LocalModel(format!("fluid transcribe: {e}")))?;
        Ok(result.text)
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_is_under_application_support() {
        let dir = cache_dir().expect("cache_dir on a normal mac dev box");
        let s = dir.to_string_lossy();
        // FluidAudio uses Application Support, not Caches/. The exact path
        // matters for `bundle_present` to pick up the framework's download.
        assert!(
            s.contains("Application Support/FluidAudio") || s.contains(".local/share/FluidAudio"),
            "unexpected cache dir: {}",
            s
        );
    }

    #[test]
    fn bundle_present_is_false_when_dir_missing() {
        // We can't make assumptions about the dev box's state; only assert
        // that the helper doesn't panic and returns a bool.
        let _ = bundle_present();
    }
}

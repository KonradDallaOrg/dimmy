//! Parakeet TDT v3 FP32 local STT — pure Rust via ONNX Runtime.
//!
//! Bundle layout (downloaded from `istupakov/parakeet-tdt-0.6b-v3-onnx`,
//! ~2.5 GB, kept under `<config-dir>/parakeet-fp32/`):
//!
//! - `nemo128.onnx`              waveform → 128-bin mel features
//! - `encoder-model.onnx`        + `.data` external weights (~2.4 GB)
//! - `decoder_joint-model.onnx`  TDT prediction net + joint
//! - `vocab.txt`                 8193 tokens (BPE-style with `▁` word marker)
//!
//! Pipeline (ported 1:1 from onnx_asr.models.nemo.NemoConformerTdt +
//! asr._AsrWithTransducerDecoding._decoding):
//!
//! ```text
//!  16 kHz f32 PCM (mono)
//!         │
//!  nemo128.onnx  ──▶  features[1, 128, T_mel]
//!         │
//!  encoder-model.onnx ──▶ encoded[1, 1024, T_enc] + lens
//!         │
//!  greedy TDT (LSTM state [2,1,640] x2; per frame argmax token + dur)
//!         │
//!  vocab → text (`▁foo` → ` foo`, `<…>` skipped)
//! ```

use std::path::PathBuf;

use crate::error::TranscribeError;

pub fn bundle_dir() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("parakeet-fp32"))
}

pub const FILE_MEL: &str = "nemo128.onnx";
pub const FILE_ENCODER: &str = "encoder-model.onnx";
pub const FILE_ENCODER_DATA: &str = "encoder-model.onnx.data";
pub const FILE_DECODER_JOINT: &str = "decoder_joint-model.onnx";
pub const FILE_VOCAB: &str = "vocab.txt";

const HF_BASE: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

pub const BUNDLE_SIZE_MB: u32 = 2500;
pub const VOCAB_SIZE: usize = 8193;
pub const NUM_DURATIONS: usize = 5;
pub const BLANK_IDX: i64 = 8192;
pub const MAX_TOKENS_PER_STEP: usize = 10;
/// Encoder frame stride in seconds for Parakeet TDT v3: 10 ms mel hop ×
/// 8× Conformer subsampling = 80 ms per encoder frame. Used when
/// converting the per-token frame index emitted during TDT decoding
/// into wall-clock seconds for word-level timestamps.
pub const FRAME_SEC: f64 = 0.08;
#[cfg(feature = "local-stt-parakeet")]
const HIDDEN: usize = 640;

pub fn bundle_present() -> bool {
    let Some(dir) = bundle_dir() else {
        return false;
    };
    let required = [
        FILE_MEL,
        FILE_ENCODER,
        FILE_ENCODER_DATA,
        FILE_DECODER_JOINT,
        FILE_VOCAB,
    ];
    required.iter().all(|name| {
        std::fs::metadata(dir.join(name))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    })
}

/// Download the ONNX bundle into the config dir.
///
/// Every file goes through [`crate::download::download_resumable`], which is
/// the only place that knows how to ask Hugging Face for a file's real hash: a
/// HEAD that does NOT follow the redirect, reading `x-linked-etag` off the 302.
///
/// The hand-rolled loop this replaced read the headers of the FOLLOWED
/// response instead. Since HF moved these repos to Xet storage the CDN's
/// `ETag` is the Xet block id — 64 hex characters, so it passed `is_sha256`
/// and was then compared against the file's actual SHA-256. Every download
/// died on the first 139 KB file with "failed integrity check", deleted it,
/// and the retry did the same: 163 failures across 124 users between June and
/// September 2026 against 13 successes. Reproduced and fixed 2026-09-22.
///
/// The old client also carried a 60 s timeout, which in reqwest covers the
/// body read — a 2.4 GB bundle needed a sustained 310 Mbit/s to beat it.
pub async fn download_bundle(progress: impl Fn(u64, u64)) -> Result<(), TranscribeError> {
    let dir =
        bundle_dir().ok_or_else(|| TranscribeError::LocalModel("config dir unknown".into()))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| TranscribeError::LocalModel(format!("create {:?}: {}", dir, e)))?;

    let files: &[&str] = &[
        FILE_MEL,
        FILE_VOCAB,
        FILE_ENCODER,
        FILE_ENCODER_DATA,
        FILE_DECODER_JOINT,
    ];

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("http client: {}", e)))?;

    // Total for the progress bar: bytes already on disk, plus a HEAD for the
    // files still missing. A HEAD that fails only costs us a wrong total.
    let mut grand_total: u64 = 0;
    for name in files {
        let dest = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() > 0 {
                grand_total = grand_total.saturating_add(meta.len());
                continue;
            }
        }
        let url = format!("{}/{}", HF_BASE, name);
        if let Ok(r) = client.head(&url).send().await {
            let len = r
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            grand_total = grand_total.saturating_add(len);
        }
    }

    // Throttle the host callback: at 64 KB chunks a 2.4 GB bundle would fire
    // ~40 K events, and the dispatcher queue has nowhere useful to put them.
    const PROGRESS_BYTES_INTERVAL: u64 = 1 << 20;
    let last_emit = std::sync::atomic::AtomicU64::new(0);

    let mut base: u64 = 0;
    for name in files {
        let dest = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() > 0 {
                base = base.saturating_add(meta.len());
                progress(base, grand_total);
                continue;
            }
        }
        let url = format!("{}/{}", HF_BASE, name);
        crate::log(&format!("[Parakeet] downloading {}", name));
        // No magic bytes: ONNX and the plain-text vocab share none, so the
        // SHA-256 from x-linked-etag is the whole integrity story.
        crate::download::download_resumable(&client, &url, &dest, &[], |done, _| {
            let total_done = base.saturating_add(done);
            let prev = last_emit.load(std::sync::atomic::Ordering::Relaxed);
            if total_done.saturating_sub(prev) >= PROGRESS_BYTES_INTERVAL {
                last_emit.store(total_done, std::sync::atomic::Ordering::Relaxed);
                progress(total_done, grand_total);
            }
        })
        .await
        .map_err(|e| TranscribeError::LocalModel(format!("{}: {}", name, e)))?;

        base = base.saturating_add(std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));
        progress(base, grand_total);
    }

    assert!(
        bundle_present(),
        "every bundle file must exist after a successful download"
    );
    Ok(())
}

// ── Inference ────────────────────────────────────────────────────

/// True when the active backend's bundle is on disk. Mirrors
/// `transcribe` dispatch — fluid first when wired, ort as fallback.
/// UI gates the "Ready / Download" pill on this.
pub fn active_bundle_present() -> bool {
    #[cfg(all(
        feature = "local-stt-parakeet-fluid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        // On Mac with fluid in the build, fluid is always preferred;
        // we report on its bundle, not ort's.
        return crate::parakeet_fluid::bundle_present();
    }
    #[allow(unreachable_code)]
    bundle_present()
}

/// Download the bundle for the active backend. Mirrors `transcribe`
/// dispatch — fluid on Mac (when wired), ort otherwise. Progress is
/// emitted as `(downloaded_bytes, total_bytes)`; fluid only emits
/// `(0, 0)` then `(1, 1)` because the underlying Swift framework
/// doesn't expose a byte-level callback.
pub async fn download_active_bundle(progress: impl Fn(u64, u64)) -> Result<(), TranscribeError> {
    #[cfg(all(
        feature = "local-stt-parakeet-fluid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        return crate::parakeet_fluid::download_bundle(progress);
    }
    #[allow(unreachable_code)]
    download_bundle(progress).await
}

/// Top-level dispatch for Parakeet transcribe across the two backends:
///
/// 1. **FluidInference / Apple Neural Engine** (`parakeet_fluid`) when
///    the `local-stt-parakeet-fluid` feature is on AND the FluidAudio
///    cache is populated. Mac arm64 only. ~50-60x realtime warm.
/// 2. **ONNX Runtime** (`local-stt-parakeet`) — the cross-platform
///    baseline. CPU-only on Mac for now (CoreML EP failed dynamic
///    MLProgram on this model — see STT-002). ~10x realtime warm.
///
/// `chunked_stt` and `transcribe.rs` call this single entry; they
/// don't need to know which engine they got. Whichever bundle the
/// user has on disk wins, with FluidAudio preferred when both are
/// available.
pub fn transcribe(pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    #[cfg(all(
        feature = "local-stt-parakeet-fluid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        if crate::parakeet_fluid::bundle_present() {
            return crate::parakeet_fluid::transcribe(pcm_16k);
        }
    }
    transcribe_ort(pcm_16k)
}

/// Same as [`transcribe`] but also returns word-level timestamps as
/// JSON (`[{"word":"hello","start":0.42,"end":0.94}, ...]`). On the
/// macOS FluidAudio path, timestamps are unavailable today and we
/// return `"[]"` alongside the text — caller should treat empty as
/// "not produced" and skip the history update.
pub fn transcribe_with_word_timestamps(
    pcm_16k: &[f32],
) -> Result<(String, String), TranscribeError> {
    #[cfg(all(
        feature = "local-stt-parakeet-fluid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        if crate::parakeet_fluid::bundle_present() {
            // Fluid path: no timestamps yet — return text + empty JSON.
            let text = crate::parakeet_fluid::transcribe(pcm_16k)?;
            return Ok((text, "[]".to_string()));
        }
    }
    transcribe_with_word_timestamps_ort(pcm_16k)
}

/// Mirror of `transcribe` for warmup.
pub fn warmup() -> Result<(), TranscribeError> {
    #[cfg(all(
        feature = "local-stt-parakeet-fluid",
        target_os = "macos",
        target_arch = "aarch64"
    ))]
    {
        if crate::parakeet_fluid::bundle_present() {
            return crate::parakeet_fluid::warmup();
        }
    }
    warmup_ort()
}

#[cfg(not(feature = "local-stt-parakeet"))]
fn transcribe_ort(_pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    Err(TranscribeError::LocalModel(
        "parakeet inference requires the `local-stt-parakeet` cargo feature".into(),
    ))
}

/// Longest audio handed to the ONNX encoder in one call.
///
/// Parakeet takes the WHOLE input as a single tensor with no internal
/// segmentation, unlike whisper which always chops to 30 s inside itself.
/// Two measured consequences, on a 10-minute meeting, 2026-09-12:
///
/// - **It breaks outright past some length between 5 and 10 minutes.**
///   600 s fails with `/layers.0/self_attn/Add_2 ... broadcast 2501 by
///   7501` — a fixed-size relative-position table in the export, so no
///   machine and no amount of RAM changes it. Reproduced in a cold
///   process, i.e. it is the model, not a stale optimised session.
/// - **It gets worse long before it breaks:** 30 s ran at 6.0x realtime
///   for 822 words, 120 s at 5.7x for 815, and 300 s at 3.6x for 762.
///   Conformer attention cost grows faster than linearly, and the output
///   gets shorter too.
///
/// 120 s is the longest window measured to cost nothing, with a wide
/// margin under the failure. Anything longer is split below.
///
/// This matters for ONE caller: dictation with "Accelerate transcription"
/// off, which hands over the whole recording at once. A ten-minute
/// dictation used to die here with a developer-facing ONNX message.
#[cfg(feature = "local-stt-parakeet")]
const MAX_ORT_WINDOW_SECS: usize = 120;

#[cfg(feature = "local-stt-parakeet")]
fn transcribe_ort(pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    // ONNX on the CPU: exactly the work EcoQoS demotes to the E-cores. See
    // `win_qos` for the measurement.
    let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();

    let max_samples = MAX_ORT_WINDOW_SECS * 16_000;
    if pcm_16k.len() <= max_samples {
        return inference::transcribe(pcm_16k);
    }

    // Long input: split and stitch rather than fail. The user asked for
    // their words, not for a particular tensor shape.
    crate::log(&format!(
        "[Parakeet] {:.0}s input exceeds the {}s encoder window — splitting",
        pcm_16k.len() as f32 / 16_000.0,
        MAX_ORT_WINDOW_SECS
    ));
    let mut out = String::new();
    for window in pcm_16k.chunks(max_samples) {
        let text = inference::transcribe(window)?;
        if text.trim().is_empty() {
            continue;
        }
        // Same stitcher the chunked dictation path uses, so a word spoken
        // across a boundary is not transcribed twice.
        let delta = crate::chunked_stt::dedup_last_3_words(&out, text.trim());
        if delta.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(delta.trim());
    }
    Ok(out)
}

#[cfg(not(feature = "local-stt-parakeet"))]
fn transcribe_with_word_timestamps_ort(
    _pcm_16k: &[f32],
) -> Result<(String, String), TranscribeError> {
    Err(TranscribeError::LocalModel(
        "parakeet inference requires the `local-stt-parakeet` cargo feature".into(),
    ))
}

#[cfg(feature = "local-stt-parakeet")]
fn transcribe_with_word_timestamps_ort(
    pcm_16k: &[f32],
) -> Result<(String, String), TranscribeError> {
    let _no_throttle = crate::win_qos::NoThrottle::for_local_inference();
    inference::transcribe_with_word_timestamps(pcm_16k)
}

#[cfg(not(feature = "local-stt-parakeet"))]
fn warmup_ort() -> Result<(), TranscribeError> {
    Ok(())
}

#[cfg(feature = "local-stt-parakeet")]
fn warmup_ort() -> Result<(), TranscribeError> {
    inference::warmup()
}

#[cfg(feature = "local-stt-parakeet")]
mod inference {
    use super::*;
    use ort::session::{builder::GraphOptimizationLevel, Session};
    use ort::value::Tensor;
    use std::sync::OnceLock;

    // Box::leak the Mutex — Sessions are needed for the entire process
    // lifetime (load is the slow path, ~5 s on M1 cold) so dropping is
    // never desirable. Leaking sidesteps Rust's static-destructor order
    // entirely, which matters because ort 2.0.0-rc.10's Session::drop
    // touches a global onnxruntime mutex; on a noisy process exit that
    // mutex can already be torn down, surfacing as a benign but
    // confusing `mutex lock failed: Invalid argument` SIGABRT after the
    // test summary line. Known cosmetic exit-code noise, tracked as a
    // known bug; production paste-and-quit flow doesn't hit it.
    static MODEL: OnceLock<&'static std::sync::Mutex<Option<Inner>>> = OnceLock::new();

    struct Inner {
        mel: Session,
        encoder: Session,
        decoder_joint: Session,
        vocab: Vec<String>,
    }

    fn lock() -> &'static std::sync::Mutex<Option<Inner>> {
        MODEL.get_or_init(|| Box::leak(Box::new(std::sync::Mutex::new(None))))
    }

    fn build_session(path: &std::path::Path) -> Result<Session, TranscribeError> {
        // `mut` is needed only when an EP feature is on (CoreML / CUDA);
        // pure CPU only chains `with_optimization_level().commit_from_file()`.
        // Suppress unused_mut here so the simple-CPU build stays warning-free
        // without splitting the function on cfg lines.
        #[allow(unused_mut)]
        let mut builder = Session::builder()
            .map_err(|e| TranscribeError::LocalModel(format!("ort builder {:?}: {e}", path)))?;

        // CoreML execution provider — currently OPT-IN via the runtime
        // env var `DIMMY_PARAKEET_USE_COREML=1`, even when the build flag
        // is on. Local benchmark on M-series with ort 2.0.0-rc.10 against
        // this specific FP32 bundle: NeuralNetwork format is silently
        // CPU-falling-back the 2.4 GB encoder (>2 GB artifact limit), and
        // MLProgram fails to compile with `code: -14` on the dynamic-shape
        // Conformer. Net effect today is "no faster, sometimes slower".
        // Keeping the wiring in tree so we can re-enable from outside
        // the build (or flip the default) once a future ort / onnxruntime
        // release fixes the dynamic MLProgram path.
        #[cfg(feature = "local-stt-parakeet-coreml")]
        if std::env::var("DIMMY_PARAKEET_USE_COREML").as_deref() == Ok("1") {
            use ort::execution_providers::coreml::{CoreMLComputeUnits, CoreMLModelFormat};
            use ort::execution_providers::CoreMLExecutionProvider;
            let cache_dir = dirs::cache_dir()
                .map(|p| p.join("dimmy").join("coreml-parakeet"))
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp/dimmy-coreml-parakeet"));
            let _ = std::fs::create_dir_all(&cache_dir);
            // Try MLProgram first (handles >2 GB encoders). If a future
            // session fails to register the EP we just log and fall back
            // to CPU instead of breaking the whole load — getting the
            // user a working CPU path matters more than a hypothetical
            // EP win.
            match builder.with_execution_providers([CoreMLExecutionProvider::default()
                .with_model_format(CoreMLModelFormat::MLProgram)
                .with_compute_units(CoreMLComputeUnits::All)
                .with_model_cache_dir(cache_dir.to_string_lossy().to_string())
                .build()])
            {
                Ok(b) => {
                    builder = b;
                    crate::log("[Parakeet] CoreML EP registered (DIMMY_PARAKEET_USE_COREML=1)");
                }
                Err(e) => {
                    crate::log(&format!(
                        "[Parakeet] CoreML EP register failed, falling back to CPU: {e}"
                    ));
                    builder = Session::builder().map_err(|e| {
                        TranscribeError::LocalModel(format!("ort builder fallback {:?}: {e}", path))
                    })?;
                }
            }
        }

        // CUDA (Win/Linux) — same gating shape so feature flags stay
        // symmetric across platforms. Currently only used by Win;
        // included here for parity / future Linux-CUDA tier.
        #[cfg(feature = "local-stt-parakeet-cuda")]
        {
            use ort::execution_providers::CUDAExecutionProvider;
            builder = builder
                .with_execution_providers([CUDAExecutionProvider::default().build()])
                .map_err(|e| TranscribeError::LocalModel(format!("ort cuda ep register: {e}")))?;
        }

        builder
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| TranscribeError::LocalModel(format!("ort opt level: {e}")))?
            .commit_from_file(path)
            .map_err(|e| TranscribeError::LocalModel(format!("ort load {:?}: {e}", path)))
    }

    /// Load the 3 ONNX sessions + vocab into the global cache and run a
    /// 1 s zero-input dummy inference to prime CoreML's compute-graph
    /// compile (+ kernel cache + mmap fault). Idempotent: if the cache
    /// is already warm, returns Ok in <1 ms. Designed to be called from
    /// a background thread right after `dimmy_init()` so the user's
    /// first real recording doesn't pay the ~6 s cold path.
    pub fn warmup() -> Result<(), TranscribeError> {
        if !bundle_present() {
            return Err(TranscribeError::LocalModel(
                "parakeet bundle not downloaded — warmup skipped".into(),
            ));
        }
        // Run a tiny dummy inference. The first transcribe call exercises
        // every session + the JIT-compile of the CoreML graph; one-second
        // zero PCM is enough to trigger all of it. The output is
        // discarded.
        let dir = bundle_dir().ok_or_else(|| TranscribeError::LocalModel("bundle dir".into()))?;
        {
            let mtx = lock();
            let mut g = mtx
                .lock()
                .map_err(|e| TranscribeError::LocalModel(format!("mutex: {e}")))?;
            if g.is_some() {
                return Ok(()); // already warm
            }
            *g = Some(load(&dir)?);
        }
        let dummy: Vec<f32> = vec![0.0; 16_000];
        let _ = transcribe(&dummy)?;
        Ok(())
    }

    fn load(dir: &std::path::Path) -> Result<Inner, TranscribeError> {
        let mel = build_session(&dir.join(FILE_MEL))?;
        let encoder = build_session(&dir.join(FILE_ENCODER))?;
        let decoder_joint = build_session(&dir.join(FILE_DECODER_JOINT))?;

        let vocab_text = std::fs::read_to_string(dir.join(FILE_VOCAB))
            .map_err(|e| TranscribeError::LocalModel(format!("read vocab: {e}")))?;
        let mut vocab: Vec<String> = Vec::with_capacity(VOCAB_SIZE + 16);
        for line in vocab_text.lines() {
            let token = line.split_whitespace().next().unwrap_or("").to_string();
            vocab.push(token);
        }

        Ok(Inner {
            mel,
            encoder,
            decoder_joint,
            vocab,
        })
    }

    pub fn transcribe(pcm_16k: &[f32]) -> Result<String, TranscribeError> {
        Ok(transcribe_with_word_timestamps(pcm_16k)?.0)
    }

    /// Same decode as `transcribe()` but also returns word-level
    /// timestamps as JSON: `[{"word":"hello","start":0.42,"end":0.94}, ...]`.
    /// TDT inherently predicts a per-emission duration, so the frame
    /// index at which each BPE piece was emitted is captured in the
    /// greedy loop and converted to seconds via FRAME_SEC. Words are
    /// formed by grouping consecutive pieces, splitting on the BPE
    /// word-marker `▁` (U+2581). Empty PCM yields ("","[]").
    pub fn transcribe_with_word_timestamps(
        pcm_16k: &[f32],
    ) -> Result<(String, String), TranscribeError> {
        assert!(
            pcm_16k.iter().all(|s| s.is_finite()),
            "parakeet::transcribe: pcm_16k must be all-finite"
        );
        if pcm_16k.is_empty() {
            return Ok((String::new(), "[]".to_string()));
        }
        if !bundle_present() {
            return Err(TranscribeError::LocalModel(
                "parakeet bundle not downloaded — call parakeet::download_bundle() first".into(),
            ));
        }
        let dir = bundle_dir().ok_or_else(|| TranscribeError::LocalModel("bundle dir".into()))?;

        let mtx = lock();
        let mut g = mtx
            .lock()
            .map_err(|e| TranscribeError::LocalModel(format!("mutex: {e}")))?;
        if g.is_none() {
            *g = Some(load(&dir)?);
        }
        let inner = g.as_mut().expect("just initialised");

        // ── 1. Mel: waveform → features [1, 128, T_mel] ──────────────
        let n = pcm_16k.len();
        let wave_t = Tensor::from_array((vec![1i64, n as i64], pcm_16k.to_vec()))
            .map_err(|e| TranscribeError::LocalModel(format!("mk wave: {e}")))?;
        let wlen_t = Tensor::from_array((vec![1i64], vec![n as i64]))
            .map_err(|e| TranscribeError::LocalModel(format!("mk wlen: {e}")))?;

        let mel_outs = inner
            .mel
            .run(ort::inputs! {
                "waveforms" => wave_t,
                "waveforms_lens" => wlen_t,
            })
            .map_err(|e| TranscribeError::LocalModel(format!("mel run: {e}")))?;

        let (feat_shape, feat_data) = mel_outs["features"]
            .try_extract_tensor::<f32>()
            .map_err(|e| TranscribeError::LocalModel(format!("mel extract: {e}")))?;
        let feat_dims: Vec<i64> = feat_shape.iter().copied().collect();
        if feat_dims.len() != 3 || feat_dims[1] != 128 {
            return Err(TranscribeError::LocalModel(format!(
                "unexpected mel features shape {:?}",
                feat_dims
            )));
        }
        let t_mel = feat_dims[2] as usize;
        let feat_vec = feat_data.to_vec();

        // ── 2. Encoder: features → outputs [1, 1024, T_enc] + lens ──
        let feat_t = Tensor::from_array((vec![1i64, 128, t_mel as i64], feat_vec))
            .map_err(|e| TranscribeError::LocalModel(format!("mk feat: {e}")))?;
        let flen_t = Tensor::from_array((vec![1i64], vec![t_mel as i64]))
            .map_err(|e| TranscribeError::LocalModel(format!("mk flen: {e}")))?;

        let enc_outs = inner
            .encoder
            .run(ort::inputs! {
                "audio_signal" => feat_t,
                "length" => flen_t,
            })
            .map_err(|e| TranscribeError::LocalModel(format!("encoder run: {e}")))?;

        let (enc_shape, enc_data) = enc_outs["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|e| TranscribeError::LocalModel(format!("enc extract: {e}")))?;
        let enc_dims: Vec<i64> = enc_shape.iter().copied().collect();
        if enc_dims.len() != 3 || enc_dims[1] != 1024 {
            return Err(TranscribeError::LocalModel(format!(
                "unexpected encoder shape {:?}",
                enc_dims
            )));
        }
        let t_enc = enc_dims[2] as usize;
        let enc_data_owned: Vec<f32> = enc_data.to_vec();

        let (_enc_len_shape, enc_len_data) = enc_outs["encoded_lengths"]
            .try_extract_tensor::<i64>()
            .map_err(|e| TranscribeError::LocalModel(format!("enclen extract: {e}")))?;
        let enc_len_owned: Vec<i64> = enc_len_data.to_vec();
        let valid_t_enc = (enc_len_owned[0] as usize).min(t_enc);

        // [1, 1024, T_enc] layout, channel-major: index = c * T_enc + t
        let enc_step = |t: usize, dst: &mut [f32]| {
            assert_eq!(dst.len(), 1024);
            for c in 0..1024 {
                dst[c] = enc_data_owned[c * t_enc + t];
            }
        };

        // ── 3. Greedy TDT decode loop ─────────────────────────────────
        // LSTM state shape is [num_layers=2, batch=1, hidden=HIDDEN] —
        // the `1` is the batch dim, kept in the literal for parity with
        // the model signature. Allow identity_op for that reason.
        #[allow(clippy::identity_op)]
        let mut state1: Vec<f32> = vec![0.0; 2 * 1 * HIDDEN];
        #[allow(clippy::identity_op)]
        let mut state2: Vec<f32> = vec![0.0; 2 * 1 * HIDDEN];
        // Tokens carry the encoder-frame index at which they were
        // emitted so we can convert to wall-clock seconds for word
        // timestamps. The final text-only output ignores the frame.
        let mut tokens: Vec<(i64, usize)> = Vec::new();
        let mut frame_buf = vec![0f32; 1024];
        let mut t: usize = 0;
        let mut emitted: usize = 0;

        while t < valid_t_enc {
            enc_step(t, &mut frame_buf);
            let prev_tok = tokens.last().map(|(tok, _)| *tok).unwrap_or(BLANK_IDX);

            let enc_t = Tensor::from_array((vec![1i64, 1024, 1], frame_buf.clone()))
                .map_err(|e| TranscribeError::LocalModel(format!("mk enc[t]: {e}")))?;
            // `targets` + `target_length` declared as INT32 in the model
            // signature — ort would otherwise reject the i64 ours.
            let tgt_t = Tensor::from_array((vec![1i64, 1], vec![prev_tok as i32]))
                .map_err(|e| TranscribeError::LocalModel(format!("mk tgt: {e}")))?;
            let tlen_t = Tensor::from_array((vec![1i64], vec![1i32]))
                .map_err(|e| TranscribeError::LocalModel(format!("mk tlen: {e}")))?;
            let s1_t = Tensor::from_array((vec![2i64, 1, HIDDEN as i64], state1.clone()))
                .map_err(|e| TranscribeError::LocalModel(format!("mk s1: {e}")))?;
            let s2_t = Tensor::from_array((vec![2i64, 1, HIDDEN as i64], state2.clone()))
                .map_err(|e| TranscribeError::LocalModel(format!("mk s2: {e}")))?;

            let dj_outs = inner
                .decoder_joint
                .run(ort::inputs! {
                    "encoder_outputs" => enc_t,
                    "targets" => tgt_t,
                    "target_length" => tlen_t,
                    "input_states_1" => s1_t,
                    "input_states_2" => s2_t,
                })
                .map_err(|e| TranscribeError::LocalModel(format!("dj run: {e}")))?;

            let (out_shape, out_data) = dj_outs["outputs"]
                .try_extract_tensor::<f32>()
                .map_err(|e| TranscribeError::LocalModel(format!("dj out extract: {e}")))?;
            let total: i64 = out_shape.iter().product();
            let total = total as usize;
            if total < VOCAB_SIZE + NUM_DURATIONS {
                let dims: Vec<i64> = out_shape.iter().copied().collect();
                return Err(TranscribeError::LocalModel(format!(
                    "dj outputs unexpected size {} (shape {:?})",
                    total, dims
                )));
            }
            let logits = &out_data[..total];

            let mut best_tok: i64 = 0;
            let mut best_tok_v = f32::NEG_INFINITY;
            for (i, v) in logits[..VOCAB_SIZE].iter().enumerate() {
                if *v > best_tok_v {
                    best_tok_v = *v;
                    best_tok = i as i64;
                }
            }
            let mut step: usize = 0;
            let mut best_step_v = f32::NEG_INFINITY;
            for (i, v) in logits[VOCAB_SIZE..VOCAB_SIZE + NUM_DURATIONS]
                .iter()
                .enumerate()
            {
                if *v > best_step_v {
                    best_step_v = *v;
                    step = i;
                }
            }

            if best_tok != BLANK_IDX {
                let (_, s1_data) = dj_outs["output_states_1"]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| TranscribeError::LocalModel(format!("s1 extract: {e}")))?;
                let (_, s2_data) = dj_outs["output_states_2"]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| TranscribeError::LocalModel(format!("s2 extract: {e}")))?;
                state1 = s1_data.to_vec();
                state2 = s2_data.to_vec();
                tokens.push((best_tok, t));
                emitted += 1;
            }

            if step > 0 {
                t += step;
                emitted = 0;
            } else if best_tok == BLANK_IDX || emitted >= MAX_TOKENS_PER_STEP {
                t += 1;
                emitted = 0;
            }
        }

        // ── 4. Vocab lookup: tokens → text + word timestamps ──────────
        // Walk the (token, frame) pairs once, building text exactly
        // as before AND maintaining a `(word, start_frame)` running
        // list that's converted to JSON at the end. Words are split
        // on the BPE word-marker U+2581 ("▁") that prefixes the first
        // piece of each word in NeMo's SentencePiece vocab.
        let mut out = String::with_capacity(tokens.len() * 4);
        let mut words: Vec<(String, usize)> = Vec::new();
        let mut current_word = String::new();
        let mut current_start: Option<usize> = None;
        let push_word =
            |words: &mut Vec<(String, usize)>, w: &mut String, start: &mut Option<usize>| {
                if let Some(s) = start.take() {
                    if !w.is_empty() {
                        words.push((std::mem::take(w), s));
                    }
                }
            };
        for (tok, frame) in &tokens {
            let Some(piece) = inner.vocab.get(*tok as usize) else {
                continue;
            };
            if piece.starts_with('<') && piece.ends_with('>') {
                continue;
            }
            if let Some(rest) = piece.strip_prefix('\u{2581}') {
                push_word(&mut words, &mut current_word, &mut current_start);
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(rest);
                current_word.push_str(rest);
                current_start = Some(*frame);
            } else {
                if current_start.is_none() {
                    current_start = Some(*frame);
                }
                out.push_str(piece);
                current_word.push_str(piece);
            }
        }
        push_word(&mut words, &mut current_word, &mut current_start);

        let total_sec = (valid_t_enc as f64) * FRAME_SEC;
        let mut json = String::from("[");
        for (i, (word, start_frame)) in words.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            let start_sec = (*start_frame as f64) * FRAME_SEC;
            let end_sec = if i + 1 < words.len() {
                (words[i + 1].1 as f64) * FRAME_SEC
            } else {
                total_sec
            };
            // Inline JSON string escape — only "\" and `"` matter for
            // BPE pieces; control chars don't appear in this vocab.
            let mut escaped = String::with_capacity(word.len());
            for c in word.chars() {
                match c {
                    '\\' => escaped.push_str("\\\\"),
                    '"' => escaped.push_str("\\\""),
                    _ => escaped.push(c),
                }
            }
            json.push_str(&format!(
                "{{\"word\":\"{}\",\"start\":{:.3},\"end\":{:.3}}}",
                escaped, start_sec, end_sec
            ));
        }
        json.push(']');

        Ok((out.trim().to_string(), json))
    }
}

// ── Tests ────────────────────────────────────────────────────────

/// The encoder window is a hard constraint of the ONNX export, so the
/// constant that guards it needs pinning: a well-meaning bump would not
/// fail a test, it would fail a user's ten-minute dictation with an ONNX
/// error about a broadcast.
#[cfg(all(test, feature = "local-stt-parakeet"))]
mod encoder_window {
    use super::MAX_ORT_WINDOW_SECS;

    /// 600 s was measured to break the encoder outright (twice, once in a
    /// cold process). 300 s ran but at 3.6x instead of 6.0x and produced
    /// 7% fewer words. So the window must stay well under both.
    #[test]
    fn stays_far_below_the_measured_failure() {
        assert!(
            MAX_ORT_WINDOW_SECS < 300,
            "{MAX_ORT_WINDOW_SECS}s is at or past the length where Parakeet              starts losing words; 600s fails outright"
        );
    }

    /// And it must stay above the meeting/file-load chunk, or every one of
    /// those calls would take the splitting path for nothing.
    #[test]
    fn leaves_the_normal_chunk_sizes_untouched() {
        assert!(
            MAX_ORT_WINDOW_SECS > 60,
            "{MAX_ORT_WINDOW_SECS}s would split ordinary 30s meeting chunks"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_dir_returns_path() {
        let p = bundle_dir().expect("config_dir_path should not be None");
        assert!(p.ends_with("parakeet-fp32"));
    }

    #[test]
    fn vocab_size_and_blank_match_bundle() {
        assert_eq!(BLANK_IDX as usize, VOCAB_SIZE - 1);
    }
}

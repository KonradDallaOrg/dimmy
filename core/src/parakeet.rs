//! Parakeet TDT v3 FP32 local STT via ONNX Runtime.
//!
//! ## Status (2026-05-05)
//!
//! - [x] Bundle path resolution + presence check
//! - [x] Streaming download from HuggingFace with progress callback
//! - [x] `local-stt-parakeet` cargo feature + ort/ndarray deps
//! - [ ] Native ort inference (encoder + TDT greedy decoder)
//! - [ ] FFI exposure (`dimmy_parakeet_*`)
//! - [ ] UI integration (Settings → Voice input → Local backend = Parakeet)
//!
//! Scaffold work is complete + compiles. The inference body is left as
//! `Err(LocalModel("not yet implemented in Rust …"))` with a hand-written
//! design note below: porting the greedy TDT loop from
//! `onnx_asr.models.nemo.NemoConformerTdt` (Python) to `ort` 2.0.0-rc.10
//! is the next step. See `docs/dev/parakeet-local-stt.md` for the
//! architecture + porting plan + the live time-travel POC numbers from
//! the WSL Python reference (337-547 ms warm on CPU, 5 s chunks, 80 %
//! real-time margin per `tests/stt_benchmark/test_chunked.py`).
//!
//! ## Bundle layout (downloaded to `<config-dir>/parakeet-fp32/`)
//!
//! - `nemo128.onnx`              waveform → 128-bin mel features (~140 KB)
//! - `encoder-model.onnx`        + `.data` external weights (~2.4 GB)
//! - `decoder_joint-model.onnx`  TDT prediction net + joint (~73 MB)
//! - `vocab.txt`                 8193 tokens (BPE-style, `▁` = word start)
//!
//! ## Pipeline (target)
//!
//! ```text
//!  16 kHz f32 PCM (mono)
//!         │
//!  nemo128.onnx  ──▶  features[1, 128, T]      (T = frames at 10ms hop)
//!         │
//!  encoder-model.onnx ──▶ encoded[1, 1024, T'] (T' = T / 8 sub-sampling)
//!         │
//!  ┌── greedy TDT loop ──┐
//!  │  decoder LSTM state init zeros [2, 1, 640] x2
//!  │  prev_token = blank_idx (8192) at t=0
//!  │  while t < T':
//!  │    (logits[V+5], states') = decoder_joint(enc[t], prev_token, state)
//!  │    token = argmax(logits[..V])
//!  │    step  = argmax(logits[V..V+5])     // 0..=4 frames to skip (TDT-v3)
//!  │    if token != blank: emit + commit state
//!  │    if step > 0: t += step             // jump
//!  │    elif token == blank || emitted == 10: t += 1
//!  └─────────────────────┘
//!         │
//!  vocab lookup → text (concat with `▁` → space)
//! ```
//!
//! ## GPU
//!
//! `local-stt-parakeet-cuda` (Win) and `local-stt-parakeet-coreml` (Mac)
//! register the matching ort execution provider. Falls back to CPU when
//! the EP fails to initialise.

use std::path::PathBuf;

use crate::error::TranscribeError;

// ── Bundle paths ─────────────────────────────────────────────────

/// Where the Parakeet bundle files live on this machine.
/// `~/.config/dimmy/parakeet-fp32/` on Linux, `%APPDATA%\dimmy\…`
/// on Windows, `~/Library/Application Support/dimmy/…` on macOS.
pub fn bundle_dir() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("parakeet-fp32"))
}

pub const FILE_MEL: &str = "nemo128.onnx";
pub const FILE_ENCODER: &str = "encoder-model.onnx";
pub const FILE_ENCODER_DATA: &str = "encoder-model.onnx.data";
pub const FILE_DECODER_JOINT: &str = "decoder_joint-model.onnx";
pub const FILE_VOCAB: &str = "vocab.txt";

const HF_BASE: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

/// Approximate bundle size in MB. Used by the FFI / UI to render a
/// download progress bar that doesn't surprise.
pub const BUNDLE_SIZE_MB: u32 = 2500;

/// Vocabulary size (real tokens, excluding the blank). Matches the
/// `decoder_joint-model.onnx` output layout: first 8193 logits are
/// vocab + blank, last 5 are TDT duration buckets.
pub const VOCAB_SIZE: usize = 8193;

/// TDT v3 supports skip durations 0..=4 (5 buckets). Encoded as the
/// argmax over `logits[VOCAB_SIZE..]`.
pub const NUM_DURATIONS: usize = 5;

/// Token id used for "blank" in the TDT vocabulary. Last entry in
/// `vocab.txt`.
pub const BLANK_IDX: i64 = 8192;

/// Maximum tokens emitted at the same encoder frame before forcing
/// `t += 1`. Mirrors NemoConformerRnnt's default `max_tokens_per_step`.
pub const MAX_TOKENS_PER_STEP: usize = 10;

/// Cheap presence check for the FFI / UI to gate "Download Parakeet"
/// vs "Use Parakeet". Verifies all 5 required files are non-empty.
pub fn bundle_present() -> bool {
    let Some(dir) = bundle_dir() else { return false };
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

/// Streaming download of the FP32 bundle from HuggingFace. Reports
/// (bytes_done, bytes_total) via `progress`. Blocking — call from a
/// background task. Atomic per-file: each file is written to `.part`
/// then renamed.
pub fn download_bundle(
    mut progress: impl FnMut(u64, u64),
) -> Result<(), TranscribeError> {
    let dir = bundle_dir().ok_or_else(|| TranscribeError::LocalModel("config dir unknown".into()))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| TranscribeError::LocalModel(format!("create {:?}: {}", dir, e)))?;

    let files: &[&str] = &[
        FILE_MEL,
        FILE_VOCAB,
        FILE_ENCODER,
        FILE_ENCODER_DATA,
        FILE_DECODER_JOINT,
    ];

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("http client: {}", e)))?;

    // First pass: HEAD missing files to compute the grand total so the
    // progress bar fills smoothly across all 5 files.
    let mut grand_total: u64 = 0;
    let mut grand_done: u64 = 0;
    for name in files {
        let dest = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() > 0 {
                grand_done += meta.len();
                grand_total += meta.len();
                continue;
            }
        }
        let url = format!("{}/{}", HF_BASE, name);
        let r = client
            .head(&url)
            .send()
            .map_err(|e| TranscribeError::LocalModel(format!("HEAD {}: {}", url, e)))?;
        let len: u64 = r
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
            .and_then(|s: &str| s.parse::<u64>().ok())
            .unwrap_or(0);
        grand_total += len;
    }
    progress(grand_done, grand_total);

    // Second pass: stream each missing file to a `.part` then rename.
    for name in files {
        let dest = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&dest) {
            if meta.len() > 0 {
                continue;
            }
        }
        let url = format!("{}/{}", HF_BASE, name);
        let mut resp = client
            .get(&url)
            .send()
            .map_err(|e| TranscribeError::LocalModel(format!("GET {}: {}", url, e)))?
            .error_for_status()
            .map_err(|e| TranscribeError::LocalModel(format!("GET {}: {}", url, e)))?;

        let tmp = dest.with_extension("part");
        let mut out = std::fs::File::create(&tmp)
            .map_err(|e| TranscribeError::LocalModel(format!("create {:?}: {}", tmp, e)))?;
        let mut buf = [0u8; 1 << 16];
        loop {
            use std::io::{Read, Write};
            let n = resp
                .read(&mut buf)
                .map_err(|e| TranscribeError::LocalModel(format!("read {}: {}", url, e)))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| TranscribeError::LocalModel(format!("write {:?}: {}", tmp, e)))?;
            grand_done = grand_done.saturating_add(n as u64);
            progress(grand_done, grand_total);
        }
        std::fs::rename(&tmp, &dest)
            .map_err(|e| TranscribeError::LocalModel(format!("rename {:?}: {}", tmp, e)))?;
    }

    Ok(())
}

// ── Inference ────────────────────────────────────────────────────
//
// Stub. Will be filled in during the next session — full design + the
// onnx_asr Python reference loop are documented at the top of this
// file and in docs/dev/parakeet-local-stt.md. The Cargo dependencies
// (`ort`, `ndarray`) are wired so a follow-up commit can write the
// bodies without touching the dependency tree.

#[cfg(not(feature = "local-stt-parakeet"))]
pub fn transcribe(_pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    Err(TranscribeError::LocalModel(
        "parakeet inference requires the `local-stt-parakeet` cargo feature".into(),
    ))
}

#[cfg(feature = "local-stt-parakeet")]
pub fn transcribe(pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    assert!(
        pcm_16k.iter().all(|s| s.is_finite()),
        "parakeet::transcribe: pcm_16k must be all-finite"
    );
    if pcm_16k.is_empty() {
        return Ok(String::new());
    }
    if !bundle_present() {
        return Err(TranscribeError::LocalModel(
            "parakeet bundle not downloaded — call parakeet::download_bundle() first".into(),
        ));
    }
    Err(TranscribeError::LocalModel(
        "parakeet greedy TDT decoder pending native impl — see \
         docs/dev/parakeet-local-stt.md for the porting plan from \
         the onnx_asr Python reference"
            .into(),
    ))
}

// ── Tests (always compiled — exercise the path resolution + presence
// check even without the feature flag) ────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_dir_returns_path() {
        // On any host with a config dir, bundle_dir should resolve.
        let p = bundle_dir().expect("config_dir_path should not be None");
        assert!(p.ends_with("parakeet-fp32"));
    }

    #[test]
    fn vocab_size_and_blank_match_bundle() {
        // BLANK_IDX is the last entry in an 8193-token vocab → idx 8192.
        assert_eq!(BLANK_IDX as usize, VOCAB_SIZE - 1);
    }
}

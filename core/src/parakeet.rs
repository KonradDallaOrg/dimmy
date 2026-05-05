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
const HIDDEN: usize = 640;

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

#[cfg(not(feature = "local-stt-parakeet"))]
pub fn transcribe(_pcm_16k: &[f32]) -> Result<String, TranscribeError> {
    Err(TranscribeError::LocalModel(
        "parakeet inference requires the `local-stt-parakeet` cargo feature".into(),
    ))
}

#[cfg(feature = "local-stt-parakeet")]
pub use inference::transcribe;

#[cfg(feature = "local-stt-parakeet")]
mod inference {
    use super::*;
    use ort::session::{builder::GraphOptimizationLevel, Session};
    use ort::value::Tensor;
    use std::sync::OnceLock;

    static MODEL: OnceLock<std::sync::Mutex<Option<Inner>>> = OnceLock::new();

    struct Inner {
        mel: Session,
        encoder: Session,
        decoder_joint: Session,
        vocab: Vec<String>,
    }

    fn lock() -> &'static std::sync::Mutex<Option<Inner>> {
        MODEL.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn build_session(path: &std::path::Path) -> Result<Session, TranscribeError> {
        Session::builder()
            .map_err(|e| TranscribeError::LocalModel(format!("ort builder {:?}: {e}", path)))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| TranscribeError::LocalModel(format!("ort opt level: {e}")))?
            .commit_from_file(path)
            .map_err(|e| TranscribeError::LocalModel(format!("ort load {:?}: {e}", path)))
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
        let mut state1: Vec<f32> = vec![0.0; 2 * 1 * HIDDEN];
        let mut state2: Vec<f32> = vec![0.0; 2 * 1 * HIDDEN];
        let mut tokens: Vec<i64> = Vec::new();
        let mut frame_buf = vec![0f32; 1024];
        let mut t: usize = 0;
        let mut emitted: usize = 0;

        while t < valid_t_enc {
            enc_step(t, &mut frame_buf);
            let prev_tok = *tokens.last().unwrap_or(&BLANK_IDX);

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
                tokens.push(best_tok);
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

        // ── 4. Vocab lookup: tokens → text ────────────────────────────
        let mut out = String::with_capacity(tokens.len() * 4);
        for tok in tokens {
            if let Some(piece) = inner.vocab.get(tok as usize) {
                if let Some(rest) = piece.strip_prefix('\u{2581}') {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(rest);
                } else if piece.starts_with('<') && piece.ends_with('>') {
                    continue;
                } else {
                    out.push_str(piece);
                }
            }
        }
        Ok(out.trim().to_string())
    }
}

// ── Tests ────────────────────────────────────────────────────────

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

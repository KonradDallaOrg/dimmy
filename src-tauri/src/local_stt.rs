//! Local speech-to-text via whisper.cpp (through the whisper-rs crate).
//!
//! Provides model discovery, downloading from HuggingFace, and local
//! transcription gated behind the `local-stt` Cargo feature.

use std::path::{Path, PathBuf};

use crate::error::TranscribeError;

// ── Model catalogue ───────────────────────────────────────────────

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
pub const DEFAULT_MODEL: &str = "ggml-base-q8_0.bin";

pub struct LocalModel {
    pub name: &'static str,
    pub filename: &'static str,
    pub size_mb: u32,
    pub description: &'static str,
}

pub const AVAILABLE_MODELS: &[LocalModel] = &[
    LocalModel {
        name: "Tiny",
        filename: "ggml-tiny-q8_0.bin",
        size_mb: 42,
        description: "Fastest, lower accuracy",
    },
    LocalModel {
        name: "Base",
        filename: "ggml-base-q8_0.bin",
        size_mb: 78,
        description: "Good balance of speed and accuracy",
    },
    LocalModel {
        name: "Small",
        filename: "ggml-small-q5_1.bin",
        size_mb: 181,
        description: "High accuracy, slower",
    },
    LocalModel {
        name: "Medium",
        filename: "ggml-medium-q5_0.bin",
        size_mb: 514,
        description: "Very high accuracy, requires 2GB+ RAM",
    },
];

// ── Model directory helpers ───────────────────────────────────────

/// Returns `<data_dir>/dimmy/models` (e.g. `~/Library/Application Support/dimmy/models`).
pub fn model_directory() -> PathBuf {
    let base = dirs::data_dir().expect("data_dir must be available on all supported platforms");
    base.join("dimmy").join("models")
}

/// Check whether a given model file already exists on disk.
pub fn model_exists(filename: &str) -> bool {
    assert!(!filename.is_empty(), "model filename must not be empty");
    model_path(filename).is_file()
}

/// Full path to a model file inside the model directory.
pub fn model_path(filename: &str) -> PathBuf {
    assert!(!filename.is_empty(), "model filename must not be empty");
    model_directory().join(filename)
}

// ── Model download ────────────────────────────────────────────────

/// Download a model from HuggingFace to the local model directory.
///
/// - Skips the download if the model file already exists.
/// - Writes to a `.part` temp file and renames on completion (atomic).
/// - Calls `on_progress(bytes_downloaded, total_bytes)` during download.
///   `total_bytes` is `0` if the server didn't send `Content-Length`.
pub async fn download_model<F>(filename: &str, on_progress: F) -> Result<PathBuf, TranscribeError>
where
    F: Fn(u64, u64),
{
    assert!(!filename.is_empty(), "model filename must not be empty");
    assert!(
        filename.ends_with(".bin"),
        "model filename must end with .bin"
    );

    let dest = model_path(filename);
    if dest.is_file() {
        crate::log(&format!(
            "[LocalSTT] Model already exists: {}",
            dest.display()
        ));
        return Ok(dest);
    }

    let dir = model_directory();
    std::fs::create_dir_all(&dir).map_err(|e| {
        TranscribeError::LocalModel(format!(
            "failed to create model dir {}: {}",
            dir.display(),
            e
        ))
    })?;

    let url = format!("{}/{}", MODEL_BASE_URL, filename);
    crate::log(&format!("[LocalSTT] Downloading {} ...", url));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("HTTP client error: {}", e)))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| TranscribeError::LocalModel(format!("download request failed: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let body_trunc = if body.len() > 200 {
            &body[..200]
        } else {
            &body
        };
        return Err(TranscribeError::LocalModel(format!(
            "download failed: HTTP {} — {}",
            status, body_trunc
        )));
    }

    let total_bytes = resp.content_length().unwrap_or(0);
    let part_path = dir.join(format!("{}.part", filename));

    let mut file = std::fs::File::create(&part_path).map_err(|e| {
        TranscribeError::LocalModel(format!(
            "cannot create temp file {}: {}",
            part_path.display(),
            e
        ))
    })?;

    let mut downloaded: u64 = 0;

    // Stream the response body using chunk() (no `stream` feature needed).
    use std::io::Write;
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| TranscribeError::LocalModel(format!("download stream error: {}", e)))?
    {
        file.write_all(&chunk)
            .map_err(|e| TranscribeError::LocalModel(format!("write error: {}", e)))?;
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total_bytes);
    }

    drop(file); // flush & close before rename

    // Atomic rename: .part → final
    std::fs::rename(&part_path, &dest).map_err(|e| {
        TranscribeError::LocalModel(format!(
            "rename {} → {} failed: {}",
            part_path.display(),
            dest.display(),
            e
        ))
    })?;

    crate::log(&format!(
        "[LocalSTT] Download complete: {} ({} bytes)",
        dest.display(),
        downloaded
    ));
    assert!(dest.is_file(), "model file must exist after download");

    Ok(dest)
}

// ── Local transcription (feature-gated) ───────────────────────────

#[cfg(feature = "local-stt")]
pub fn transcribe_local(
    model_file: &Path,
    samples: &[f32], // 16 kHz mono
    language: &str,
) -> Result<String, TranscribeError> {
    use std::ffi::c_int;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    // ── Precondition assertions ──────────────────────────────────
    assert!(!samples.is_empty(), "samples must not be empty");
    assert!(
        samples.iter().all(|s| s.is_finite()),
        "all samples must be finite (no NaN/Inf)"
    );

    if !model_file.is_file() {
        return Err(TranscribeError::LocalModel(format!(
            "model file not found: {}",
            model_file.display()
        )));
    }

    // ── Create whisper context ───────────────────────────────────
    let ctx_params = WhisperContextParameters::default();
    let ctx = WhisperContext::new_with_params(model_file, ctx_params)
        .map_err(|e| TranscribeError::LocalModel(format!("failed to load model: {}", e)))?;

    let mut state = ctx
        .create_state()
        .map_err(|e| TranscribeError::LocalModel(format!("failed to create state: {}", e)))?;

    // ── Configure parameters ─────────────────────────────────────
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

    // Language
    if !language.is_empty() {
        params.set_language(Some(language));
    } else {
        params.set_detect_language(true);
    }

    // Threading: min(4, available_parallelism)
    let n_threads: c_int = std::thread::available_parallelism()
        .map(|n| n.get().min(4) as c_int)
        .unwrap_or(2);
    params.set_n_threads(n_threads);

    // Suppress all print output
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    // Short audio optimization: single segment for < 30s
    const SAMPLES_30S: usize = 30 * 16_000;
    if samples.len() < SAMPLES_30S {
        params.set_single_segment(true);
    }

    // ── Run inference ────────────────────────────────────────────
    state
        .full(params, samples)
        .map_err(|e| TranscribeError::LocalModel(format!("whisper inference failed: {}", e)))?;

    // ── Collect segment texts ────────────────────────────────────
    let n_segments = state.full_n_segments();

    let mut text = String::new();
    for i in 0..n_segments {
        let segment = state
            .get_segment(i)
            .ok_or_else(|| TranscribeError::LocalModel(format!("segment {} out of bounds", i)))?;
        let seg_text = segment.to_str().map_err(|e| {
            TranscribeError::LocalModel(format!("failed to read segment {}: {}", i, e))
        })?;
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(seg_text.trim());
    }

    let result = text.trim().to_string();

    // ── Postcondition ────────────────────────────────────────────
    if result.is_empty() {
        return Err(TranscribeError::Empty);
    }

    Ok(result)
}

/// Stub when `local-stt` feature is disabled.
#[cfg(not(feature = "local-stt"))]
pub fn transcribe_local(
    _model_file: &Path,
    _samples: &[f32],
    _language: &str,
) -> Result<String, TranscribeError> {
    Err(TranscribeError::LocalModel(
        "local STT not available: compile with `local-stt` feature".to_string(),
    ))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_directory_is_valid() {
        let dir = model_directory();
        let s = dir.to_str().unwrap();
        assert!(s.contains("dimmy"), "path should contain 'dimmy': {}", s);
        assert!(s.contains("models"), "path should contain 'models': {}", s);
    }

    #[test]
    fn model_exists_false_for_missing() {
        assert!(!model_exists("nonexistent-model.bin"));
    }

    #[test]
    fn available_models_are_valid() {
        for model in AVAILABLE_MODELS {
            assert!(!model.name.is_empty(), "model name must not be empty");
            assert!(
                model.filename.ends_with(".bin"),
                "model filename must end with .bin: {}",
                model.filename
            );
            assert!(
                model.size_mb > 0,
                "model size must be positive: {}",
                model.name
            );
            assert!(
                !model.description.is_empty(),
                "model description must not be empty: {}",
                model.name
            );
        }
    }

    #[test]
    fn default_model_is_in_available_list() {
        assert!(
            AVAILABLE_MODELS.iter().any(|m| m.filename == DEFAULT_MODEL),
            "DEFAULT_MODEL '{}' must appear in AVAILABLE_MODELS",
            DEFAULT_MODEL
        );
    }

    #[test]
    fn model_path_contains_filename() {
        let p = model_path("ggml-tiny-q8_0.bin");
        assert!(
            p.ends_with("ggml-tiny-q8_0.bin"),
            "model_path should end with filename: {}",
            p.display()
        );
        assert!(
            p.to_str().unwrap().contains("dimmy"),
            "model_path should be under dimmy dir"
        );
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn transcribe_local_rejects_missing_model() {
        let samples = vec![0.0f32; 16000];
        let result = transcribe_local(Path::new("/nonexistent/model.bin"), &samples, "en");
        assert!(result.is_err());
        if let Err(TranscribeError::LocalModel(msg)) = result {
            assert!(
                msg.contains("not found"),
                "error should mention 'not found': {}",
                msg
            );
        } else {
            panic!("Expected LocalModel error");
        }
    }

    #[cfg(not(feature = "local-stt"))]
    #[test]
    fn transcribe_local_stub_returns_error() {
        let samples = vec![0.0f32; 16000];
        let result = transcribe_local(Path::new("/any/model.bin"), &samples, "en");
        assert!(result.is_err());
        if let Err(TranscribeError::LocalModel(msg)) = result {
            assert!(
                msg.contains("not available"),
                "stub error should mention 'not available': {}",
                msg
            );
        } else {
            panic!("Expected LocalModel error from stub");
        }
    }
}

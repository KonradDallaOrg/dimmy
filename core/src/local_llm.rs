//! Local LLM text enhancement via llama.cpp (through the llama-cpp-2 crate).
//!
//! Provides model discovery, downloading from HuggingFace, and local text
//! generation gated behind the `local-llm` Cargo feature.
//! Mirrors the architecture of `local_stt.rs` for consistency.

use std::path::{Path, PathBuf};

use crate::error::LlmError;

// ── Model catalogue ───────────────────────────────────────────────

const LLM_MODEL_BASE_URL: &str = "https://huggingface.co/unsloth/gemma-4-E2B-it-GGUF/resolve/main";
pub const DEFAULT_LLM_MODEL: &str = "gemma-4-E2B-it-Q4_K_M.gguf";

pub struct LlmModel {
    pub name: &'static str,
    pub filename: &'static str,
    pub size_mb: u32,
    pub description: &'static str,
    /// Custom download URL. When `None`, uses `LLM_MODEL_BASE_URL/filename`.
    pub url: Option<&'static str>,
}

pub const AVAILABLE_LLM_MODELS: &[LlmModel] = &[
    LlmModel {
        name: "Gemma 4 E2B Q4",
        filename: "gemma-4-E2B-it-Q4_K_M.gguf",
        size_mb: 3300,
        description: "Best quality for 4GB+ VRAM, 140+ languages",
        url: None,
    },
    LlmModel {
        name: "Phi-4 Mini Q4",
        filename: "phi-4-mini-instruct-q4_k_m.gguf",
        size_mb: 2500,
        description: "Fast, high quality, multilingual (3.8B params)",
        url: Some("https://huggingface.co/matrixportalx/Phi-4-mini-instruct-Q4_K_M-GGUF/resolve/main/phi-4-mini-instruct-q4_k_m.gguf"),
    },
];

// ── Model directory helpers ──────────────────────────────────────

/// Returns `<data_dir>/dimmy/llm-models` (separate from whisper models).
pub fn llm_model_directory() -> PathBuf {
    let base = dirs::data_dir().expect("data_dir must be available on all supported platforms");
    base.join("dimmy").join("llm-models")
}

/// Check whether a given LLM model file already exists on disk.
pub fn model_exists(filename: &str) -> bool {
    assert!(!filename.is_empty(), "LLM model filename must not be empty");
    model_path(filename).is_file()
}

/// Full path to an LLM model file inside the LLM model directory.
pub fn model_path(filename: &str) -> PathBuf {
    assert!(!filename.is_empty(), "LLM model filename must not be empty");
    llm_model_directory().join(filename)
}

// ── Model download ───────────────────────────────────────────────

/// Download an LLM model from HuggingFace to the local LLM model directory.
///
/// - Skips the download if the model file already exists.
/// - Writes to a `.part` temp file and renames on completion (atomic).
/// - Calls `on_progress(bytes_downloaded, total_bytes)` during download.
pub async fn download_model<F>(filename: &str, on_progress: F) -> Result<PathBuf, LlmError>
where
    F: Fn(u64, u64),
{
    assert!(!filename.is_empty(), "LLM model filename must not be empty");
    assert!(
        filename.ends_with(".gguf"),
        "LLM model filename must end with .gguf"
    );

    let dest = model_path(filename);
    if dest.is_file() {
        crate::log(&format!(
            "[LocalLLM] Model already exists: {}",
            dest.display()
        ));
        return Ok(dest);
    }

    let dir = llm_model_directory();
    std::fs::create_dir_all(&dir).map_err(|e| {
        LlmError::LocalModel(format!(
            "failed to create LLM model dir {}: {}",
            dir.display(),
            e
        ))
    })?;

    // Use per-model custom URL if available, otherwise default base URL.
    let url = AVAILABLE_LLM_MODELS
        .iter()
        .find(|m| m.filename == filename)
        .and_then(|m| m.url)
        .map(|u| u.to_string())
        .unwrap_or_else(|| format!("{}/{}", LLM_MODEL_BASE_URL, filename));
    crate::log(&format!("[LocalLLM] Downloading {} ...", url));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800)) // 30 min for large models
        .build()
        .map_err(|e| LlmError::LocalModel(format!("HTTP client error: {}", e)))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LlmError::LocalModel(format!("download request failed: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let body_trunc = if body.len() > 200 {
            &body[..200]
        } else {
            &body
        };
        return Err(LlmError::LocalModel(format!(
            "download failed: HTTP {} — {}",
            status, body_trunc
        )));
    }

    let total_bytes = resp.content_length().unwrap_or(0);
    let part_path = dir.join(format!("{}.part", filename));

    let mut file = std::fs::File::create(&part_path).map_err(|e| {
        LlmError::LocalModel(format!(
            "cannot create temp file {}: {}",
            part_path.display(),
            e
        ))
    })?;

    let mut downloaded: u64 = 0;

    use std::io::Write;
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| LlmError::LocalModel(format!("download stream error: {}", e)))?
    {
        file.write_all(&chunk)
            .map_err(|e| LlmError::LocalModel(format!("write error: {}", e)))?;
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total_bytes);
    }

    drop(file); // flush & close before rename

    // Atomic rename: .part → final
    std::fs::rename(&part_path, &dest).map_err(|e| {
        LlmError::LocalModel(format!(
            "rename {} → {} failed: {}",
            part_path.display(),
            dest.display(),
            e
        ))
    })?;

    crate::log(&format!(
        "[LocalLLM] Download complete: {} ({} bytes)",
        dest.display(),
        downloaded
    ));
    assert!(dest.is_file(), "LLM model file must exist after download");

    Ok(dest)
}

// ── Prompt formatting ────────────────────────────────────────────

/// Strip all special/control tags from LLM output.
/// Uses regex to catch: <think>...</think>, <start_of_turn>, <|think|>, etc.
fn strip_special_tags(text: &str) -> String {
    // 1. Remove thinking blocks with their content: <think>...</think> and <|think|>...<|/think|>
    let re_think = regex::Regex::new(r"(?s)<think>.*?</think>|<\|think\|>.*?<\|/think\|>")
        .expect("think regex must compile");
    let text = re_think.replace_all(text, "");

    // 2. Remove remaining standalone special tags
    let re = regex::Regex::new(
        r"</?(?:think|start_of_turn|end_of_turn|pad|s)>|<\|/?(?:think|end|endoftext|assistant|user|system)\|?>"
    ).expect("strip_special_tags regex must compile");

    let cleaned = re.replace_all(&text, "");

    // Clean up whitespace
    cleaned
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Build a LOCAL system prompt optimized for small models (< 10B params).
///
/// Unlike the cloud prompt (long preamble + 7 rules), this uses ultra-short,
/// direct instructions that small models can actually follow.
/// The cloud preamble is too complex for E2B — the model ignores most rules.
pub fn build_local_system_prompt(
    style: crate::llm::LlmStyle,
    tone: crate::llm::LlmTone,
    custom_prompt: &str,
    translate_to: &str,
) -> String {
    use crate::llm::{LlmStyle, LlmTone};

    if style.is_off() && (translate_to.is_empty() || translate_to == "none") {
        return String::new();
    }

    // Ultra-short style instructions — small models need direct, simple commands
    let style_instr = match style {
        LlmStyle::Off => "",
        LlmStyle::Correct => "Fix grammar, punctuation, and spelling. Keep the same words.",
        LlmStyle::Summarize => "Summarize in 1-2 sentences. Keep key facts only.",
        LlmStyle::Elaborate => "Expand with more detail. Add context and explanations.",
        LlmStyle::Comprehensible => "Rewrite to be clearer and easier to understand.",
        LlmStyle::Professional => "Rewrite in formal, professional business tone.",
        LlmStyle::Prompt => "Rewrite as a clear, structured AI prompt. Fix grammar, organize logically.",
        LlmStyle::Genz => "Rewrite in Gen-Z slang. Use 'no cap', 'fr fr', 'slay', 'lowkey', 'bussin'.",
        LlmStyle::Boomer => "Rewrite in a formal, old-fashioned, overly polite tone.",
        LlmStyle::Emoji => "Rewrite with many emojis. Add 2-4 emojis per sentence.",
        LlmStyle::Acronyms => "Add common acronyms and abbreviations where possible.",
        LlmStyle::Imbruttito => "Riscrivi in stile milanese imbruttito. Usa 'performare', 'deliverare', 'taggare', gergo business italiano-inglese.",
        LlmStyle::Custom => custom_prompt,
    };

    let tone_instr = match tone {
        LlmTone::None => "",
        LlmTone::Formal => "Use formal vocabulary.",
        LlmTone::Friendly => "Use a warm, friendly tone.",
        LlmTone::Concise => "Be very brief.",
        LlmTone::Academic => "Use academic, scholarly language.",
    };

    let translate_instr = if !translate_to.is_empty() && translate_to != "none" {
        format!("Translate the output to {}.", translate_to)
    } else {
        String::new()
    };

    // Compose — keep it SHORT. Every extra word confuses small models.
    let mut parts: Vec<&str> = Vec::new();
    if !style_instr.is_empty() {
        parts.push(style_instr);
    }
    if !tone_instr.is_empty() {
        parts.push(tone_instr);
    }

    let mut prompt = parts.join(" ");

    if !translate_instr.is_empty() {
        prompt = format!("{} {}", prompt, translate_instr);
    }

    prompt
}

/// Build the full prompt for local LLM inference.
/// Uses Gemma turn format with thinking mode disabled via `<|/think|>`.
pub fn build_local_prompt(system_prompt: &str, user_text: &str) -> String {
    assert!(
        !user_text.is_empty(),
        "user text must not be empty for local LLM"
    );

    // Detect input language for reinforcement — but skip if translating
    let translating = system_prompt.contains("Translate");
    let lang_hint = if translating {
        "" // Don't fight the translation instruction
    } else if user_text.contains('è')
        || user_text.contains('ò')
        || user_text.contains('à')
        || user_text.contains('ù')
        || user_text.contains("cioè")
        || user_text.contains("perché")
    {
        "Rispondi SOLO in italiano."
    } else {
        "IMPORTANT: Respond in the EXACT SAME language as the input text. Do NOT translate."
    };

    let prompt = format!(
        "<start_of_turn>user\n\
         {system} {lang} Output ONLY the result, nothing else.\n\n\
         {text}\n\
         <end_of_turn>\n<start_of_turn>model\n<|/think|>\n",
        system = system_prompt,
        lang = lang_hint,
        text = user_text
    );

    // Negative space: ensure we never trigger thinking mode
    assert!(
        !prompt.contains("<think>"),
        "prompt must not contain thinking tags"
    );
    assert!(
        prompt.len() < 15_000,
        "prompt is unreasonably long: {} chars",
        prompt.len()
    );

    prompt
}

// ── LLM inference cache ─────────────────────────────────────────
//
// Loading a GGUF model takes 2-10 seconds depending on size.
// We cache the LlamaModel globally and reuse it across calls.
// LlamaContext is created per-call (cheap, ~1ms) because it's !Send+!Sync.

#[cfg(feature = "local-llm")]
mod llm_cache {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use llama_cpp_4::context::params::LlamaContextParams;
    use llama_cpp_4::llama_backend::LlamaBackend;
    use llama_cpp_4::llama_batch::LlamaBatch;
    use llama_cpp_4::model::params::LlamaModelParams;
    use llama_cpp_4::model::{AddBos, LlamaModel};
    use llama_cpp_4::sampling::LlamaSampler;

    struct CachedLlmModel {
        model: LlamaModel,
        backend: LlamaBackend,
        model_path: PathBuf,
    }

    // LlamaModel is Send+Sync. LlamaBackend is Send+Sync.
    // We protect access with a Mutex.
    unsafe impl Send for CachedLlmModel {}

    static CACHE: Mutex<Option<CachedLlmModel>> = Mutex::new(None);

    /// Load model if needed, run text generation, return result. Lock held during load only.
    pub fn generate(
        model_path: &std::path::Path,
        system_prompt: &str,
        user_text: &str,
        max_tokens: u32,
    ) -> Result<String, crate::error::LlmError> {
        let mut guard = CACHE.lock().map_err(|e| {
            crate::error::LlmError::LocalModel(format!("LLM cache lock poisoned: {}", e))
        })?;

        // ── Load or reuse cached model ──────────────────────────
        let needs_reload = match &*guard {
            Some(cached) => cached.model_path != model_path,
            None => true,
        };

        if needs_reload {
            crate::log(&format!(
                "[LocalLLM] Loading model into cache: {}",
                model_path.display()
            ));

            let backend = LlamaBackend::init()
                .map_err(|e| crate::error::LlmError::LocalModel(format!("backend init: {}", e)))?;

            let gpu_device = crate::local_stt::preferred_gpu_device();
            crate::log(&format!("[LocalLLM] Using GPU device: {}", gpu_device));

            let model_params = LlamaModelParams::default()
                .with_n_gpu_layers(99)
                .with_main_gpu(gpu_device);

            let model =
                LlamaModel::load_from_file(&backend, model_path, &model_params).map_err(|e| {
                    crate::error::LlmError::LocalModel(format!("failed to load LLM model: {}", e))
                })?;

            *guard = Some(CachedLlmModel {
                model,
                backend,
                model_path: model_path.to_path_buf(),
            });
            crate::log("[LocalLLM] Model cached successfully");
        } else {
            crate::log("[LocalLLM] Using cached LLM model");
        }

        let cached = guard
            .as_ref()
            .expect("LLM cache must be populated after load");

        // ── Build prompt and tokenize ───────────────────────────
        let full_prompt = super::build_local_prompt(system_prompt, user_text);

        let tokens = cached
            .model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| {
                crate::error::LlmError::LocalModel(format!("tokenization failed: {}", e))
            })?;

        assert!(
            !tokens.is_empty(),
            "tokenized prompt must produce at least one token"
        );

        // ── Create context (per-call, cheap) ────────────────────
        let ctx_size = std::num::NonZeroU32::new((tokens.len() as u32 + max_tokens + 64).max(512))
            .expect("context size must be > 0");
        let ctx_params = LlamaContextParams::default().with_n_ctx(Some(ctx_size));

        let mut ctx = cached
            .model
            .new_context(&cached.backend, ctx_params)
            .map_err(|e| {
                crate::error::LlmError::LocalModel(format!("context creation failed: {}", e))
            })?;

        // ── Feed prompt tokens ──────────────────────────────────
        let mut batch = LlamaBatch::new(tokens.len(), 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| {
                    crate::error::LlmError::LocalModel(format!("batch add failed: {}", e))
                })?;
        }

        ctx.decode(&mut batch).map_err(|e| {
            crate::error::LlmError::LocalModel(format!("prompt decode failed: {}", e))
        })?;

        // ── Generate tokens ─────────────────────────────────────
        let mut sampler =
            LlamaSampler::chain_simple([LlamaSampler::temp(0.3), LlamaSampler::greedy()]);

        let eos = cached.model.token_eos();
        let mut output = String::new();
        let mut n_generated: u32 = 0;
        let mut next_pos = tokens.len() as i32;

        loop {
            if n_generated >= max_tokens {
                break;
            }

            let new_token = sampler.sample(&ctx, -1);
            sampler.accept(new_token);

            if new_token == eos {
                break;
            }

            let piece = cached
                .model
                .token_to_str(new_token, llama_cpp_4::model::Special::Tokenize)
                .unwrap_or_default();

            // Stop on turn markers (model trying to generate next turn)
            if piece.contains("<end_of_turn>")
                || piece.contains("<start_of_turn>")
                || piece.contains("</s>")
                || piece.contains("<|endoftext|>")
            {
                break;
            }

            // Skip any special/control tokens — they start with < and end with >
            // This catches <|think|>, <|/think|>, <pad>, etc.
            let trimmed = piece.trim();
            if trimmed.starts_with('<') && trimmed.ends_with('>') {
                n_generated += 1;
                continue;
            }

            output.push_str(&piece);
            n_generated += 1;

            // Prepare next decode
            batch.clear();
            batch.add(new_token, next_pos, &[0], true).map_err(|e| {
                crate::error::LlmError::LocalModel(format!("batch add failed: {}", e))
            })?;
            next_pos += 1;

            ctx.decode(&mut batch)
                .map_err(|e| crate::error::LlmError::LocalModel(format!("decode failed: {}", e)))?;
        }

        crate::log(&format!(
            "[LocalLLM] Raw output ({} tokens): {:?}",
            n_generated,
            &output[..output.len().min(200)]
        ));

        // Post-process: strip ALL remaining special tags from output
        let cleaned = super::strip_special_tags(&output);

        crate::log(&format!(
            "[LocalLLM] Cleaned output ({} chars): {:?}",
            cleaned.len(),
            &cleaned[..cleaned.len().min(200)]
        ));

        Ok(cleaned)
    }

    /// Clear the cached LLM model (e.g. on shutdown or model change).
    pub fn clear() {
        if let Ok(mut guard) = CACHE.lock() {
            if guard.is_some() {
                crate::log("[LocalLLM] Clearing LLM model cache");
            }
            *guard = None;
        }
    }
}

/// Clear the LLM model cache (call on shutdown or model change).
#[cfg(feature = "local-llm")]
pub fn clear_llm_cache() {
    llm_cache::clear();
}

#[cfg(not(feature = "local-llm"))]
pub fn clear_llm_cache() {}

// ── Local LLM processing (feature-gated) ─────────────────────────

/// Process text with a local LLM model for enhancement.
#[cfg(feature = "local-llm")]
pub fn process_text_local(
    model_file: &Path,
    text: &str,
    style: crate::llm::LlmStyle,
    tone: crate::llm::LlmTone,
    custom_prompt: &str,
    translate_to: &str,
) -> Result<String, LlmError> {
    // ── Precondition assertions ─────────────────────────────────
    assert!(!text.is_empty(), "text must not be empty for local LLM");

    let system_prompt = build_local_system_prompt(style, tone, custom_prompt, translate_to);
    if system_prompt.is_empty() {
        return Ok(text.to_string());
    }

    if !model_file.is_file() {
        return Err(LlmError::LocalModel(format!(
            "LLM model file not found: {}",
            model_file.display()
        )));
    }

    // Estimate max output tokens: ~2x input length, min 256, max 1024
    let estimated_tokens = (text.split_whitespace().count() as u32 * 3).clamp(256, 1024);

    let result = llm_cache::generate(model_file, &system_prompt, text, estimated_tokens)?;

    if result.is_empty() {
        crate::log("[LocalLLM] WARNING: empty generation, returning original text");
        return Ok(text.to_string());
    }

    Ok(result)
}

/// Stub when `local-llm` feature is disabled.
#[cfg(not(feature = "local-llm"))]
pub fn process_text_local(
    _model_file: &Path,
    _text: &str,
    _style: crate::llm::LlmStyle,
    _tone: crate::llm::LlmTone,
    _custom_prompt: &str,
    _translate_to: &str,
) -> Result<String, LlmError> {
    Err(LlmError::LocalModel(
        "local LLM not available: compile with `local-llm` feature".to_string(),
    ))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Model catalogue tests ───────────────────────────────────

    #[test]
    fn llm_model_directory_is_valid() {
        let dir = llm_model_directory();
        let s = dir.to_str().unwrap();
        assert!(s.contains("dimmy"), "path should contain 'dimmy': {}", s);
        assert!(
            s.contains("llm-models"),
            "path should contain 'llm-models': {}",
            s
        );
    }

    #[test]
    fn llm_model_exists_false_for_missing() {
        assert!(!model_exists("nonexistent-model.gguf"));
    }

    #[test]
    fn llm_available_models_are_valid() {
        for model in AVAILABLE_LLM_MODELS {
            assert!(!model.name.is_empty(), "model name must not be empty");
            assert!(
                model.filename.ends_with(".gguf"),
                "model filename must end with .gguf: {}",
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
            if let Some(url) = model.url {
                assert!(
                    url.starts_with("https://"),
                    "custom URL must be HTTPS: {} ({})",
                    url,
                    model.name
                );
                assert!(
                    url.contains(model.filename),
                    "custom URL must contain filename: {} ({})",
                    url,
                    model.name
                );
            }
        }
    }

    #[test]
    fn llm_no_duplicate_filenames() {
        let mut seen = std::collections::HashSet::new();
        for model in AVAILABLE_LLM_MODELS {
            assert!(
                seen.insert(model.filename),
                "duplicate filename in AVAILABLE_LLM_MODELS: {}",
                model.filename
            );
        }
    }

    #[test]
    fn llm_default_model_in_list() {
        assert!(
            AVAILABLE_LLM_MODELS
                .iter()
                .any(|m| m.filename == DEFAULT_LLM_MODEL),
            "DEFAULT_LLM_MODEL '{}' must appear in AVAILABLE_LLM_MODELS",
            DEFAULT_LLM_MODEL
        );
    }

    #[test]
    fn llm_model_path_contains_filename() {
        let p = model_path("test-model.gguf");
        assert!(
            p.ends_with("test-model.gguf"),
            "model_path should end with filename: {}",
            p.display()
        );
        assert!(
            p.to_str().unwrap().contains("dimmy"),
            "model_path should be under dimmy dir"
        );
    }

    #[test]
    fn llm_model_path_uses_llm_models_dir() {
        let p = model_path("test-model.gguf");
        let s = p.to_str().unwrap();
        assert!(
            s.contains("llm-models"),
            "LLM model path must use 'llm-models' dir, not 'models': {}",
            s
        );
    }

    #[test]
    #[should_panic(expected = "must not be empty")]
    fn download_panics_on_empty_filename() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(download_model("", |_, _| {})).ok();
    }

    #[test]
    #[should_panic(expected = ".gguf")]
    fn download_panics_on_non_gguf() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(download_model("model.bin", |_, _| {})).ok();
    }

    // ── Prompt tests ────────────────────────────────────────────

    #[test]
    fn prompt_no_thinking_tags() {
        let prompt = build_local_prompt("Fix grammar.", "ciao come stai");
        assert!(
            !prompt.contains("<think>"),
            "prompt must not contain thinking tags"
        );
        assert!(
            !prompt.contains("</think>"),
            "prompt must not contain closing thinking tags"
        );
    }

    #[test]
    fn prompt_has_turn_markers() {
        let prompt = build_local_prompt("Fix grammar.", "test text");
        assert!(
            prompt.contains("<start_of_turn>user"),
            "prompt must contain user turn marker"
        );
        assert!(
            prompt.contains("<start_of_turn>model"),
            "prompt must contain model turn marker"
        );
        assert!(
            prompt.contains("<end_of_turn>"),
            "prompt must contain end turn marker"
        );
    }

    #[test]
    fn prompt_includes_system_and_user() {
        let prompt = build_local_prompt("Fix all errors.", "il mio testo");
        assert!(
            prompt.contains("Fix all errors."),
            "prompt must contain system prompt"
        );
        assert!(
            prompt.contains("il mio testo"),
            "prompt must contain user text"
        );
        assert!(
            prompt.contains("Output ONLY"),
            "prompt must contain output instruction"
        );
    }

    #[test]
    fn local_preamble_is_short_and_direct() {
        let prompt = build_local_system_prompt(
            crate::llm::LlmStyle::Correct,
            crate::llm::LlmTone::None,
            "",
            "none",
        );
        assert!(
            prompt.contains("Fix grammar"),
            "Correct style must contain 'Fix grammar': {}",
            prompt
        );
        // Local prompts must be short — under 200 chars for small models
        assert!(
            prompt.len() < 200,
            "local prompt must be short for small models, got {} chars",
            prompt.len()
        );
    }

    #[test]
    fn local_preamble_with_tone() {
        let prompt = build_local_system_prompt(
            crate::llm::LlmStyle::Professional,
            crate::llm::LlmTone::Formal,
            "",
            "none",
        );
        assert!(prompt.contains("professional"), "must contain style");
        assert!(prompt.contains("formal"), "must contain tone: {}", prompt);
    }

    #[test]
    fn local_preamble_with_translation() {
        let prompt = build_local_system_prompt(
            crate::llm::LlmStyle::Correct,
            crate::llm::LlmTone::None,
            "",
            "English",
        );
        assert!(
            prompt.contains("Translate"),
            "must contain translation instruction: {}",
            prompt
        );
        assert!(prompt.contains("English"), "must contain target language");
    }

    #[test]
    fn local_preamble_empty_when_off() {
        let prompt = build_local_system_prompt(
            crate::llm::LlmStyle::Off,
            crate::llm::LlmTone::None,
            "",
            "none",
        );
        assert!(prompt.is_empty(), "Off style must produce empty prompt");
    }

    // ── Cache tests ─────────────────────────────────────────────

    #[test]
    fn clear_llm_cache_idempotent() {
        clear_llm_cache();
        clear_llm_cache(); // must not panic
    }

    // ── Tag stripping tests ─────────────────────────────────────

    #[test]
    fn strip_tags_removes_think() {
        let input = "<think>reasoning here</think>Actual output text.";
        let result = strip_special_tags(input);
        assert_eq!(result, "Actual output text.");
    }

    #[test]
    fn strip_tags_removes_turn_markers() {
        let input = "Hello world<end_of_turn>";
        let result = strip_special_tags(input);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn strip_tags_preserves_normal_text() {
        let input = "Temperature is < 5 degrees and > 0.";
        let result = strip_special_tags(input);
        assert_eq!(result, "Temperature is < 5 degrees and > 0.");
    }

    #[test]
    fn strip_tags_handles_pipe_tokens() {
        let input = "<|think|>some thinking<|/think|>The answer.";
        let result = strip_special_tags(input);
        assert_eq!(result, "The answer.");
    }

    // ── Feature-gated tests ─────────────────────────────────────

    #[cfg(feature = "local-llm")]
    #[test]
    fn generate_rejects_missing_model() {
        let result = process_text_local(
            Path::new("/nonexistent/model.gguf"),
            "test text",
            crate::llm::LlmStyle::Correct,
            crate::llm::LlmTone::None,
            "",
            "none",
        );
        assert!(result.is_err());
        if let Err(LlmError::LocalModel(msg)) = result {
            assert!(
                msg.contains("not found"),
                "error should mention 'not found': {}",
                msg
            );
        } else {
            panic!("Expected LocalModel error");
        }
    }

    #[cfg(feature = "local-llm")]
    #[test]
    #[should_panic(expected = "must not be empty")]
    fn process_text_local_rejects_empty() {
        let _ = process_text_local(
            Path::new("/any/model.gguf"),
            "",
            crate::llm::LlmStyle::Correct,
            crate::llm::LlmTone::None,
            "",
            "none",
        );
    }

    #[cfg(not(feature = "local-llm"))]
    #[test]
    fn process_text_local_stub_disabled() {
        let result = process_text_local(
            Path::new("/any/model.gguf"),
            "test",
            crate::llm::LlmStyle::Correct,
            crate::llm::LlmTone::None,
            "",
            "none",
        );
        assert!(result.is_err());
        if let Err(LlmError::LocalModel(msg)) = result {
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

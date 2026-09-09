//! Qwen3-ASR local speech-to-text, through llama.cpp's multimodal path.
//!
//! This is a THIRD local STT engine next to whisper (`local_stt`) and Parakeet
//! (`parakeet`), and it exists because it is measurably better than whisper on
//! hard conversational Italian. Measured 2026-09-09 on a real meeting mic track
//! (`43bc6a8c`), with our own whisper large-v3-turbo transcript of the same
//! audio as the reference:
//!
//! | said                                | whisper turbo            | Qwen3-ASR 1.7B |
//! |-------------------------------------|--------------------------|----------------|
//! | "tag NFC"                           | "tag NFC" / "del FC"     | "tag NFC"      |
//! | "non e` IMpossibile ma piu` difficile" | "non e` possibile"    | "non e` impossibile" |
//! | "non riesci ad avere qualcosa..."   | "non riesce... non riuscire" | correct   |
//!
//! The second row is the reason to care: whisper wrote the OPPOSITE of what
//! was said, and that sentence goes straight into a recap.
//!
//! Speed, warm, model resident, on a 4 GB T600 at full TGP: a 15 s window in
//! ~2.5 s, a 3 s utterance in ~0.5 s. Unlike whisper it does NOT pad every
//! input to a 30 s encoder window, so cost tracks the audio you actually give
//! it -- which is exactly the waste we measured on 3 s dictation chunks.
//!
//! Two shapes are the caller's problem, not the model's:
//!
//! * the answer is prefixed `language Italian<asr_text>...` (llama.cpp #26749).
//!   [`strip_asr_scaffolding`] is the single place that undoes it.
//! * near-silence produces a short Chinese hallucination, the same way whisper
//!   produces "Grazie". The existing chunk VAD gate must stay in front of it,
//!   unchanged, for the same reason.

/// Separates the language verdict from the transcript in the model's answer.
const ASR_TEXT_MARKER: &str = "<asr_text>";
/// What the language verdict is prefixed with, e.g. `language Italian`.
const LANGUAGE_PREFIX: &str = "language ";

/// Split `language Italian<asr_text>ma poi non...` into the language the model
/// heard and the transcript itself.
///
/// The language is a bonus we get for free: it is the model's own verdict on
/// the audio, which is what [`crate::lang_detect`] runs a separate whisper pass
/// to obtain. Returns `None` for it when the answer carries no verdict, and
/// returns the whole answer as the transcript when there is no marker at all,
/// so a future llama.cpp that drops the prefix keeps working.
pub fn strip_asr_scaffolding(raw: &str) -> (Option<String>, String) {
    let trimmed = raw.trim();
    let Some((head, body)) = trimmed.split_once(ASR_TEXT_MARKER) else {
        return (None, trimmed.to_string());
    };
    let language = head
        .trim()
        .strip_prefix(LANGUAGE_PREFIX)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    (language, body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_language_from_transcript() {
        let (lang, text) = strip_asr_scaffolding("language Italian<asr_text>ma poi non riescono");
        assert_eq!(lang.as_deref(), Some("Italian"));
        assert_eq!(text, "ma poi non riescono");
    }

    #[test]
    fn keeps_the_answer_when_there_is_no_marker() {
        let (lang, text) = strip_asr_scaffolding("  plain transcript  ");
        assert_eq!(lang, None);
        assert_eq!(text, "plain transcript");
    }

    #[test]
    fn tolerates_a_marker_without_a_language_verdict() {
        let (lang, text) = strip_asr_scaffolding("<asr_text>hello");
        assert_eq!(lang, None);
        assert_eq!(text, "hello");
    }

    #[test]
    fn tolerates_an_unexpected_head() {
        let (lang, text) = strip_asr_scaffolding("something else<asr_text>hello");
        assert_eq!(lang, None);
        assert_eq!(text, "hello");
    }

    #[test]
    fn near_silence_yields_an_empty_transcript_not_a_marker() {
        // What the model actually returned on a near-silent mic window.
        let (lang, text) = strip_asr_scaffolding("language Chinese<asr_text>\u{55EF}\u{3002}");
        assert_eq!(lang.as_deref(), Some("Chinese"));
        assert!(!text.contains(ASR_TEXT_MARKER));
    }

    #[test]
    fn the_default_model_is_in_the_catalog() {
        assert!(find(DEFAULT_MODEL).is_some());
    }

    #[test]
    fn every_entry_names_two_distinct_files_and_a_real_size() {
        for m in AVAILABLE_MODELS {
            assert_ne!(m.model_file, m.mmproj_file, "{}", m.name);
            assert!(m.mmproj_file.starts_with("mmproj-"), "{}", m.name);
            assert!(m.model_file.ends_with(".gguf"), "{}", m.name);
            assert!(m.size_mb > 0, "{}", m.name);
            assert!(!m.repo.is_empty(), "{}", m.name);
        }
    }

    #[test]
    fn model_files_are_unique_across_the_catalog() {
        // The picker keys on model_file, so a duplicate would make one
        // entry unselectable.
        let mut seen: Vec<&str> = AVAILABLE_MODELS.iter().map(|m| m.model_file).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before);
    }

    #[test]
    fn an_unknown_model_is_neither_found_nor_present() {
        assert!(find("not-a-model.gguf").is_none());
        assert!(!bundle_present("not-a-model.gguf"));
    }

    #[test]
    fn an_empty_answer_stays_empty() {
        let (lang, text) = strip_asr_scaffolding("");
        assert_eq!(lang, None);
        assert!(text.is_empty());
    }
}

// -- Catalog + download ------------------------------------------------
//
// Unlike whisper, one Qwen3-ASR entry is TWO files: the text model and the
// audio projector (`mmproj-`). Either alone is useless -- with the projector
// missing the model would answer from the prompt alone, which reads as a
// fluent transcript of audio it never heard. So the pair is the unit of both
// presence and download, the way the Parakeet bundle is.

/// One selectable Qwen3-ASR variant.
pub struct QwenAsrModel {
    pub name: &'static str,
    pub model_file: &'static str,
    pub mmproj_file: &'static str,
    /// Both files together, which is what the user is asked to download.
    pub size_mb: u32,
    pub description: &'static str,
    repo: &'static str,
}

pub const AVAILABLE_MODELS: &[QwenAsrModel] = &[
    QwenAsrModel {
        name: "Qwen3-ASR 0.6B",
        model_file: "Qwen3-ASR-0.6B-Q8_0.gguf",
        mmproj_file: "mmproj-Qwen3-ASR-0.6B-Q8_0.gguf",
        size_mb: 971,
        description: "Twice as fast, weaker on acronyms",
        repo: "ggml-org/Qwen3-ASR-0.6B-GGUF",
    },
    QwenAsrModel {
        name: "Qwen3-ASR 1.7B",
        model_file: "Qwen3-ASR-1.7B-Q8_0.gguf",
        mmproj_file: "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
        size_mb: 2404,
        description: "Best accuracy on conversational speech",
        repo: "ggml-org/Qwen3-ASR-1.7B-GGUF",
    },
];

/// Measured better than the 0.6B on every acronym in the reference meeting,
/// at half the speed and still 5x realtime. See the doc in docs/dev.
pub const DEFAULT_MODEL: &str = "Qwen3-ASR-1.7B-Q8_0.gguf";

/// Whether the engine is compiled into THIS build.
///
/// The catalog is data and is always here, but a build without
/// `local-stt-qwen` cannot transcribe with it. The FFI listing hides the
/// variants when this is false, so a lean build never shows a row that
/// would fail only once the user has downloaded 2.4 GB and pressed record.
pub const fn engine_available() -> bool {
    cfg!(feature = "local-stt-qwen")
}

pub fn find(model_file: &str) -> Option<&'static QwenAsrModel> {
    AVAILABLE_MODELS.iter().find(|m| m.model_file == model_file)
}

/// Both halves live beside the whisper models: they are STT weights and the
/// user thinks of them in one place.
pub fn file_path(file: &str) -> std::path::PathBuf {
    crate::local_stt::model_path(file)
}

/// True only when BOTH halves are on disk.
pub fn bundle_present(model_file: &str) -> bool {
    let Some(m) = find(model_file) else {
        return false;
    };
    file_path(m.model_file).is_file() && file_path(m.mmproj_file).is_file()
}

/// Fetch both halves, resumable and integrity-checked, through the shared
/// downloader. Progress is reported against the pair, not per file, because
/// that is the number the user is watching.
pub async fn download_bundle<F>(
    model_file: &str,
    on_progress: F,
) -> Result<(), crate::error::TranscribeError>
where
    F: Fn(u64, u64),
{
    use crate::error::TranscribeError;
    let m = find(model_file).ok_or_else(|| {
        TranscribeError::LocalModel(format!("unknown Qwen3-ASR model '{}'", model_file))
    })?;

    let dir = crate::local_stt::model_directory();
    std::fs::create_dir_all(&dir)
        .map_err(|e| TranscribeError::LocalModel(format!("create {}: {}", dir.display(), e)))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| TranscribeError::LocalModel(format!("HTTP client: {}", e)))?;

    let total = u64::from(m.size_mb) * 1024 * 1024;
    let mut base: u64 = 0;
    for file in [m.model_file, m.mmproj_file] {
        let dest = file_path(file);
        if !dest.is_file() {
            let url = format!("https://huggingface.co/{}/resolve/main/{}", m.repo, file);
            crate::log(&format!("[QwenASR] Downloading {} ...", url));
            crate::download::download_resumable(&client, &url, &dest, &[b"GGUF"], |done, _| {
                on_progress(base + done, total)
            })
            .await
            .map_err(TranscribeError::LocalModel)?;
        }
        base += std::fs::metadata(&dest).map(|md| md.len()).unwrap_or(0);
        on_progress(base, total);
    }

    assert!(
        bundle_present(model_file),
        "both halves must exist after a successful download"
    );
    Ok(())
}

/// What one call returns: the transcript, plus the language the model itself
/// says it heard.
#[derive(Debug, Clone)]
pub struct Transcript {
    /// The model's own verdict, e.g. `"Italian"`. `None` when it gave none.
    pub language: Option<String>,
    pub text: String,
}

#[cfg(feature = "local-stt-qwen")]
pub use engine::{clear_model_cache, transcribe, QwenAsr};

/// No-op when the engine is compiled out, so the VRAM handover in
/// `local_llm` needs no `cfg` of its own.
#[cfg(not(feature = "local-stt-qwen"))]
pub fn clear_model_cache() {}

/// Same shape as the Parakeet stub: the routing layer stays free of `cfg`
/// and a build without the engine says so once, in words, instead of
/// failing to compile the call site.
#[cfg(not(feature = "local-stt-qwen"))]
pub fn transcribe(
    _pcm_16k: &[f32],
    _model_file: &str,
) -> Result<Transcript, crate::error::TranscribeError> {
    Err(crate::error::TranscribeError::LocalModel(
        "Qwen3-ASR requires the local-stt-qwen cargo feature".to_string(),
    ))
}

#[cfg(feature = "local-stt-qwen")]
mod engine {
    use std::num::NonZeroU32;
    use std::path::Path;

    use llama_cpp_4::context::params::LlamaContextParams;
    use llama_cpp_4::llama_batch::LlamaBatch;
    use llama_cpp_4::model::params::LlamaModelParams;
    use llama_cpp_4::model::{LlamaChatMessage, LlamaModel, Special};
    use llama_cpp_4::mtmd::{
        MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputChunks, MtmdInputText,
    };
    use llama_cpp_4::token::LlamaToken;

    use super::{strip_asr_scaffolding, Transcript};
    use crate::error::TranscribeError;

    /// Context window. A 15 s window costs a few hundred audio tokens, so this
    /// is generous on purpose: the encoder emits one chunk per 7.5 s of audio
    /// and a whole meeting is never handed over in one call.
    const N_CTX: u32 = 4096;
    /// Prompt tokens per decode. llama.cpp sizes the compute buffer from this
    /// and asserts `n_tokens_all <= n_batch` with a GGML_ASSERT that kills the
    /// process, so it must not be smaller than the audio chunk it is fed.
    const N_BATCH: u32 = 512;
    /// Hard stop on generation. Speech runs at roughly 3 words a second, so a
    /// 15 s window is ~50 words; anything past this is the model looping, and
    /// truncating beats hanging the dictation.
    const MAX_NEW_TOKENS: usize = 512;
    /// Encoder threads. The audio encoder is on the GPU; these only feed it.
    const N_THREADS: i32 = 4;

    /// A loaded Qwen3-ASR: the text model plus its audio projector.
    ///
    /// Loading costs ~6 s, so this is meant to be held for the life of a
    /// dictation session or a meeting, exactly like `local_llm`'s cache. The
    /// warm per-window cost is what makes the engine usable at all.
    pub struct QwenAsr {
        // Declaration order IS drop order, and this order is load-bearing:
        // the mtmd context was built FROM the model and must be freed first.
        mtmd: MtmdContext,
        model: LlamaModel,
    }

    impl QwenAsr {
        /// Load the model and its projector from disk.
        pub fn load(model_path: &Path, mmproj_path: &Path) -> Result<Self, TranscribeError> {
            let backend = crate::local_llm::shared_backend()
                .map_err(|e| TranscribeError::LocalModel(format!("{:?}", e)))?;

            let model = LlamaModel::load_from_file(
                backend,
                model_path,
                &LlamaModelParams::default().with_n_gpu_layers(99),
            )
            .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr model load: {}", e)))?;

            let mtmd = MtmdContext::init_from_file(
                mmproj_path,
                &model,
                MtmdContextParams::default()
                    .use_gpu(true)
                    .n_threads(N_THREADS)
                    .print_timings(false),
            )
            .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr mmproj load: {}", e)))?;

            // A vision-only projector would tokenize the audio into nothing and
            // the model would answer from the prompt alone -- a confident,
            // fluent transcript of audio it never heard. Refuse instead.
            if !mtmd.supports_audio() {
                return Err(TranscribeError::LocalModel(
                    "qwen-asr: projector carries no audio encoder".to_string(),
                ));
            }

            Ok(Self { mtmd, model })
        }

        /// Transcribe one window of 16 kHz mono PCM.
        pub fn transcribe(&self, pcm_16k: &[f32]) -> Result<Transcript, TranscribeError> {
            assert!(!pcm_16k.is_empty(), "qwen-asr: empty audio window");
            assert!(
                pcm_16k.iter().all(|s| s.is_finite()),
                "qwen-asr: NaN/Inf in audio window"
            );

            let backend = crate::local_llm::shared_backend()
                .map_err(|e| TranscribeError::LocalModel(format!("{:?}", e)))?;
            let mut lctx = self
                .model
                .new_context(
                    backend,
                    LlamaContextParams::default()
                        .with_n_ctx(NonZeroU32::new(N_CTX))
                        .with_n_batch(N_BATCH),
                )
                .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr context: {}", e)))?;

            let bitmap = MtmdBitmap::from_audio(pcm_16k)
                .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr audio: {}", e)))?;

            // The GGUF carries its own turn format and the model will not say a
            // word without it: fed a raw prompt it emits end-of-generation on
            // the FIRST token and the transcript comes back empty, which is
            // indistinguishable from silence. `apply_chat_template` reads the
            // template out of the model, the same way local_llm does, so we
            // carry no per-family branch of our own.
            let user = format!("Transcribe the audio. {}", MtmdContext::default_marker());
            let messages = vec![LlamaChatMessage::new("user".to_string(), user)
                .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr chat msg: {}", e)))?];
            let prompt = self
                .model
                .apply_chat_template(None, &messages, /* add_ass */ true)
                .map_err(|e| {
                    TranscribeError::LocalModel(format!("qwen-asr chat template: {}", e))
                })?;
            let input = MtmdInputText::new(&prompt, true, true);
            let mut chunks = MtmdInputChunks::new();
            self.mtmd
                .tokenize(&input, &[&bitmap], &mut chunks)
                .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr tokenize: {}", e)))?;

            let mut n_past: i32 = 0;
            self.mtmd
                .eval_chunks(
                    lctx.as_ptr(),
                    &chunks,
                    0,
                    0,
                    N_BATCH as i32,
                    true,
                    &mut n_past,
                )
                .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr encode: {}", e)))?;

            // Greedy on purpose: a transcript is not a place for sampling.
            let eos = self.model.token_eos();
            let mut raw = String::new();
            for _ in 0..MAX_NEW_TOKENS {
                let next = lctx
                    .get_logits()
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.total_cmp(b))
                    .map(|(i, _)| LlamaToken::new(i as i32))
                    .ok_or_else(|| {
                        TranscribeError::LocalModel("qwen-asr: no logits".to_string())
                    })?;
                if next == eos || self.model.is_eog_token(next) {
                    break;
                }
                raw.push_str(
                    &self
                        .model
                        .token_to_str(next, Special::Tokenize)
                        .map_err(|e| {
                            TranscribeError::LocalModel(format!("qwen-asr detokenize: {}", e))
                        })?,
                );

                let mut batch = LlamaBatch::new(1, 1);
                batch
                    .add(next, n_past, &[0], true)
                    .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr batch: {}", e)))?;
                lctx.decode(&mut batch)
                    .map_err(|e| TranscribeError::LocalModel(format!("qwen-asr decode: {}", e)))?;
                n_past += 1;
            }

            let (language, text) = strip_asr_scaffolding(&raw);
            if text.is_empty() {
                return Err(TranscribeError::Empty);
            }
            Ok(Transcript { language, text })
        }
    }

    // -- Resident model, and the VRAM handover ----------------------

    /// The loaded variant, keyed by which one it is. Held for the life of
    /// the session: the 4.3 s load is what the warm per-window cost buys.
    static CACHE: std::sync::Mutex<Option<(String, QwenAsr)>> = std::sync::Mutex::new(None);

    /// Transcribe one window, loading or switching the model if needed.
    ///
    /// The handover is the same rule the LLM already applies to whisper: if
    /// the incoming weights would not comfortably fit alongside what is
    /// resident, the residents are dropped first. Measured the hard way on a
    /// 4 GB card the same night this landed -- with 3831 MiB of 4096 already
    /// taken, nothing was offloaded and every engine crawled at a fifth of
    /// its speed with no error anywhere.
    pub fn transcribe(pcm_16k: &[f32], model_file: &str) -> Result<Transcript, TranscribeError> {
        assert!(!model_file.is_empty(), "qwen-asr: no model selected");
        let mut guard = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let loaded = guard.as_ref().is_some_and(|(f, _)| f == model_file);
        if !loaded {
            let m = super::find(model_file).ok_or_else(|| {
                TranscribeError::LocalModel(format!("unknown Qwen3-ASR model '{}'", model_file))
            })?;
            // Drop ours BEFORE asking for more, so switching variants never
            // holds two sets of weights at once.
            *guard = None;
            if crate::local_llm::stt_should_yield(
                u64::from(m.size_mb),
                crate::hardware::detect().vram_mb,
            ) {
                crate::log("[QwenASR] freeing resident models before load");
                crate::local_llm::clear_llm_cache();
                crate::local_stt::clear_model_cache();
            }
            crate::log(&format!("[QwenASR] Loading {} ...", m.name));
            let asr = QwenAsr::load(
                &super::file_path(m.model_file),
                &super::file_path(m.mmproj_file),
            )?;
            *guard = Some((model_file.to_string(), asr));
        }

        guard
            .as_ref()
            .expect("model just loaded")
            .1
            .transcribe(pcm_16k)
    }

    /// Drop the resident model. Called when something else needs the VRAM.
    pub fn clear_model_cache() {
        let mut guard = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.take().is_some() {
            crate::log("[QwenASR] model unloaded");
        }
    }
}

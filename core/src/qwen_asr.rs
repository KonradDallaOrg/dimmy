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
    fn an_empty_answer_stays_empty() {
        let (lang, text) = strip_asr_scaffolding("");
        assert_eq!(lang, None);
        assert!(text.is_empty());
    }
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
pub use engine::QwenAsr;

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
}

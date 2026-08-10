use base64::Engine;
use reqwest::multipart;

use crate::audio::ProcessedAudio;
use crate::provider::Provider;

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Calculate HTTP timeout based on payload size.
/// Base 30s + 1s per MB. Ensures large chunks don't time out prematurely.
fn timeout_for_payload(wav_data: &[u8]) -> std::time::Duration {
    let secs = 30 + wav_data.len() / (1024 * 1024);
    // Timeout must be at least 30s and at most 10 minutes
    assert!(secs >= 30, "timeout below 30s minimum");
    let capped = secs.min(600);
    std::time::Duration::from_secs(capped as u64)
}

/// How an OpenAI-compatible STT model takes its language hint.
///
/// `whisper-1` and the `gpt-4o-transcribe` family take a singular `language`
/// string. The `gpt-transcribe` generation takes a `languages[]` LIST instead
/// (it can be handed several candidates), and rejects the singular form. The
/// two spellings are not interchangeable, so the model id decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttFieldStyle {
    /// `language` (single ISO code). whisper-1, gpt-4o-transcribe, and every
    /// OpenAI-compatible third party (Groq, Together, Fireworks).
    Whisper,
    /// `languages[]` (repeated). gpt-transcribe, gpt-live-transcribe.
    Structured,
}

/// Which hint spelling `model` expects. Prefix-matched so dated snapshots
/// (`gpt-transcribe-2026-07-31`) resolve like their base model.
///
/// Note the prefixes do NOT collide with `gpt-4o-transcribe`, which is an
/// older model on the whisper-style contract despite the similar name.
/// Anything unrecognised falls back to whisper-style: that is the format
/// every OpenAI-compatible third-party endpoint implements, so an unknown
/// model can only ever be a model we have not catalogued yet, never a
/// different protocol.
pub fn stt_field_style(model: &str) -> SttFieldStyle {
    let m = model.trim().to_ascii_lowercase();
    if m.starts_with("gpt-transcribe") || m.starts_with("gpt-live-transcribe") {
        SttFieldStyle::Structured
    } else {
        SttFieldStyle::Whisper
    }
}

/// Send WAV audio to any OpenAI-compatible transcription endpoint,
/// or to Google Gemini's generateContent endpoint (auto-detected from URL).
/// `language` is an ISO 639-1 code (e.g. "it", "en"). Empty string = auto-detect.
pub async fn transcribe_audio(
    api_url: &str,
    model: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: &str,
) -> Result<String, crate::error::TranscribeError> {
    // SECURITY: validate URL — reject non-HTTPS, dangerous schemes, CRLF injection, etc.
    if let Err(reason) = Provider::validate_url(api_url) {
        return Err(crate::error::TranscribeError::InsecureUrl(reason));
    }

    // Route to provider-specific path
    match Provider::from_url(api_url) {
        Provider::Deepgram => {
            return transcribe_audio_deepgram(api_url, model, api_key, wav_data, language, prompt)
                .await
        }
        Provider::Gemini => {
            // Gemini transcription is multimodal generateContent with
            // audio inline. The user may have configured api_url as
            // either the full method path (...:generateContent) or
            // just the base / models prefix — in that case we build
            // the full URL using `model`. Without this, the OpenAI
            // multipart fallback would POST to a base URL Google
            // doesn't expose as an endpoint and return 404.
            let full_url = if api_url.contains(":generateContent")
                || api_url.contains(":streamGenerateContent")
            {
                api_url.to_string()
            } else {
                let base = api_url.trim_end_matches('/');
                if base.ends_with("/models") {
                    format!("{}/{}:generateContent", base, model)
                } else if base.contains("/models/") {
                    // already has /models/<id>, append the method
                    format!("{}:generateContent", base)
                } else {
                    // unknown shape — assume v1beta and append models/<id>
                    format!("{}/models/{}:generateContent", base, model)
                }
            };
            return transcribe_audio_gemini(&full_url, api_key, wav_data, language, prompt).await;
        }
        _ => {}
    }

    let timeout = timeout_for_payload(wav_data);

    let file_part = multipart::Part::bytes(wav_data.to_vec())
        .file_name("recording.wav")
        .mime_str("audio/wav")?;

    // `json` is the one response_format every model on this path accepts —
    // gpt-transcribe supports ONLY json (verbose_json and the
    // timestamp_granularities[] that rides on it stay whisper-1-only).
    let mut form = multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "json")
        .part("file", file_part);

    // Add language hint if specified (prevents Whisper from translating).
    // The field name is model-dependent — see `stt_field_style`.
    if !language.is_empty() {
        form = match stt_field_style(model) {
            SttFieldStyle::Whisper => form.text("language", language.to_string()),
            SttFieldStyle::Structured => form.text("languages[]", language.to_string()),
        };
    }

    // Prompt guides Whisper's output style (punctuation, vocabulary, spelling).
    // It's not an instruction — Whisper mimics the style of the prompt text.
    if !prompt.is_empty() {
        form = form.text("prompt", prompt.to_string());
        crate::log(&format!(
            "[DictBias] provider={} prompt_chars={}",
            Provider::from_url(api_url).as_str(),
            prompt.len()
        ));
    }

    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let response = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let body = Provider::scrub_api_key(crate::truncate_utf8(&body, 200), api_key);
        return Err(crate::error::TranscribeError::Api {
            status: status.as_u16(),
            body,
        });
    }

    let result: TranscriptionResponse = response.json().await?;
    Ok(result.text)
}

/// Compose the full Deepgram request URL from a base `api_url`, the
/// configured `model` (e.g. `nova-3`), a `language` code (empty =
/// auto-detect), and a comma-separated vocabulary biasing `prompt`.
///
/// Deepgram takes the model as a QUERY param (unlike OpenAI's multipart
/// form field), and the host stores it in a separate config field — so we
/// must append `&model=<model>` here. Burned 2026-06-06: the model was
/// dropped entirely on the Deepgram path (only Gemini threaded it through),
/// so a bare `api_url` hit Deepgram's default model and every nova-3-only
/// param (keyterm, multilingual) 400'd.
///
/// Each comma-separated dict term becomes a `&keyterm=<term>` param —
/// Deepgram's native vocabulary biasing, which is nova-3-only, so we emit
/// keyterms only when the resolved model is nova-3. Pure for unit testing.
pub fn compose_deepgram_url(api_url: &str, model: &str, language: &str, prompt: &str) -> String {
    let mut url = api_url.to_string();

    // Append the model unless the api_url already pins one.
    if !model.is_empty() && !url.contains("model=") {
        let sep = if url.contains('?') { "&" } else { "?" };
        url = format!("{}{}model={}", url, sep, model);
    }

    let is_nova3 = url.contains("nova-3");

    // Nova-3 only accepts `en` or `multi` for the language parameter — a
    // single non-English code (e.g. `it`, `fr`) returns HTTP 400. Map any
    // non-English language to `multi` (multilingual code-switching, GA) and
    // an empty language to `multi` too (nova-3 has no `detect_language`).
    // Older models (nova-2, enhanced, base) keep per-language codes and the
    // `detect_language=true` auto path.
    let sep = if url.contains('?') { "&" } else { "?" };
    if is_nova3 {
        let lang = if language.eq_ignore_ascii_case("en") {
            "en"
        } else {
            "multi"
        };
        url = format!("{}{}language={}", url, sep, lang);
    } else if !language.is_empty() {
        url = format!("{}{}language={}", url, sep, language);
    } else {
        url = format!("{}{}detect_language=true", url, sep);
    }

    // keyterm is a nova-3-only parameter — sending it on any other model
    // 400s the request. Only emit it for nova-3.
    if is_nova3 && !prompt.is_empty() {
        for term in prompt
            .split(',')
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            // Minimal escape: spaces, commas, ampersands, and percent.
            // Deepgram tolerates a lot, but our query-string separator
            // is '&' and our dict separator is ',' — so escape those
            // two plus '%' (for safety) and ' ' (for cleanliness).
            let escaped = term
                .replace('%', "%25")
                .replace('&', "%26")
                .replace(' ', "%20");
            url = format!("{}&keyterm={}", url, escaped);
        }
    }
    url
}

/// Compose the system-text part for the Gemini multimodal request. The
/// audio is supplied in a separate inline_data part — this function
/// produces only the leading text prompt. Pure for unit testing.
pub fn compose_gemini_prompt_text(language: &str, prompt: &str) -> String {
    let lang_hint = if !language.is_empty() {
        format!(" The audio is in {}.", language)
    } else {
        String::new()
    };
    let vocab_hint = if !prompt.trim().is_empty() {
        format!(" Vocabulary you may hear in this audio: {}.", prompt)
    } else {
        String::new()
    };
    format!(
        "Transcribe this audio exactly as spoken. Output ONLY the transcribed text, nothing else. \
        No introductions, no explanations, no labels.{}{}",
        lang_hint, vocab_hint
    )
}

/// Deepgram-specific transcription: sends raw WAV bytes with Token auth.
/// The URL should be: https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true
async fn transcribe_audio_deepgram(
    api_url: &str,
    model: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: &str,
) -> Result<String, crate::error::TranscribeError> {
    let url = compose_deepgram_url(api_url, model, language, prompt);
    if !prompt.is_empty() {
        crate::log(&format!(
            "[DictBias] provider=deepgram keyterm_count={} prompt_chars={}",
            prompt.split(',').filter(|t| !t.trim().is_empty()).count(),
            prompt.len()
        ));
    }

    let timeout = timeout_for_payload(wav_data);
    let client = reqwest::Client::builder().timeout(timeout).build()?;

    let response = client
        .post(&url)
        .header("Authorization", format!("Token {}", api_key))
        .header("Content-Type", "audio/wav")
        .body(wav_data.to_vec())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let scrubbed = Provider::scrub_api_key(crate::truncate_utf8(&body, 200), api_key);
        // Deepgram 4xx bodies are request-parameter errors (e.g. an invalid
        // `language`/`model`/`keyterm` combo), NOT transcribed content — safe
        // to log so the failure isn't a silent "HTTP 400".
        crate::log(&format!(
            "[STT] deepgram HTTP {} body={}",
            status.as_u16(),
            scrubbed
        ));
        return Err(crate::error::TranscribeError::Api {
            status: status.as_u16(),
            body: scrubbed,
        });
    }

    let result: serde_json::Value = response.json().await?;
    let text = result["results"]["channels"][0]["alternatives"][0]["transcript"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err(crate::error::TranscribeError::Empty);
    }

    Ok(text)
}

/// Deepgram transcription returning per-word `(start_secs, punctuated_word)`.
/// Same request as [`transcribe_audio_deepgram`] but parses the `words` array
/// (nova models return it with `smart_format`). Lets meeting re-transcription
/// rebuild speaker-turn lines with REAL timestamps from one call per band,
/// instead of one HTTP POST per 15 s window. Falls back to a single pseudo-word
/// holding the whole transcript at t=0 if no `words` array is present.
pub async fn transcribe_audio_deepgram_words(
    api_url: &str,
    model: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: &str,
) -> Result<Vec<(f64, String)>, crate::error::TranscribeError> {
    let url = compose_deepgram_url(api_url, model, language, prompt);
    let timeout = timeout_for_payload(wav_data);
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let response = client
        .post(&url)
        .header("Authorization", format!("Token {}", api_key))
        .header("Content-Type", "audio/wav")
        .body(wav_data.to_vec())
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let scrubbed = Provider::scrub_api_key(crate::truncate_utf8(&body, 200), api_key);
        crate::log(&format!(
            "[STT] deepgram(words) HTTP {} body={}",
            status.as_u16(),
            scrubbed
        ));
        return Err(crate::error::TranscribeError::Api {
            status: status.as_u16(),
            body: scrubbed,
        });
    }
    let result: serde_json::Value = response.json().await?;
    let alt = &result["results"]["channels"][0]["alternatives"][0];
    let mut out: Vec<(f64, String)> = Vec::new();
    if let Some(words) = alt["words"].as_array() {
        for w in words {
            let start = w["start"].as_f64().unwrap_or(0.0);
            let word = w["punctuated_word"]
                .as_str()
                .or_else(|| w["word"].as_str())
                .unwrap_or("");
            if !word.is_empty() {
                out.push((start, word.to_string()));
            }
        }
    }
    if out.is_empty() {
        let text = alt["transcript"].as_str().unwrap_or("").trim().to_string();
        if !text.is_empty() {
            out.push((0.0, text));
        }
    }
    Ok(out)
}

/// Gemini-specific transcription: sends audio as base64 inline data to generateContent.
/// The URL already contains the model name, e.g.:
///   https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent
async fn transcribe_audio_gemini(
    api_url: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: &str,
) -> Result<String, crate::error::TranscribeError> {
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(wav_data);
    let text_part = compose_gemini_prompt_text(language, prompt);
    if !prompt.trim().is_empty() {
        crate::log(&format!(
            "[DictBias] provider=gemini vocab_chars={}",
            prompt.len()
        ));
    }

    let body = serde_json::json!({
        "contents": [{
            "parts": [
                { "text": text_part },
                {
                    "inline_data": {
                        "mime_type": "audio/wav",
                        "data": audio_b64
                    }
                }
            ]
        }]
    });

    let timeout = timeout_for_payload(wav_data);
    let client = reqwest::Client::builder().timeout(timeout).build()?;

    let response = client
        .post(api_url)
        .header("x-goog-api-key", api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(crate::error::TranscribeError::Api {
            status: status.as_u16(),
            body: Provider::scrub_api_key(crate::truncate_utf8(&body, 200), api_key),
        });
    }

    // Gemini response: { "candidates": [{ "content": { "parts": [{ "text": "..." }] } }] }
    let result: serde_json::Value = response.json().await?;
    let text = result["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err(crate::error::TranscribeError::Empty);
    }

    Ok(text)
}

/// Transcribe ProcessedAudio locally using whisper.cpp via the `local_stt` module.
///
/// Downsamples audio to 16kHz (Whisper's native rate), then runs local inference.
/// No network calls — all processing happens on-device.
pub fn transcribe_audio_local(
    audio: &crate::audio::ProcessedAudio,
    language: &str,
    model_filename: &str,
    prompt: &str,
) -> Result<String, crate::error::TranscribeError> {
    // Preconditions
    assert!(
        !audio.samples.is_empty(),
        "transcribe_audio_local: audio samples must not be empty"
    );
    assert!(
        audio.samples.iter().all(|s| s.is_finite()),
        "transcribe_audio_local: all audio samples must be finite (no NaN/Inf)"
    );
    assert!(
        !model_filename.is_empty(),
        "transcribe_audio_local: model_filename must not be empty"
    );
    assert!(
        audio.sample_rate > 0,
        "transcribe_audio_local: sample_rate must be positive"
    );

    let samples_16k = stt_input_16k(audio);

    // Postcondition: downsampled samples must be non-empty and finite
    assert!(
        !samples_16k.is_empty(),
        "transcribe_audio_local: downsampled samples must not be empty"
    );

    let model_path = crate::local_stt::model_path(model_filename);
    crate::local_stt::transcribe_local(&model_path, &samples_16k, language, prompt)
}

/// The single door every local recogniser walks through: 48 kHz capture in,
/// 16 kHz denoised mono out.
///
/// One place, deliberately. Noise suppression used to sit upstream of AEC3,
/// which meant meetings were filtered and dictation was not (mic-only capture
/// never spins up the AEC), and the audio we ARCHIVED was already filtered.
/// Routing every engine through here makes "exactly one suppressor" a property
/// of the code rather than something to remember.
fn stt_input_16k(audio: &crate::audio::ProcessedAudio) -> Vec<f32> {
    let samples_16k = crate::preprocess::downsample_to_16k(&audio.samples, audio.sample_rate);
    #[cfg(feature = "denoise-gtcrn")]
    let samples_16k = crate::gtcrn::maybe_denoise_16k(&samples_16k).into_owned();
    samples_16k
}

/// Transcribe ProcessedAudio locally using Parakeet TDT v3 FP32. No
/// language argument: Parakeet is auto-language (trained on 25 EU
/// languages incl. Italian + English). Returns Empty if the user
/// recording was silence; LocalModel error if the bundle is missing.
pub fn transcribe_audio_local_parakeet(
    audio: &crate::audio::ProcessedAudio,
) -> Result<String, crate::error::TranscribeError> {
    assert!(
        !audio.samples.is_empty(),
        "transcribe_audio_local_parakeet: audio samples must not be empty"
    );
    assert!(
        audio.samples.iter().all(|s| s.is_finite()),
        "transcribe_audio_local_parakeet: all samples must be finite"
    );
    assert!(
        audio.sample_rate > 0,
        "transcribe_audio_local_parakeet: sample_rate must be positive"
    );

    let samples_16k = stt_input_16k(audio);
    assert!(
        !samples_16k.is_empty(),
        "transcribe_audio_local_parakeet: downsampled samples must not be empty"
    );

    let text = crate::parakeet::transcribe(&samples_16k)?;
    if text.trim().is_empty() {
        return Err(crate::error::TranscribeError::Empty);
    }
    Ok(text)
}

/// Same as `transcribe_audio_local_parakeet` but also returns word-level
/// timestamps as JSON (`[{"word":"hi","start":0.42,"end":0.94}, ...]`).
/// Used by the file-load path so the saved history row carries the
/// timestamps that drive the History detail panel's playback scrub.
pub fn transcribe_audio_local_parakeet_with_word_ts(
    audio: &crate::audio::ProcessedAudio,
) -> Result<(String, String), crate::error::TranscribeError> {
    assert!(
        !audio.samples.is_empty(),
        "transcribe_audio_local_parakeet_with_word_ts: audio samples must not be empty"
    );
    assert!(
        audio.samples.iter().all(|s| s.is_finite()),
        "transcribe_audio_local_parakeet_with_word_ts: all samples must be finite"
    );
    assert!(
        audio.sample_rate > 0,
        "transcribe_audio_local_parakeet_with_word_ts: sample_rate must be positive"
    );

    let samples_16k = stt_input_16k(audio);
    assert!(
        !samples_16k.is_empty(),
        "transcribe_audio_local_parakeet_with_word_ts: downsampled samples must not be empty"
    );

    let (text, ts_json) = crate::parakeet::transcribe_with_word_timestamps(&samples_16k)?;
    if text.trim().is_empty() {
        return Err(crate::error::TranscribeError::Empty);
    }
    Ok((text, ts_json))
}

/// Transcribe ProcessedAudio, automatically chunking if it exceeds the provider's
/// file size limit. Chunk size = 80% of `max_wav_bytes` (safety margin).
///
/// For short audio that fits in one request, this is a transparent pass-through.
/// For long audio: splits at silence boundaries, transcribes each chunk sequentially,
/// concatenates results with spaces.
///
/// `on_progress` is called before sending each chunk: (current_1indexed, total_chunks).
#[allow(clippy::too_many_arguments)]
pub async fn transcribe_chunked(
    api_url: &str,
    model: &str,
    api_key: &str,
    audio: ProcessedAudio,
    language: &str,
    prompt: &str,
    max_wav_bytes: usize,
    on_progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
) -> Result<String, crate::error::TranscribeError> {
    // max_wav_bytes must be positive — zero would force infinite chunking
    assert!(
        max_wav_bytes > 0,
        "transcribe_chunked: max_wav_bytes must be positive"
    );

    let estimated = audio.estimate_wav_size();

    if estimated <= max_wav_bytes {
        // Fits in one request — pass through unchanged (no chunking overhead)
        let payload = audio
            .to_wav_payload()
            .map_err(|e| crate::error::TranscribeError::Api {
                status: 0,
                body: e.to_string(),
            })?;
        return transcribe_audio(api_url, model, api_key, &payload.data, language, prompt).await;
    }

    // Calculate max chunk size in samples from the byte limit.
    // WAV 16kHz 16-bit mono = 32000 bytes/sec after downsample.
    // Convert: max_bytes → max_seconds → max_samples (at native sample rate).
    let bytes_per_sec: usize = 32000;
    let max_secs = max_wav_bytes / bytes_per_sec;
    // 80% safety margin for WAV header, rounding, etc.
    let safe_secs = (max_secs as f64 * 0.8) as usize;
    // Must be at least 5 seconds to produce usable transcription
    let safe_secs = safe_secs.max(5);
    let max_chunk_samples = safe_secs * audio.sample_rate as usize;

    assert!(
        max_chunk_samples > 0,
        "transcribe_chunked: max_chunk_samples computed as zero"
    );

    let chunks = audio.split_at_silence(max_chunk_samples);
    let total = chunks.len();

    // Sanity: we should have at least 1 chunk
    assert!(total > 0, "split_at_silence returned zero chunks");

    let mut results = Vec::with_capacity(total);

    for (i, chunk) in chunks.into_iter().enumerate() {
        if let Some(cb) = &on_progress {
            cb(i + 1, total);
        }

        let payload = chunk
            .to_wav_payload()
            .map_err(|e| crate::error::TranscribeError::Api {
                status: 0,
                body: format!("chunk {}/{}: {}", i + 1, total, e),
            })?;

        let text =
            transcribe_audio(api_url, model, api_key, &payload.data, language, prompt).await?;
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            results.push(trimmed.to_string());
        }
    }

    if results.is_empty() {
        return Err(crate::error::TranscribeError::Empty);
    }

    Ok(results.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Deepgram URL composition ────────────────────────────────────

    #[test]
    fn deepgram_url_appends_language_when_set() {
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", "en", "");
        assert!(url.contains("&language=en"), "url={}", url);
        assert!(!url.contains("detect_language"), "url={}", url);
    }

    #[test]
    fn deepgram_url_uses_auto_detect_when_language_empty_on_older_models() {
        // detect_language is a nova-2-and-older path; nova-3 has no such param.
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-2", "", "");
        assert!(url.contains("&detect_language=true"), "url={}", url);
        assert!(!url.contains("&language="), "url={}", url);
    }

    #[test]
    fn deepgram_url_nova3_maps_non_english_language_to_multi() {
        // Nova-3 rejects single non-English codes (HTTP 400). The configured
        // `language=it` must become `language=multi`, never `language=it`.
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", "it", "");
        assert!(url.contains("&language=multi"), "url={}", url);
        assert!(!url.contains("language=it"), "url={}", url);
    }

    #[test]
    fn deepgram_url_nova3_keeps_english() {
        // English on nova-3 stays monolingual `en` (highest-accuracy path),
        // not coerced to multi.
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", "en", "");
        assert!(url.contains("&language=en"), "url={}", url);
        assert!(!url.contains("multi"), "url={}", url);
    }

    #[test]
    fn deepgram_url_nova3_empty_language_uses_multi_not_detect() {
        // nova-3 has no detect_language; empty config language → multi.
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", "", "");
        assert!(url.contains("&language=multi"), "url={}", url);
        assert!(!url.contains("detect_language"), "url={}", url);
    }

    #[test]
    fn deepgram_url_nova2_keeps_specific_language() {
        // Older models still support per-language codes — don't coerce them.
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-2", "it", "");
        assert!(url.contains("&language=it"), "url={}", url);
        assert!(!url.contains("multi"), "url={}", url);
    }

    #[test]
    fn deepgram_url_uses_question_mark_when_base_has_no_query() {
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "", "en", "");
        assert!(url.contains("?language=en"), "url={}", url);
    }

    #[test]
    fn deepgram_url_appends_model_from_arg_when_absent() {
        // The host stores the model in a separate config field; a bare
        // api_url must get `?model=<model>`, otherwise Deepgram uses its
        // default model and nova-3-only params (keyterm) 400. This is the
        // 2026-06-06 batch-dictation regression.
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", "it", "API");
        assert!(url.contains("model=nova-3"), "url={}", url);
        // model present → nova-3 coercion + keyterm both fire.
        assert!(url.contains("language=multi"), "url={}", url);
        assert!(url.contains("keyterm=API"), "url={}", url);
    }

    #[test]
    fn deepgram_url_does_not_double_append_model() {
        // If the api_url already pins a model, don't append a second one.
        let url = compose_deepgram_url(
            "https://api.deepgram.com/v1/listen?model=nova-3",
            "nova-2",
            "en",
            "",
        );
        assert_eq!(url.matches("model=").count(), 1, "url={}", url);
        assert!(url.contains("model=nova-3"), "url={}", url);
    }

    #[test]
    fn deepgram_url_keyterm_only_on_nova3() {
        // keyterm is nova-3-only; on nova-2 it must be omitted (it 400s).
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-2", "it", "API");
        assert!(!url.contains("keyterm"), "url={}", url);
    }

    #[test]
    fn deepgram_url_appends_each_dict_term_as_keyterm() {
        let url = compose_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            "en",
            "Velopack, Notion, foobar",
        );
        assert!(url.contains("&keyterm=Velopack"), "url={}", url);
        assert!(url.contains("&keyterm=Notion"), "url={}", url);
        assert!(url.contains("&keyterm=foobar"), "url={}", url);
        assert_eq!(url.matches("&keyterm=").count(), 3);
    }

    #[test]
    fn deepgram_url_escapes_space_ampersand_percent_in_terms() {
        let url = compose_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            "",
            "hello world,A&B,50%off",
        );
        assert!(url.contains("&keyterm=hello%20world"), "url={}", url);
        assert!(url.contains("&keyterm=A%26B"), "url={}", url);
        assert!(url.contains("&keyterm=50%25off"), "url={}", url);
        // Defensive: raw '&' or '%' inside a term would break the
        // query string. Make sure we never emit one.
        assert!(
            !url.contains("hello world"),
            "raw space leaked into url={}",
            url
        );
        assert!(
            !url.contains("A&B"),
            "raw ampersand leaked into url={}",
            url
        );
    }

    #[test]
    fn deepgram_url_filters_empty_terms_and_trims_whitespace() {
        // Trailing comma + extra spaces — common from textbox edits.
        let url = compose_deepgram_url(
            "https://api.deepgram.com/v1/listen",
            "nova-3",
            "en",
            "  alpha , , beta ,,,",
        );
        assert!(url.contains("&keyterm=alpha"));
        assert!(url.contains("&keyterm=beta"));
        // Empty + whitespace-only terms must not produce a bare
        // `&keyterm=` — Deepgram would 400 the request.
        assert!(!url.contains("&keyterm=&"), "url={}", url);
        assert!(!url.ends_with("&keyterm="), "url={}", url);
    }

    #[test]
    fn deepgram_url_empty_prompt_emits_no_keyterm() {
        let url = compose_deepgram_url("https://api.deepgram.com/v1/listen", "nova-3", "en", "");
        assert!(!url.contains("keyterm"), "url={}", url);
    }

    // ── Gemini prompt-text composition ──────────────────────────────

    #[test]
    fn gemini_prompt_starts_with_transcribe_instruction() {
        let text = compose_gemini_prompt_text("", "");
        assert!(
            text.starts_with("Transcribe this audio exactly as spoken."),
            "Gemini must lead with the transcribe instruction so the model doesn't generate prose: {}",
            text
        );
        assert!(text.contains("Output ONLY the transcribed text"));
    }

    #[test]
    fn gemini_prompt_includes_language_hint_when_set() {
        let text = compose_gemini_prompt_text("Italian", "");
        assert!(
            text.contains("The audio is in Italian"),
            "missing language hint: {}",
            text
        );
    }

    #[test]
    fn gemini_prompt_no_language_means_no_language_hint() {
        let text = compose_gemini_prompt_text("", "");
        assert!(
            !text.contains("The audio is in"),
            "should not emit a language hint when language is empty: {}",
            text
        );
    }

    #[test]
    fn gemini_prompt_includes_vocab_hint_when_dict_present() {
        let text = compose_gemini_prompt_text("en", "Velopack, Notion");
        assert!(
            text.contains("Vocabulary you may hear"),
            "missing vocab hint: {}",
            text
        );
        assert!(text.contains("Velopack"));
        assert!(text.contains("Notion"));
    }

    #[test]
    fn gemini_prompt_no_vocab_when_prompt_empty_or_whitespace() {
        for empty in &["", "   ", "\t\n"] {
            let text = compose_gemini_prompt_text("en", empty);
            assert!(
                !text.contains("Vocabulary you may hear"),
                "spurious vocab hint for empty/whitespace prompt={:?}: {}",
                empty,
                text
            );
        }
    }

    #[test]
    fn gemini_prompt_language_and_vocab_both_appended() {
        let text = compose_gemini_prompt_text("en", "alpha");
        assert!(text.contains("The audio is in en"));
        assert!(text.contains("Vocabulary you may hear in this audio: alpha"));
        // Order matters for prompt-engineering legibility: language
        // hint before vocab hint (matches the original inline format).
        let lang_idx = text.find("The audio is in").unwrap();
        let vocab_idx = text.find("Vocabulary you may hear").unwrap();
        assert!(
            lang_idx < vocab_idx,
            "language hint must precede vocab hint"
        );
    }

    #[test]
    fn gpt_transcribe_family_uses_the_structured_language_field() {
        for m in [
            "gpt-transcribe",
            "gpt-live-transcribe",
            "gpt-transcribe-2026-07-31",
            "GPT-Transcribe",
        ] {
            assert_eq!(
                stt_field_style(m),
                SttFieldStyle::Structured,
                "{m} should take languages[]"
            );
        }
    }

    #[test]
    fn whisper_family_keeps_the_singular_language_field() {
        // gpt-4o-transcribe is the trap: same vendor, similar name, OLD
        // contract. Sending it languages[] would drop the hint silently.
        for m in [
            "whisper-1",
            "gpt-4o-transcribe",
            "gpt-4o-mini-transcribe",
            "whisper-large-v3-turbo",
            "nvidia/parakeet-tdt-0.6b-v3",
            "",
        ] {
            assert_eq!(
                stt_field_style(m),
                SttFieldStyle::Whisper,
                "{m} should take a singular language"
            );
        }
    }
}

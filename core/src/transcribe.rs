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
            return transcribe_audio_deepgram(api_url, api_key, wav_data, language, prompt).await
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

    let mut form = multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "json")
        .part("file", file_part);

    // Add language hint if specified (prevents Whisper from translating)
    if !language.is_empty() {
        form = form.text("language", language.to_string());
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
        let body = Provider::scrub_api_key(&body[..body.len().min(200)], api_key);
        return Err(crate::error::TranscribeError::Api {
            status: status.as_u16(),
            body,
        });
    }

    let result: TranscriptionResponse = response.json().await?;
    Ok(result.text)
}

/// Deepgram-specific transcription: sends raw WAV bytes with Token auth.
/// The URL should be: https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true
async fn transcribe_audio_deepgram(
    api_url: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: &str,
) -> Result<String, crate::error::TranscribeError> {
    let mut url = api_url.to_string();
    let sep = if url.contains('?') { "&" } else { "?" };
    if !language.is_empty() {
        url = format!("{}{}language={}", url, sep, language);
    } else {
        // Auto-detect language when none specified
        url = format!("{}{}detect_language=true", url, sep);
    }

    // Vocabulary biasing — Deepgram's native param is `keyterm` (Nova-3+
    // models) / `keywords` (legacy). We split the composed prompt by
    // comma so each dict entry becomes a separate keyterm; the user's
    // free-form prompt text falls in as one big keyterm (mostly
    // harmless — Deepgram tolerates long phrases). Format:
    //   ...&keyterm=Velopack&keyterm=Notion&keyterm=foobar
    // Skipped when prompt is empty. See
    // https://developers.deepgram.com/docs/keyterm for the parameter
    // semantics + per-language support.
    if !prompt.is_empty() {
        for term in prompt
            .split(',')
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            // url-encode minimally — Deepgram tolerates spaces,
            // but commas / ampersands in a dict entry would break
            // the query string. We picked '&' as the URL separator
            // above and ',' as our dict separator, so escape just
            // those two plus '%' for safety.
            let escaped = term
                .replace('%', "%25")
                .replace('&', "%26")
                .replace(' ', "%20");
            url = format!("{}&keyterm={}", url, escaped);
        }
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
        return Err(crate::error::TranscribeError::Api {
            status: status.as_u16(),
            body: Provider::scrub_api_key(&body[..body.len().min(200)], api_key),
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

    let lang_hint = if !language.is_empty() {
        format!(" The audio is in {}.", language)
    } else {
        String::new()
    };

    // Gemini multimodal generateContent has no Whisper-style `prompt`
    // form field. We inject the composed dict by appending a "Domain
    // vocabulary you may hear" instruction to the system text part —
    // the LLM-style biasing works because Gemini IS an LLM that reads
    // the entire prompt before transcribing. Empty string → no append,
    // keeps the request short.
    let vocab_hint = if !prompt.trim().is_empty() {
        crate::log(&format!(
            "[DictBias] provider=gemini vocab_chars={}",
            prompt.len()
        ));
        format!(" Vocabulary you may hear in this audio: {}.", prompt)
    } else {
        String::new()
    };

    let body = serde_json::json!({
        "contents": [{
            "parts": [
                {
                    "text": format!(
                        "Transcribe this audio exactly as spoken. Output ONLY the transcribed text, nothing else. \
                        No introductions, no explanations, no labels.{}{}",
                        lang_hint, vocab_hint
                    )
                },
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
            body: Provider::scrub_api_key(&body[..body.len().min(200)], api_key),
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

    // Downsample to 16kHz for Whisper
    let samples_16k = crate::preprocess::downsample_to_16k(&audio.samples, audio.sample_rate);

    // Postcondition: downsampled samples must be non-empty and finite
    assert!(
        !samples_16k.is_empty(),
        "transcribe_audio_local: downsampled samples must not be empty"
    );

    let model_path = crate::local_stt::model_path(model_filename);
    crate::local_stt::transcribe_local(&model_path, &samples_16k, language, prompt)
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

    let samples_16k = crate::preprocess::downsample_to_16k(&audio.samples, audio.sample_rate);
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

    let samples_16k = crate::preprocess::downsample_to_16k(&audio.samples, audio.sample_rate);
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

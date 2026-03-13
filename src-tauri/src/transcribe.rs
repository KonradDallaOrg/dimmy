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
            return transcribe_audio_deepgram(api_url, api_key, wav_data, language).await
        }
        Provider::Gemini if api_url.contains("generateContent") => {
            return transcribe_audio_gemini(api_url, api_key, wav_data, language).await;
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
) -> Result<String, crate::error::TranscribeError> {
    let mut url = api_url.to_string();
    let sep = if url.contains('?') { "&" } else { "?" };
    if !language.is_empty() {
        url = format!("{}{}language={}", url, sep, language);
    } else {
        // Auto-detect language when none specified
        url = format!("{}{}detect_language=true", url, sep);
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
) -> Result<String, crate::error::TranscribeError> {
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(wav_data);

    let lang_hint = if !language.is_empty() {
        format!(" The audio is in {}.", language)
    } else {
        String::new()
    };

    let body = serde_json::json!({
        "contents": [{
            "parts": [
                {
                    "text": format!(
                        "Transcribe this audio exactly as spoken. Output ONLY the transcribed text, nothing else. \
                        No introductions, no explanations, no labels.{}", lang_hint
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

/// Transcribe ProcessedAudio, automatically chunking if it exceeds the provider's
/// file size limit. Chunk size = 80% of `max_wav_bytes` (safety margin).
///
/// For short audio that fits in one request, this is a transparent pass-through.
/// For long audio: splits at silence boundaries, transcribes each chunk sequentially,
/// concatenates results with spaces.
///
/// `on_progress` is called before sending each chunk: (current_1indexed, total_chunks).
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
        let payload = audio.to_wav_payload().map_err(|e| {
            crate::error::TranscribeError::Api {
                status: 0,
                body: e.to_string(),
            }
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

        let payload = chunk.to_wav_payload().map_err(|e| {
            crate::error::TranscribeError::Api {
                status: 0,
                body: format!("chunk {}/{}: {}", i + 1, total, e),
            }
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

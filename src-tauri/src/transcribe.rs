use base64::Engine;
use reqwest::multipart;

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
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
) -> Result<String, Box<dyn std::error::Error>> {
    // SECURITY: reject HTTP URLs to prevent API key leak over plaintext,
    // except localhost/127.0.0.1 for self-hosted setups
    if let Ok(parsed) = url::Url::parse(api_url) {
        if parsed.scheme() == "http" {
            let host = parsed.host_str().unwrap_or("");
            if host != "localhost" && host != "127.0.0.1" && host != "::1" {
                return Err("Refusing to send API key over HTTP. Use HTTPS or localhost.".into());
            }
        }
    }

    // Route to Deepgram-specific path
    if api_url.contains("deepgram.com") {
        return transcribe_audio_deepgram(api_url, api_key, wav_data, language).await;
    }

    // Route to Gemini-specific path if the URL points to googleapis.com
    if api_url.contains("googleapis.com") && api_url.contains("generateContent") {
        return transcribe_audio_gemini(api_url, api_key, wav_data, language).await;
    }

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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, body).into());
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
) -> Result<String, Box<dyn std::error::Error>> {
    let mut url = api_url.to_string();
    let sep = if url.contains('?') { "&" } else { "?" };
    if !language.is_empty() {
        url = format!("{}{}language={}", url, sep, language);
    } else {
        // Auto-detect language when none specified
        url = format!("{}{}detect_language=true", url, sep);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

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
        return Err(format!("Deepgram API error {}: {}", status, body).into());
    }

    let result: serde_json::Value = response.json().await?;
    let text = result["results"]["channels"][0]["alternatives"][0]["transcript"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err("Deepgram returned empty transcription".into());
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
) -> Result<String, Box<dyn std::error::Error>> {
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

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
        return Err(format!("Gemini API error {}: {}", status, body).into());
    }

    // Gemini response: { "candidates": [{ "content": { "parts": [{ "text": "..." }] } }] }
    let result: serde_json::Value = response.json().await?;
    let text = result["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err("Gemini returned empty transcription".into());
    }

    Ok(text)
}

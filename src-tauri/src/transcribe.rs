use reqwest::multipart;

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Send WAV audio to any OpenAI-compatible transcription endpoint.
/// `language` is an ISO 639-1 code (e.g. "it", "en"). Empty string = auto-detect.
pub async fn transcribe_audio(
    api_url: &str,
    model: &str,
    api_key: &str,
    wav_data: &[u8],
    language: &str,
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

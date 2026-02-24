/// LLM post-processing: OpenAI-compatible chat completions client.
///
/// Styles define *what* the LLM does with the text.
/// Tones modify *how* it writes the result.

/// (name, system prompt instruction)
pub const STYLES: &[(&str, &str)] = &[
    ("off", ""),
    ("correct", "Fix grammar, spelling, and punctuation errors. Remove filler words (um, uh, like, you know). Keep the original meaning and language. Do NOT translate. Output only the corrected text."),
    ("summarize", "Summarize the following text concisely, preserving the key points. Keep the same language as the input. Output only the summary."),
    ("elaborate", "Expand on the following text, adding detail and context while keeping the same meaning and language. Output only the elaborated text."),
    ("comprehensible", "Rewrite the following text to be clearer and easier to understand, while keeping the same meaning and language. Output only the rewritten text."),
    ("professional", "Rewrite the following text in a professional, polished tone suitable for business communication. Keep the same language. Output only the rewritten text."),
    ("custom", ""),
];

/// (name, system prompt modifier)
pub const TONES: &[(&str, &str)] = &[
    ("none", ""),
    ("formal", "Use a formal register and vocabulary."),
    ("friendly", "Use a warm, friendly, and approachable tone."),
    ("concise", "Be as brief as possible. Remove unnecessary words."),
    ("academic", "Use an academic, scholarly tone with precise language."),
];

/// Build the system prompt from a style + tone combination.
/// If style is "off", returns empty string (caller should skip LLM).
/// If style is "custom", uses `custom_prompt` instead of the style instruction.
pub fn build_system_prompt(style: &str, tone: &str, custom_prompt: &str) -> String {
    if style == "off" {
        return String::new();
    }

    let style_instruction = if style == "custom" {
        custom_prompt.to_string()
    } else {
        STYLES
            .iter()
            .find(|(name, _)| *name == style)
            .map(|(_, instr)| instr.to_string())
            .unwrap_or_default()
    };

    let tone_modifier = TONES
        .iter()
        .find(|(name, _)| *name == tone)
        .map(|(_, instr)| instr.to_string())
        .unwrap_or_default();

    if tone_modifier.is_empty() {
        style_instruction
    } else if style_instruction.is_empty() {
        tone_modifier
    } else {
        format!("{} {}", style_instruction, tone_modifier)
    }
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(serde::Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(serde::Deserialize)]
struct ChatMessage {
    content: String,
}

/// Send text to an OpenAI-compatible chat completions endpoint for processing.
pub async fn process_text(
    api_url: &str,
    model: &str,
    api_key: &str,
    text: &str,
    style: &str,
    tone: &str,
    custom_prompt: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let system_prompt = build_system_prompt(style, tone, custom_prompt);
    if system_prompt.is_empty() {
        return Ok(text.to_string());
    }

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

    let body = serde_json::json!({
        "model": model,
        "temperature": 0.3,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": text },
        ],
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let response = client
        .post(api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("LLM API error {}: {}", status, body).into());
    }

    let result: ChatResponse = response.json().await?;
    let content = result
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .unwrap_or_else(|| text.to_string());

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_returns_empty() {
        let prompt = build_system_prompt("off", "none", "");
        assert!(prompt.is_empty());
    }

    #[test]
    fn off_ignores_tone() {
        let prompt = build_system_prompt("off", "formal", "");
        assert!(prompt.is_empty());
    }

    #[test]
    fn correct_no_tone() {
        let prompt = build_system_prompt("correct", "none", "");
        assert!(prompt.contains("Fix grammar"));
        assert!(!prompt.contains("formal"));
    }

    #[test]
    fn correct_with_formal_tone() {
        let prompt = build_system_prompt("correct", "formal", "");
        assert!(prompt.contains("Fix grammar"));
        assert!(prompt.contains("formal"));
    }

    #[test]
    fn summarize_with_concise() {
        let prompt = build_system_prompt("summarize", "concise", "");
        assert!(prompt.contains("Summarize"));
        assert!(prompt.contains("brief"));
    }

    #[test]
    fn elaborate_with_friendly() {
        let prompt = build_system_prompt("elaborate", "friendly", "");
        assert!(prompt.contains("Expand"));
        assert!(prompt.contains("friendly"));
    }

    #[test]
    fn comprehensible_with_academic() {
        let prompt = build_system_prompt("comprehensible", "academic", "");
        assert!(prompt.contains("clearer"));
        assert!(prompt.contains("academic"));
    }

    #[test]
    fn professional_no_tone() {
        let prompt = build_system_prompt("professional", "none", "");
        assert!(prompt.contains("professional"));
    }

    #[test]
    fn custom_uses_custom_prompt() {
        let prompt = build_system_prompt("custom", "none", "Translate to Italian");
        assert_eq!(prompt, "Translate to Italian");
    }

    #[test]
    fn custom_with_tone() {
        let prompt = build_system_prompt("custom", "formal", "Translate to Italian");
        assert!(prompt.contains("Translate to Italian"));
        assert!(prompt.contains("formal"));
    }

    #[test]
    fn custom_empty_prompt_with_tone() {
        let prompt = build_system_prompt("custom", "friendly", "");
        assert!(prompt.contains("friendly"));
    }

    #[test]
    fn unknown_style_returns_tone_only() {
        let prompt = build_system_prompt("nonexistent", "formal", "");
        assert!(prompt.contains("formal"));
    }

    #[test]
    fn unknown_tone_returns_style_only() {
        let prompt = build_system_prompt("correct", "nonexistent", "");
        assert!(prompt.contains("Fix grammar"));
    }

    #[test]
    fn all_styles_covered() {
        let names: Vec<&str> = STYLES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"off"));
        assert!(names.contains(&"correct"));
        assert!(names.contains(&"summarize"));
        assert!(names.contains(&"elaborate"));
        assert!(names.contains(&"comprehensible"));
        assert!(names.contains(&"professional"));
        assert!(names.contains(&"custom"));
    }

    #[test]
    fn all_tones_covered() {
        let names: Vec<&str> = TONES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"none"));
        assert!(names.contains(&"formal"));
        assert!(names.contains(&"friendly"));
        assert!(names.contains(&"concise"));
        assert!(names.contains(&"academic"));
    }
}

//! TIER A — live model smoke test (manual, NOT CI).
//!
//! Drives the REAL `llm::process_raw_prompt` (the exact code path the recap +
//! command paths use) against every cloud LLM/recap model in
//! `assets/model-catalog.json`, with your API keys read from the repo `.env`
//! (or the environment). For each model it asserts the model id AND Dimmy's
//! exact request shape are accepted by the provider and produce a response —
//! this is what catches the bug classes we keep hitting: a 404 wrong id
//! (bare `gpt-5.4` / `gemini-3.1-pro`), a 400 wrong parameter (OpenAI gpt-5
//! wanting `max_completion_tokens`), a model that only exists on another API
//! (`gpt-5-pro` → v1/responses), etc.
//!
//! Because it drives the production function (not a Python reimplementation),
//! the request body can never drift from what users actually send.
//!
//! It is `#[ignore]` so CI never runs it (needs network + keys + spends a few
//! tokens). Run it occasionally — when refreshing the model catalog or before
//! cutting an rc:
//!
//!     cargo test --test live_models -- --ignored --nocapture
//!
//! Keys (only the providers you have are tested): the repo `.env` is parsed
//! for OPENAI_KEY / ANTHROPIC_KEY / GEMINI_KEY / GROQ_KEY / TOGETHER_API_KEY /
//! FIREWORKS_API_KEY (and the *_API_KEY aliases). A provider with no key is
//! skipped with a note.

use std::collections::HashMap;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Parse the repo `.env` (and the live process env) into a name→value map.
fn load_keys() -> HashMap<String, String> {
    let mut map: HashMap<String, String> = std::env::vars().collect();
    if let Ok(text) = std::fs::read_to_string(repo_root().join(".env")) {
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || !line.contains('=') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            if let Some((name, value)) = line.split_once('=') {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                map.entry(name.trim().to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
    }
    map
}

fn key_for(provider: &str, keys: &HashMap<String, String>) -> Option<String> {
    let names: &[&str] = match provider {
        "openai" => &["OPENAI_API_KEY", "OPENAI_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY", "ANTHROPIC_KEY"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY", "GEMINI_KEY"],
        "groq" => &["GROQ_API_KEY", "GROQ_KEY"],
        "together" => &["TOGETHER_API_KEY", "TOGETHER_KEY"],
        "fireworks" => &["FIREWORKS_API_KEY", "FIREWORKS_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY", "OPENROUTER_KEY"],
        _ => &[],
    };
    names
        .iter()
        .find_map(|n| keys.get(*n).filter(|v| !v.is_empty()).cloned())
}

#[test]
#[ignore = "live: needs API keys + network; run manually with --ignored"]
fn every_catalog_llm_model_accepts_our_request_shape() {
    let keys = load_keys();
    let catalog: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("assets/model-catalog.json"))
            .expect("read model-catalog.json"),
    )
    .expect("parse model-catalog.json");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut failures: Vec<String> = Vec::new();
    let mut tested = 0usize;

    for prov in catalog["providers"].as_array().unwrap() {
        let pid = prov["id"].as_str().unwrap();
        let url = prov["llm_url"].as_str().unwrap_or("");
        if url.is_empty() {
            continue; // STT-only provider (e.g. deepgram) — no chat path
        }
        let key = match key_for(pid, &keys) {
            Some(k) => k,
            None => {
                eprintln!("· {pid}: no key in .env — skipped");
                continue;
            }
        };
        for m in prov["models"].as_array().unwrap() {
            let tasks = m["tasks"].as_array().unwrap();
            let is_chat = tasks.iter().any(|t| t == "llm" || t == "recap");
            if !is_chat {
                continue; // stt-only model
            }
            let model = m["id"].as_str().unwrap();
            tested += 1;
            // Generous budget: reasoning models spend tokens before output;
            // a tight budget makes OpenAI gpt-5 return a 400, which would be a
            // false failure. 2048 leaves room for "ok".
            //
            // Retry transient CONNECTION errors (this sweep opens ~30 fresh
            // TLS connections back-to-back; Windows ephemeral-port pressure +
            // Cloudflare connection throttling cause sporadic "error sending
            // request" that are not model/shape problems). A real 4xx (bad id
            // / bad param) is surfaced immediately — it isn't a Network error.
            let mut res = Err(dimmy_lib::error::LlmError::Network("init".into()));
            for attempt in 0..4 {
                std::thread::sleep(std::time::Duration::from_millis(400));
                res = rt.block_on(dimmy_lib::llm::process_raw_prompt(
                    url,
                    model,
                    &key,
                    "Reply with exactly the word: ok",
                    2048,
                    "api_key",
                ));
                // Transient = transport error, 429 rate-limit, or 5xx. A real
                // 4xx (404 bad id / 400 bad param) is NOT retried — that's the
                // signal we want to surface.
                let transient = matches!(
                    &res,
                    Err(dimmy_lib::error::LlmError::Network(_))
                        | Err(dimmy_lib::error::LlmError::Api { status: 429, .. })
                        | Err(dimmy_lib::error::LlmError::Api {
                            status: 500..=599,
                            ..
                        })
                );
                match &res {
                    Ok(_) => break,
                    Err(_) if transient && attempt < 3 => {
                        std::thread::sleep(std::time::Duration::from_millis(
                            1500 * (attempt as u64 + 1),
                        ));
                    }
                    Err(_) => break,
                }
            }
            match res {
                Ok(text) => {
                    let snippet: String = text.chars().take(40).collect();
                    eprintln!("✓ {pid}/{model} -> {snippet:?}");
                }
                Err(e) => {
                    eprintln!("✗ {pid}/{model} -> {e}");
                    failures.push(format!("{pid}/{model}: {e}"));
                }
            }
        }
    }

    eprintln!(
        "\n{tested} chat models tested, {} failure(s)",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "live model failures (fix the catalog id or the request shape):\n{}",
        failures.join("\n")
    );
}

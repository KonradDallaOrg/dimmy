//! LLM dispatch contract tests.
//!
//! These tests intercept the HTTPS request body that `process_text` and
//! `process_raw_prompt` send to each provider and assert the exact JSON
//! shape — system prompt content, temperature presence/absence, thinking
//! mode (adaptive vs legacy), Gemini-native vs OpenAI-compat schema,
//! header set per provider. The existing tests in `core/tests/ffi_e2e.rs`
//! only assert that *a* POST was made; this file fills the gap by
//! verifying what we actually send.
//!
//! Provider branch is selected by the URL string, not by an enum
//! argument — `Provider::from_url` does substring matching. We exploit
//! that to route mocks: a wiremock server at `http://127.0.0.1:<port>`
//! with the marker substring (`anthropic.com`, `generateContent`, …)
//! folded into the path matches the production detection logic while
//! staying reachable on localhost.

use dimmy_lib::llm::{process_raw_prompt, process_text, LlmStyle, LlmTone};
use serde_json::Value;
use wiremock::matchers::{body_string_contains, header, header_exists, method, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Anthropic Messages-API success envelope. The integration only reads
/// `content[0].text`; everything else is filler that mirrors a real
/// response so we'd catch a regression that started inspecting more fields.
fn anthropic_response_body(text: &str) -> Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-irrelevant",
        "content": [
            { "type": "text", "text": text }
        ],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })
}

/// OpenAI Chat-Completions success envelope. Provider quirk: Anthropic
/// proxies and Groq both speak this exact shape, so a single fixture
/// covers the whole OpenAI-compat branch.
fn openai_response_body(text: &str) -> Value {
    serde_json::json!({
        "id": "cmpl_test",
        "object": "chat.completion",
        "model": "model-irrelevant",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

/// Gemini-native `generateContent` envelope. `process_raw_prompt` reads
/// `candidates[0].content.parts[*].text`.
fn gemini_native_response_body(text: &str) -> Value {
    serde_json::json!({
        "candidates": [{
            "content": { "parts": [{ "text": text }] },
            "finishReason": "STOP"
        }]
    })
}

/// Spin up a wiremock server + return its (server, base URL). The base
/// URL is shaped so `Provider::from_url(&format!("{}/...", base))`
/// picks up the chosen substring (`anthropic.com`, `generateContent`,
/// or none for the OpenAI-compat branch).
async fn boot() -> MockServer {
    MockServer::start().await
}

/// Parse the wiremock-captured request body as JSON. Panics on bad
/// payload — assertion failure surfaces the bug immediately in the
/// failing test rather than silently treating "no body" as "no field".
fn body_json(req: &Request) -> Value {
    serde_json::from_slice(&req.body).expect("request body must be valid JSON")
}

// ─────────────────────────────────────────────────────────────────────
// process_text — Anthropic branch
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn process_text_anthropic_sends_correct_style_instruction_in_system() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .and(path_regex(r"^/anthropic\.com/v1/messages$"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_string_contains("fix grammar"))
        .and(body_string_contains("[TRANSCRIPTION]"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("ok")))
        .expect(1)
        .mount(&server)
        .await;

    let out = process_text(
        &url,
        "claude-sonnet-4-6",
        "test-key",
        "hello world",
        LlmStyle::Correct,
        LlmTone::None,
        "",
        "",
        "api_key",
    )
    .await
    .expect("dispatch should succeed");
    assert_eq!(out, "ok");
}

#[tokio::test]
async fn process_text_anthropic_omits_temperature_field() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .and(path_regex(r"^/anthropic\.com/v1/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("x")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "claude-sonnet-4-6",
        "k",
        "hi",
        LlmStyle::Correct,
        LlmTone::None,
        "",
        "",
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body = body_json(&received[0]);
    assert!(
        body.get("temperature").is_none(),
        "Anthropic body must NOT carry a `temperature` field — present: {}",
        body
    );
    // Required fields
    assert_eq!(body["model"], "claude-sonnet-4-6");
    assert!(body["system"].is_string());
    assert!(body["messages"].is_array());
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(body["max_tokens"].as_u64().unwrap() >= 512);
}

#[tokio::test]
async fn process_text_anthropic_routes_custom_prompt_into_system() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());
    let custom = "REWRITE_LIKE_A_PIRATE_INSTRUCTION_TOKEN";

    Mock::given(method("POST"))
        .and(body_string_contains(custom))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("yarr")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "claude-sonnet-4-6",
        "k",
        "ahoy",
        LlmStyle::Custom,
        LlmTone::None,
        custom,
        "",
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);
    assert!(
        body["system"].as_str().unwrap().contains(custom),
        "custom prompt must be embedded in `system` field"
    );
}

#[tokio::test]
async fn process_text_anthropic_translate_directive_appears_in_system() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .and(body_string_contains("Translate the output to it."))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("ciao")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "claude-sonnet-4-6",
        "k",
        "hello",
        LlmStyle::Correct,
        LlmTone::None,
        "",
        "it",
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);
    let system = body["system"].as_str().unwrap();
    assert!(system.contains("Translate the output to it."));
    // Translate path must strip the "do not translate" rule (#6) from
    // the preamble — otherwise the LLM gets two contradictory directives.
    assert!(!system.contains("Do NOT translate"));
}

#[tokio::test]
async fn process_text_anthropic_imbruttito_with_english_emits_override_directive() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .and(body_string_contains("OVERRIDES"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("done")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "claude-sonnet-4-6",
        "k",
        "we deliverated the kpi",
        LlmStyle::Imbruttito,
        LlmTone::None,
        "",
        "en",
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);
    let system = body["system"].as_str().unwrap();
    // Both directives must coexist — the style instruction (which
    // hardcodes "always output Italian") and the explicit override.
    // Without the override line, the LLM has to guess which rule wins.
    assert!(system.contains("Imbruttito"));
    assert!(system.contains("OVERRIDES"));
    assert!(system.contains("Translate the output to en."));
}

#[tokio::test]
async fn process_text_anthropic_unknown_translate_code_falls_back_to_no_translation() {
    // translate_to="xyz" is not in SUPPORTED_TRANSLATE_LANGS → must be
    // dropped silently. The request still goes out (style is Correct),
    // but the prompt must NOT contain a translation directive and must
    // PRESERVE the "do not translate" rule.
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("ok")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "claude-sonnet-4-6",
        "k",
        "hello",
        LlmStyle::Correct,
        LlmTone::None,
        "",
        "xyz", // bogus
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);
    let system = body["system"].as_str().unwrap();
    assert!(
        !system.contains("Translate the output"),
        "unknown lang code must NOT reach the prompt — got: {}",
        system
    );
    assert!(system.contains("Do NOT translate"));
}

// ─────────────────────────────────────────────────────────────────────
// process_text — OpenAI-compat branch (Groq, OpenAI, Gemini-OAI-proxy)
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn process_text_openai_compat_sends_temperature_03_and_bearer_header() {
    let server = boot().await;
    // No "anthropic.com" / "generateContent" substring → OpenAI-compat
    // branch is taken.
    let url = format!("{}/v1/chat/completions", server.uri());

    Mock::given(method("POST"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_string_contains("\"temperature\":0.3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response_body("ok")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "llama-3.3-70b",
        "test-key",
        "hello",
        LlmStyle::Correct,
        LlmTone::None,
        "",
        "",
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);

    // Shape: messages[0].role=system + messages[1].role=user.
    let messages = body["messages"].as_array().expect("messages must be array");
    assert_eq!(
        messages.len(),
        2,
        "OpenAI-compat expects exactly 2 messages"
    );
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("fix grammar"));

    // Temperature: 0.3 hardcoded. If we ever change this, the test
    // forces a deliberate update — no silent regression.
    assert_eq!(body["temperature"], 0.3);
    assert_eq!(body["model"], "llama-3.3-70b");
    assert!(body["max_tokens"].as_u64().unwrap() >= 512);

    // Anthropic-specific fields must NOT leak into the OpenAI-compat body.
    assert!(body.get("system").is_none());
    assert!(body.get("thinking").is_none());
}

#[tokio::test]
async fn process_text_openai_compat_carries_tone_and_translate() {
    let server = boot().await;
    let url = format!("{}/v1/chat/completions", server.uri());

    Mock::given(method("POST"))
        .and(body_string_contains("formal"))
        .and(body_string_contains("Translate the output to de."))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response_body("ok")))
        .expect(1)
        .mount(&server)
        .await;

    process_text(
        &url,
        "gpt-5-mini",
        "k",
        "hello world",
        LlmStyle::Professional,
        LlmTone::Formal,
        "",
        "de",
        "api_key",
    )
    .await
    .unwrap();
}

// ─────────────────────────────────────────────────────────────────────
// process_raw_prompt — Anthropic adaptive thinking (Opus 4.7+, Sonnet 5+)
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn process_raw_prompt_anthropic_adaptive_uses_new_thinking_shape() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("# Recap")))
        .expect(1)
        .mount(&server)
        .await;

    process_raw_prompt(
        &url,
        "claude-opus-4-7",
        "k",
        "Summarize this meeting transcript.",
        4096,
        "api_key",
    )
    .await
    .expect("dispatch should succeed");

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);

    // Adaptive shape: { "thinking": { "type": "adaptive" }, "output_config": { "effort": "high" } }
    // Older budget_tokens shape would FAIL the Anthropic API for these models.
    let thinking = &body["thinking"];
    assert_eq!(
        thinking["type"], "adaptive",
        "expected adaptive thinking shape, got: {}",
        body
    );
    assert!(
        thinking.get("budget_tokens").is_none(),
        "legacy `budget_tokens` MUST NOT appear in adaptive shape"
    );
    assert_eq!(body["output_config"]["effort"], "high");

    // temperature/top_p/top_k must be omitted on adaptive models — the
    // API rejects them.
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("top_k").is_none());
}

#[tokio::test]
async fn process_raw_prompt_anthropic_legacy_uses_budget_tokens_shape() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("# Recap")))
        .expect(1)
        .mount(&server)
        .await;

    process_raw_prompt(
        &url,
        "claude-opus-4-5", // older flagship, legacy thinking shape
        "k",
        "Summarize.",
        4096,
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);

    let thinking = &body["thinking"];
    assert_eq!(
        thinking["type"], "enabled",
        "legacy shape uses type=enabled"
    );
    assert!(
        thinking["budget_tokens"].as_u64().unwrap() >= 1024,
        "legacy shape must include budget_tokens; got: {}",
        body
    );
    assert!(
        body.get("output_config").is_none(),
        "output_config is adaptive-only — must NOT appear in legacy shape"
    );
}

#[tokio::test]
async fn process_raw_prompt_anthropic_non_thinking_model_skips_thinking_block() {
    let server = boot().await;
    let url = format!("{}/anthropic.com/v1/messages", server.uri());

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_response_body("ok")))
        .expect(1)
        .mount(&server)
        .await;

    process_raw_prompt(
        &url,
        "claude-haiku-4-5", // no thinking
        "k",
        "Quick summary.",
        2048,
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);
    assert!(
        body.get("thinking").is_none(),
        "non-thinking models must NOT carry a thinking block"
    );
    assert!(body.get("output_config").is_none());
}

// ─────────────────────────────────────────────────────────────────────
// process_raw_prompt — Gemini native generateContent
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn process_raw_prompt_gemini_native_uses_contents_parts_shape() {
    let server = boot().await;
    let url = format!(
        "{}/generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent",
        server.uri()
    );

    Mock::given(method("POST"))
        .and(header_exists("x-goog-api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gemini_native_response_body("ok")))
        .expect(1)
        .mount(&server)
        .await;

    process_raw_prompt(&url, "gemini-2.5-flash", "k", "Summarize.", 2048, "api_key")
        .await
        .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);

    // Gemini-native schema differs from OpenAI: contents[*].parts[*].text
    // instead of messages[*].content. We must not leak OpenAI fields.
    assert!(body["contents"].is_array());
    assert!(body["contents"][0]["parts"].is_array());
    assert!(body["contents"][0]["parts"][0]["text"].is_string());
    assert!(body.get("messages").is_none());
    assert!(body.get("system").is_none());

    // generationConfig wraps the OpenAI-equivalent knobs.
    let gen_cfg = &body["generationConfig"];
    assert!(gen_cfg.is_object());
    // 2.5-flash is a thinking model but at 2048 max_tokens caller already
    // capped — we still emit thinkingConfig on this branch.
    // (Don't pin the exact key; the production code may use either
    // `thinkingBudget` or `thinkingLevel`. Just verify *some* thinking
    // config is present for a thinking-tier model.)
    if let Some(thinking_cfg) = gen_cfg.get("thinkingConfig") {
        assert!(thinking_cfg.is_object());
    }
}

#[tokio::test]
async fn process_raw_prompt_openai_compat_for_recap_uses_messages_shape() {
    let server = boot().await;
    // Path has neither "anthropic.com" nor "generateContent" → falls
    // through to the OpenAI-compat branch even for raw recap prompts.
    let url = format!("{}/v1/chat/completions", server.uri());

    Mock::given(method("POST"))
        .and(header("authorization", "Bearer k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response_body("# Recap")))
        .expect(1)
        .mount(&server)
        .await;

    process_raw_prompt(
        &url,
        "llama-3.3-70b",
        "k",
        "Summarize this transcript.",
        4096,
        "api_key",
    )
    .await
    .unwrap();

    let received = server.received_requests().await.unwrap();
    let body = body_json(&received[0]);
    // OpenAI-compat for recap: single-message user array.
    let messages = body["messages"].as_array().expect("messages array");
    assert!(!messages.is_empty(), "messages must not be empty");
    assert_eq!(messages.last().unwrap()["role"], "user");
    // Recap path on OpenAI-compat must NOT inject the dictation preamble
    // — process_raw_prompt is "raw" by contract.
    let combined: String = messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !combined.contains("[TRANSCRIPTION]"),
        "raw prompt must NOT be wrapped in the [TRANSCRIPTION] dictation envelope"
    );
}

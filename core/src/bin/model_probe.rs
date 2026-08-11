//! Live model discovery + verification against the real provider APIs.
//!
//! Two modes:
//!
//!   list [provider]   GET each provider's /models endpoint with the user's
//!                     own key and print the ids the account can actually
//!                     reach. This is how new model ids get discovered —
//!                     never by guessing a name from a changelog.
//!
//!   probe [provider]  For every catalog model tagged `llm`/`recap`, send a
//!                     one-word request through the SAME production path the
//!                     recap and command-mode features use
//!                     (`llm::process_raw_prompt`). That matters: a
//!                     hand-written curl passes on a model whose *thinking*
//!                     shape our request builder gets wrong, because curl
//!                     doesn't run our builder. This does.
//!
//! Read-only w.r.t. the keystore. Never prints a key.
//!
//! Run with a SEPARATE target dir so the cdylib is not re-emitted over the
//! `dimmy_lib.dll` the installed app is using:
//!   CARGO_TARGET_DIR=E:\probe cargo run --release --bin model_probe -- list

use dimmy_lib::provider::{KeyringScope, Provider};
use serde_json::Value;

/// Cheap, unambiguous probe. Any healthy chat model answers this in a
/// handful of tokens; an EMPTY reply means the request shape was wrong
/// even when HTTP said 200 — the gpt-5 `max_completion_tokens` failure
/// mode, and the adaptive-thinking one.
const PROMPT: &str = "Reply with exactly one word: OK";

fn key_for(store: &dimmy_lib::keystore::KeyStore, provider: Provider) -> String {
    // Same resolution order as ffi.rs: the vendor's LLM scope, then the
    // SAME vendor's STT key (one key per provider serves both).
    store
        .load_key(KeyringScope::Llm(provider), false)
        .or_else(|| store.load_key(KeyringScope::Stt(provider), false))
        .unwrap_or_default()
}

/// Ask a provider what it will actually serve this account.
async fn list_models(client: &reqwest::Client, pid: &str, url: &str, auth: &str, key: &str) {
    let req = match auth {
        "anthropic" => client
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "gemini" => client.get(format!("{url}?key={key}&pageSize=200")),
        "deepgram" => client
            .get(url)
            .header("Authorization", format!("Token {key}")),
        _ => client.get(url).bearer_auth(key),
    };

    match req.send().await {
        Err(e) => println!("  [{pid}] request failed: {e}"),
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                let head: String = body.chars().take(200).collect();
                println!("  [{pid}] HTTP {status}: {head}");
                return;
            }
            let v: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => {
                    println!("  [{pid}] unparseable response: {e}");
                    return;
                }
            };
            // openai/groq/together/fireworks/openrouter -> {data:[{id}]}
            // anthropic -> {data:[{id}]}, gemini -> {models:[{name}]}
            // Together answers with a BARE array; everyone else wraps it.
            let mut ids: Vec<String> = Vec::new();
            let arrays = [v.as_array(), v["data"].as_array(), v["models"].as_array()];
            for arr in arrays.into_iter().flatten() {
                for m in arr {
                    if let Some(id) = m["id"].as_str().or_else(|| m["name"].as_str()) {
                        ids.push(id.trim_start_matches("models/").to_string());
                    }
                }
            }
            ids.sort();
            println!("  [{pid}] {} models:", ids.len());
            for id in ids {
                println!("      {id}");
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let catalog: Value = serde_json::from_str(dimmy_lib::catalog::MODEL_CATALOG_JSON)
        .expect("embedded catalog must be valid JSON");
    let store = dimmy_lib::keystore::KeyStore::new();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("http client");

    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "probe".to_string());
    let only = std::env::args().nth(2);

    let mut pass = 0usize;
    let mut fail: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for p in catalog["providers"].as_array().unwrap() {
        let pid = p["id"].as_str().unwrap_or_default();
        if let Some(f) = &only {
            if pid != f {
                continue;
            }
        }
        let llm_url = p["llm_url"].as_str().unwrap_or_default();
        let stt_url = p["stt_url"].as_str().unwrap_or_default();
        let any_url = if llm_url.is_empty() { stt_url } else { llm_url };
        if any_url.is_empty() {
            continue;
        }
        let provider = Provider::from_url(any_url);
        let key = key_for(&store, provider);

        if mode == "list" {
            if key.is_empty() {
                println!("  [{pid}] no key stored — skipped");
                continue;
            }
            let endpoint = p["models_endpoint"].as_str().unwrap_or_default();
            if endpoint.is_empty() {
                println!("  [{pid}] no models_endpoint in catalog");
                continue;
            }
            let auth = p["auth"].as_str().unwrap_or("bearer");
            list_models(&client, pid, endpoint, auth, &key).await;
            continue;
        }

        // ---- probe mode ----
        if llm_url.is_empty() {
            continue; // STT-only vendor (deepgram)
        }
        for m in p["models"].as_array().unwrap() {
            let id = m["id"].as_str().unwrap_or_default();
            let tasks: Vec<&str> = m["tasks"]
                .as_array()
                .map(|a| a.iter().filter_map(|t| t.as_str()).collect())
                .unwrap_or_default();
            if !tasks.contains(&"llm") && !tasks.contains(&"recap") {
                continue; // STT model — different code path, probed elsewhere
            }
            if key.is_empty() {
                skipped.push(format!("{pid}/{id} (no key)"));
                continue;
            }

            print!("{pid:<11} {id:<48} ");
            use std::io::Write;
            let _ = std::io::stdout().flush();

            match dimmy_lib::llm::process_raw_prompt(llm_url, id, &key, PROMPT, 2048, "api_key")
                .await
            {
                Ok(reply) => {
                    let trimmed: &str = reply.trim();
                    if trimmed.is_empty() {
                        println!("EMPTY REPLY (HTTP ok, unusable)");
                        fail.push(format!("{pid}/{id}: empty reply"));
                    } else {
                        let preview: String = trimmed.chars().take(40).collect();
                        println!("ok -> {preview:?}");
                        pass += 1;
                    }
                }
                Err(e) => {
                    // LlmError's Display deliberately hides the body (house
                    // rule: never log a response that could echo a key).
                    // Here we're a local diagnostic, and the body is the
                    // only thing that says WHY a 400 happened.
                    let msg = match &e {
                        dimmy_lib::error::LlmError::Api { status, body } => {
                            format!("HTTP {status}: {body}")
                        }
                        other => format!("{other}"),
                    };
                    let msg = msg.replace('\n', " ");
                    let msg: String = msg.chars().take(230).collect();
                    println!("FAIL: {msg}");
                    fail.push(format!("{pid}/{id}: {msg}"));
                }
            }
        }
    }

    if mode != "list" {
        println!(
            "\n===== {pass} ok, {} failed, {} skipped =====",
            fail.len(),
            skipped.len()
        );
        for f in &fail {
            println!("  FAIL  {f}");
        }
        for s in &skipped {
            println!("  skip  {s}");
        }
        if !fail.is_empty() {
            std::process::exit(1);
        }
    }
}

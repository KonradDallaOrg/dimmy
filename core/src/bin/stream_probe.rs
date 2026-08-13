//! Does streaming the recap actually buy anything? Measures time-to-first-
//! token against total time on a realistic summarisation prompt, and checks
//! whether stream:true unlocks the models that refuse non-streaming.
//!
//! CARGO_TARGET_DIR=E:\probe cargo run --release --bin stream_probe

use dimmy_lib::provider::{KeyringScope, Provider};

use std::time::Instant;

const TRANSCRIPT: &str = "\
Marco: allora, la migrazione delle API. Abbiamo tre servizi che ancora puntano al vecchio endpoint.
Giulia: quali esattamente? Il billing l'ho spostato la settimana scorsa.
Marco: billing no, e' fatto. Restano notifiche, export e il job notturno di riconciliazione.
Giulia: il job notturno e' quello che mi preoccupa, gira alle tre e nessuno lo guarda.
Marco: propongo di spostarlo per ultimo, dopo che gli altri due sono stabili da una settimana.
Giulia: d'accordo. Io mi prendo notifiche, tu export?
Marco: si. Scadenza fine mese per entrambi, poi il job a meta' del mese dopo.
Giulia: serve avvisare il team di supporto, useranno il vecchio formato nei ticket.
Marco: vero, mando io una nota. Ultima cosa: teniamo il vecchio endpoint in sola lettura per un mese.";

async fn probe(label: &str, url: &str, model: &str, key: &str, stream: bool) {
    let prompt = format!(
        "Riassumi questa riunione in markdown: titolo H1, contesto, decisioni, azioni con owner.\n\n{TRANSCRIPT}"
    );
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 1200,
        "stream": stream,
    });
    let client = reqwest::Client::new();
    let t0 = Instant::now();
    let resp = match client.post(url).bearer_auth(key).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            println!(
                "{label:<34} {}: request error {e}",
                if stream { "STREAM" } else { "BATCH " }
            );
            return;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let b = resp.text().await.unwrap_or_default();
        let b: String = b.chars().filter(|c| *c != '\n').take(110).collect();
        println!(
            "{label:<34} {}: HTTP {status} {b}",
            if stream { "STREAM" } else { "BATCH " }
        );
        return;
    }

    if !stream {
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
        println!(
            "{label:<34} BATCH : first={:>6.1}s total={:>6.1}s chars={}",
            t0.elapsed().as_secs_f32(),
            t0.elapsed().as_secs_f32(),
            text.len()
        );
        return;
    }

    // SSE: each `data: {json}` line carries choices[0].delta.content
    let mut first: Option<f32> = None;
    let mut chars = 0usize;
    let mut buf = String::new();
    // `chunk()` needs no reqwest "stream" feature — worth knowing, since the
    // real implementation then needs no new dependency either.
    let mut resp = resp;
    while let Ok(Some(bytes)) = resp.chunk().await {
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            let line = line.trim();
            let Some(payload) = line.strip_prefix("data: ") else {
                continue;
            };
            if payload == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
                continue;
            };
            if let Some(d) = v["choices"][0]["delta"]["content"].as_str() {
                if !d.is_empty() {
                    if first.is_none() {
                        first = Some(t0.elapsed().as_secs_f32());
                    }
                    chars += d.len();
                }
            }
        }
    }
    println!(
        "{label:<34} STREAM: first={:>6.1}s total={:>6.1}s chars={}",
        first.unwrap_or(-1.0),
        t0.elapsed().as_secs_f32(),
        chars
    );
}

#[tokio::main]
async fn main() {
    let store = dimmy_lib::keystore::KeyStore::new();
    let k = |p: Provider| {
        store
            .load_key(KeyringScope::Llm(p), false)
            .or_else(|| store.load_key(KeyringScope::Stt(p), false))
            .unwrap_or_default()
    };
    let fw = "https://api.fireworks.ai/inference/v1/chat/completions";
    let tg = "https://api.together.ai/v1/chat/completions";
    let (kfw, ktg) = (k(Provider::Fireworks), k(Provider::Together));

    // The latency question: same model, both modes.
    probe(
        "fireworks/kimi-k3",
        fw,
        "accounts/fireworks/models/kimi-k3",
        &kfw,
        false,
    )
    .await;
    probe(
        "fireworks/kimi-k3",
        fw,
        "accounts/fireworks/models/kimi-k3",
        &kfw,
        true,
    )
    .await;

    // The unlock question: Together refuses these without stream:true.
    probe(
        "together/Qwen3.7-Plus",
        tg,
        "Qwen/Qwen3.7-Plus",
        &ktg,
        false,
    )
    .await;
    probe("together/Qwen3.7-Plus", tg, "Qwen/Qwen3.7-Plus", &ktg, true).await;
}

//! llm_style_matrix — every pill enhancement style, on every local model,
//! over REAL dictations.
//!
//! Until 2026-09-04 the local sampler ran with a repetition penalty of 106
//! (it should be ~1.1), so every judgement ever formed about these models and
//! these prompts was formed on a broken system — including the decision to
//! demote Gemma in favour of Phi-4 Mini. This re-measures from scratch.
//!
//! Input is `%TEMP%/llmbench/samples.json`, drawn from the user's own history
//! rather than invented: invented text is always cleaner than dictation, and
//! cleaning up dictation is the entire job.
//!
//! ONE MODEL PER PROCESS: `LlamaBackend::init()` refuses a second call
//! (BackendAlreadyInitialized), so switching models in-process fails on the
//! second one. The caller loops.
//!
//! Usage:
//!   llm_style_matrix <model-label> [--translate]

use dimmy_lib::llm::{LlmStyle, LlmTone};
use std::path::PathBuf;
use std::time::Instant;

#[derive(serde::Deserialize)]
struct Sample {
    text: String,
    lang: String,
    #[allow(dead_code)]
    wc: i64,
}

fn models() -> Vec<(&'static str, PathBuf)> {
    let dir = dimmy_lib::local_llm::llm_model_directory();
    let mut v: Vec<(&'static str, PathBuf)> = vec![
        ("gemma-QAT", dir.join("gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf")),
        ("gemma-Q4", dir.join("gemma-4-E2B-it-Q4_K_M.gguf")),
        ("phi-4-mini", dir.join("phi-4-mini-instruct-q4_k_m.gguf")),
    ];
    let qwen = PathBuf::from(r"E:\llm-bench\qwen3-4b\Qwen3-4B-Instruct-2507-Q4_K_M.gguf");
    if qwen.is_file() {
        v.push(("qwen3-4B", qwen));
    }
    // TranslateGemma: a Gemma 3 fine-tuned for translation only. Same
    // architecture we already load, so it costs nothing to try.
    let tg = PathBuf::from(r"E:\llm-bench\tg\translategemma-4b-it.Q4_K_M.gguf");
    if tg.is_file() {
        v.push(("translategemma", tg));
    }
    v.retain(|(_, p)| p.is_file());
    v
}

fn styles() -> Vec<(&'static str, LlmStyle, LlmTone)> {
    vec![
        ("Correct", LlmStyle::Correct, LlmTone::None),
        ("Correct+Formal", LlmStyle::Correct, LlmTone::Formal),
        ("Summarize", LlmStyle::Summarize, LlmTone::Concise),
        ("Elaborate", LlmStyle::Elaborate, LlmTone::None),
        ("Comprehensible", LlmStyle::Comprehensible, LlmTone::None),
        ("Professional", LlmStyle::Professional, LlmTone::Formal),
        ("Prompt", LlmStyle::Prompt, LlmTone::None),
        ("Imbruttito", LlmStyle::Imbruttito, LlmTone::None),
    ]
}

fn one_line(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(n).collect()
}

fn main() {
    let translate = std::env::args().any(|a| a == "--translate");
    let dir = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
    // DIMMY_SAMPLES picks the phrase set. Tuning and validating on the same
    // six phrases would only prove the prompts had been fitted to those six;
    // the second set is drawn from different dictations, spread across three
    // length bands on purpose.
    let file = std::env::var("DIMMY_SAMPLES").unwrap_or_else(|_| "samples.json".into());
    let raw = std::fs::read_to_string(format!("{dir}/llmbench/{file}"))
        .unwrap_or_else(|_| panic!("{file} — generate it first"));
    let samples: Vec<Sample> = serde_json::from_str(&raw).expect("samples.json parse");

    let want = std::env::args()
        .nth(1)
        .filter(|a| !a.starts_with("--"))
        .unwrap_or_default();

    for (name, path) in models() {
        if !want.is_empty() && name != want {
            continue;
        }
        eprintln!("\n##### {name} #####");
        if translate {
            // Only the first sample: what matters is whether the model CAN
            // switch language at all, not how it handles six of them.
            let s = &samples[0];
            for target in ["en", "es", "de", "fr"] {
                let t = Instant::now();
                match dimmy_lib::local_llm::process_text_local(
                    &path,
                    &s.text,
                    LlmStyle::Correct,
                    LlmTone::None,
                    "",
                    target,
                    &s.lang,
                ) {
                    Ok(out) => println!(
                        "{name}\t{}->{target}\t{:.1}s\t{}",
                        s.lang,
                        t.elapsed().as_secs_f64(),
                        one_line(&out, 240)
                    ),
                    Err(e) => println!("{name}\t{}->{target}\tERR\t{e}", s.lang),
                }
            }
            continue;
        }
        // Every style over the SAME phrases: a style judged on one phrase is
        // judged on nothing, and holding the set fixed is what makes two
        // styles -- or two models -- comparable at all.
        for (label, style, tone) in styles() {
            for (i, s) in samples.iter().enumerate() {
                let t = Instant::now();
                match dimmy_lib::local_llm::process_text_local(
                    &path, &s.text, style, tone, "", "", &s.lang,
                ) {
                    Ok(out) => println!(
                        "{name}	{label}	{i}	{:.1}	{}",
                        t.elapsed().as_secs_f64(),
                        one_line(&out, 300)
                    ),
                    Err(e) => println!("{name}	{label}	{i}	ERR	{e}"),
                }
            }
        }
    }
}

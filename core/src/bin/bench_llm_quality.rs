//! bench_llm_quality — Compare local LLM vs cloud on all styles + translation.
//!
//! Usage:
//!   cargo run --release --features local-llm-vulkan --bin bench_llm_quality

use std::time::Instant;

fn main() {
    let model_path = dimmy_lib::local_llm::model_path(dimmy_lib::local_llm::DEFAULT_LLM_MODEL);
    if !model_path.is_file() {
        eprintln!("Model not found: {}", model_path.display());
        std::process::exit(1);
    }

    let text_it = "allora praticamente ieri sono andato dal meccanico e mi ha detto che la macchina ha un problema al motore cioè il filtro dell olio è da cambiare e anche le pasticche dei freni sono consumate insomma devo spendere tipo 500 euro che palle";

    let text_it2 = "vorrei capire come posso fare per implementare un sistema di cache in rust che tiene in memoria il modello di machine learning e lo riusa tra le chiamate senza doverlo ricaricare ogni volta dal disco";

    use dimmy_lib::llm::{LlmStyle, LlmTone};

    let tests: Vec<(&str, &str, LlmStyle, LlmTone, &str)> = vec![
        // Style tests
        ("Correct", text_it, LlmStyle::Correct, LlmTone::None, "none"),
        (
            "Correct+Formal",
            text_it,
            LlmStyle::Correct,
            LlmTone::Formal,
            "none",
        ),
        (
            "Professional",
            text_it,
            LlmStyle::Professional,
            LlmTone::None,
            "none",
        ),
        (
            "Summarize",
            text_it,
            LlmStyle::Summarize,
            LlmTone::Concise,
            "none",
        ),
        (
            "Elaborate",
            text_it,
            LlmStyle::Elaborate,
            LlmTone::None,
            "none",
        ),
        (
            "Comprehensible",
            text_it,
            LlmStyle::Comprehensible,
            LlmTone::None,
            "none",
        ),
        ("Prompt", text_it2, LlmStyle::Prompt, LlmTone::None, "none"),
        (
            "Imbruttito",
            text_it,
            LlmStyle::Imbruttito,
            LlmTone::None,
            "none",
        ),
        ("Emoji", text_it, LlmStyle::Emoji, LlmTone::None, "none"),
        // Translation tests
        (
            "Correct→English",
            text_it,
            LlmStyle::Correct,
            LlmTone::None,
            "English",
        ),
        (
            "Correct→French",
            text_it,
            LlmStyle::Correct,
            LlmTone::None,
            "French",
        ),
    ];

    println!("| Test | Time | Output |");
    println!("|------|------|--------|");

    for (name, text, style, tone, translate_to) in &tests {
        let start = Instant::now();
        let result = dimmy_lib::local_llm::process_text_local(
            &model_path,
            text,
            *style,
            *tone,
            "",
            translate_to,
        );
        let elapsed = start.elapsed();

        match result {
            Ok(output) => {
                let preview = if output.len() > 200 {
                    format!("{}...", &output[..200])
                } else {
                    output.clone()
                };
                println!(
                    "| {} | {:.1}s | {} |",
                    name,
                    elapsed.as_secs_f64(),
                    preview.replace('|', "\\|").replace('\n', " ")
                );
            }
            Err(e) => {
                println!(
                    "| {} | {:.1}s | ERROR: {} |",
                    name,
                    elapsed.as_secs_f64(),
                    e
                );
            }
        }
    }

    dimmy_lib::local_llm::clear_llm_cache();
}

//! test_local_llm — Test local LLM enhancement with all styles.
//!
//! Usage:
//!   cargo run --release --features local-llm-vulkan --bin test_local_llm

use std::time::Instant;

fn main() {
    let model_filename = dimmy_lib::local_llm::DEFAULT_LLM_MODEL;
    let model_path = dimmy_lib::local_llm::model_path(model_filename);

    if !model_path.is_file() {
        eprintln!("Model not found: {}", model_path.display());
        eprintln!("Download it first or run the app to trigger download.");
        std::process::exit(1);
    }

    eprintln!(
        "Model: {} ({} MB)",
        model_filename,
        model_path
            .metadata()
            .map(|m| m.len() / 1_000_000)
            .unwrap_or(0)
    );
    eprintln!("Path: {}\n", model_path.display());

    let test_text_it = "allora praticamente ieri sono andato dal meccanico e mi ha detto che la macchina ha un problema al motore cioè il filtro dell olio è da cambiare e anche le pasticche dei freni sono consumate insomma devo spendere tipo 500 euro che palle";
    let test_text_prompt = "vorrei capire come posso fare per implementare un sistema di cache in rust che tiene in memoria il modello di machine learning e lo riusa tra le chiamate senza doverlo ricaricare ogni volta dal disco";

    let tests: Vec<(
        &str,
        &str,
        dimmy_lib::llm::LlmStyle,
        dimmy_lib::llm::LlmTone,
    )> = vec![
        (
            "Correct",
            test_text_it,
            dimmy_lib::llm::LlmStyle::Correct,
            dimmy_lib::llm::LlmTone::None,
        ),
        (
            "Professional",
            test_text_it,
            dimmy_lib::llm::LlmStyle::Professional,
            dimmy_lib::llm::LlmTone::Formal,
        ),
        (
            "Summarize+Concise",
            test_text_it,
            dimmy_lib::llm::LlmStyle::Summarize,
            dimmy_lib::llm::LlmTone::Concise,
        ),
        (
            "Prompt",
            test_text_prompt,
            dimmy_lib::llm::LlmStyle::Prompt,
            dimmy_lib::llm::LlmTone::None,
        ),
    ];

    for (name, text, style, tone) in &tests {
        eprintln!("=== {} ===", name);
        eprintln!("Input: {}...", &text[..text.len().min(80)]);

        let start = Instant::now();
        let result =
            dimmy_lib::local_llm::process_text_local(&model_path, text, *style, *tone, "", "none");
        let elapsed = start.elapsed();

        match result {
            Ok(output) => {
                eprintln!("Output: {}", output);
                eprintln!(
                    "Time: {:.1}s | Input: {} chars | Output: {} chars",
                    elapsed.as_secs_f64(),
                    text.len(),
                    output.len()
                );

                // Quality checks
                if output.contains("Sure")
                    || output.contains("I understand")
                    || output.contains("Here is")
                {
                    eprintln!("WARNING: Output contains prohibited preamble!");
                }
                if *name != "Prompt" && output.len() < 10 {
                    eprintln!("WARNING: Output suspiciously short!");
                }
            }
            Err(e) => {
                eprintln!("ERROR: {}", e);
            }
        }
        eprintln!();
    }

    // Cleanup
    dimmy_lib::local_llm::clear_llm_cache();
    eprintln!("Done. Cache cleared.");
}

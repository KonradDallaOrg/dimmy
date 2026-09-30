//! A reasoning model (MiniCPM5, Qwen3.5) must produce a rewrite, not come back
//! empty.
//!
//! Until 2026-09-30 every call to one of these models returned "" and the app
//! pasted the user's own text back as if the style had run: the generation
//! loop skipped the opening `<think>` token WITHOUT feeding it back to the
//! model, so the model sampled `<think>` again, 256 times. Measured on 76 of
//! 76 calls for MiniCPM5-2B, Qwen3.5-2B and Qwen3.5-4B alike.
//!
//! Needs a real model, so it is ignored by default:
//!   DIMMY_TEST_THINKING_GGUF=C:/llm-bench/Qwen3.5-2B-Q4_K_M.gguf \
//!   cargo test --release --features local-llm-vulkan --test local_llm_thinking -- --ignored

#![cfg(feature = "local-llm")]

use dimmy_lib::llm::{LlmStyle, LlmTone};

#[test]
#[ignore = "needs DIMMY_TEST_THINKING_GGUF pointing at a reasoning model"]
fn a_reasoning_model_rewrites_instead_of_returning_nothing() {
    let path = std::env::var("DIMMY_TEST_THINKING_GGUF").expect("set DIMMY_TEST_THINKING_GGUF");
    let input = "allora oggi abbiamo parlato del budget e alla fine abbiamo deciso di spostare il lancio a marzo perché il fornitore è in ritardo e quindi ci sentiamo la settimana prossima";
    let out = dimmy_lib::local_llm::process_text_local(
        std::path::Path::new(&path),
        input,
        LlmStyle::Summarize,
        LlmTone::Concise,
        "",
        "",
        "it",
    )
    .expect("generation");
    assert!(!out.trim().is_empty(), "empty output");
    assert_ne!(
        out.trim(),
        input,
        "the input came back untouched: the model produced nothing"
    );
    assert!(
        !out.contains("<think>") && !out.contains("</think>"),
        "reasoning leaked: {out}"
    );
    assert!(
        out.split_whitespace().count() < input.split_whitespace().count(),
        "a summary must be shorter: {out}"
    );
}

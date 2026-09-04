//! llm_ctx_sweep — which n_ctx values does the local LLM survive?
//!
//! Gemma 4 E2B on Vulkan produced a recap on one machine (n_ctx 6144) and
//! aborted the process on another (n_ctx 6656) with the same llama.cpp, the
//! same NVIDIA T600, the same free VRAM and the same graph. n_ctx was the
//! only measured difference, and production picks it per call:
//!
//!     ctx_size = prompt_tokens + max_tokens + 64
//!
//! An abort kills the process, so a sweep cannot run in one: this binary
//! tests ONE size and exits, and the caller loops over it.
//!
//! Usage:
//!   cargo run --release --features local-llm-vulkan --bin llm_ctx_sweep -- <prompt_tokens>

use std::path::PathBuf;

/// Count the `llm_stream` events the core emits while generating, so the
/// local streaming path is verified by OBSERVING it rather than by trusting
/// that a flag reached a call site.
static STREAM_DELTAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static STREAM_START: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static STREAM_END: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn count_events(json: *const std::os::raw::c_char) {
    use std::sync::atomic::Ordering;
    if json.is_null() {
        return;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    if !s.contains("\"llm_stream\"") {
        return;
    }
    if s.contains("\"phase\":\"delta\"") {
        STREAM_DELTAS.fetch_add(1, Ordering::Relaxed);
    } else if s.contains("\"phase\":\"start\"") {
        STREAM_START.fetch_add(1, Ordering::Relaxed);
    } else if s.contains("\"phase\":\"end\"") {
        STREAM_END.fetch_add(1, Ordering::Relaxed);
    }
}

fn main() {
    dimmy_lib::ffi::dimmy_set_event_callback(count_events);
    let want: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(6000);
    let max_tokens: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);

    let model = std::env::var("DIMMY_LLM_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dimmy_lib::local_llm::model_path(dimmy_lib::local_llm::DEFAULT_LLM_MODEL)
        });
    if !model.is_file() {
        eprintln!("SKIP model not found: {}", model.display());
        std::process::exit(2);
    }

    // One short word per token, near enough for a sweep: the point is to land
    // in a given 512-cell bucket, not to hit an exact count.
    let prompt = format!(
        "Summarise in one sentence.\n{}",
        "parola ".repeat(want.saturating_sub(16))
    );

    eprintln!("PROBE want_tokens={want} max_tokens={max_tokens}");

    // First call pays the model load; the second is the steady-state cost the
    // user actually waits for on a second recap in the same session.
    let t0 = std::time::Instant::now();
    let first = dimmy_lib::local_llm::process_raw_prompt_local(&model, &prompt, max_tokens);
    let load_and_run = t0.elapsed();
    match first {
        Ok(s) => {
            let t1 = std::time::Instant::now();
            let warm = dimmy_lib::local_llm::process_raw_prompt_local(&model, &prompt, max_tokens);
            let warm_secs = t1.elapsed();
            let warm_chars = warm.map(|w| w.len()).unwrap_or(0);
            use std::sync::atomic::Ordering;
            // And the dictation rewrite must stay SILENT: it replaces text at
            // the cursor and shares this generator, so a stray delta would put
            // dictated words into the meeting window's recap pane.
            let before = STREAM_DELTAS.load(Ordering::Relaxed);
            let _ = dimmy_lib::local_llm::process_text_local(
                &model,
                "allora praticamente ieri sono andato dal meccanico",
                dimmy_lib::llm::LlmStyle::Correct,
                dimmy_lib::llm::LlmTone::None,
                "",
                "",
            );
            println!(
                "DICTATION deltas={} (must be 0)",
                STREAM_DELTAS.load(Ordering::Relaxed) - before
            );
            println!(
                "STREAM start={} deltas={} end={}",
                STREAM_START.load(Ordering::Relaxed),
                STREAM_DELTAS.load(Ordering::Relaxed),
                STREAM_END.load(Ordering::Relaxed)
            );
            println!(
                "OK cold={:.1}s warm={:.1}s chars={} warm_chars={}",
                load_and_run.as_secs_f64(),
                warm_secs.as_secs_f64(),
                s.len(),
                warm_chars
            );
            std::process::exit(0);
        }
        Err(e) => {
            println!("ERR {e}");
            std::process::exit(1);
        }
    }
}

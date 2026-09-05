//! dl_probe — download one catalogue model through the production path.
//!
//! HuggingFace's Xet rollout made the CDN's `etag` a Xet content hash rather
//! than the file's SHA-256, and reqwest follows the redirect that carried the
//! real one. Taking the CDN's value as the SHA failed every integrity check
//! and deleted the download, which made every unsloth model in the catalogue
//! unfetchable (measured 2026-09-05).
//!
//! This runs the real `download_resumable`, so the fix is proven by fetching a
//! file rather than by reading the code.
//!
//! Usage: dl_probe <filename.gguf> [dest_dir]

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let file = std::env::args()
        .nth(1)
        .expect("usage: dl_probe <file.gguf>");
    let dir = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "E:\\llm-bench".to_string());
    let url = dimmy_lib::local_llm::AVAILABLE_LLM_MODELS
        .iter()
        .find(|m| m.filename == file)
        .and_then(|m| m.url)
        .expect("model not in the catalogue, or has no explicit url");

    let dest = std::path::PathBuf::from(&dir).join(&file);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .expect("http client");

    let t = std::time::Instant::now();
    // The callback is Fn, not FnMut, so the progress marker lives in a cell.
    let last = std::cell::Cell::new(0u64);
    // Count EVERY callback: the throttle's whole point is how many there are.
    let calls = std::cell::Cell::new(0u64);
    let r =
        dimmy_lib::download::download_resumable(&client, url, &dest, &[b"GGUF"], |done, total| {
            calls.set(calls.get() + 1);
            let pct = if total > 0 { done * 100 / total } else { 0 };
            if pct >= last.get() + 20 {
                last.set(pct);
                eprintln!("  {pct}%  ({done} / {total})");
            }
        })
        .await;

    match r {
        Ok(()) => println!(
            "OK  {:.0}s  {} MB  {} progress callbacks",
            t.elapsed().as_secs_f64(),
            std::fs::metadata(&dest)
                .map(|m| m.len() / 1_048_576)
                .unwrap_or(0),
            calls.get()
        ),
        Err(e) => println!("FALLITO: {e}"),
    }
}

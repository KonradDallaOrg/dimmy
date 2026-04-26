fn main() {
    // When both local-stt (whisper-rs) and local-llm (llama-cpp-2) are enabled,
    // both crates compile their own copy of ggml from source. The ggml symbols
    // (quantize_*, ggml_*) collide at link time.
    //
    // Both crates use recent, compatible ggml versions so allowing multiple
    // definitions is safe — the linker picks the first definition which is
    // functionally identical in both copies.
    #[cfg(all(feature = "local-stt", feature = "local-llm"))]
    {
        #[cfg(target_os = "windows")]
        println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");

        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");

        #[cfg(target_os = "macos")]
        println!("cargo:rustc-link-arg=-Wl,-multiply_defined,suppress");
    }

    // Telemetry secrets — read from env at compile time, embed via env!().
    // Missing values become empty strings; the runtime client treats an
    // empty key as "telemetry disabled" and stays silent. This keeps
    // local dev builds working without secrets while letting CI inject
    // real values via GitHub Secrets (POSTHOG_API_KEY / SENTRY_DSN).
    let posthog_key = std::env::var("POSTHOG_API_KEY").unwrap_or_default();
    let sentry_dsn = std::env::var("SENTRY_DSN").unwrap_or_default();
    println!("cargo:rustc-env=DIMMY_POSTHOG_API_KEY={}", posthog_key);
    println!("cargo:rustc-env=DIMMY_SENTRY_DSN={}", sentry_dsn);
    println!("cargo:rerun-if-env-changed=POSTHOG_API_KEY");
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");
}

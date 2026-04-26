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
    //
    // CRITICAL: trim() before embedding. Secrets copy-pasted into a
    // GitHub Secret often carry a trailing `\n` or `\r\n`, and the
    // value flows through env vars → build.rs env::var → cargo:rustc-env
    // → const &'static str without any normalisation. A trailing newline
    // breaks `sentry::types::Dsn::from_str` (which sentry-core 0.47
    // panics on inside `sentry::init`) and would similarly corrupt
    // the PostHog Authorization header. Strip ASCII whitespace once,
    // here, so every consumer downstream sees a clean string.
    let posthog_key = std::env::var("POSTHOG_API_KEY")
        .unwrap_or_default()
        .trim()
        .to_string();
    let sentry_dsn = std::env::var("SENTRY_DSN")
        .unwrap_or_default()
        .trim()
        .to_string();

    // Build-time sanity checks. Non-fatal — emit `cargo:warning` so the
    // CI log surfaces "secret looks bad" without breaking the build. A
    // hard error would mean a typo in the GitHub Secret bricks every
    // build until someone manually rotates it; warnings let the build
    // proceed (with telemetry disabled at runtime via the parse-DSN
    // pre-flight in `telemetry::sentry_pipeline::init`) while making
    // the misconfiguration loud.
    if !sentry_dsn.is_empty() {
        let looks_ok = sentry_dsn.starts_with("https://")
            && sentry_dsn.contains('@')
            && sentry_dsn.matches('/').count() >= 3
            && !sentry_dsn.chars().any(|c| c.is_whitespace());
        if !looks_ok {
            println!(
                "cargo:warning=SENTRY_DSN is set but does not match the expected \
                shape (https://<key>@<host>.ingest.<region>.sentry.io/<project_id>). \
                Sentry will be disabled at runtime by the parse-DSN pre-flight. \
                Verify the GitHub Secret value is not truncated and has no embedded \
                whitespace."
            );
        }
    }
    if !posthog_key.is_empty() && !posthog_key.starts_with("phc_") {
        println!(
            "cargo:warning=POSTHOG_API_KEY is set but does not start with `phc_`. \
            PostHog write keys always start with `phc_`. Verify the GitHub Secret \
            holds the project write key, not a personal API token."
        );
    }

    println!("cargo:rustc-env=DIMMY_POSTHOG_API_KEY={}", posthog_key);
    println!("cargo:rustc-env=DIMMY_SENTRY_DSN={}", sentry_dsn);
    println!("cargo:rerun-if-env-changed=POSTHOG_API_KEY");
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");
}

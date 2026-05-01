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
    // CRITICAL: trim() + strip BOM before embedding. Secrets copy-pasted
    // into a GitHub Secret often carry a trailing `\n` or `\r\n`, and
    // sometimes a UTF-8 BOM (EF BB BF) when copied from a browser/Word.
    // The value flows env vars → build.rs env::var → cargo:rustc-env →
    // const &'static str without any normalisation. A trailing newline
    // breaks `sentry::types::Dsn::from_str` (sentry-core 0.47 panics
    // inside `sentry::init`); a BOM byte makes a PostHog `phc_…` key
    // look like `\u{FEFF}phc_…` to the server, which silently drops
    // the event with HTTP 200 OK (PostHog never validates api_key —
    // verified 2026-04-27 with curl). Strip both once, here, so every
    // consumer downstream sees a clean string.
    let posthog_key = sanitize_secret(std::env::var("POSTHOG_API_KEY").unwrap_or_default());
    let sentry_dsn = sanitize_secret(std::env::var("SENTRY_DSN").unwrap_or_default());
    // Same sanitize+rerun pattern for the licensing public key so a
    // GitHub Secret that copy-pasted with a trailing newline (the most
    // common foot-gun) doesn't end up in the binary as `uut9…\n`,
    // which `verify_token` then rejects as a malformed pubkey. Also
    // adds explicit `rerun-if-env-changed` so a changed secret on
    // re-run invalidates the cached license.rs compilation even when
    // `option_env!` cache tracking is unreliable across actions/cache.
    let license_pubkey =
        sanitize_secret(std::env::var("DIMMY_LICENSE_PUBKEY").unwrap_or_default());

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

    if !license_pubkey.is_empty() {
        // Ed25519 base64url-no-pad of 32 bytes = 43 ASCII chars. Anything
        // else means the secret was truncated, padded, or copy-paste-
        // mangled. Warn loud — license verify will fail at runtime.
        if license_pubkey.len() != 43 {
            println!(
                "cargo:warning=DIMMY_LICENSE_PUBKEY is {} chars; expected 43 (base64url \
                of a 32-byte Ed25519 public key, no padding). License verify will fail.",
                license_pubkey.len()
            );
        }
    }

    println!("cargo:rustc-env=DIMMY_POSTHOG_API_KEY={}", posthog_key);
    println!("cargo:rustc-env=DIMMY_SENTRY_DSN={}", sentry_dsn);
    // Re-emit DIMMY_LICENSE_PUBKEY itself (sanitized) so `option_env!`
    // in core/src/license.rs picks up the cleaned value, not the raw
    // env that may carry trailing whitespace.
    println!("cargo:rustc-env=DIMMY_LICENSE_PUBKEY={}", license_pubkey);
    println!("cargo:rerun-if-env-changed=POSTHOG_API_KEY");
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");
    println!("cargo:rerun-if-env-changed=DIMMY_LICENSE_PUBKEY");
}

/// Strip leading UTF-8 BOM, then ASCII-trim. Returns owned String so
/// callers can move into the cargo:rustc-env line without lifetime
/// gymnastics. BOM removal must come BEFORE trim because trim() does
/// not consider U+FEFF whitespace.
fn sanitize_secret(raw: String) -> String {
    let no_bom = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
    no_bom.trim().to_string()
}

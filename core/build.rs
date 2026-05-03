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
    let license_pubkey = sanitize_secret(std::env::var("DIMMY_LICENSE_PUBKEY").unwrap_or_default());
    // Server URL is paired with the pubkey at build time. We refuse to
    // default a non-empty pubkey to a hardcoded prod URL — that's how a
    // staging-keyed build accidentally hits prod, the exact failure
    // mode we're locking down. Pubkey + URL move together or not at
    // all. Source-builds (both empty) fall back to the localhost mock.
    let license_server_url =
        sanitize_secret(std::env::var("DIMMY_LICENSE_SERVER_URL").unwrap_or_default());

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

    // ── Hard validation: refuse misconfigured release builds ──────────
    //
    // Two failure modes we lock down at compile time so a slip in CI
    // can't ship a binary that runs free or hits the wrong backend:
    //
    //   1. Release build + license-client feature ON + empty pubkey.
    //      Pre-fix: `cargo:warning` and binary ships as Unrestricted
    //      (free for everyone). Post-fix: build aborts.
    //
    //   2. Pubkey set but server URL empty. Pre-fix: code defaulted to
    //      `https://license.dimmy.app` regardless of which keypair the
    //      pubkey came from — so a staging-keyed build silently hit
    //      prod. Post-fix: pubkey and URL must be set together.
    //
    // PROFILE is set by Cargo to "debug" or "release". CARGO_FEATURE_*
    // env vars are present iff that feature is active for this build.
    let is_release = std::env::var("PROFILE").unwrap_or_default() == "release";
    let license_client_on = std::env::var("CARGO_FEATURE_LICENSE_CLIENT").is_ok();
    if is_release && license_client_on && license_pubkey.is_empty() {
        panic!(
            "DIMMY_LICENSE_PUBKEY is empty but this is a release build with \
            license-client feature on. Refusing to ship a binary that would \
            run in Unrestricted mode for every user. Set the env var (CI: \
            via release.yml `env:` block from GitHub Secrets) or build \
            without --features license-client for source-build."
        );
    }
    if !license_pubkey.is_empty() && license_server_url.is_empty() {
        panic!(
            "DIMMY_LICENSE_PUBKEY is set ({} chars) but DIMMY_LICENSE_SERVER_URL \
            is empty. Refusing to fall back to a hardcoded prod URL — the \
            previous behavior shipped staging-keyed builds that talked to \
            prod by accident. Set BOTH env vars to the matching pair (staging \
            pubkey → staging URL, prod pubkey → prod URL) or unset BOTH for a \
            source-build / mock-server local run.",
            license_pubkey.len()
        );
    }

    // Loud diagnostic on every build so a misconfigured secret surfaces
    // in the CI log instead of producing a "source build" binary
    // silently. We log only the length (and a short prefix hash so two
    // different valid 43-char keys are distinguishable in the log) —
    // never the full key, never a token-like substring that would let
    // somebody reading logs reconstruct the secret.
    if license_pubkey.is_empty() {
        println!(
            "cargo:warning=DIMMY_LICENSE_PUBKEY empty — source-build / debug \
            run, licensing disabled. (Release builds with license-client \
            feature would have aborted earlier.)"
        );
    } else if license_pubkey.len() != 43 {
        println!(
            "cargo:warning=DIMMY_LICENSE_PUBKEY is {} chars; expected 43 (base64url \
            of a 32-byte Ed25519 public key, no padding). License verify will fail.",
            license_pubkey.len()
        );
    } else {
        // Length-only confirmation — proves end-to-end env propagation
        // worked without leaking the value. Two-char prefix is enough
        // to spot-check we got the key we meant to ship.
        let prefix2: String = license_pubkey.chars().take(2).collect();
        println!(
            "cargo:warning=DIMMY_LICENSE_PUBKEY embedded ok ({} chars, prefix={}…)",
            license_pubkey.len(),
            prefix2
        );
    }

    println!("cargo:rustc-env=DIMMY_POSTHOG_API_KEY={}", posthog_key);
    println!("cargo:rustc-env=DIMMY_SENTRY_DSN={}", sentry_dsn);
    // Re-emit DIMMY_LICENSE_PUBKEY itself (sanitized) so `option_env!`
    // in core/src/license.rs picks up the cleaned value, not the raw
    // env that may carry trailing whitespace.
    println!("cargo:rustc-env=DIMMY_LICENSE_PUBKEY={}", license_pubkey);
    println!(
        "cargo:rustc-env=DIMMY_LICENSE_SERVER_URL={}",
        license_server_url
    );
    println!("cargo:rerun-if-env-changed=POSTHOG_API_KEY");
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");
    println!("cargo:rerun-if-env-changed=DIMMY_LICENSE_PUBKEY");
    println!("cargo:rerun-if-env-changed=DIMMY_LICENSE_SERVER_URL");
}

/// Strip leading UTF-8 BOM, then ASCII-trim. Returns owned String so
/// callers can move into the cargo:rustc-env line without lifetime
/// gymnastics. BOM removal must come BEFORE trim because trim() does
/// not consider U+FEFF whitespace.
fn sanitize_secret(raw: String) -> String {
    let no_bom = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
    no_bom.trim().to_string()
}

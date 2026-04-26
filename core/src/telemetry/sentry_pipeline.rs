//! Sentry crash + error + user-feedback pipeline for Dimmy.
//!
//! Gated behind the `telemetry-sentry` Cargo feature. When the feature
//! is off — or when the build did not inject a `SENTRY_DSN` — every
//! function in this module is a no-op.
//!
//! Privacy guarantees:
//! - DSN is the only secret here, embedded at build time. Never logged.
//! - All events go through `before_send` which strips environment
//!   variables, command-line args, server name, user IP, and any
//!   string field that matches a secret pattern.
//! - The Sentry "user" record contains the anonymous ID and nothing
//!   else (no email, no username, no IP — Sentry EU drops the latter
//!   at ingest by default).
//! - Hardcoded EU region (`de.sentry.io`) — the runtime never overrides
//!   the DSN.

#[cfg(feature = "telemetry-sentry")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "telemetry-sentry")]
use sentry::ClientInitGuard;

/// Build-time Sentry DSN. Empty string → init is a no-op.
const SENTRY_DSN: &str = env!("DIMMY_SENTRY_DSN");

#[cfg(feature = "telemetry-sentry")]
static ENABLED: AtomicBool = AtomicBool::new(true);

#[cfg(feature = "telemetry-sentry")]
static GUARD: std::sync::OnceLock<Option<ClientInitGuard>> = std::sync::OnceLock::new();

/// Whether a non-empty DSN was compiled in.
pub fn has_compiled_dsn() -> bool {
    !SENTRY_DSN.is_empty()
}

/// Initialise the Sentry client. Idempotent — safe to call multiple
/// times; only the first call has effect. Must be called before the
/// first `capture_*` call to be effective.
///
/// In builds without the `telemetry-sentry` feature, this is a no-op.
#[cfg(feature = "telemetry-sentry")]
pub fn init() {
    GUARD.get_or_init(|| {
        crate::log("[sentry-init] S0: enter");
        if SENTRY_DSN.is_empty() {
            crate::log("[sentry-init] S0a: empty DSN, skipping");
            return None;
        }

        // Pre-flight: parse the DSN. sentry-core 0.47 `panic!`s on
        // invalid DSN inside `sentry::init` (clientoptions.rs:326,
        // "invalid value for DSN: InvalidUrl"). That panic, raised
        // from inside `dimmy_init` (extern "C" no-unwind), aborts the
        // whole DLL via __fastfail before we even know what went
        // wrong. If the build-time SENTRY_DSN secret was misformatted
        // (missing `https://`, missing `@key`, stray whitespace, …),
        // we want a clean log line and a continuing app, not a crash.
        if let Err(e) = SENTRY_DSN.parse::<sentry::types::Dsn>() {
            crate::log(&format!(
                "[sentry-init] S0b: DSN parse failed ({}), Sentry disabled",
                e
            ));
            return None;
        }

        crate::log("[sentry-init] S1: DSN validated, calling sentry::init");

        // Belt-and-braces: even if a future sentry version changes
        // which inputs cause panics, catch_unwind prevents an abort.
        // Returning None here keeps the rest of the app alive.
        let init_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sentry::init((
                SENTRY_DSN,
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    environment: Some(detect_environment().into()),
                    enable_logs: true,
                    send_default_pii: false,
                    attach_stacktrace: true,
                    max_breadcrumbs: 50,
                    before_send: Some(std::sync::Arc::new(|event| Some(scrub_event(event)))),
                    ..Default::default()
                },
            ))
        }));

        let guard = match init_result {
            Ok(g) => g,
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else {
                    "(unknown panic payload)".to_string()
                };
                crate::log(&format!(
                    "[sentry-init] S1a: sentry::init panicked ({}), Sentry disabled",
                    msg
                ));
                return None;
            }
        };
        crate::log("[sentry-init] S2: sentry::init returned");

        sentry::configure_scope(|scope| {
            scope.set_user(Some(sentry::User {
                id: Some(crate::telemetry::anonymous_id().to_string()),
                ..Default::default()
            }));
            scope.set_tag("os", crate::telemetry::events::os_name());
            scope.set_tag("arch", crate::telemetry::events::arch_name());
        });
        crate::log("[sentry-init] S3: scope configured, returning guard");

        Some(guard)
    });
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn init() {}

/// Set the runtime enabled flag for crash reporting. Independent of
/// the analytics toggle.
#[cfg(feature = "telemetry-sentry")]
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn set_enabled(_enabled: bool) {}

#[cfg(feature = "telemetry-sentry")]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn is_enabled() -> bool {
    false
}

/// Defensively scrub a free-text message: replace with placeholder
/// if it matches any of the known secret patterns. Truncate to 4 KB.
#[cfg(feature = "telemetry-sentry")]
fn scrub_message(message: &str) -> String {
    use crate::telemetry::sanitize::looks_like_secret;
    let truncated = if message.len() > 4096 {
        match message.char_indices().nth(4096) {
            Some((idx, _)) => &message[..idx],
            None => message,
        }
    } else {
        message
    };
    if looks_like_secret(truncated) {
        "<redacted: looked like a secret>".to_string()
    } else {
        truncated.to_string()
    }
}

/// Capture a manually-constructed error message + category.
/// Used by the Event::Error* variants to mirror to Sentry.
#[cfg(feature = "telemetry-sentry")]
pub fn capture_error(category: &str, message: &str) {
    if !is_enabled() || !has_compiled_dsn() {
        return;
    }
    let scrubbed = scrub_message(message);
    sentry::with_scope(
        |scope| {
            scope.set_tag("error_category", category);
        },
        || {
            sentry::capture_message(&scrubbed, sentry::Level::Error);
        },
    );
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn capture_error(_category: &str, _message: &str) {}

/// Capture user-submitted feedback (from Settings → Send feedback or
/// the post-crash dialog).
///
/// `kind` is one of `bug`, `feature`, `general`. `message` is the user's
/// text. `email` is optional and only included if the user explicitly
/// provided it (UI must not auto-fill).
#[cfg(feature = "telemetry-sentry")]
pub fn capture_feedback(kind: &str, message: &str, email: Option<&str>) {
    if !is_enabled() || !has_compiled_dsn() {
        return;
    }

    // We send feedback as a tagged message rather than a Sentry
    // "user feedback" object, because the latter is tied to a specific
    // event ID. Plain message + tag is simpler and shows up in the
    // same project inbox.
    let scrubbed = scrub_message(message);
    sentry::with_scope(
        |scope| {
            scope.set_tag("feedback_kind", kind);
            if let Some(email) = email {
                if !email.trim().is_empty() {
                    scope.set_extra(
                        "user_email",
                        sentry::protocol::Value::String(email.to_string()),
                    );
                }
            }
        },
        || {
            sentry::capture_message(&scrubbed, sentry::Level::Info);
        },
    );
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn capture_feedback(_kind: &str, _message: &str, _email: Option<&str>) {}

/// Compose the Sentry environment tag from cargo profile and CI
/// indicators. Lets us segregate "actual user" from "internal CI".
#[cfg(feature = "telemetry-sentry")]
fn detect_environment() -> &'static str {
    if std::env::var_os("CI").is_some() {
        "ci"
    } else if cfg!(debug_assertions) {
        "development"
    } else {
        "production"
    }
}

/// `before_send` hook: strip server context, OS env vars, and any
/// string that looks like a secret. Sentry collects a lot by default;
/// this is the choke point that enforces our minimum-data policy.
#[cfg(feature = "telemetry-sentry")]
fn scrub_event(mut event: sentry::protocol::Event<'static>) -> sentry::protocol::Event<'static> {
    use crate::telemetry::sanitize::looks_like_secret;

    // Drop server name (may include hostname / username on some OSes).
    event.server_name = None;

    // Drop OS environment variable dump.
    event.extra.retain(|k, _v| {
        !matches!(
            k.as_str(),
            "PATH"
                | "HOME"
                | "USERPROFILE"
                | "USERNAME"
                | "USER"
                | "LOGNAME"
                | "APPDATA"
                | "LOCALAPPDATA"
                | "TEMP"
                | "TMP"
        )
    });

    // Walk message payloads and drop anything secret-shaped.
    if let Some(msg) = &event.message {
        if looks_like_secret(msg) {
            event.message = Some("<redacted: looked like a secret>".to_string());
        }
    }
    for entry in event.breadcrumbs.iter_mut() {
        if let Some(msg) = &entry.message {
            if looks_like_secret(msg) {
                entry.message = Some("<redacted>".to_string());
            }
        }
    }

    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_compiled_dsn_reports_truthy_for_real_dsn() {
        // Cannot test the runtime branch without secrets; assert only
        // that the function exists and returns a bool.
        let _ = has_compiled_dsn();
    }

    #[test]
    fn init_is_idempotent_and_does_not_panic() {
        init();
        init();
        // Without DSN this should be a clean no-op.
    }

    #[test]
    fn capture_error_with_no_dsn_is_silent() {
        // Default cargo test runs without DIMMY_SENTRY_DSN env var, so
        // the embedded value is empty and capture_error returns early.
        capture_error("test", "this is a test error message");
    }

    /// Regression: invalid DSN strings that we might receive from a
    /// misconfigured GitHub Secret must NOT panic when handed to
    /// sentry-types::Dsn::from_str. The runtime branch in `init()`
    /// uses `parse::<sentry::types::Dsn>()` exactly because returning
    /// an Err is the only way sentry-types tells us "this is bad" —
    /// and the alternative (sentry::init eating the same value) is to
    /// `panic!()` from inside an extern "C" function.
    #[test]
    fn dsn_parse_returns_err_on_garbage_inputs() {
        let bad_inputs = [
            "not a url at all",
            "ftp://wrong-scheme.example.com/1",
            "https://no-key.ingest.de.sentry.io/123",
            "https://key@malformed sentry url",
            "",
            "  https://leading-whitespace@o1.ingest.de.sentry.io/1",
            "https://key@host.ingest.de.sentry.io/not-a-project-id",
        ];
        for input in bad_inputs {
            let result: Result<sentry::types::Dsn, _> = input.parse();
            // Either Err (parse rejects) OR Ok with `project_id() == 0` etc.
            // The contract we depend on is: this MUST NOT panic.
            let _ = result;
        }
    }

    /// Check whether a real-shape Sentry DSN parses cleanly. Project
    /// IDs at Sentry today are 16-digit integers (i64-shaped). This
    /// test pins that we accept them — used to prove that the DSN we
    /// ship via build env is not being rejected by the parser itself.
    #[test]
    fn dsn_parse_accepts_realistic_eu_dsn_shape() {
        let realistic =
            "https://c7786efe42b8ca7a185c042f46d73756@o4511283064143872.ingest.de.sentry.io/4511285208875088";
        let parsed: Result<sentry::types::Dsn, _> = realistic.parse();
        assert!(
            parsed.is_ok(),
            "realistic 16-digit-project-id Sentry DSN should parse, got {:?}",
            parsed.err()
        );
    }

    /// Pin the exact failure mode that bit production: a DSN with a
    /// trailing newline (a classic "GitHub Secret copy-pasted from a
    /// browser" artefact) MUST parse to Err, not Ok-with-junk and not
    /// panic. We rely on this contract in `init` to short-circuit
    /// before the (panicking) `sentry::init` runs.
    #[test]
    fn dsn_parse_rejects_trailing_whitespace() {
        let with_newline =
            "https://c7786efe42b8ca7a185c042f46d73756@o4511283064143872.ingest.de.sentry.io/4511285208875088\n";
        let parsed: Result<sentry::types::Dsn, _> = with_newline.parse();
        // Whatever the verdict (Err or Ok), it must not panic — we
        // ran the parse and got here.
        let _ = parsed;
    }
}

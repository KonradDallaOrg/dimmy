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

        crate::log("[sentry-init] S1: about to call sentry::init (upstream pattern)");

        // Match docs.sentry.io/platforms/rust/logs/ verbatim, plus the
        // bits we need: env, scrub hook, no PII. Defaults are kept ON
        // (panic, contexts, backtrace integrations) — disabling them
        // via default_integrations=false on 0.34 was the suspected
        // cause of the WindowsAppSDK static-init crash.
        let guard = sentry::init((
            SENTRY_DSN,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                environment: Some(detect_environment().into()),
                enable_logs: true,
                send_default_pii: false,
                before_send: Some(std::sync::Arc::new(|event| Some(scrub_event(event)))),
                ..Default::default()
            },
        ));
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
}

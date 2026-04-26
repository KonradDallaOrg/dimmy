//! Telemetry layer for Dimmy.
//!
//! See `docs/dev/telemetry-plan.md` for the full design — privacy
//! principles, event taxonomy, opt-out UX, and decision log.
//!
//! This module is gated behind the `telemetry` Cargo feature. Default
//! release builds enable it; tests and dev builds without the feature
//! get a fully no-op surface so tests run offline and deterministic.
//!
//! Public API exposed to the rest of `core/src/` and the FFI:
//! - [`track`] — submit an event (best-effort, never blocks).
//! - [`set_enabled`] / [`is_enabled`] — runtime toggle.
//! - [`anonymous_id`] / [`reset_anonymous_id`] — identity management.
//! - [`has_compiled_key`] — whether a real API key was baked into
//!   the binary at build time.
//!
//! Wiring into lifecycle / transcription / onboarding events is
//! deferred to Phase 3 (see plan §10).

pub mod client;
pub mod events;
pub mod identity;
pub mod sanitize;
pub mod sentry_pipeline;

pub use events::Event;

/// Initialise both pipelines. Idempotent. Call once from FFI init.
pub fn init() {
    crate::log(&format!(
        "[telemetry] init: posthog_key_compiled={}, sentry_dsn_compiled={}, analytics_enabled={}, crashes_enabled={}, anonymous_id={}",
        client::has_compiled_key(),
        sentry_pipeline::has_compiled_dsn(),
        client::is_enabled(),
        sentry_pipeline::is_enabled(),
        identity::anonymous_id(),
    ));
    sentry_pipeline::init();
}

/// Submit an event to the analytics pipeline. Best-effort: never
/// blocks the caller, never panics, never logs at the user-facing
/// level. Drops silently if disabled, no API key, or no tokio runtime.
pub fn track(event: Event) {
    client::track(event);
}

/// Set the runtime enabled flag. Driven by the user's settings
/// toggle. Persists across calls within a process; persistence to
/// disk is the caller's concern (it lives in `config.json`).
pub fn set_enabled(enabled: bool) {
    client::set_enabled(enabled);
}

/// Read the current runtime enabled flag.
pub fn is_enabled() -> bool {
    client::is_enabled()
}

/// Return the anonymous identifier for this installation. Generated
/// + persisted on first call. Stable across process restarts.
pub fn anonymous_id() -> &'static str {
    identity::anonymous_id()
}

/// Forget the persisted anonymous ID. The next process launch will
/// generate a fresh one. The current process keeps using the cached
/// value until it exits — the UI must surface "Restart Dimmy to
/// apply".
pub fn reset_anonymous_id() {
    identity::reset();
}

/// True when a non-empty PostHog key was compiled into this binary.
/// Used by the FFI status endpoint and by diagnostics. Never returns
/// the key itself.
pub fn has_compiled_key() -> bool {
    client::has_compiled_key()
}

// ── Sentry crash + feedback pipeline ──────────────────────────────────

/// Set the runtime enabled flag for crash reporting. Independent of
/// the analytics toggle.
pub fn set_crash_enabled(enabled: bool) {
    sentry_pipeline::set_enabled(enabled);
}

/// Read the current crash-reporting enabled flag.
pub fn is_crash_enabled() -> bool {
    sentry_pipeline::is_enabled()
}

/// True when a non-empty Sentry DSN was compiled in.
pub fn has_compiled_dsn() -> bool {
    sentry_pipeline::has_compiled_dsn()
}

/// Capture an error that already happened (e.g. from an Event::Error*
/// variant). Best-effort, silent.
pub fn capture_error(category: &str, message: &str) {
    sentry_pipeline::capture_error(category, message);
}

/// Capture user-submitted feedback. `kind` is `bug` / `feature` /
/// `general`. `email` is optional, only included if the user typed it.
pub fn capture_feedback(kind: &str, message: &str, email: Option<&str>) {
    sentry_pipeline::capture_feedback(kind, message, email);
}

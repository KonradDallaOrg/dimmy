//! PostHog HTTP client wrapper.
//!
//! No SDK dependency: a thin wrapper over `reqwest` that POSTs events
//! to PostHog EU's `/i/v0/e/` endpoint. The wrapper is best-effort —
//! network failure, HTTP errors, and serialisation errors are all
//! swallowed silently so a flaky telemetry pipeline can never affect
//! the user-facing flow.
//!
//! Privacy guarantees enforced here:
//! - The API key is loaded once at process start from the build-time
//!   env var. Never logged.
//! - The endpoint is hardcoded to the EU region. There is no way to
//!   redirect this at runtime.
//! - Outgoing payloads are checked by `looks_like_secret()` as a
//!   last-resort grep before send. If anything in the JSON serialisation
//!   matches a secret pattern, the event is dropped and a counter
//!   incremented (visible only via the local log).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::telemetry::events::Event;
use crate::telemetry::sanitize::looks_like_secret;

/// PostHog EU ingest endpoint. Hardcoded; never overridable at runtime.
const POSTHOG_ENDPOINT: &str = "https://eu.i.posthog.com/i/v0/e/";

/// Build-time API key. Empty string if not provided — runtime falls
/// back to disabled.
const POSTHOG_API_KEY: &str = env!("DIMMY_POSTHOG_API_KEY");

/// Whether telemetry is currently enabled at runtime. Defaults true,
/// flipped by the user's settings toggle (opt-out model).
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Reqwest client built lazily on first send. Sharing one client
/// reuses the connection pool across events.
static HTTP: OnceLock<reqwest::Client> = OnceLock::new();

/// Dedicated tokio runtime for telemetry sends. We can't rely on the
/// caller's runtime (most FFI entry points are called from C# main
/// thread or other contexts where no tokio runtime is active).
/// Lazy-init on first send; lives for the rest of the process.
/// Single worker thread is enough — events are tiny and infrequent.
static TELEMETRY_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Counters for the local log only. Never exfiltrated.
static SENT: AtomicU64 = AtomicU64::new(0);
static DROPPED_SECRET_GUARD: AtomicU64 = AtomicU64::new(0);
static DROPPED_DISABLED: AtomicU64 = AtomicU64::new(0);
static DROPPED_NO_KEY: AtomicU64 = AtomicU64::new(0);

/// True when a non-empty API key was compiled in. Used by the FFI
/// status check.
pub fn has_compiled_key() -> bool {
    !POSTHOG_API_KEY.is_empty()
}

/// Set the runtime enabled flag (driven by the user's settings).
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Read the runtime enabled flag.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn http_client() -> &'static reqwest::Client {
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("Dimmy/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn telemetry_runtime() -> Option<&'static tokio::runtime::Runtime> {
    static FAILED: AtomicBool = AtomicBool::new(false);
    if FAILED.load(Ordering::Relaxed) {
        return None;
    }
    Some(TELEMETRY_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("dimmy-telemetry")
            .enable_all()
            .build()
            .unwrap_or_else(|e| {
                FAILED.store(true, Ordering::Relaxed);
                panic!("dimmy: failed to build telemetry tokio runtime: {}", e);
            })
    }))
}

/// Submit an event. Best-effort. Returns immediately after queueing
/// the async send.
///
/// The caller does not need a tokio runtime — if no runtime is active,
/// the call is dropped silently (we don't want to spawn a runtime per
/// event).
pub fn track(event: Event) {
    if !is_enabled() {
        DROPPED_DISABLED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if !has_compiled_key() {
        DROPPED_NO_KEY.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let payload = match build_payload(&event) {
        Ok(p) => p,
        Err(_) => return,
    };

    // Defensive last-ditch grep: if for any reason the serialised JSON
    // contains something that looks like a secret, drop the event and
    // increment the counter. This guards against a future code change
    // that accidentally puts user content into a property.
    if looks_like_secret(&payload) {
        DROPPED_SECRET_GUARD.fetch_add(1, Ordering::Relaxed);
        return;
    }

    spawn_send(payload);
}

fn build_payload(event: &Event) -> Result<String, serde_json::Error> {
    let body = serde_json::json!({
        "api_key": POSTHOG_API_KEY,
        "event": event.name(),
        "distinct_id": crate::telemetry::identity::anonymous_id(),
        "properties": event.properties(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    serde_json::to_string(&body)
}

fn spawn_send(payload: String) {
    // Use our dedicated telemetry runtime — most FFI call sites
    // (dimmy_init, dimmy_stop_recording's success branch, …) run on
    // the C# main thread or other contexts where no tokio runtime is
    // currently active. Relying on Handle::try_current() dropped events
    // silently in V3/V4. The dedicated runtime is lazy-init at first
    // call and reused for the rest of the process lifetime.
    if let Some(rt) = telemetry_runtime() {
        rt.spawn(async move {
            send(payload).await;
        });
    }
}

async fn send(payload: String) {
    let client = http_client();
    let resp = client
        .post(POSTHOG_ENDPOINT)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            SENT.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            // Silent. Telemetry must never spam dimmy.log on transient
            // network errors. The counters above are accessible from
            // diagnostics if we ever need to debug.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::events::Event;

    #[test]
    fn build_payload_includes_required_fields() {
        let e = Event::AppStarted {
            version: "0.6.20",
            os: "linux",
            arch: "x86_64",
            cold_start_ms: 123,
        };
        let p = build_payload(&e).expect("build");
        assert!(p.contains("\"event\":\"app.started\""));
        assert!(p.contains("\"distinct_id\""));
        assert!(p.contains("\"properties\""));
    }

    #[test]
    fn track_with_disabled_does_not_panic() {
        set_enabled(false);
        track(Event::OnboardingStarted);
        set_enabled(true);
    }
}

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
    let event_name = event.name();
    crate::log(&format!("[telemetry] track event={}", event_name));

    if !is_enabled() {
        DROPPED_DISABLED.fetch_add(1, Ordering::Relaxed);
        crate::log("[telemetry] dropped: disabled (analytics toggle off)");
        return;
    }
    if !has_compiled_key() {
        DROPPED_NO_KEY.fetch_add(1, Ordering::Relaxed);
        crate::log("[telemetry] dropped: no compile-time POSTHOG_API_KEY");
        return;
    }

    let payload = match build_payload(&event) {
        Ok(p) => p,
        Err(e) => {
            crate::log(&format!("[telemetry] build_payload error: {}", e));
            return;
        }
    };

    // Defensive last-ditch grep: if for any reason the serialised JSON
    // contains something that looks like a secret, drop the event and
    // increment the counter. This guards against a future code change
    // that accidentally puts user content into a property.
    if looks_like_secret(&payload) {
        DROPPED_SECRET_GUARD.fetch_add(1, Ordering::Relaxed);
        crate::log(&format!(
            "[telemetry] dropped: secret-guard tripped on event={}",
            event_name
        ));
        return;
    }

    crate::log(&format!(
        "[telemetry] spawn send for event={} (payload {} bytes)",
        event_name,
        payload.len()
    ));
    spawn_send(payload);
}

/// Process-lifetime session identifier. A fresh UUIDv4 generated on
/// first event; reused for every subsequent event in the same process.
/// Lets PostHog group activity into sessions without us tracking
/// open/close round-trips. Anonymous-ID stays stable across launches;
/// session_id changes per launch.
static SESSION_ID: OnceLock<String> = OnceLock::new();

fn session_id() -> &'static str {
    SESSION_ID.get_or_init(crate::telemetry::identity::new_uuid_v4)
}

fn build_payload(event: &Event) -> Result<String, serde_json::Error> {
    // No `timestamp` field on purpose: PostHog drops events whose
    // explicit `timestamp` differs from receive time by more than a
    // small (undocumented) tolerance. Verified 2026-04-26: backdated
    // payloads sent via curl returned HTTP 200 but never appeared in
    // the events table; identical payloads without `timestamp` were
    // ingested. PostHog uses request receive time when the field is
    // absent, which is what we want — events are emitted at the
    // moment they happen, network latency is tiny.
    //
    // Augment with common properties (app_version, os, arch,
    // session_id). These let dashboards filter/segment by version and
    // group activity by session without burdening every Event variant
    // with the same boilerplate. We use `entry().or_insert_with` so a
    // variant that already declares its own `version` (e.g.
    // PerfStartupMs) wins over the generic value.
    let mut props = event.properties();
    if let serde_json::Value::Object(ref mut map) = props {
        map.entry("app_version".to_string())
            .or_insert_with(|| serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()));
        map.entry("os".to_string()).or_insert_with(|| {
            serde_json::Value::String(crate::telemetry::events::os_name().to_string())
        });
        map.entry("arch".to_string()).or_insert_with(|| {
            serde_json::Value::String(crate::telemetry::events::arch_name().to_string())
        });
        map.entry("session_id".to_string())
            .or_insert_with(|| serde_json::Value::String(session_id().to_string()));
    }

    let body = serde_json::json!({
        "api_key": POSTHOG_API_KEY,
        "event": event.name(),
        "distinct_id": crate::telemetry::identity::anonymous_id(),
        "properties": props,
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
    match telemetry_runtime() {
        Some(rt) => {
            crate::log("[telemetry] runtime ok, spawning send task");
            rt.spawn(async move {
                send(payload).await;
            });
        }
        None => {
            crate::log("[telemetry] dropped: telemetry runtime unavailable");
        }
    }
}

async fn send(payload: String) {
    crate::log("[telemetry] send: HTTP POST starting");
    let client = http_client();
    let resp = client
        .post(POSTHOG_ENDPOINT)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if status.is_success() {
                SENT.fetch_add(1, Ordering::Relaxed);
                crate::log(&format!(
                    "[telemetry] send: HTTP {} OK (sent={})",
                    status.as_u16(),
                    SENT.load(Ordering::Relaxed)
                ));
            } else {
                crate::log(&format!(
                    "[telemetry] send: HTTP {} non-success",
                    status.as_u16()
                ));
            }
        }
        Err(e) => {
            // Sanitize: never log full URL (could include path) or full
            // error message (could include stray text). Just the
            // category.
            let cat = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else if e.is_request() {
                "request"
            } else {
                "other"
            };
            crate::log(&format!("[telemetry] send: error category={}", cat));
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

    /// Every payload must carry the four shared properties added in
    /// the Phase 2 enrichment: `app_version`, `os`, `arch`,
    /// `session_id`. These are what dashboards filter on, so missing
    /// any of them silently degrades segment analysis.
    #[test]
    fn build_payload_carries_shared_properties() {
        let e = Event::ConfigPreprocessingChanged { enabled: true };
        let p = build_payload(&e).expect("build");
        // Use the parsed JSON to be insensitive to key/whitespace
        // serialisation choices.
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        let props = &v["properties"];
        assert!(props["app_version"].is_string(), "missing app_version");
        assert!(props["os"].is_string(), "missing os");
        assert!(props["arch"].is_string(), "missing arch");
        assert!(props["session_id"].is_string(), "missing session_id");
        assert_eq!(props["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(props["enabled"], true);
    }

    /// Variant-declared property values must win over the generic
    /// shared-property fallback. PerfStartupMs declares its own
    /// `version`; the build_payload generic enrichment is keyed on
    /// `app_version` instead, so both can coexist. This test pins
    /// that the variant's `version` is preserved verbatim and that
    /// `app_version` is added alongside it.
    #[test]
    fn build_payload_variant_fields_win_over_shared_defaults() {
        let e = Event::PerfStartupMs {
            value: 999,
            version: "0.0.0-test",
        };
        let p = build_payload(&e).expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        assert_eq!(v["properties"]["version"], "0.0.0-test");
        assert_eq!(v["properties"]["app_version"], env!("CARGO_PKG_VERSION"));
    }

    /// session_id stays stable for the lifetime of the process —
    /// every event in the same run should carry the same value so
    /// PostHog can group activity into a session without us tracking
    /// open/close events explicitly.
    #[test]
    fn session_id_stable_across_events() {
        let a = build_payload(&Event::OnboardingStarted).expect("a");
        let b = build_payload(&Event::OnboardingStarted).expect("b");
        let va: serde_json::Value = serde_json::from_str(&a).expect("ja");
        let vb: serde_json::Value = serde_json::from_str(&b).expect("jb");
        assert_eq!(
            va["properties"]["session_id"], vb["properties"]["session_id"],
            "session_id must be stable within a process"
        );
        assert!(va["properties"]["session_id"].as_str().unwrap().len() == 36);
    }
}

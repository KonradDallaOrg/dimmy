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

/// Logged once on first event send to make "key is wrong" failures
/// debuggable from the client log alone. PostHog's ingest endpoint
/// returns HTTP 200 + `{"status":"Ok"}` for *every* request,
/// including those with a non-existent api_key (verified 2026-04-27
/// with a fake `phc_FAKE_…` value), so HTTP 200 in our log proves
/// nothing about the event arriving in the dashboard. Logging the
/// masked prefix + length lets ops correlate the embedded key with
/// the GitHub Secret value without exfiltrating the secret.
static KEY_DIAG_LOGGED: AtomicBool = AtomicBool::new(false);

fn log_key_diagnostic_once() {
    if KEY_DIAG_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    let key = POSTHOG_API_KEY;
    if key.is_empty() {
        crate::log("[telemetry] key-diag: POSTHOG_API_KEY is empty, telemetry disabled");
        return;
    }
    // Mask: keep the public `phc_` prefix + first 4 hex chars; redact
    // the rest. `phc_` keys are write-only, so the prefix alone is
    // safe to log (it identifies the project, not the user). If the
    // key is mis-shaped (no `phc_` prefix) we still log the first 4
    // chars — knowing it's wrong is more valuable than guarding the
    // value of a key that's already broken.
    let prefix: String = key.chars().take(8).collect();
    let len = key.len();
    let starts_phc = key.starts_with("phc_");
    crate::log(&format!(
        "[telemetry] key-diag: prefix={}… len={} starts_with_phc={}",
        prefix, len, starts_phc
    ));
    if !starts_phc {
        crate::log(
            "[telemetry] key-diag: WARNING POSTHOG_API_KEY does not start with `phc_`. \
             PostHog will silently drop events. Verify GitHub Secret value.",
        );
    }
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

    // First event in this process: log the key prefix (masked) so
    // "events sent but never appear in dashboard" failures can be
    // diagnosed from the client log alone (PostHog returns 200 OK for
    // every request, including bogus keys).
    log_key_diagnostic_once();

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
    let event_name = event.name();
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
        // Which BUILD, not just which version. `app_version` is the same
        // string for every rc of a version, so it cannot answer "did this
        // start in rc6?" — a question that cost a wrong diagnosis on
        // 2026-09-02. Categorical build identity, no user content.
        map.entry("build_id".to_string()).or_insert_with(|| {
            serde_json::Value::String(crate::telemetry::sentry_pipeline::BUILD_ID.to_string())
        });

        // Privacy: explicitly opt out of PostHog's automatic IP
        // capture. By default the ingest server records the originating
        // IP. We never want it. Setting `$ip: null` in properties is
        // the documented opt-out (PostHog respects it before geo-
        // resolution runs).
        map.insert("$ip".to_string(), serde_json::Value::Null);

        // PostHog Person properties — attach user-level metadata so
        // dashboards can build cohorts (retention, "users on v0.6.20",
        // platform breakdown) without joining events. Three flavours:
        //   - `$set_once`: only takes effect on first event for this
        //     distinct_id. Subsequent events leave the value alone.
        //     Used for fields that describe the user's "first contact".
        //   - `$set`: overwrites every time. Used for "current state"
        //     fields. Sent on every event so PostHog always has the
        //     freshest value.
        //   - `$add`: atomic increment (PostHog Person operator). Used
        //     for cumulative counters (total_transcriptions etc.) so
        //     cohort queries can filter "users with >10 transcriptions"
        //     in O(1) without an events-table scan.
        let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let app_version = env!("CARGO_PKG_VERSION");
        let os = crate::telemetry::events::os_name();
        let arch = crate::telemetry::events::arch_name();

        map.insert(
            "$set_once".to_string(),
            serde_json::json!({
                "first_seen_at": now_iso,
                "first_app_version": app_version,
                "first_os": os,
                "first_arch": arch,
            }),
        );

        // Build $set incrementally so we can inject event-specific
        // "latest_*" fields (provider, mode) when the source event
        // carries them. The base layer is the always-present platform
        // metadata; on top we splice in latest_stt_* from
        // transcription.completed and latest_llm_* from llm.applied.
        let mut set_obj = serde_json::Map::new();
        set_obj.insert(
            "latest_app_version".to_string(),
            serde_json::Value::String(app_version.to_string()),
        );
        set_obj.insert(
            "latest_seen_at".to_string(),
            serde_json::Value::String(now_iso),
        );
        set_obj.insert(
            "latest_os".to_string(),
            serde_json::Value::String(os.to_string()),
        );
        set_obj.insert(
            "latest_arch".to_string(),
            serde_json::Value::String(arch.to_string()),
        );
        if event_name == "transcription.completed" {
            if let Some(p) = map.get("provider").cloned() {
                set_obj.insert("latest_stt_provider".to_string(), p);
            }
            if let Some(m) = map.get("mode").cloned() {
                set_obj.insert("latest_stt_mode".to_string(), m);
            }
        }
        if event_name == "llm.applied" {
            if let Some(p) = map.get("provider").cloned() {
                set_obj.insert("latest_llm_provider".to_string(), p);
            }
        }
        map.insert("$set".to_string(), serde_json::Value::Object(set_obj));

        // $add: atomic counters for cohort segmentation. Each branch is
        // a discrete event-name match so we never silently double-count
        // (e.g. transcription.completed and transcription.failed do NOT
        // both feed `total_transcriptions`).
        let add_block: Option<serde_json::Value> = match event_name {
            "transcription.completed" => Some(serde_json::json!({"total_transcriptions": 1})),
            "transcription.failed" => Some(serde_json::json!({"total_transcription_failures": 1})),
            "transcription.cancelled" => {
                Some(serde_json::json!({"total_transcriptions_cancelled": 1}))
            }
            "llm.applied" => Some(serde_json::json!({"total_llm_uses": 1})),
            "llm.failed" => Some(serde_json::json!({"total_llm_failures": 1})),
            "app.started" => Some(serde_json::json!({"total_sessions": 1})),
            _ => None,
        };
        if let Some(add) = add_block {
            map.insert("$add".to_string(), add);
        }
    }

    let body = serde_json::json!({
        "api_key": POSTHOG_API_KEY,
        "event": event_name,
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

    /// `$set_once` and `$set` Person-property blocks must be present
    /// on every payload. PostHog needs them to populate the User
    /// (Person) record so cohort/retention queries work without
    /// joining the events table on every operation.
    #[test]
    fn build_payload_emits_person_property_set_blocks() {
        let e = Event::AppStarted {
            version: "0.6.20",
            os: "windows",
            arch: "x86_64",
            cold_start_ms: 100,
        };
        let p = build_payload(&e).expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        let props = &v["properties"];

        let set_once = &props["$set_once"];
        assert!(set_once.is_object(), "$set_once must be an object");
        assert!(set_once["first_seen_at"].is_string());
        assert_eq!(set_once["first_app_version"], env!("CARGO_PKG_VERSION"));
        assert!(set_once["first_os"].is_string());
        assert!(set_once["first_arch"].is_string());

        let set = &props["$set"];
        assert!(set.is_object(), "$set must be an object");
        assert_eq!(set["latest_app_version"], env!("CARGO_PKG_VERSION"));
        assert!(set["latest_seen_at"].is_string());
        assert!(set["latest_os"].is_string());
        assert!(set["latest_arch"].is_string());
    }

    /// `first_seen_at` must be in ISO 8601 UTC format (PostHog parses
    /// timestamps in this shape for the People view).
    #[test]
    fn person_property_first_seen_at_is_iso_utc() {
        let p = build_payload(&Event::OnboardingStarted).expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        let ts = v["properties"]["$set_once"]["first_seen_at"]
            .as_str()
            .expect("first_seen_at present");
        // 2026-04-27T07:30:12Z — at least 20 chars, ends with Z, has T.
        assert!(ts.len() >= 20, "ISO timestamp too short: {}", ts);
        assert!(ts.ends_with('Z'), "ISO timestamp must end with Z: {}", ts);
        assert!(ts.contains('T'), "ISO timestamp must contain T: {}", ts);
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

    /// `$ip: null` must always be present so PostHog skips its
    /// server-side IP capture / geo-resolution pipeline. Sentry EU
    /// already drops IPs by default; PostHog needs explicit opt-out.
    #[test]
    fn build_payload_opts_out_of_ip_capture() {
        let p = build_payload(&Event::OnboardingStarted).expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        assert!(
            v["properties"]["$ip"].is_null(),
            "$ip must be null (got {:?})",
            v["properties"]["$ip"]
        );
    }

    /// Cumulative counters via PostHog `$add` operator: the right
    /// counter increments for the right event, no double-counting,
    /// no counter on events that shouldn't carry one.
    #[test]
    fn build_payload_increments_correct_counter_per_event() {
        let cases: &[(Event, &str)] = &[
            (
                Event::TranscriptionCompleted {
                    mode: "cloud",
                    provider: "groq",
                    local_backend: "",
                    entry_point: "hotkey",
                    audio_secs: 1.0,
                    processing_ms: 100,
                    word_count: 5,
                    language: "en".into(),
                    success: true,
                    had_filler_removal: false,
                    had_llm: false,
                    engine: "batch",
                },
                "total_transcriptions",
            ),
            (
                Event::TranscriptionFailed {
                    mode: "cloud",
                    provider: "groq",
                    error_category: "401",
                },
                "total_transcription_failures",
            ),
            (
                Event::TranscriptionCancelled { audio_secs: 2.0 },
                "total_transcriptions_cancelled",
            ),
            (
                Event::LlmApplied {
                    mode: "cloud",
                    provider: "openai",
                    style: "casual".into(),
                    tone: "neutral".into(),
                    processing_ms: 200,
                    success: true,
                },
                "total_llm_uses",
            ),
            (
                Event::LlmFailed {
                    mode: "cloud",
                    provider: "openai",
                    error_category: "429",
                },
                "total_llm_failures",
            ),
            (
                Event::AppStarted {
                    version: "0.6.20",
                    os: "windows",
                    arch: "x86_64",
                    cold_start_ms: 50,
                },
                "total_sessions",
            ),
        ];
        for (event, expected_counter) in cases {
            let p = build_payload(event).expect("build");
            let v: serde_json::Value = serde_json::from_str(&p).expect("json");
            let add = &v["properties"]["$add"];
            assert!(add.is_object(), "$add missing for {:?}", event);
            assert_eq!(
                add[expected_counter], 1,
                "counter {} should be 1 for {:?}",
                expected_counter, event
            );
        }
    }

    /// Events that don't represent a user action (config changes,
    /// feature triggers, perf snapshots) must NOT increment any
    /// `$add` counter — those signals are already captured by the
    /// event itself; double-counting via $add would inflate cohort
    /// metrics.
    #[test]
    fn build_payload_omits_add_for_non_counter_events() {
        for event in [
            Event::FeatureHotkeyTriggered,
            Event::FeatureApiKeySet {
                scope: "stt",
                provider: "groq",
            },
            Event::ConfigPreprocessingChanged { enabled: true },
            Event::PerfGpuStatus {
                backend: "vulkan",
                fell_back_to_cpu: false,
                known_bad: false,
            },
        ] {
            let p = build_payload(&event).expect("build");
            let v: serde_json::Value = serde_json::from_str(&p).expect("json");
            assert!(
                v["properties"].get("$add").is_none()
                    || v["properties"]["$add"]
                        .as_object()
                        .map(|o| o.is_empty())
                        .unwrap_or(true),
                "event {:?} must NOT carry an $add block",
                event
            );
        }
    }

    /// `latest_stt_provider` must be set on `transcription.completed`
    /// only — not on every event. This is what enables filters like
    /// "users whose latest STT provider is groq".
    #[test]
    fn build_payload_sets_latest_stt_provider_on_transcription_completed() {
        let e = Event::TranscriptionCompleted {
            mode: "cloud",
            provider: "anthropic",
            local_backend: "",
            entry_point: "hotkey",
            audio_secs: 1.0,
            processing_ms: 100,
            word_count: 5,
            language: "it".into(),
            success: true,
            had_filler_removal: false,
            had_llm: false,
            engine: "batch",
        };
        let p = build_payload(&e).expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        assert_eq!(v["properties"]["$set"]["latest_stt_provider"], "anthropic");
        assert_eq!(v["properties"]["$set"]["latest_stt_mode"], "cloud");
    }

    /// `latest_llm_provider` must be set on `llm.applied` only.
    #[test]
    fn build_payload_sets_latest_llm_provider_on_llm_applied() {
        let e = Event::LlmApplied {
            mode: "cloud",
            provider: "openai",
            style: "casual".into(),
            tone: "neutral".into(),
            processing_ms: 200,
            success: true,
        };
        let p = build_payload(&e).expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        assert_eq!(v["properties"]["$set"]["latest_llm_provider"], "openai");
    }

    /// New event variants must roundtrip through name() and properties()
    /// without panicking and produce the expected event names.
    #[test]
    fn new_feature_events_roundtrip() {
        assert_eq!(
            Event::FeatureHotkeyTriggered.name(),
            "feature.hotkey_triggered"
        );
        assert_eq!(
            Event::FeatureApiKeySet {
                scope: "stt",
                provider: "groq",
            }
            .name(),
            "feature.api_key_set"
        );
        // Properties roundtrip — must not panic and produce the
        // declared field names.
        let p = build_payload(&Event::FeatureApiKeySet {
            scope: "llm",
            provider: "openai",
        })
        .expect("build");
        let v: serde_json::Value = serde_json::from_str(&p).expect("json");
        assert_eq!(v["properties"]["scope"], "llm");
        assert_eq!(v["properties"]["provider"], "openai");
    }
}

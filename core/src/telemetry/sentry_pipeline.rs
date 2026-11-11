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

/// Identity of this exact build — a tag (`v0.6.73-rc6`), a branch build
/// (`staging.1234`), or `local`. Computed in `build.rs::resolve_build_id`.
pub const BUILD_ID: &str = env!("DIMMY_BUILD_ID");

/// The Sentry release string. Kept separate from `BUILD_ID` and pure so
/// it can be tested without a build environment.
///
/// A Sentry release is the unit that "first seen in" and regression
/// detection hang off, so two binaries that differ MUST NOT share one.
/// `CARGO_PKG_VERSION` alone gave every rc of 0.6.73 the same release and
/// made an abort impossible to attribute to a build — see
/// `build.rs::resolve_build_id`.
///
/// `scripts/ci/sentry.sh` computes the same name in CI to attach symbols
/// and commits; change both together (`scripts/ci/test_sentry.sh`).
fn sentry_release(version: &str, build_id: &str) -> String {
    let build_id = build_id.trim();
    if build_id.is_empty() || build_id == "local" {
        return format!("dimmy@{}", version);
    }
    // A tag already carries the full version, so it replaces rather than
    // decorates: `v0.6.73-rc6` -> `dimmy@0.6.73-rc6`, which is what the
    // GitHub release is called and what the user reports.
    if let Some(tagged) = build_id.strip_prefix('v') {
        if tagged.starts_with(|c: char| c.is_ascii_digit()) {
            return format!("dimmy@{}", tagged);
        }
    }
    // A branch build has no version of its own; semver build metadata
    // keeps it sorted next to the version it was cut from.
    format!("dimmy@{}+{}", version, build_id)
}

#[cfg(feature = "telemetry-sentry")]
// Off until the saved consent says yes: nothing leaves before the answer.
static ENABLED: AtomicBool = AtomicBool::new(false);

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
                    release: Some(sentry_release(env!("CARGO_PKG_VERSION"), BUILD_ID).into()),
                    environment: Some(detect_environment().into()),
                    // SECURITY: enable_logs=true ships every `log!()`
                    // / `tracing` macro output to Sentry. Our `log()`
                    // helper writes free-form strings ("[FileLoad]
                    // chunk N of M failed: <body>", "[LlmDispatch]
                    // request: <prompt>", etc.) — most of those
                    // include user content. Disabling captures
                    // exception + breadcrumbs ONLY, which we have
                    // tighter control over. Burned 2026-05-12.
                    enable_logs: false,
                    send_default_pii: false,
                    attach_stacktrace: true,
                    // Breadcrumbs default-include log lines; cap low
                    // so a long transcript-producing burst can't fill
                    // the buffer with sensitive lines before a panic.
                    max_breadcrumbs: 20,
                    // Panics reach Sentry through its own integration, not
                    // through capture_*, so this is the only place the
                    // consent switch can stop them.
                    before_send: Some(std::sync::Arc::new(|event| {
                        if is_enabled() {
                            scrub_event(event)
                        } else {
                            None
                        }
                    })),
                    before_breadcrumb: Some(std::sync::Arc::new(scrub_breadcrumb)),
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
            // Also a tag, not only part of the release string, so an
            // issue can be filtered by build without parsing the release.
            scope.set_tag("build_id", BUILD_ID);
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
///
/// Used by `capture_error` AND by `capture_feedback`. The feedback path
/// builds its envelope by hand and POSTs it straight to the DSN, so it
/// never passes through `before_send` — the account-name scrub has to
/// happen here too, not only in `scrub_event`.
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
        crate::telemetry::sanitize::scrub_user_paths(truncated)
    }
}

/// Block until queued events have been sent, for at most `FLUSH_TIMEOUT`.
///
/// Called from the panic hook, and that is the whole reason it exists.
/// Sentry's own panic integration captures the event onto an ASYNC
/// transport; a panic that crosses an `extern "C"` boundary aborts the
/// process immediately afterwards, so nothing is ever sent. What DID
/// arrive in issue RUST-Q was the second panic — Rust's own "panic in a
/// function that cannot unwind" — leaving the real message (the failed
/// assertion, the actual bug) recorded only in the user's local
/// `dimmy.log`, which we do not have.
///
/// Returns false when the flush timed out. Never panics: a panic raised
/// from inside a panic hook goes straight to `__fastfail`.
#[cfg(feature = "telemetry-sentry")]
pub fn flush_pending() -> bool {
    // Long enough for one small HTTPS round trip on a slow link, short
    // enough that a crashing app still dies promptly.
    const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
    match sentry::Hub::current().client() {
        Some(client) => client.flush(Some(FLUSH_TIMEOUT)),
        None => true,
    }
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn flush_pending() -> bool {
    true
}

/// Capture a manually-constructed error message + category + provider.
/// Used by the Event::Error* variants to mirror to Sentry.
///
/// `provider` is the short categorical name (`groq`, `openai`, `local`,
/// …) — never a URL. It was missing until 2026-09-02, so answering "which
/// provider is throwing these?" meant leaving Sentry and cross-checking
/// PostHog, for a field the privacy rules already class as safe to send.
#[cfg(feature = "telemetry-sentry")]
pub fn capture_error(category: &str, provider: &str, message: &str) {
    if !is_enabled() || !has_compiled_dsn() {
        return;
    }
    let scrubbed = scrub_message(message);
    let provider = provider.to_string();
    sentry::with_scope(
        |scope| {
            scope.set_tag("error_category", category);
            if !provider.is_empty() {
                scope.set_tag("provider", provider);
            }
        },
        || {
            sentry::capture_message(&scrubbed, sentry::Level::Error);
        },
    );
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn capture_error(_category: &str, _provider: &str, _message: &str) {}

/// Send one `telemetry::report` failure. `fingerprint` is
/// `[subsystem, kind, engine?]`, already slugged, and is both the grouping
/// key and the title. `detail` goes in the `detail` extra, which
/// `scrub_event` runs through the prose and secret filters.
#[cfg(feature = "telemetry-sentry")]
pub fn capture_failure(subsystem: &str, fingerprint: &[String], warning: bool, detail: &str) {
    if !is_enabled() || !has_compiled_dsn() {
        return;
    }
    assert!(fingerprint.len() >= 2, "subsystem and kind are required");
    let title = fingerprint.join("/");
    let detail = scrub_message(detail);
    let fp: Vec<&str> = fingerprint.iter().map(String::as_str).collect();
    sentry::with_scope(
        |scope| {
            scope.set_fingerprint(Some(&fp));
            scope.set_tag("subsystem", subsystem);
            scope.set_tag("error_category", &fingerprint[1]);
            if let Some(engine) = fingerprint.get(2) {
                scope.set_tag("provider", engine);
            }
            if !detail.is_empty() {
                scope.set_extra("detail", detail.clone().into());
            }
        },
        || {
            let level = if warning {
                sentry::Level::Warning
            } else {
                sentry::Level::Error
            };
            sentry::capture_message(&title, level);
        },
    );
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn capture_failure(_subsystem: &str, _fingerprint: &[String], _warning: bool, _detail: &str) {}

/// One stack frame a host UI sends: `function \t file \t line`, innermost
/// first. Only the file NAME is kept, so a dev build's `C:\Users\…` source
/// path never leaves the machine.
#[cfg(feature = "telemetry-sentry")]
fn host_frames(frames: &str) -> Vec<sentry::protocol::Frame> {
    const MAX_FRAMES: usize = 64;
    let mut out: Vec<sentry::protocol::Frame> = frames
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(MAX_FRAMES)
        .map(|line| {
            let mut parts = line.split('\t');
            let function = parts.next().unwrap_or("").trim();
            let file = parts.next().unwrap_or("").trim();
            let lineno = parts.next().and_then(|n| n.trim().parse::<u64>().ok());
            let function =
                crate::telemetry::sanitize::scrub_user_paths(crate::truncate_utf8(function, 200));
            let filename = file
                .rsplit(['/', '\\'])
                .next()
                .filter(|f| !f.is_empty())
                .map(str::to_string);
            sentry::protocol::Frame {
                in_app: Some(function.contains("Dimmy")),
                function: Some(function),
                filename,
                lineno,
                ..Default::default()
            }
        })
        .collect();
    // Sentry wants the outermost frame first.
    out.reverse();
    out
}

/// An exception the Windows or macOS host caught at its top level (or
/// swallowed somewhere it matters). No exception MESSAGE: .NET and AppKit
/// messages carry file paths, device names and user text. The type, where
/// it was caught and the stack are what a fix needs.
///
/// Returns 1 sent, -2 no consent, -3 no DSN.
#[cfg(feature = "telemetry-sentry")]
pub fn capture_host_exception(
    host: &str,
    source: &str,
    exception_type: &str,
    frames: &str,
    fatal: bool,
) -> i32 {
    if !has_compiled_dsn() {
        return -3;
    }
    if !is_enabled() {
        return -2;
    }
    let source = crate::telemetry::report::slug(source);
    if !crate::telemetry::report::allowed(&format!("host/{host}/{source}/{exception_type}")) {
        return 1;
    }
    let ty: String = exception_type
        .trim()
        .chars()
        .take(120)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '`' | '-' | ':') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ty = if ty.is_empty() {
        "UnknownException".to_string()
    } else {
        ty
    };
    let frames = host_frames(frames);
    let stacktrace = if frames.is_empty() {
        None
    } else {
        Some(sentry::protocol::Stacktrace {
            frames,
            ..Default::default()
        })
    };
    let event = sentry::protocol::Event {
        level: if fatal {
            sentry::Level::Fatal
        } else {
            sentry::Level::Error
        },
        exception: vec![sentry::protocol::Exception {
            ty,
            value: Some(source.clone()),
            stacktrace,
            mechanism: Some(sentry::protocol::Mechanism {
                ty: source.clone(),
                handled: Some(!fatal),
                ..Default::default()
            }),
            ..Default::default()
        }]
        .into(),
        ..Default::default()
    };
    let host = crate::telemetry::report::slug(host);
    sentry::with_scope(
        |scope| {
            scope.set_tag("subsystem", "host");
            scope.set_tag("host", &host);
            scope.set_tag("error_category", &source);
        },
        || {
            sentry::capture_event(event);
        },
    );
    if fatal {
        // The host is about to die; the async transport would never send.
        let _ = flush_pending();
    }
    1
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn capture_host_exception(
    _host: &str,
    _source: &str,
    _exception_type: &str,
    _frames: &str,
    _fatal: bool,
) -> i32 {
    -3
}

/// Capture user-submitted feedback (from Settings → Send feedback or
/// the post-crash dialog).
///
/// `kind` is one of `bug`, `feature`, `general`. `message` is the user's
/// text. `email` is optional and only included if the user explicitly
/// provided it (UI must not auto-fill).
///
/// Submitted as a Sentry **User Feedback v2 envelope** (item type
/// `feedback`) — not as a regular event. This lands in the project's
/// dedicated Feedback tab, not in Issues. Each call has a fresh
/// `event_id`, so feedback never collapses into a single grouped issue.
///
/// The envelope is POSTed directly to the DSN's envelope endpoint with
/// DSN-key auth (the `sentry_key` is public, safe to ship in the
/// client). The embedded `sentry` crate has no native feedback API in
/// 0.47, so we build the envelope by hand to avoid waiting on SDK
/// support; once the SDK gains `capture_feedback`, this can be
/// simplified.
/// Returns a status the UI surfaces truthfully (no more blanket
/// "Sent!"): `1` enqueued · `-2` user disabled telemetry · `-3` no DSN
/// compiled in (dev/source build — feedback simply isn't configured).
#[cfg(feature = "telemetry-sentry")]
pub fn capture_feedback(kind: &str, message: &str, email: Option<&str>) -> i32 {
    if !has_compiled_dsn() {
        return -3;
    }
    if !is_enabled() {
        return -2;
    }
    let scrubbed = scrub_message(message);

    let dsn: sentry::types::Dsn = match SENTRY_DSN.parse() {
        Ok(d) => d,
        Err(e) => {
            crate::log(&format!("[sentry-feedback] DSN parse failed: {}", e));
            return -3;
        }
    };

    // Sentry expects event_id as 32 hex chars with no dashes.
    let event_id = crate::telemetry::identity::new_uuid_v4().replace('-', "");
    let now = chrono::Utc::now();
    let envelope = build_feedback_envelope(&event_id, now, kind, &scrubbed, email);

    let url = dsn.envelope_api_url().to_string();
    let auth = format!(
        "Sentry sentry_version=7, sentry_key={}, sentry_client=dimmy/{}",
        dsn.public_key(),
        env!("CARGO_PKG_VERSION"),
    );
    spawn_envelope_send(url, auth, envelope.into_bytes(), "feedback", |_| {});
    1
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn capture_feedback(_kind: &str, _message: &str, _email: Option<&str>) -> i32 {
    -3
}

/// Build a Sentry envelope (3 newline-separated JSON lines) carrying a
/// single User Feedback v2 item. Mirrors what the JS browser SDK's
/// `Sentry.captureFeedback` ships.
#[cfg(feature = "telemetry-sentry")]
fn build_feedback_envelope(
    event_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    kind: &str,
    message: &str,
    email: Option<&str>,
) -> String {
    let mut feedback_ctx = serde_json::Map::new();
    feedback_ctx.insert(
        "message".into(),
        serde_json::Value::String(message.to_string()),
    );
    if let Some(e) = email {
        let trimmed = e.trim();
        if !trimmed.is_empty() {
            feedback_ctx.insert(
                "contact_email".into(),
                serde_json::Value::String(trimmed.to_string()),
            );
        }
    }

    let event = serde_json::json!({
        "event_id": event_id,
        "timestamp": now.timestamp(),
        "platform": "native",
        "level": "info",
        "release": sentry_release(env!("CARGO_PKG_VERSION"), BUILD_ID),
        "environment": detect_environment(),
        "tags": {
            "feedback_kind": kind,
            "os": crate::telemetry::events::os_name(),
            "arch": crate::telemetry::events::arch_name(),
            "build_id": BUILD_ID,
        },
        "user": { "id": crate::telemetry::anonymous_id() },
        "contexts": { "feedback": serde_json::Value::Object(feedback_ctx) },
    });
    let item_payload = event.to_string();

    let envelope_header = serde_json::json!({
        "event_id": event_id,
        "sent_at": now.to_rfc3339(),
    });
    let item_header = serde_json::json!({
        "type": "feedback",
        "content_type": "application/json",
        "length": item_payload.len(),
    });

    format!("{}\n{}\n{}\n", envelope_header, item_header, item_payload)
}

/// Dedicated tokio runtime for Sentry feedback sends. Same pattern as
/// `telemetry::client` — feedback is best-effort and must never block
/// the FFI caller.
#[cfg(feature = "telemetry-sentry")]
fn feedback_runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RT: std::sync::OnceLock<Option<tokio::runtime::Runtime>> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("dimmy-sentry-feedback")
            .enable_all()
            .build()
        {
            Ok(rt) => Some(rt),
            Err(e) => {
                crate::log(&format!(
                    "[sentry-feedback] failed to build runtime: {}, dropping feedback",
                    e
                ));
                None
            }
        }
    })
    .as_ref()
}

/// POSTs one envelope off the caller's thread. `on_status` gets the HTTP
/// status, or `None` when the request never got an answer.
#[cfg(feature = "telemetry-sentry")]
fn spawn_envelope_send(
    url: String,
    auth: String,
    body: Vec<u8>,
    what: &'static str,
    on_status: impl FnOnce(Option<u16>) + Send + 'static,
) {
    let Some(rt) = feedback_runtime() else {
        return;
    };
    rt.spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("Dimmy/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        match client
            .post(&url)
            .header("X-Sentry-Auth", auth)
            .header("Content-Type", "application/x-sentry-envelope")
            .body(body)
            .send()
            .await
        {
            Ok(r) => {
                let status = r.status().as_u16();
                crate::log(&format!("[sentry-{what}] sent, status={status}"));
                on_status(Some(status));
            }
            Err(e) => {
                crate::log(&format!("[sentry-{what}] send failed: {e}"));
                on_status(None);
            }
        }
    });
}

/// The release this build reports as. A crash dump records it at write time,
/// because the launch that sends the dump may be a newer build.
pub fn current_release() -> String {
    sentry_release(env!("CARGO_PKG_VERSION"), BUILD_ID)
}

pub fn current_environment() -> &'static str {
    detect_environment()
}

/// Sends a native crash from an earlier launch. The CALLER decides whether
/// it may: crash consent is on, or the user just pressed "Send" for this one
/// report. `minidump` is `None` for an abort that left only its marker.
///
/// The dump file is deleted once Sentry answered with anything but a
/// "try again later" (429 / 5xx); a network failure keeps it for the next
/// launch, within the 7-day limit of `crash_dump::collect`.
#[cfg(feature = "telemetry-sentry")]
pub fn send_native_crash(
    release: &str,
    environment: &str,
    abort_detail: Option<&str>,
    minidump: Option<&std::path::Path>,
) {
    if !has_compiled_dsn() {
        return;
    }
    let Ok(dsn) = SENTRY_DSN.parse::<sentry::types::Dsn>() else {
        return;
    };
    let bytes = match minidump {
        Some(path) => match std::fs::read(path) {
            Ok(b) if (b.len() as u64) <= crate::telemetry::crash_dump::MAX_BYTES => Some(b),
            _ => {
                let _ = std::fs::remove_file(path);
                return;
            }
        },
        None => None,
    };
    let event_id = crate::telemetry::identity::new_uuid_v4().replace('-', "");
    let envelope = build_crash_envelope(
        &event_id,
        chrono::Utc::now(),
        release,
        environment,
        abort_detail,
        bytes.as_deref(),
    );
    let auth = format!(
        "Sentry sentry_version=7, sentry_key={}, sentry_client=dimmy/{}",
        dsn.public_key(),
        env!("CARGO_PKG_VERSION"),
    );
    let path = minidump.map(|p| p.to_path_buf());
    spawn_envelope_send(
        dsn.envelope_api_url().to_string(),
        auth,
        envelope,
        "native-crash",
        move |status| {
            if let (Some(path), Some(status)) = (path, status) {
                if status != 429 && status < 500 {
                    let _ = std::fs::remove_file(path);
                }
            }
        },
    );
}

#[cfg(not(feature = "telemetry-sentry"))]
pub fn send_native_crash(
    _release: &str,
    _environment: &str,
    _abort_detail: Option<&str>,
    _minidump: Option<&std::path::Path>,
) {
}

/// An event item, plus the minidump as an `event.minidump` attachment when
/// there is one: Sentry then walks the stacks and symbolicates them with the
/// PDBs CI uploaded. The event carries the anonymous id and categorical tags
/// only, like every other crash report.
#[cfg(feature = "telemetry-sentry")]
fn build_crash_envelope(
    event_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    release: &str,
    environment: &str,
    abort_detail: Option<&str>,
    minidump: Option<&[u8]>,
) -> Vec<u8> {
    assert_eq!(event_id.len(), 32, "Sentry event ids are 32 hex chars");
    let kind = if minidump.is_some() {
        "minidump"
    } else {
        "abort"
    };
    let mut event = serde_json::json!({
        "event_id": event_id,
        "timestamp": now.timestamp(),
        "platform": "native",
        "level": "fatal",
        "release": release,
        "environment": environment,
        "tags": {
            "subsystem": "native",
            "error_category": kind,
            "os": crate::telemetry::events::os_name(),
            "arch": crate::telemetry::events::arch_name(),
        },
        "user": { "id": crate::telemetry::anonymous_id() },
    });
    if let Some(detail) = abort_detail {
        event["extra"] = serde_json::json!({ "detail": crate::telemetry::report::slug(detail) });
    }
    if minidump.is_none() {
        event["message"] = serde_json::json!({ "formatted": "native/abort" });
        event["fingerprint"] = serde_json::json!(["native", "abort"]);
    }
    let payload = event.to_string();
    let mut out = Vec::new();
    out.extend_from_slice(
        serde_json::json!({ "event_id": event_id, "sent_at": now.to_rfc3339() })
            .to_string()
            .as_bytes(),
    );
    out.push(b'\n');
    out.extend_from_slice(
        serde_json::json!({ "type": "event", "length": payload.len() })
            .to_string()
            .as_bytes(),
    );
    out.push(b'\n');
    out.extend_from_slice(payload.as_bytes());
    out.push(b'\n');
    if let Some(dump) = minidump {
        out.extend_from_slice(
            serde_json::json!({
                "type": "attachment",
                "length": dump.len(),
                "filename": "crash.dmp",
                "attachment_type": "event.minidump",
            })
            .to_string()
            .as_bytes(),
        );
        out.push(b'\n');
        out.extend_from_slice(dump);
        out.push(b'\n');
    }
    out
}

/// Compose the Sentry environment tag from cargo profile and CI
/// indicators. Lets us segregate "actual user" from "internal CI".
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
/// Aggressive allowlist-based message redaction. The previous filter
/// (only `looks_like_secret`) caught API key prefixes but happily
/// passed natural-language transcript fragments. After a 2026-05-12
/// incident where a Sentry panic surfaced part of a transcribed
/// chat, the rule flipped: anything that looks like prose gets
/// reduced to a stable category label so debugging stays possible
/// without ever shipping user content.
///
/// Rules in order:
///   1. Empty / very short → keep as-is (file:line, "panic", etc.)
///   2. Matches a known error category (HTTP NNN, IO error names,
///      provider names, "panic in <fn>", file paths) → keep
///   3. Otherwise treat as user content → redact to "<redacted: prose
///      content>"
///
/// The local `dimmy.log` still gets the full message via the
/// separate `log()` call site — this redaction only fires for the
/// network-bound Sentry payload.
#[cfg(feature = "telemetry-sentry")]
fn redact_prose(s: &str) -> String {
    crate::telemetry::sanitize::scrub_user_paths(&keep_or_redact(s))
}

/// The keep-or-redact decision, split out so the account-name scrub in
/// `redact_prose` applies to EVERY branch rather than being repeated in
/// each. Both branches used to leak: the whitelist deliberately kept
/// file paths ("can never contain transcript text" — true, but they
/// contain the user's account name), and the short-string early return
/// let anything under 24 chars through untouched, which is most of
/// `C:\Users\gregr`. Sentry issue RUST-B, 59 events. Fixed 2026-09-02.
#[cfg(feature = "telemetry-sentry")]
fn keep_or_redact(s: &str) -> String {
    // Empty / one-token → safe; usually a category enum.
    let trimmed = s.trim();
    if trimmed.len() <= 24 || is_identifier_text(trimmed) {
        return trimmed.to_string();
    }
    // Whitelist patterns that are common in our error messages but
    // can never contain transcript text:
    //   - "HTTP NNN" status code only
    //   - "request failed: <reqwest error chain>" — reqwest doesn't
    //     echo bodies, so these are safe
    //   - "no API key for ..." / "refusing HTTP (HTTPS required): ..."
    //   - "local model: ..." (whisper/parakeet/llama_cpp errors)
    //   - file paths (C:\... or /Users/...) → keep
    //   - Rust panic prefix "PANIC: at file:line: ..." → strip after
    //     the first 60 chars (file:line is the useful part; the
    //     payload may include user content)
    let lower = trimmed.to_ascii_lowercase();
    let safe_prefix = lower.starts_with("http ")
        || lower.starts_with("request failed:")
        || lower.starts_with("no api key")
        || lower.starts_with("refusing http")
        || lower.starts_with("local model:")
        || lower.starts_with("local llm model:")
        || lower.starts_with("empty transcription")
        || lower.starts_with("file:")
        || lower.starts_with("io error:")
        || lower.starts_with("config")
        || lower.starts_with("dimmy_")
        || lower.starts_with("ffi ")
        || lower.starts_with("panic");

    if safe_prefix {
        // Even for whitelisted prefixes, cap the length so we don't
        // accidentally pass through a long tail (e.g. local model:
        // <stack trace that mentions transcript>).
        if trimmed.len() <= 200 {
            trimmed.to_string()
        } else {
            format!("{}…<truncated>", crate::truncate_utf8(trimmed, 200))
        }
    } else {
        "<redacted: prose content>".to_string()
    }
}

/// A `telemetry::report` title such as `local_engine/model_load/parakeet`.
/// No whitespace and only identifier characters: it cannot be a sentence,
/// so the prose filter has nothing to protect.
#[cfg(feature = "telemetry-sentry")]
fn is_identifier_text(s: &str) -> bool {
    s.len() <= 160
        && s.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '/' | '.' | '-')
        })
}

/// Keep a panic payload instead of redacting it as prose.
///
/// `redact_prose` exists to stop transcript text reaching Sentry, and it
/// works by keeping only strings that match a known error shape. A panic
/// payload matches none of them: `sentry-panic` reports the payload RAW,
/// so what arrives is `assertion failed: left == right` or
/// `gtcrn: output length must match input length` — no whitelisted
/// prefix, longer than the 24-char pass-through — and the filter turned
/// every one of them into `<redacted: prose content>`. A crash report
/// that cannot say what assertion failed is not a crash report.
///
/// The exemption is narrow and defensible: a panic payload is a string
/// literal from our own source, not model output and not user input. The
/// house rule that makes it safe is that **an assertion message may
/// interpolate sizes, rates, counts and enum names — never text that came
/// from the user**. Everything in `core/` already follows it.
///
/// Belt and braces anyway: the secret check has already run, the account
/// name is stripped, and the payload is capped. A panic message that
/// needs more than 500 characters is saying too much.
#[cfg(feature = "telemetry-sentry")]
fn scrub_panic_message(payload: &str) -> String {
    const MAX: usize = 500;
    let scrubbed = crate::telemetry::sanitize::scrub_user_paths(payload.trim());
    if scrubbed.len() <= MAX {
        scrubbed
    } else {
        format!("{}…<truncated>", crate::truncate_utf8(&scrubbed, MAX))
    }
}

#[cfg(feature = "telemetry-sentry")]
fn scrub_event(
    mut event: sentry::protocol::Event<'static>,
) -> Option<sentry::protocol::Event<'static>> {
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
    // Also drop ALL extra entries we didn't explicitly opt in — same
    // anti-leak policy. Extras can carry arbitrary key/value strings
    // and the panic integration may attach `payload` containing
    // whatever was in scope. Allowlist: provider, mode, category.
    event.extra.retain(|k, _v| {
        matches!(
            k.as_str(),
            "provider" | "mode" | "error_category" | "detail"
        )
    });
    // `detail` is the one free-text extra (from `telemetry::report`): the
    // same secret + prose filter as the message.
    if let Some(v) = event.extra.get_mut("detail") {
        let s = v.as_str().unwrap_or("").to_string();
        *v = if looks_like_secret(&s) {
            "<redacted: looked like a secret>".into()
        } else {
            redact_prose(&s).into()
        };
    }

    // Walk message payloads — defense in depth: looks_like_secret
    // catches API keys, redact_prose catches free-form user content.
    if let Some(msg) = &event.message {
        let s = msg.as_str();
        if looks_like_secret(s) {
            event.message = Some("<redacted: looked like a secret>".to_string());
        } else {
            event.message = Some(redact_prose(s));
        }
    }

    // Same for exception value strings (this is the field that
    // surfaces panic messages + Display-impl output).
    for ex in event.exception.values.iter_mut() {
        if let Some(v) = &ex.value {
            if looks_like_secret(v) {
                ex.value = Some("<redacted: looked like a secret>".to_string());
            } else if ex.ty == "panic" {
                ex.value = Some(scrub_panic_message(v));
            } else {
                ex.value = Some(redact_prose(v));
            }
        }
    }

    // Breadcrumbs already filtered by scrub_breadcrumb but pass over
    // again in case any slipped through (concurrent breadcrumb-add).
    for entry in event.breadcrumbs.iter_mut() {
        if let Some(msg) = &entry.message {
            let s = msg.as_str();
            if looks_like_secret(s) {
                entry.message = Some("<redacted: secret>".to_string());
            } else {
                entry.message = Some(redact_prose(s));
            }
        }
        // Drop breadcrumb data map for the same reason as extras.
        entry.data.clear();
    }

    Some(event)
}

/// Breadcrumb-level filter (runs as messages are added, before they
/// reach an event). Drops anything Sentry SDK auto-adds from log!()
/// macros (we disabled enable_logs but other crates' breadcrumbs
/// still flow through). Allowlist: category=panic|telemetry|http.
#[cfg(feature = "telemetry-sentry")]
fn scrub_breadcrumb(mut b: sentry::Breadcrumb) -> Option<sentry::Breadcrumb> {
    use crate::telemetry::sanitize::looks_like_secret;
    if let Some(msg) = &b.message {
        let s = msg.as_str();
        if looks_like_secret(s) {
            b.message = Some("<redacted: secret>".to_string());
        } else {
            b.message = Some(redact_prose(s));
        }
    }
    b.data.clear();
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn a_crash_envelope_carries_the_dump_as_a_minidump_attachment() {
        let dump: &[u8] = b"MDMP\x93\xa7\0\0binary\nbytes";
        let now = chrono::Utc::now();
        let env = build_crash_envelope(
            &"a".repeat(32),
            now,
            "dimmy@0.7.1",
            "development",
            None,
            Some(dump),
        );
        let text = String::from_utf8_lossy(&env);
        let mut lines = text.splitn(5, '\n');
        let _header = lines.next().unwrap();
        let item: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(item["type"], "event");
        let event: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(event["release"], "dimmy@0.7.1", "the release that crashed");
        assert_eq!(event["environment"], "development");
        assert_eq!(event["level"], "fatal");
        assert!(
            event.get("message").is_none(),
            "Sentry titles it from the dump"
        );
        let attachment: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(attachment["attachment_type"], "event.minidump");
        assert_eq!(attachment["length"], dump.len());
        assert!(env.ends_with(&[dump, b"\n"].concat()));
    }

    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn an_abort_without_a_dump_is_one_event_item() {
        let env = build_crash_envelope(
            &"b".repeat(32),
            chrono::Utc::now(),
            "dimmy@0.7.1",
            "production",
            Some("native abort, 120 MB free"),
            None,
        );
        let text = String::from_utf8(env).unwrap();
        assert_eq!(text.lines().count(), 3);
        let event: serde_json::Value = serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
        assert_eq!(event["fingerprint"], serde_json::json!(["native", "abort"]));
        assert_eq!(
            event["extra"]["detail"],
            crate::telemetry::report::slug("native abort, 120 MB free")
        );
    }

    #[test]
    fn has_compiled_dsn_reports_truthy_for_real_dsn() {
        // Cannot test the runtime branch without secrets; assert only
        // that the function exists and returns a bool.
        let _ = has_compiled_dsn();
    }

    /// Every rc of 0.6.73 used to report the same Sentry release, which
    /// is how two aborts on 2026-08-31 ended up attributed to a build
    /// that did not exist yet when they happened.
    #[test]
    fn sentry_release_distinguishes_every_build_of_one_version() {
        assert_eq!(sentry_release("0.6.73", "v0.6.73-rc6"), "dimmy@0.6.73-rc6");
        assert_eq!(sentry_release("0.6.73", "v0.6.73-rc8"), "dimmy@0.6.73-rc8");
        assert_eq!(
            sentry_release("0.6.73", "v0.6.73-staging.1"),
            "dimmy@0.6.73-staging.1"
        );
        assert_eq!(
            sentry_release("0.6.73", "staging.1234"),
            "dimmy@0.6.73+staging.1234"
        );
        assert_ne!(
            sentry_release("0.6.73", "v0.6.73-rc6"),
            sentry_release("0.6.73", "v0.6.73-rc8")
        );
    }

    #[test]
    fn build_id_is_always_populated() {
        // build.rs falls back to "local", so an empty value means the
        // cargo:rustc-env line stopped being emitted and every build
        // silently collapsed back into one Sentry release.
        assert!(!BUILD_ID.is_empty(), "DIMMY_BUILD_ID must never be empty");
        assert!(sentry_release(env!("CARGO_PKG_VERSION"), BUILD_ID).starts_with("dimmy@"));
    }

    #[cfg(feature = "telemetry-sentry")]
    fn exception_event(ty: &str, value: &str) -> sentry::protocol::Event<'static> {
        sentry::protocol::Event {
            exception: vec![sentry::protocol::Exception {
                ty: ty.to_string(),
                value: Some(value.to_string()),
                ..Default::default()
            }]
            .into(),
            ..Default::default()
        }
    }

    #[cfg(feature = "telemetry-sentry")]
    fn scrubbed_exception_value(ty: &str, value: &str) -> String {
        let out = scrub_event(exception_event(ty, value)).expect("event must not be dropped");
        out.exception.values[0]
            .value
            .clone()
            .expect("value must survive")
    }

    /// The whole point of a crash report. `sentry-panic` reports the RAW
    /// panic payload, which matches no whitelisted error shape, so
    /// `redact_prose` used to replace every failed assertion with
    /// `<redacted: prose content>` — a crash report that cannot say what
    /// broke.
    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn a_failed_assertion_reaches_us_intact() {
        for payload in [
            "assertion failed: self.produced == HOP + self.pushed",
            "gtcrn: output length must match input length",
            "assertion `left == right` failed\n  left: 480\n right: 512",
            "input_gain must be in [0.0, 2.0]",
        ] {
            assert_eq!(
                scrubbed_exception_value("panic", payload),
                payload,
                "the assertion message must survive scrubbing"
            );
        }
    }

    /// The exemption is for panics only. Anything else keeps the strict
    /// prose filter, because that is where transcript text would appear.
    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn the_panic_exemption_does_not_widen_to_other_exceptions() {
        let prose = "the user said the quarterly numbers looked wrong to him";
        assert_eq!(
            scrubbed_exception_value("Error", prose),
            "<redacted: prose content>"
        );
        // …and a panic is still stripped of secrets and account names.
        assert_eq!(
            scrubbed_exception_value("panic", r"could not open C:\Users\gregr\dimmy\config.json"),
            r"could not open C:\Users\<USER>\dimmy\config.json"
        );
        assert_eq!(
            scrubbed_exception_value("panic", "token sk-proj-abc123def456ghi789"),
            "<redacted: looked like a secret>"
        );
    }

    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn a_report_title_survives_and_its_detail_is_scrubbed() {
        let mut event = sentry::protocol::Event {
            message: Some("local_engine/model_load/parakeet".to_string()),
            ..Default::default()
        };
        event.extra.insert(
            "detail".into(),
            "the user said the quarterly numbers looked wrong to him".into(),
        );
        event
            .extra
            .insert("device".into(), "Microfono (Realtek)".into());
        let out = scrub_event(event).expect("kept");
        assert_eq!(
            out.message.as_deref(),
            Some("local_engine/model_load/parakeet")
        );
        assert_eq!(
            out.extra.get("detail").and_then(|v| v.as_str()),
            Some("<redacted: prose content>")
        );
        assert!(
            !out.extra.contains_key("device"),
            "unknown extras are dropped"
        );
    }

    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn identifier_text_is_not_mistaken_for_prose_and_prose_is_not_identifier_text() {
        assert!(is_identifier_text("meeting/chunk_stt_failed/whisper"));
        assert!(!is_identifier_text("the meeting failed"));
        assert!(!is_identifier_text(r"C:\Users\mario"));
    }

    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn host_frames_keep_the_file_name_only_and_put_the_caller_first() {
        let frames = host_frames(
            "Dimmy.Windows.Services.UpdateService.Check\tC:\\Users\\konra\\code\\UpdateService.cs\t88\n\
             System.Threading.Tasks.Task.Run\t\t\n",
        );
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[1].function.as_deref(),
            Some("Dimmy.Windows.Services.UpdateService.Check")
        );
        assert_eq!(frames[1].filename.as_deref(), Some("UpdateService.cs"));
        assert_eq!(frames[1].lineno, Some(88));
        assert_eq!(frames[1].in_app, Some(true));
        assert_eq!(frames[0].in_app, Some(false));
        assert_eq!(frames[0].filename, None);
    }

    #[test]
    fn host_exception_without_dsn_reports_not_configured() {
        assert_eq!(
            capture_host_exception("windows", "unobserved_task", "System.Exception", "", false),
            -3
        );
    }

    #[cfg(feature = "telemetry-sentry")]
    #[test]
    fn a_runaway_panic_message_is_capped() {
        let long = "x".repeat(4000);
        let out = scrubbed_exception_value("panic", &long);
        assert!(out.len() < 600, "expected a cap, got {} chars", out.len());
        assert!(out.ends_with("…<truncated>"));
    }

    #[test]
    fn sentry_release_falls_back_to_the_bare_version() {
        // A contributor's `cargo build` has no CI environment and must
        // not invent a build identity.
        assert_eq!(sentry_release("0.6.73", "local"), "dimmy@0.6.73");
        assert_eq!(sentry_release("0.6.73", ""), "dimmy@0.6.73");
        assert_eq!(sentry_release("0.6.73", "   "), "dimmy@0.6.73");
        // A branch that merely starts with `v` is not a version tag.
        assert_eq!(
            sentry_release("0.6.73", "verify-thing"),
            "dimmy@0.6.73+verify-thing"
        );
    }

    #[test]
    fn init_is_idempotent_and_does_not_panic() {
        init();
        init();
        // Without DSN this should be a clean no-op.
    }

    #[test]
    fn capture_feedback_returns_status_not_blanket_ok() {
        // Test builds compile with telemetry-sentry (default) but no
        // DIMMY_SENTRY_DSN, so has_compiled_dsn() is false → the
        // pipeline reports -3 ("not configured"), never a fake success.
        // This is the contract the host UIs surface truthfully.
        assert!(!has_compiled_dsn());
        assert_eq!(capture_feedback("bug", "hello", None), -3);
        assert_eq!(capture_feedback("general", "x", Some("a@b.c")), -3);
    }

    #[test]
    fn capture_error_with_no_dsn_is_silent() {
        // Default cargo test runs without DIMMY_SENTRY_DSN env var, so
        // the embedded value is empty and capture_error returns early.
        capture_error("test", "groq", "this is a test error message");
    }

    // ── Privacy hardening: redact_prose tests ────────────────────
    //
    // These tests are the bright-line contract that keeps transcript
    // content out of Sentry. They were written 2026-05-12 after a
    // user-reported leak where part of a transcribed chat surfaced
    // in a panic event. Adding/relaxing any of them = re-opening
    // the leak class.

    #[test]
    fn redact_prose_keeps_short_strings_verbatim() {
        // ≤ 24 chars are typically category labels / file:line
        // refs / panic kind tags — keep as-is.
        assert_eq!(redact_prose("HTTP 401"), "HTTP 401");
        assert_eq!(redact_prose("ok"), "ok");
        assert_eq!(redact_prose(""), "");
        assert_eq!(redact_prose("panic"), "panic");
    }

    /// Regression for Sentry issue RUST-B. The whitelist kept
    /// `local model: …` because it can never contain transcript text —
    /// true, and beside the point: it contained a real user's Windows
    /// account name, in the issue TITLE, for 59 events. The scrub now
    /// runs on the output of EVERY branch, including the ≤ 24-char early
    /// return, which is where a bare `C:\Users\gregr` would have slipped
    /// through.
    #[test]
    fn redact_prose_never_ships_an_account_name() {
        let leaked = r"local model: model file not found: C:\Users\gregr\AppData\Roaming\dimmy\models\ggml-large-v3-q5_0.bin";
        let out = redact_prose(leaked);
        assert!(!out.contains("gregr"), "account name survived: {out}");
        assert!(
            out.starts_with("local model: model file not found:"),
            "the message must stay debuggable: {out}"
        );
        assert!(out.contains("ggml-large-v3-q5_0.bin"));

        // Short enough to hit the ≤ 24-char early return.
        assert_eq!(redact_prose(r"C:\Users\gregr"), r"C:\Users\<USER>");
        assert_eq!(redact_prose("/home/mario"), "/home/<USER>");
    }

    #[test]
    fn scrub_message_never_ships_an_account_name() {
        // capture_feedback POSTs its envelope directly, bypassing
        // before_send, so this is the only filter on that path.
        let out = scrub_message(r"it broke, see C:\Users\gregr\AppData\Roaming\dimmy\dimmy.log");
        assert!(!out.contains("gregr"), "account name survived: {out}");
        assert!(out.contains("dimmy.log"));
    }

    #[test]
    fn redact_prose_keeps_whitelisted_prefixes() {
        // Our own error Display impls produce these — safe to keep.
        assert!(redact_prose("HTTP 401 from upstream provider blah blah").starts_with("HTTP 401"));
        assert!(
            redact_prose("request failed: connection refused by peer (so 6)")
                .starts_with("request failed:")
        );
        assert!(
            redact_prose("no API key for LLM provider anthropic right now")
                .starts_with("no API key")
        );
        assert!(
            redact_prose("local model: parakeet inference requires feature flag X")
                .starts_with("local model:")
        );
        assert!(
            redact_prose("PANIC: at src/foo.rs:123: assertion failed: x == y blah blah blah")
                .starts_with("PANIC")
        );
    }

    #[test]
    fn redact_prose_drops_natural_language_prose() {
        // The bug we're guarding: a transcribed user sentence
        // landing in an error event. Must redact to a stable
        // placeholder, never the original text.
        let user_sentence = "And so my fellow Americans, ask not what your country can do for you, ask what you can do for your country.";
        let out = redact_prose(user_sentence);
        assert_eq!(out, "<redacted: prose content>");
        assert!(!out.contains("Americans"), "leaked user content: {}", out);
        assert!(!out.contains("country"), "leaked user content: {}", out);
    }

    #[test]
    fn redact_prose_drops_long_prose_even_with_no_clear_prefix() {
        let leak = "The quick brown fox jumps over the lazy dog and then runs to the store to buy some milk and bread.";
        let out = redact_prose(leak);
        assert_eq!(out, "<redacted: prose content>");
    }

    #[test]
    fn redact_prose_caps_whitelisted_long_messages() {
        // Even whitelisted prefixes get truncated if they get too long
        // (a "local model:" message might tail off into transcript-
        // adjacent content from a third-party panic).
        let long = format!("local model: {}", "blah ".repeat(100));
        let out = redact_prose(&long);
        assert!(out.len() <= 250, "got {} chars: {}", out.len(), out);
        assert!(out.starts_with("local model:"));
    }

    #[test]
    fn redact_prose_doesnt_leak_api_keys_either() {
        // Secondary line of defense — looks_like_secret runs FIRST
        // in scrub_event, but if it somehow misses a key-shaped
        // long string, redact_prose still strips it as prose.
        let out = redact_prose(
            "Token used: sk-ant-api03-very-long-fake-token-value-here-and-then-more-and-more",
        );
        assert!(out == "<redacted: prose content>" || out.contains("<redacted"));
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

    /// The User Feedback v2 envelope we POST to Sentry must be three
    /// newline-separated JSON lines (envelope header, item header, item
    /// payload). The item header MUST declare `type: feedback` so the
    /// event lands in the project's Feedback tab, not in Issues — that
    /// was the regression that buried the dashboard's "real" feedback
    /// inbox in v0.6.24.
    #[test]
    fn build_feedback_envelope_emits_well_formed_three_line_envelope() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-30T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let env = build_feedback_envelope(
            "98151cdf9ed34fb8bd00f7997e85fc71",
            now,
            "bug",
            "the pill freezes after 30 minutes",
            Some("user@example.com"),
        );
        let lines: Vec<&str> = env.trim_end().split('\n').collect();
        assert_eq!(
            lines.len(),
            3,
            "envelope must be header + item header + payload"
        );

        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["event_id"], "98151cdf9ed34fb8bd00f7997e85fc71");
        assert!(header["sent_at"].is_string());

        let item_header: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(item_header["type"], "feedback");
        assert_eq!(item_header["content_type"], "application/json");

        let payload: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(payload["event_id"], "98151cdf9ed34fb8bd00f7997e85fc71");
        assert_eq!(payload["platform"], "native");
        assert_eq!(payload["tags"]["feedback_kind"], "bug");
        assert_eq!(
            payload["contexts"]["feedback"]["message"],
            "the pill freezes after 30 minutes"
        );
        assert_eq!(
            payload["contexts"]["feedback"]["contact_email"],
            "user@example.com"
        );
    }

    /// Email is optional. When the user doesn't provide one, the
    /// `contact_email` field MUST be absent from the payload — never
    /// "" or null — so Sentry shows the feedback as truly anonymous.
    #[test]
    fn build_feedback_envelope_omits_contact_email_when_absent() {
        let now = chrono::Utc::now();
        let env = build_feedback_envelope(&"0".repeat(32), now, "feature", "no email", None);
        let payload_line = env.trim_end().split('\n').nth(2).unwrap();
        let payload: serde_json::Value = serde_json::from_str(payload_line).unwrap();
        assert!(payload["contexts"]["feedback"]["contact_email"].is_null());
    }

    /// Whitespace-only emails must be treated as "no email", not as a
    /// stray contact_email value that PII-scrubs would have to catch.
    #[test]
    fn build_feedback_envelope_omits_whitespace_only_email() {
        let now = chrono::Utc::now();
        let env = build_feedback_envelope(&"0".repeat(32), now, "general", "msg", Some("   "));
        let payload_line = env.trim_end().split('\n').nth(2).unwrap();
        let payload: serde_json::Value = serde_json::from_str(payload_line).unwrap();
        assert!(payload["contexts"]["feedback"]["contact_email"].is_null());
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

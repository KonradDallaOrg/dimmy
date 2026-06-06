# Telemetry implementation reference

_As-built state — kept in sync with code in `core/src/telemetry/`._
_Last updated: 2026-04-27 (Phase 3a)._

User-facing privacy policy: [`PRIVACY.md`](../../PRIVACY.md). This doc covers the engineering layer: what each module does, how events are routed, what the dashboards expect.

---

## Architecture

```
┌─────────────────────┐      ┌──────────────────────────┐
│  FFI layer (ffi.rs) │ ──▶  │  crate::telemetry::track │
│  call sites         │      │      (events.rs)         │
└─────────────────────┘      └──────────────┬───────────┘
                                            │
                ┌───────────────────────────┴───────────────┐
                ▼                                            ▼
   ┌──────────────────────────┐              ┌──────────────────────────┐
   │  client.rs (PostHog)     │              │  sentry_pipeline.rs      │
   │  - build_payload (JSON)  │              │  - capture_error         │
   │  - dedicated tokio rt    │              │  - capture_feedback      │
   │  - HTTPS POST EU ingest  │              │  - panic hook (auto)     │
   └──────────────────────────┘              └──────────────────────────┘
```

Two pipelines. Same events trigger different routes:

- Every analytics event goes through `client.rs` → PostHog EU.
- A subset (errors, panics, feedback) ALSO goes through `sentry_pipeline.rs` → Sentry EU.

Both pipelines are best-effort: a flaky network never affects the user-facing flow.

---

## Modules

### `core/src/telemetry/mod.rs`
Public surface re-exports + the analytics enable/disable toggles. Two independent flags backed by atomics:
- `client::set_enabled` (PostHog analytics)
- `sentry_pipeline::set_enabled` (Sentry crash reports)

Both default to `true`. The C# host calls `dimmy_telemetry_set_enabled` / `dimmy_telemetry_set_crash_enabled` when the user flips the Settings → Privacy toggles.

### `core/src/telemetry/events.rs`
The typed event taxonomy (single source of truth). Every event is a variant of the `Event` enum with explicit property fields. There is **no** free-string event API — adding a new event requires adding a variant.

Current variants (V16+):

| Event | Properties |
|---|---|
| `app.started` | version, os, arch, cold_start_ms |
| `app.session_ended` | duration_secs, transcribe_count |
| `app.update_check` | current_version, available_version *(reserved — not yet emitted)* |
| `app.update_applied` | from_version, to_version *(reserved)* |
| `onboarding.started` | – *(reserved — needs C# emit)* |
| `onboarding.step_completed` | step *(reserved)* |
| `onboarding.completed` | path, duration_secs *(reserved)* |
| `onboarding.abandoned` | last_step *(reserved)* |
| `config.stt_mode_changed` | mode |
| `config.cloud_provider_changed` | provider |
| `config.llm_enabled_changed` | enabled |
| `config.llm_style_changed` | style |
| `config.shortcut_changed` | – *(reserved)* |
| `config.preprocessing_changed` | enabled |
| `config.input_gain_changed` | gain |
| `transcription.completed` | mode, provider, audio_secs, processing_ms, word_count, language, success, had_filler_removal, had_llm |
| `transcription.failed` | mode, provider, error_category |
| `transcription.cancelled` | audio_secs |
| `llm.applied` | mode, provider, style, tone, processing_ms, success |
| `llm.failed` | mode, provider, error_category |
| `perf.startup_ms` | value, version *(reserved — overlaps app.started.cold_start_ms)* |
| `perf.gpu_status` | backend, fell_back_to_cpu, known_bad |
| `perf.transcribe_overhead_pct` | value, mode, provider *(reserved)* |
| `error.cloud_stt` | provider, status_code, error_category *(reserved — covered by transcription.failed)* |
| `error.cloud_llm` | provider, status_code, error_category *(reserved)* |
| `error.local_stt` | model, error_category *(reserved)* |
| `error.local_llm` | model, error_category *(reserved)* |
| `error.gpu_crash` | backend, context |
| `error.audio_health` | code *(reserved)* |
| `feature.hotkey_triggered` | – |
| `feature.api_key_set` | scope, provider |

### `core/src/telemetry/client.rs` — PostHog
Plain HTTP POST to `https://eu.i.posthog.com/i/v0/e/`. No SDK dependency.

#### Payload shape (V16+)
```json
{
  "api_key": "phc_…",
  "event": "transcription.completed",
  "distinct_id": "<UUIDv4>",
  "properties": {
    // event-variant fields
    "mode": "cloud", "provider": "groq", "audio_secs": 1.2, …,

    // common (added by build_payload to every event)
    "app_version": "0.6.20", "os": "windows", "arch": "x86_64",
    "session_id": "<UUIDv4 per process>",
    "$ip": null,

    // Person property operators (PostHog)
    "$set_once": {
      "first_seen_at": "<ISO UTC>",
      "first_app_version": "0.6.20",
      "first_os": "windows", "first_arch": "x86_64"
    },
    "$set": {
      "latest_app_version": "0.6.20",
      "latest_seen_at": "<ISO UTC>",
      "latest_os": "windows", "latest_arch": "x86_64",
      // Conditional, by event:
      "latest_stt_provider": "groq",     // on transcription.completed
      "latest_stt_mode": "cloud",        // on transcription.completed
      "latest_llm_provider": "openai"    // on llm.applied
    },
    "$add": {
      // Conditional, exactly one counter per matching event:
      "total_transcriptions": 1          // on transcription.completed
      // OR total_transcription_failures, total_transcriptions_cancelled,
      // total_llm_uses, total_llm_failures, total_sessions.
    }
  }
}
```

#### `$add` counter mapping (idempotent per event)
| Event | Counter |
|---|---|
| `transcription.completed` | `total_transcriptions` |
| `transcription.failed` | `total_transcription_failures` |
| `transcription.cancelled` | `total_transcriptions_cancelled` |
| `llm.applied` | `total_llm_uses` |
| `llm.failed` | `total_llm_failures` |
| `app.started` | `total_sessions` |

Other events (`config.*`, `feature.*`, `perf.*`) do **NOT** carry an `$add` block — those are dimensional events, not counters.

#### Defenses
- `before_send` equivalent: `looks_like_secret(payload)` runs as the last step before POST. Any payload matching a secret pattern (Sentry DSN, `phc_`, `phx_`, AWS key shape, JWT, etc.) is dropped and `DROPPED_SECRET_GUARD` increments. Catches code regressions that accidentally serialise a secret into a property.
- `$ip: null` in every event — opts out of PostHog's server-side IP capture / geo-resolution.
- Build-time secret sanitisation (`build.rs::sanitize_secret`): strips leading UTF-8 BOM + ASCII whitespace before embedding `POSTHOG_API_KEY` / `SENTRY_DSN` as `&'static str`.
- Runtime key-diag: on first `track()`, log `[telemetry] key-diag: prefix=phc_xxxx… len=N starts_with_phc=true` so a wrong key is debuggable from `dimmy.log` alone (PostHog itself returns 200 OK for invalid keys — verified 2026-04-27).

#### Runtime
A dedicated single-worker tokio runtime (lazy-init on first send). Most FFI entry points are called from the C# main thread without an active runtime, so we can't rely on `Handle::try_current()`. The dedicated runtime survives for the rest of the process.

### `core/src/telemetry/sentry_pipeline.rs` — Sentry
Wrapper around the `sentry` crate (v0.47, default-features = false, explicit `native-tls` to avoid the rustls/CryptoProvider static-init crash that bricked V8/V11/V12 on WindowsAppSDK + Velopack).

#### What gets sent to Sentry
- **Manual `capture_error(category, message)`** — called from:
  - `dimmy_stop_recording` failure paths (`category` = error_category from STT failure)
  - `dimmy_process_with_llm` failure paths
- **Manual `capture_feedback(kind, message, email)`** — called from `dimmy_telemetry_capture_feedback` (UI Settings → Send feedback).
- **Auto: panic hook** via the `panic` feature of the `sentry` crate. Captures every Rust panic and ships it as a Fatal event.
- **Auto: contexts + backtraces** via the `contexts` and `backtrace` features. OS / device / runtime metadata, `attach_stacktrace = true`.
- **Auto: breadcrumbs** ring buffer, capped at 50 (`max_breadcrumbs: 50`). HTTP/console breadcrumbs only — `crate::log` calls do NOT yet feed Sentry breadcrumbs (Phase 3b candidate).

#### Defenses
- `before_send` hook scrubs:
  - `event.server_name` → null
  - environment variables (`PATH`, `HOME`, `USERPROFILE`, `USER`, `LOGNAME`, `APPDATA`, `LOCALAPPDATA`, `TEMP`, `TMP`)
  - any string field matching `looks_like_secret`
  - same scrub on every breadcrumb message
- `send_default_pii: false` — Sentry skips its default IP / username collection.
- DSN pre-flight: `parse::<sentry::types::Dsn>()` before `sentry::init`. Bad DSN → log + skip, NEVER panic. (sentry-core 0.47 panics on `InvalidUrl`; the pre-flight prevents that panic from crossing the `extern "C"` `dimmy_init` boundary and aborting the cdylib.)
- `catch_unwind` around `sentry::init` itself — belt-and-braces if a future sentry version chooses a different panic surface.
- User record carries the anonymous ID + os/arch tags only. No email, no username.

### `core/src/telemetry/identity.rs`
Anonymous ID generator + persistence. UUIDv4, written to `~/.config/dimmy/analytics_id`. Cached in `OnceLock<String>`. Resettable from FFI (`dimmy_telemetry_reset_anonymous_id`).

`new_uuid_v4()` is also used by `client.rs::session_id` for the per-process session UUID (not persisted).

### `core/src/telemetry/sanitize.rs`
Pure functions used by both pipelines:
- `provider_from_url(url) -> &'static str` — categorical provider tag.
- `error_category(status_code, error_kind) -> &'static str` — stable error categorisation.
- `scrub_path(path) -> String` — replaces `~/`, `/Users/<name>/`, `C:\Users\<name>\` with `<HOME>/`.
- `looks_like_secret(payload) -> bool` — regex-based detector for Sentry DSN, PostHog `phc_`/`phx_`, AWS access keys, JWT-shaped tokens, etc.

11 unit tests covering each.

---

## Build-time secret injection

`core/build.rs`:
1. Reads `POSTHOG_API_KEY` and `SENTRY_DSN` from env.
2. `sanitize_secret(raw)` strips leading UTF-8 BOM + ASCII whitespace.
3. Emits `cargo:rustc-env=DIMMY_POSTHOG_API_KEY=…` and `cargo:rustc-env=DIMMY_SENTRY_DSN=…`.
4. If POSTHOG_API_KEY is non-empty but does not start with `phc_`, or SENTRY_DSN is non-empty but doesn't match the canonical shape, emits `cargo:warning=…` so the CI log surfaces the misconfig.
5. `cargo:rerun-if-env-changed` for both secrets.

GitHub Secrets:
- `POSTHOG_API_KEY` — write-only `phc_` project key. Set with `printf '%s' '<key>' | gh secret set POSTHOG_API_KEY` to avoid trailing-newline corruption.
- `SENTRY_DSN` — full DSN URL. Same rule.

---

## Telemetry-OFF / dev builds

A build is "telemetry-OFF" in two ways:
1. **Cargo feature `telemetry-sentry` disabled** (`--no-default-features`): the `sentry` crate isn't linked at all, all `sentry_pipeline::*` functions are `cfg`-stubbed to no-ops.
2. **No build-time secrets**: env vars unset → `DIMMY_POSTHOG_API_KEY` / `DIMMY_SENTRY_DSN` are empty strings → both pipelines short-circuit with a log line. The runtime never panics on missing secrets.

For local dev: just `cargo build` without secrets. PostHog/Sentry will log `[telemetry] dropped: no compile-time POSTHOG_API_KEY` and `[sentry-init] S0a: empty DSN, skipping` — that's the expected silent-but-loud behaviour.

---

## Verifying the live pipeline

### Did my event reach PostHog?
1. `findstr /i "telemetry" "%APPDATA%\dimmy\dimmy.log"` (or `grep` on macOS/Linux). Look for `[telemetry] track event=…` followed by `[telemetry] send: HTTP 200 OK (sent=N)`.
2. **`HTTP 200 OK is NOT proof of arrival.** PostHog returns 200 for any payload, including ones with bogus api_keys. Verify via the API:
   ```
   curl -H "Authorization: Bearer phx_…" \
     "https://eu.posthog.com/api/projects/<id>/events/?distinct_id=<aid>&limit=20"
   ```
3. Real ingestion latency on EU is ~15-20s API, ~1 min UI. The legacy "1-10 min" claim was wrong (it conflated ingestion delay with silent drops on bad keys).

### Did Sentry receive my error?
1. `findstr /i "sentry" "%APPDATA%\dimmy\dimmy.log"`. The init sequence logs `[sentry-init] S0..S3` markers.
2. Sentry UI: **dimmy.sentry.io** → Issues. New events appear within seconds; the inbox de-dupes by stack signature.

---

## Phase roadmap

| Phase | Status | Scope |
|---|---|---|
| 1: Pipeline | ✅ V15 | PostHog + Sentry actually working in Velopack-installed binary. Required 5 PRs (#27 sentry 0.47, #28 catch_unwind dimmy_init, #29 DSN pre-flight, #31 BOM strip + key-diag, #32 Person properties). |
| 2: `config.*_changed` events + `app_version` / `session_id` enrichment | ✅ V14 | PR #30. |
| 2.5: Person `$set_once` + `$set` blocks for cohort analysis | ✅ V16 | PR #32. |
| **3a: $add counters + latest_*_provider + feature.*_used + gpu_status + $ip:null** | **✅ V17** | **This PR.** |
| 3b: Sentry hardening | ⏳ Deferred | (a) `crate::log` → Sentry breadcrumb hook, (b) `sentry::start_session()` for release health "crash-free users %", (c) `sentry-cli upload-dif` in `release.yml` for de-mangled stack traces. |
| 3c: C#-side onboarding + UI feature events | ⏳ Deferred | `onboarding.*` (4 events), `feature.history_opened`, `feature.settings_opened`, `feature.feedback_sent` mirror in PostHog. |

---

## Adding a new event

1. Add a variant to the `Event` enum in `core/src/telemetry/events.rs`.
2. Add it to `Event::name()` with a stable kebab-case event name.
3. If it should carry a `$add` counter, add the match arm in `core/src/telemetry/client.rs::build_payload`.
4. Wire the emit at the call site (`crate::telemetry::track(Event::… { … })`).
5. Add a unit test in `core/src/telemetry/client::tests` that builds the payload and asserts the property shape.
6. Update this doc and `PRIVACY.md` if the new event collects a new category of information.

NEVER include user content (transcript text, prompt text, file paths, hostnames, microphone names) in a property. The `looks_like_secret` filter is a safety net, not a substitute for review.

---

## Coverage map (source of truth — Layer 1)

Every `Event` variant lives here with a status, and so do the known gaps. The
`telemetry_coverage` test (Layer 2) fails if a variant is missing from this
section, so this table cannot silently drift from the code.

Status legend: `live` = emitted in prod; `reserved` = defined but intentionally
not wired yet (must stay listed in the test's `RESERVED`); `TODO` = a gap we
intend to cover (no variant yet — tracked here so Layer 3 proposes it).

### Live variants (emitted)

```
Lifecycle      AppStarted  AppSessionEnded
Onboarding     OnboardingStarted  OnboardingStepCompleted  OnboardingCompleted  OnboardingAbandoned
Config         ConfigSttModeChanged  ConfigCloudProviderChanged  ConfigLlmEnabledChanged
               ConfigLlmStyleChanged  ConfigPreprocessingChanged  ConfigInputGainChanged
               ConfigAutostartChanged  ConfigRecapModelChanged
Transcription  TranscriptionCompleted  TranscriptionFailed  TranscriptionCancelled
LLM            LlmApplied  LlmFailed
Perf/GPU       PerfGpuStatus  ErrorGpuCrash
Feature        FeatureHotkeyTriggered  FeatureApiKeySet
Licensing      LicenseActivated  LicenseActivationFailed  LicenseRefreshed  LicenseRefreshFailed
               LicenseScopeDenied  LicenseDeviceDeactivated
Meeting        MeetingStarted  MeetingStopped  MeetingPaused  MeetingResumed
               MeetingRecapCompleted  MeetingImportedFromFile
File load      FileLoadStarted  FileLoadCompleted
Dictionary     UserDictWordAdded  UserDictWordRemoved  UserDictSizeSnapshot
Notion         NotionConnected  NotionDisconnected  NotionRecapSent
App rules      AppRulesEvaluated  AppRuleAdded  AppRuleRemoved  AppRuleReordered
Pill           PillVisibilityToggled  PillStyleScrolled  PillLanguageScrolled  PillContextMenuOpened
Update         UpdateChannelChanged  UpdateApplyDeferred
Permissions    PermissionGranted  PermissionDenied
Claude Code    ClaudeCodeStatusProbed  ClaudeCodeLoginSpawned  ClaudeCodeLoginCompleted  ClaudeCodeInvocation
```

`PerfStartupMs` is referenced (not orphaned) but low value — fold into
`AppStarted.cold_start_ms`.

### Reserved variants (defined, NOT emitted — wire or delete)

```
AppUpdateCheck  AppUpdateApplied  ConfigShortcutChanged  PerfTranscribeOverheadPct
ErrorCloudStt  ErrorCloudLlm  ErrorLocalStt  ErrorLocalLlm  ErrorAudioHealth
```

Errors are NOT lost while these are dead: `transcription.failed` / `llm.failed`
carry the category to PostHog and the raw message goes to Sentry. These 5
`Error*` variants are redundant; either wire granular per-surface errors or
delete them. They must stay in `RESERVED` in `core/tests/telemetry_coverage.rs`.

### Gaps (TODO — no variant yet; Layer 3 should propose these)

| Feature | Plan | Priority |
|---|---|---|
| Streaming + local-stream dictation | reuse `TranscriptionCompleted` + an `engine` prop (`batch`/`deepgram_stream`/`local_stream`); fix `entry_point` (now hardcoded "hotkey") | must |
| Local model download | new `ModelDownload` event {phase, model_bucket, success, error_category} | must |
| Recording consent gate | new `ConsentShown` / `ConsentResolved{outcome}` | must |
| Command mode (generate/transform) | new `CommandInvoked{kind, success}` | nice |
| Call detection + nudge | new `CallDetected` / `CallNudge{outcome}` | nice |
| Checkout initiated | new `LicenseCheckoutStarted{tier}` (purchase funnel) | nice |

Deliberately skipped: per-chunk caption events (high cardinality), audio
device-change recovery (Sentry/log only), `config.shortcut_changed` (usage is
already proven by `FeatureHotkeyTriggered`).

---

## Coverage automation (how this is kept in sync)

Three layers; the goal is to never silently ship a user-facing feature with no
metric, without over-instrumenting.

- **Layer 1 — this coverage map.** Single source of truth (above). Adding a
  variant means adding a row here.
- **Layer 2 — `core/tests/telemetry_coverage.rs` (deterministic, every CI run).**
  Fails if any `Event` variant is (a) never emitted and not in `RESERVED`, or
  (b) missing from the coverage map above. So a PR that adds a variant MUST
  wire an emit (or reserve it) AND document it. No LLM, runs in `cargo test`.
- **Layer 3 — `/telemetry-audit` skill (judgment, at release).** Diffs commits
  since the last tag, finds user-facing features (new `dimmy_*` FFI entries,
  new modules, new config-driven behavior) that lack an event, and PROPOSES
  events (name + privacy-safe props) for human approval. This catches the
  "should this have a metric?" question that Layer 2 cannot. Run it before each
  `staging.N` / `rcN`. See `.claude/skills/telemetry-audit/SKILL.md`.

The division: Layer 2 is mechanical hygiene (no dead/undocumented variants),
Layer 3 is the judgment call (does a new surface deserve a metric). Together
they cover dev-time (every PR) and release-time without noise.

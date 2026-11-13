# Telemetry implementation reference

_As-built state — kept in sync with code in `core/src/telemetry/`._
_Last checked against the code: 2026-11-11 (commit ebc0b52f, v0.7.15). Earlier revisions of this file stopped at 2026-04-27 (Phase 3a) and had drifted._

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
- Events raised by the host UI (onboarding, pill, update, permission, user dictionary add / remove, Notion, app-rule add / remove / reorder, file load, meeting recap, meeting-from-file, the `*.login_completed` events) enter through `dimmy_telemetry_track_typed` (`ffi.rs`), which maps an allow-listed event name to an `Event` variant and rejects anything else (returns -2). The hosts never build PostHog payloads. Neither host links the Sentry SDK either: they report exceptions through `dimmy_telemetry_capture_host_exception`.

Both pipelines are best-effort: a flaky network never affects the user-facing flow. The two on/off switches are independent: `telemetry::track` first mirrors a failure event to Sentry (`report::failure_of`, gated by the crash switch), then hands the event to PostHog (gated by the analytics switch).

---

## Modules

### `core/src/telemetry/mod.rs`
Public surface re-exports + the analytics enable/disable toggles. Two independent flags backed by atomics:
- `client::set_enabled` (PostHog analytics)
- `sentry_pipeline::set_enabled` (Sentry crash reports)

Both are off until the saved consent says yes (opt-in since 0.7.14; the commit that introduced it, 1203fad2, is contained in the `v0.7.14` tag and not in 0.7.13). The answers live in `config.json` as `analytics_consent` and `crash_reports_consent`: `null` = not asked yet, which counts as off. `dimmy_init` calls `telemetry::apply_consent(analytics, crash_reports)` right after it loads the config and before it emits `app.started`; only an explicit `true` turns a pipeline on. `dimmy_telemetry_set_enabled` / `dimmy_telemetry_set_crash_enabled` (called by the Windows and macOS hosts from the first-launch dialog and from Settings → Privacy) set the flag AND write the answer to `config.json` at once.

`client::is_enabled()` is also false in an automated process: `DIMMY_TELEMETRY_OPT_OUT`, `CI`, `GITHUB_ACTIONS` or `TF_BUILD` set to anything but empty / `0` / `false` (read once per process). `test-install.yml` sets `DIMMY_TELEMETRY_OPT_OUT=1`. This does not touch the Sentry switch.

### `core/src/telemetry/events.rs`
The typed event taxonomy (single source of truth). Every event is a variant of the `Event` enum with explicit property fields. There is **no** free-string event API — adding a new event requires adding a variant.

Core variants, with their properties. This table is not exhaustive: the next table lists the other variants, and the Coverage map further down names every variant and its status (the `telemetry_coverage` test enforces that).

| Event | Properties |
|---|---|
| `app.started` | version, os, arch, cold_start_ms |
| `app.session_ended` | duration_secs, transcribe_count |
| `app.update_check` | current_version, available_version *(reserved — never emitted)* |
| `app.update_applied` | from_version, to_version *(reserved — never emitted)* |
| `onboarding.started` | – *(emitted by the Windows and macOS onboarding through `dimmy_telemetry_track_typed`)* |
| `onboarding.step_completed` | step (`welcome` \| `permissions` \| `provider` \| `shortcut` \| `model_download` \| `try_it` \| `stay_updated`) |
| `onboarding.completed` | path (`cloud` \| `local` \| `unknown`), duration_secs |
| `onboarding.abandoned` | last_step (same set as `step`) |
| `config.stt_mode_changed` | mode |
| `config.cloud_provider_changed` | provider *(fires for a change of the STT provider AND of the LLM provider; both use this one event)* |
| `config.llm_enabled_changed` | enabled |
| `config.llm_style_changed` | style |
| `config.shortcut_changed` | – *(reserved — never emitted)* |
| `config.preprocessing_changed` | enabled |
| `config.input_gain_changed` | gain |
| `transcription.completed` | mode, provider, local_backend, entry_point, audio_secs, processing_ms, word_count, language, success, had_filler_removal, had_llm, engine *(the only emit site is the dictation stop in `ffi.rs`: `entry_point` is always `"hotkey"` and `success` is always `true`; failures go to `transcription.failed`)* |
| `transcription.failed` | mode, provider, error_category |

**`provider` on local transcriptions** is `local_whisper` / `local_parakeet` / `local_qwen` / `local_whistle` / `local_nemotron` (`sanitize::local_provider`), named after the engine that actually RAN (post-fallback; an unknown backend maps to `local_whisper`). `local_backend` carries the bare name from the same mapping (`sanitize::local_backend_tag`: `whisper` \| `parakeet` \| `qwen` \| `whistle` \| `nemotron`, empty for cloud). Until 2026-09-14 `provider` was the constant `local_whisper` for every local engine, so any chart grouped by `provider` counted Parakeet and Qwen as whisper. Historical `transcription.completed` events can be re-attributed through `local_backend` (present since 2026-05-12); historical `transcription.failed` events cannot, they never carried it.
| `transcription.cancelled` | audio_secs |
| `llm.applied` | mode, provider, style, tone, processing_ms, success *(one emit site, the cloud / subscription dispatch in `ffi.rs`: `mode` is always `"cloud"`; the local-LLM path emits nothing)* |
| `llm.failed` | mode, provider, error_category *(same site, `mode` is always `"cloud"`)* |
| `perf.startup_ms` | value, version *(never emitted; only a unit test in `client.rs` builds it. `app.started.cold_start_ms` carries the figure)* |
| `perf.gpu_status` | backend, fell_back_to_cpu, known_bad |
| `perf.transcribe_overhead_pct` | value, mode, provider *(reserved — never emitted)* |
| `error.cloud_stt` | provider, status_code, error_category *(reserved — never emitted; covered by transcription.failed)* |
| `error.cloud_llm` | provider, status_code, error_category *(reserved — never emitted)* |
| `error.local_stt` | model, error_category *(reserved — never emitted)* |
| `error.local_llm` | model, error_category *(reserved — never emitted)* |
| `error.gpu_crash` | backend, context *(emitted once at launch when a known-bad GPU marker exists. `context` is the free-form string the GPU sentinel wrote, e.g. `whisper_load: <model path>`. It is NOT path-scrubbed on the PostHog route: `sanitize::scrub_path` is called nowhere on it. Only the Sentry route scrubs account names)* |
| `error.audio_health` | code *(reserved — never emitted)* |
| `feature.hotkey_triggered` | – |
| `feature.api_key_set` | scope, provider |
| `feature.meeting_shortcut` | source (`hotkey` \| `menu` \| `jumplist`) |

Other variants (all emitted; values are categorical, bucketed or boolean, never content):

| Event | Properties |
|---|---|
| `config.autostart_changed` | enabled |
| `config.recap_model_changed` | recap_model_bucket |
| `dictation.capture_ratio` | ratio_bucket (`lt_50` \| `50_85` \| `85_95` \| `ge_95`), source (`mic` \| `mix`) |
| `model.download_completed` | kind, success |
| `consent.logged` | kind |
| `license.activated` / `license.refreshed` | tier |
| `license.activation_failed` / `license.refresh_failed` | error_category |
| `license.scope_denied` | scope (`managed_stt` \| `managed_llm` \| `auto_update` \| `history_sync` \| `premium_styles`) |
| `license.device_deactivated` | is_self |
| `license.interest_registered` | tier (`monthly` \| `annual` \| `lifetime`) |
| `meeting.started` / `meeting.paused` / `meeting.resumed` / `meeting.stop_timeout` | – |
| `meeting.stopped` | duration_bucket, words_bucket, had_recap |
| `meeting.capture_ratio` | ratio_bucket |
| `meeting.transcription_behind` | engine, elapsed_bucket |
| `meeting.transcription_unavailable` | engine, reason |
| `audio.loopback_rate_mismatch` | claimed, measured |
| `meeting.recap_completed` | provider, recap_model_bucket, processing_ms_bucket, success, error_category |
| `meeting.imported_from_file` | – |
| `file_load.started` | source (`drop` \| `picker`), audio_secs_bucket |
| `file_load.completed` | audio_secs_bucket, processing_ms_bucket, words_bucket, success |
| `user_dict.word_added` | source (`hotkey` \| `services_menu` \| `settings_ui`) |
| `user_dict.word_removed` | – |
| `user_dict.size_snapshot` | size_bucket |
| `notion.connected` / `notion.disconnected` | – |
| `notion.recap_sent` | auto, ok |
| `app_rules.evaluated` | matched, rules_total_bucket |
| `app_rules.added` / `app_rules.removed` / `app_rules.reordered` | – |
| `pill.visibility_toggled` | visible, source |
| `pill.style_scrolled` / `pill.language_scrolled` / `pill.context_menu_opened` | – |
| `update.channel_changed` | from, to |
| `update.apply_deferred` | – |
| `permission.granted` / `permission.denied` | service (`microphone` \| `accessibility` \| `input_monitoring`) |
| `claude_code.*`, `codex.*`, `gemini_cli.*`, `kimi_cli.*` | `status_probed{status}`, `login_spawned`, `login_completed{outcome}`, `invocation{kind, processing_ms_bucket, success, error_category}` |

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
    "build_id": "v0.6.73-rc6",   // tag | "<branch>.<run>" | "local"
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
- `before_send` equivalent: `looks_like_secret` runs on `event.properties()` (the event's own properties, NOT the envelope, which carries our own `phc_` key) as the last step before POST. A match drops the event and `DROPPED_SECRET_GUARD` increments. `looks_like_secret` is not a regex: it matches the substrings `api_key=`, `api-key:`, `authorization:`, `bearer `, and any token that starts with `sk-`, `phc_`, `phx_`, `sntrys_` or `gsk_` followed by at least 6 more characters. It knows nothing of AWS keys, JWTs or Sentry DSNs. Catches code regressions that accidentally serialise a secret into a property.
- Nothing on this route looks for paths or user names in a property. A free-text property (`error.gpu_crash.context`) is only as clean as its call site makes it.
- `$ip: null` in every event — opts out of PostHog's server-side IP capture / geo-resolution.
- Build-time secret sanitisation (`build.rs::sanitize_secret`): strips leading UTF-8 BOM + ASCII whitespace before embedding `POSTHOG_API_KEY` / `SENTRY_DSN` as `&'static str`.
- Runtime key-diag: on first `track()`, log `[telemetry] key-diag: prefix=phc_xxxx… len=N starts_with_phc=true` so a wrong key is debuggable from `dimmy.log` alone (PostHog itself returns 200 OK for invalid keys — verified 2026-04-27).

#### Runtime
A dedicated single-worker tokio runtime (lazy-init on first send). Most FFI entry points are called from the C# main thread without an active runtime, so we can't rely on `Handle::try_current()`. The dedicated runtime survives for the rest of the process.

### `core/src/telemetry/sentry_pipeline.rs` — Sentry
Wrapper around the `sentry` crate (v0.47, default-features = false, features `backtrace`, `contexts`, `panic`, `reqwest`, `native-tls` (explicit, to avoid the rustls/CryptoProvider static-init crash that bricked V8/V11/V12 on WindowsAppSDK + Velopack) and `logs`; `ClientOptions.enable_logs` is `false`, so no log line is shipped). The release string is `dimmy@<version>` plus the build identity (`sentry_release`); the environment is `ci` when `CI` is set, `development` in a debug build, else `production`; the tags are `os`, `arch`, `build_id`.

#### What gets sent to Sentry

Only for users who said yes to crash reports (opt-in since 0.7.14). Nothing
below runs before `apply_consent` turns the pipeline on.

- **Handled failures — `telemetry::report(Subsystem, kind, engine, detail)`**
  (`core/src/telemetry/report.rs`). The one way in for an error that is caught
  and logged. `(subsystem, kind, engine)` is the Sentry **fingerprint** and the
  issue title (`meeting/finalize_failed`, `local_engine/create_state_failed/whisper_gpu`),
  so one failure is exactly one issue. Tags: `subsystem`, `error_category`
  (= kind), `provider` (= engine). `detail` goes in the `detail` extra through
  the prose + secret + account-name filters. Level `warning` for
  environmental kinds (`network`, `timeout`, `auth`, `rate_limit`,
  `too_large`, `not_logged_in`, `not_installed`, `offline`, `cancelled`),
  `error` otherwise. At most 3 events per fingerprint per process.
  - Call sites: dictation STT, dictation LLM, recap, command mode, meeting
    (start, chunk STT on the transcription thread, finalize, STT-thread spawn,
    join timeout, capture buffers taken, re-transcribe/diarize/write), audio
    (no device, config, sample format, stream build, stream error callback,
    loopback), AEC build, file load / decode, history WAV save, keystore save,
    config save, autostart, Notion / Confluence / calendar / Telegram /
    Claude Desktop install, Windows hotkey hook, whisper `create_state`,
    Nemotron F32 download.
  - **Never from `meeting.rs::worker_loop`** (THE AUDIO RULE). A capture-loop
    failure is reported after the sinks are finalized; the source guard
    `the_capture_loop_never_calls_the_host_or_telemetry` forbids
    `telemetry::report(` inside the loop.
  - A subscription CLI exit code arrives as `LlmError::Api { status: <100 }`;
    it is reported as kind `cli_exit`, not as "HTTP 1".
- **Failure events mirrored from `track()`** (`report::failure_of`): a PostHog
  event that already says something failed also goes to Sentry —
  `model.download_completed{success:false}`, `license.activation_failed`,
  `license.refresh_failed`, `meeting.stop_timeout`,
  `meeting.transcription_behind`, `meeting.transcription_unavailable`, low capture ratios, `loopback_rate_mismatch`,
  failed file load / Notion send, `error.gpu_crash`, failed Claude Code /
  Codex / Gemini CLI / Kimi CLI invocations. `transcription.failed` / `llm.failed` /
  `meeting.recap_completed` are not mirrored: their call sites report with the
  message (the recap mirror was a second issue, RUST-1H).
- **Host exceptions — `dimmy_telemetry_capture_host_exception(host, source, type, frames, fatal)`**.
  Windows: `Helpers/CrashReporter.cs` (AppDomain + XAML unhandled = fatal,
  `TaskScheduler.UnobservedTaskException`, update check / apply, meeting recap,
  command pipe) and a UI-thread hang watchdog (5 s, once per hang, Release
  only, suspend-aware). macOS: `AppDelegate.swift` NSException handler (fatal)
  and `HangWatchdog`; fatal signals write `last_abort.txt` for the next launch.
  Sent: exception type, source slug, frames (`function`, file NAME, line).
  Never the exception message. Throttled like `report`.
- **Manual `capture_feedback(kind, message, email)`** — called from `dimmy_telemetry_capture_feedback` (UI Settings → Send feedback). It builds a User Feedback v2 envelope by hand and POSTs it straight to the DSN's envelope endpoint, so it bypasses `before_send`; the message goes through `scrub_message` (secret check, account-name scrub, 4 KB cap). It returns -2 when the crash switch is off and -3 when no DSN is compiled in, so feedback follows the crash-report consent.
- **Auto: panic hook** via the `panic` feature of the `sentry` crate. Captures every Rust panic and ships it as a Fatal event.
- **Native aborts** (`abort_report.rs`): a SIGABRT handler writes `last_abort.txt` in the config folder with the free memory in MB, and the next launch sends it as `native/abort`. Nothing else is in it. The report waits for `apply_consent`: `telemetry::init` runs before the config is read, and from 0.7.14 to this fix it found Sentry still off and dropped every abort.
- **Native crash dumps** (`crash_dump.rs`, Windows): a crash in C/C++ code (whisper.cpp, ggml, onnxruntime, a driver) raises no Rust panic. `install` arms `SetUnhandledExceptionFilter` and keeps the previous filter (the CLR's, the CRT's), which it calls after writing, so the process ends exactly as before. The dump is written by a thread started and parked at install (a crashed thread may have no stack; creating a thread during a crash can deadlock on the loader lock); the crashed thread waits for it at most 10 s. The path is UTF-16 built at install, nothing is allocated on the crash path except inside `MiniDumpWriteDump`. Flags: `MiniDumpFilterMemory | MiniDumpFilterModulePaths | MiniDumpWithUnloadedModules | MiniDumpWithThreadInfo` — thread stacks reduced to pointer values, module names without folders, no heap, no handles. Skipped: CLR exceptions (`0xE0434352`; the host reports those). The SIGABRT handler of `abort_report.rs` writes a dump too, with a captured context and code `STATUS_FATAL_APP_EXIT`. Not caught: `__fastfail` (Rust's abort on a panic across `extern "C"` — that one already flushes the panic event — heap corruption, `Environment.FailFast`); those go to WER only. File name `<ms>_<pid>_<environment>_<release>.dmp` in `<config>/crashes/`, so the event carries the release that crashed, not the one that sends. Measured: ~60 KB for a test process, ~1.2 MB for the full Windows app (37 threads). Zero cost until a crash: one parked thread (256 KB reserved stack). Debug builds only: `DIMMY_TEST_NATIVE_CRASH=1` faults 3 s after `dimmy_init`.
- **Sending native crashes** (`crash_prompt.rs`, `sentry_pipeline::send_native_crash`): at `apply_consent` the abort marker and the dumps (`crash_dump::collect`: at most 3, newest first; older than 7 days, unanswered or oversized are deleted) go out silently when crash consent is yes, as an envelope with an `event` item and an `event.minidump` attachment; Sentry walks the stacks and symbolicates with the PDBs CI uploads. On Windows an abort leaves marker + dump, sent as ONE event (the marker's free-memory line in `extra.detail`). With consent off or unanswered they wait: `dimmy_crash_report_prompt` returns 1 once per crash (dumps renamed `.asked`, so a launch that ends before the answer deletes them next time), the host shows "Dimmy closed unexpectedly last time…" (words from `dimmy_crash_report_text`, six languages), and `dimmy_crash_report_resolve(1|0)` sends that one report or deletes it. "Always send" is the existing `dimmy_telemetry_set_crash_enabled(1)`. Hosts: Win `Views/CrashReportWindow.cs` (after `ShowStatsConsentIfNeeded`, never with it), Mac `CrashReportPrompt` in `MacPrivacyPage.swift`. A dump is deleted after any answer but 429/5xx; a network failure keeps it for the next launch.
- **Auto: contexts + backtraces** via the `contexts` and `backtrace` features. OS / device / runtime metadata, `attach_stacktrace = true`.
- **Auto: breadcrumbs** ring buffer, capped at 20 (`max_breadcrumbs: 20`). Nothing in `core/` calls `add_breadcrumb`, so only what the `sentry` crate's own integrations add can appear; `crate::log` calls do NOT feed Sentry breadcrumbs (Phase 3b candidate).

#### Defenses
- `before_send` (`scrub_event`) returns `None` (drops the event) when the crash switch is off. Sentry's panic integration reaches Sentry without going through `capture_*`, so this is the only place the consent switch can stop a panic. Otherwise it scrubs:
  - `event.server_name` → null
  - `extra`: the environment-variable names (`PATH`, `HOME`, `USERPROFILE`, `USERNAME`, `USER`, `LOGNAME`, `APPDATA`, `LOCALAPPDATA`, `TEMP`, `TMP`) are removed first, then everything outside the allow-list `provider`, `mode`, `error_category`, `detail`
  - message, exception values and `detail`: a string matching `looks_like_secret` becomes `<redacted: looked like a secret>`; otherwise `redact_prose` keeps a string of at most 24 characters, an identifier-shaped string (`subsystem/kind/engine`), or one that starts with a whitelisted prefix (`HTTP `, `request failed:`, `no API key`, `local model:`, `config`, `dimmy_`, `panic`, …; capped at 200 characters), and replaces everything else with `<redacted: prose content>`. A panic payload is the exception: it is kept (`scrub_panic_message`, secret check first, capped at 500 characters). House rule: an assertion message may name sizes, rates, counts and enum names, never user text.
  - `sanitize::scrub_user_paths` replaces the account name in any `/Users/<x>`, `\Users\<x>`, `/home/<x>` path inside those strings with `<USER>`
  - breadcrumbs: message through the same filters, `data` cleared (`before_breadcrumb`, and again in `scrub_event`)
- `send_default_pii: false` — Sentry skips its default IP / username collection.
- `telemetry::report` sends at most 3 events per fingerprint per process (`report::MAX_PER_FINGERPRINT`).
- DSN pre-flight: `parse::<sentry::types::Dsn>()` before `sentry::init`. Bad DSN → log + skip, NEVER panic. (sentry-core 0.47 panics on `InvalidUrl`; the pre-flight prevents that panic from crossing the `extern "C"` `dimmy_init` boundary and aborting the cdylib.)
- `catch_unwind` around `sentry::init` itself — belt-and-braces if a future sentry version chooses a different panic surface.
- User record carries the anonymous ID; the tags are `os`, `arch`, `build_id`. No email, no username.

### `core/src/telemetry/identity.rs`
Anonymous ID generator + persistence. UUIDv4, written to `analytics_id` in the config directory (`dirs::config_dir()/<namespace>`: `%APPDATA%\dimmy\` on Windows, `~/Library/Application Support/dimmy/` on macOS, `~/.config/dimmy/` on Linux; the namespace is `dimmy` unless the build sets `DIMMY_CONFIG_NAMESPACE`). Cached in `OnceLock<String>`. Resettable from FFI (`dimmy_telemetry_reset_anonymous_id`); the reset deletes the file, and the running process keeps its cached ID until the next launch.

`new_uuid_v4()` is also used by `client.rs::session_id` for the per-process session UUID (not persisted).

### `core/src/telemetry/sanitize.rs`
Pure helpers:
- `provider_from_url(url) -> &'static str` — categorical provider tag (`groq`, `openai`, `anthropic`, `deepgram`, `gemini`, `unset`, `custom`); never the URL.
- `local_provider(backend)` / `local_backend_tag(backend)` — the engine tag of a local transcription.
- `error_category(message, status) -> &'static str` — stable error bucket: from the HTTP status when there is one, else from keywords in the message; never returns the message.
- `bucket_audio_secs`, `bucket_capture_ratio`, `bucket_processing_ms`, `bucket_word_count`, `bucket_dict_size`, `bucket_app_rules`, `bucket_recap_model`, `round_gain` — bucketing, so a length or a count cannot fingerprint a user.
- `scrub_user_paths(text) -> String` — replaces the account name inside any `/Users/<x>`, `\Users\<x>`, `/home/<x>` path with `<USER>`. Used by the Sentry route (`scrub_message`, `redact_prose`, host frames, panic messages). The PostHog route does not call it.
- `scrub_path(path) -> String` — replaces the home-directory prefix with `<HOME>/`, else keeps the last two path segments. Defined and unit-tested, but called by neither pipeline.
- `looks_like_secret(payload) -> bool` — token-prefix and substring detector, see Defenses above. Used by both pipelines.

Unit tests live in the same file.

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
3. **Automated process** (PostHog only): `DIMMY_TELEMETRY_OPT_OUT`, `CI`, `GITHUB_ACTIONS` or `TF_BUILD` set → `client::is_enabled()` is false whatever the saved consent says.

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
| **3a: $add counters + latest_*_provider + feature.*_used + gpu_status + $ip:null** | **✅ V17** | Shipped. |
| 3b: Sentry hardening | ◐ Partly done | (a) `crate::log` → Sentry breadcrumb hook: not done. (b) `sentry::start_session()` for release health "crash-free users %": not done (no call in `core/`). (c) Debug-file upload for symbolicated stack traces: done. `scripts/ci/sentry.sh upload` (`@sentry/cli debug-files upload`) is called by `release.yml`, `staging-tester.yml` and `staging-auto-update.yml`. |
| 3c: Host-side onboarding + UI feature events | ◐ Partly done | `onboarding.*` (4 events) are emitted by the Windows and macOS hosts through `dimmy_telemetry_track_typed`. `feature.history_opened`, `feature.settings_opened`, `feature.feedback_sent` have no `Event` variant: not done. |

---

## Adding a new event

1. Add a variant to the `Event` enum in `core/src/telemetry/events.rs`.
2. Add it to `Event::name()` with a stable dotted lowercase name (`area.snake_case_action`; the `event_name_is_stable_and_lowercase` test checks the characters).
3. If it should carry a `$add` counter, add the match arm in `core/src/telemetry/client.rs::build_payload`.
4. Wire the emit at the call site (`crate::telemetry::track(Event::… { … })`). For an event the host UI raises, add an allow-listed arm in `dimmy_telemetry_track_typed` (`ffi.rs`) and call it from the host.
5. If the event reports a failure that Sentry should also see, add an arm in `report::failure_of`.
6. Add a unit test in the `tests` module of `core/src/telemetry/client.rs` that builds the payload and asserts the property shape.
7. Add the variant to the Coverage map below (the `telemetry_coverage` test fails otherwise). Update `PRIVACY.md` (its event list is the public promise) and this doc if the new event collects a new category of information.

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
Feature        FeatureHotkeyTriggered  FeatureApiKeySet  FeatureMeetingShortcut
Capture health DictationCaptureRatio  MeetingCaptureRatio  MeetingTranscriptionBehind  MeetingTranscriptionUnavailable
               LoopbackRateMismatch
Licensing      LicenseActivated  LicenseActivationFailed  LicenseRefreshed  LicenseRefreshFailed
               LicenseScopeDenied  LicenseDeviceDeactivated  LicenseInterestRegistered
Meeting        MeetingStarted  MeetingStopped  MeetingPaused  MeetingResumed
               MeetingRecapCompleted  MeetingImportedFromFile  MeetingStopTimeout
File load      FileLoadStarted  FileLoadCompleted
Model download ModelDownloadCompleted
Consent        ConsentLogged
Dictionary     UserDictWordAdded  UserDictWordRemoved  UserDictSizeSnapshot
Notion         NotionConnected  NotionDisconnected  NotionRecapSent
App rules      AppRulesEvaluated  AppRuleAdded  AppRuleRemoved  AppRuleReordered
Pill           PillVisibilityToggled  PillStyleScrolled  PillLanguageScrolled  PillContextMenuOpened
Update         UpdateChannelChanged  UpdateApplyDeferred
Permissions    PermissionGranted  PermissionDenied
Claude Code    ClaudeCodeStatusProbed  ClaudeCodeLoginSpawned  ClaudeCodeLoginCompleted  ClaudeCodeInvocation
Codex          CodexStatusProbed  CodexLoginSpawned  CodexLoginCompleted  CodexInvocation
Gemini CLI     GeminiCliStatusProbed  GeminiCliLoginSpawned  GeminiCliLoginCompleted  GeminiCliInvocation
Kimi CLI       KimiCliStatusProbed  KimiCliLoginSpawned  KimiCliLoginCompleted  KimiCliInvocation
```

The three `*LoginCompleted` events are sent by the host through
`dimmy_telemetry_track_typed`. Until 2026-09-14 only the Claude Code arm
existed there, so `codex.login_completed` and `gemini_cli.login_completed`
were dropped on arrival.

`PerfStartupMs` is defined but never emitted: the only `Event::PerfStartupMs`
outside `events.rs` is a unit test in `client.rs`, which is why the
`telemetry_coverage` test does not flag it and it is not in `RESERVED`. It
duplicates `AppStarted.cold_start_ms`; delete it or add it to `RESERVED`.

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

| Feature | Plan | Priority | Status |
|---|---|---|---|
| Streaming + local-stream dictation | `TranscriptionCompleted` gained an `engine` prop (`batch`/`deepgram_stream`/`local_stream`/`chunked_caption`) | must | DONE 2026-06-07 |
| Local model download | `ModelDownloadCompleted{kind, success}` on whisper/llm/parakeet fetch | must | DONE 2026-06-07 |
| Recording consent gate | `ConsentLogged{kind}` from `dimmy_consent_log_event` (shown/accepted/cancelled/...) | must | DONE 2026-06-07 |
| `entry_point` real value | thread pill/hotkey/meeting source (still hardcoded "hotkey") | nice | TODO |
| Model download STARTED (abandon rate) | add `ModelDownloadStarted{kind}` | nice | TODO |
| Command mode (generate/transform) | new `CommandInvoked{kind, success}` | nice | TODO |
| Call detection + nudge | new `CallDetected` / `CallNudge{outcome}` | nice | TODO |
| Checkout initiated | new `LicenseCheckoutStarted{tier}` (purchase funnel) | nice | TODO |
| "I'd buy this" while sales are closed | `LicenseInterestRegistered{tier}` from `dimmy_license_register_interest`; the email goes only to the licensing server | must | DONE 2026-11-06 |

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
  (b) missing from the coverage map above. Both checks are text searches: (a)
  counts any `Event::<Variant>` in `core/src` outside `events.rs`, unit tests
  included; (b) looks for the CamelCase variant name anywhere in this file. So a PR that adds a variant MUST
  wire an emit (or reserve it) AND document it. No LLM, runs in `cargo test`.
- **Layer 3 — `/telemetry-audit` skill (judgment, at release).** Diffs commits
  since the last tag, finds user-facing features (new `dimmy_*` FFI entries,
  new modules, new config-driven behavior) that lack an event, and PROPOSES
  events (name + privacy-safe props) for human approval. This catches the
  "should this have a metric?" question that Layer 2 cannot. Run it before each
  `staging.N` / `rcN`. See `.claude/commands/telemetry-audit.md`.

The division: Layer 2 is mechanical hygiene (no dead/undocumented variants),
Layer 3 is the judgment call (does a new surface deserve a metric). Together
they cover dev-time (every PR) and release-time without noise.

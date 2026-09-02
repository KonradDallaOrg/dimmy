# Dimmy Privacy Policy

_Last updated: 2026-05-13_

Dimmy is a voice-transcription overlay that runs locally on your computer. We collect a deliberately small amount of anonymous telemetry to understand how the app is used and to fix crashes — never enough to identify you or recover what you said.

You can disable telemetry and crash reporting at any time from **Settings → Privacy** (each toggle is independent).

---

## What we collect

### Per-install identifier
- A random UUIDv4 generated on first launch, stored locally in `~/.config/dimmy/analytics_id` (or `%APPDATA%\dimmy\analytics_id` on Windows). It exists only to de-duplicate "1 user did X 5 times" from "5 users each did X once".
- You can reset it from **Settings → Privacy → Reset anonymous ID**.
- It is never linked to anything that could identify you.

### Per-process session identifier
- A fresh UUIDv4 generated on each app launch (not persisted). Lets us answer "how many transcriptions per session?" without tracking open/close events.

### Platform context (every event)
- App version (e.g. `0.6.20`)
- Operating system family (`windows` / `macos` / `linux`)
- CPU architecture (`x86_64` / `aarch64`)

### Lifecycle events
- `app.started` — when the app launches: cold-start time in milliseconds.
- `app.session_ended` — when the app closes: total session duration (seconds), number of transcriptions in that session.

### Transcription events
- `transcription.completed` — when a transcription succeeds:
  - which path (`local` whisper.cpp on your machine vs. `cloud` provider),
  - which provider (`groq` / `openai` / `anthropic` / `gemini` / `deepgram` / `openrouter` / `local` — **categorical tag only**, never the URL or API key),
  - audio duration in seconds (number),
  - processing time in milliseconds (number),
  - **word count** (number — never the words themselves),
  - language code (e.g. `en`, `it` — ISO code),
  - whether filler-removal ran (boolean),
  - whether LLM post-processing ran (boolean),
  - which transcription engine produced it (`batch` one-shot, `deepgram_stream` realtime cloud, `local_stream` realtime local, or `chunked_caption` — **categorical tag only**).
- `transcription.failed` — provider, error category (e.g. `401`, `timeout`).
- `transcription.cancelled` — audio duration up to cancel.

### LLM post-processing events
- `llm.applied` — provider, style name, tone name, processing time. **Never the prompt, never the output.**
- `llm.failed` — provider, error category.

### Configuration changes
When you change any of these in Settings, we log that the change happened (not the value before/after for free-text fields):
- STT mode toggle (`local` ↔ `cloud`)
- Cloud provider switch (categorical: `groq` → `openai`, etc.)
- LLM enabled toggle
- LLM style dropdown
- Preprocessing toggle
- Input gain slider value (number)

We do **not** track changes to: prompt text, custom LLM prompt, microphone device name, shortcut combo string.

### Feature usage
- `feature.hotkey_triggered` — when the global hotkey starts a recording (helps us understand whether users prefer hotkey or button).
- `feature.api_key_set` — when you save an API key, we log which scope (`stt` / `llm`) and which provider (categorical). **The key value never leaves your computer.**
- `model.download_completed` — when a local model finishes downloading: which kind (`whisper` / `llm` / `parakeet` — categorical) and whether it succeeded (boolean). Never the model path or filename.
- `consent.logged` — when the meeting recording-consent gate fires: which kind of consent moment (`shown` / `accepted` / `cancelled` / `announced` / `declined` — categorical). Never the participant names, the spoken announcement, or any meeting content.

### Claude Code subscription backend
If you pick the **Claude-Code** provider in Settings (uses your Anthropic Pro / Team / Max plan via the local `claude` CLI instead of an API key):

- `claude_code.status_probed` — when the Settings card refreshes: which of three states you're in (`ready` / `not_logged_in` / `not_installed`). No path, no version.
- `claude_code.login_spawned` — when you click "Sign in via browser". No timestamp delta, no result yet.
- `claude_code.login_completed` — when the polling loop concludes: `success` (you logged in), `timeout` (3-min wait expired), or `spawn_failed` (couldn't start the CLI).
- `claude_code.invocation` — when an LLM call goes through the CLI: which call site (`rewrite` / `recap` / `test`), processing time bucket, success flag, and a categorical error label (`ok` / `not_installed` / `not_logged_in` / `timeout` / `spawn` / `exit_nonzero` / `invalid_utf8`).

**What we never send for Claude-Code:** the prompt text, the model's response, the path where `claude` is installed on your disk, the contents of `~/.claude/credentials.json` (we never read this file at all — the CLI is the only consumer), or any stderr text from the CLI (it could echo back transcript fragments via model error messages).

### Performance + stability
- `perf.startup_ms` — cold-start duration.
- `perf.gpu_status` — at each launch: which GPU backend was compiled (`vulkan` / `cuda` / `metal` / `cpu`), whether the previous launch crashed during GPU init, whether a sticky known-bad marker is set.
- `error.gpu_crash` — only on the launch immediately after a GPU crash: which backend, which call site (e.g. `whisper_load: <path>` — paths are scrubbed).

### Counters
We attach atomic increment operators on a small set of cumulative per-user counters: `total_transcriptions`, `total_transcription_failures`, `total_transcriptions_cancelled`, `total_llm_uses`, `total_llm_failures`, `total_sessions`. These let us segment "active users" from "users who installed but never tried it" without scanning every event.

### Person properties
The following are attached to your anonymous-ID record so dashboards can build cohorts and retention curves:
- `first_seen_at` — timestamp of your first event (set once).
- `first_app_version`, `first_os`, `first_arch` — your install context (set once).
- `latest_app_version`, `latest_seen_at`, `latest_os`, `latest_arch` — refreshed on every event.
- `latest_stt_provider`, `latest_stt_mode` — last STT provider used.
- `latest_llm_provider` — last LLM provider used.

### Crash reports (Sentry)
When the app crashes or hits an error path, we send to Sentry EU:
- The error message (truncated to 4 KB, secret-shaped substrings replaced with `<redacted>`).
- A Rust stack trace (function names — currently mangled in shipping builds; source-mapped in a future release).
- The platform context (OS, arch, app version, build identifier, environment = `production`).
- The anonymous ID (so we can de-duplicate the same crash from the same user).

We do **not** send: server name, hostname, username, environment variables (`PATH`, `HOME`, `USERPROFILE`, etc. are all stripped), IP addresses (Sentry EU drops them at ingest by default), microphone device names, transcripts, prompts.

Some error messages name a file that could not be read or written, so they do contain a path. Your account name is removed from it before the message leaves your machine: `C:\Users\<USER>\AppData\Roaming\dimmy\models\ggml-large-v3.bin`. Until 2026-09-02 this page said we sent no paths at all, and that was wrong: one class of error message carried the full path, account name included. It is fixed, and covered by tests.

### Feedback
The **Settings → Send feedback** form goes to Sentry as a tagged message. The text you type is included verbatim. Email is optional and only included if you explicitly type it — we never auto-fill from anywhere.

Sending feedback requires telemetry to be enabled (it shares the same Sentry channel). If you have telemetry turned off, the form tells you so and offers an **Enable & send** button — one click turns sending on and submits your message. Nothing is transmitted until you click. The app never shows a fake "sent" confirmation: if a build can't send (e.g. a dev build with no Sentry endpoint compiled in), it says so explicitly.

---

## What we never collect

- The audio you record.
- The text of any transcription.
- Any prompt text (system, user, custom).
- Any LLM output.
- Names of contacts, files, or applications you transcribe into.
- Microphone device names or hardware fingerprints.
- File paths beyond a categorical "where this kind of file lives" tag (and even those are scrubbed).
- API keys (they live in `~/.config/dimmy/keys.enc`, AES-256-GCM encrypted, never sent anywhere except to the provider you configured).
- Anthropic OAuth tokens. If you use the Claude-Code subscription provider, the token lives in `~/.claude/credentials.json` and is managed exclusively by Anthropic's official `claude` CLI. Dimmy never reads or transmits that file.
- IP addresses (Sentry EU drops them server-side; PostHog explicitly skipped via `$ip: null` in every event).
- Username / hostname / email (except the explicit feedback-form email if you choose to type one).

---

## Where the data goes

- **Analytics events**: PostHog EU (`https://eu.i.posthog.com`). Hardcoded; never overridable at runtime.
- **Crash reports + manual error captures + feedback**: Sentry EU (`https://o*.ingest.de.sentry.io`). Hardcoded.

Both services are GDPR-aligned and run in EU data centres. We control the projects; only the Dimmy team can read the data.

---

## How to inspect what's being sent

Every telemetry call is logged locally to:
- **Windows**: `%APPDATA%\dimmy\dimmy.log`
- **macOS / Linux**: `~/.config/dimmy/dimmy.log`

Look for lines starting with `[telemetry]`. Example:

```
[2026-04-27 09:47:06] [telemetry] track event=transcription.completed
[2026-04-27 09:47:06] [telemetry] spawn send for event=transcription.completed (payload 275 bytes)
[2026-04-27 09:47:06] [telemetry] send: HTTP 200 OK (sent=4)
```

You can also see exactly which key is embedded in the build by searching for `key-diag` (the prefix is logged once per process). This lets you confirm we are sending to the project we claim and not somewhere else.

---

## How to disable

Open **Settings → Privacy** in the app. Two independent toggles:
- **Send anonymous usage data** (PostHog analytics).
- **Send crash reports** (Sentry).

Disabling either takes effect immediately for events emitted after the toggle. Already-sent events cannot be recalled.

---

## Changes to this policy

Material changes will be announced in the release notes. The current version of this file is the ground truth — `git log PRIVACY.md` shows the full history.

If you have questions or want your data deleted, contact the maintainer at the email in `Cargo.toml` (`konrad.dalla@gmail.com`).

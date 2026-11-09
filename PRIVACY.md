# Dimmy Privacy Policy

_Last updated: 2026-11-08_

This policy covers the Dimmy apps (Windows, macOS, Linux), the licence and
payment service at `license.dimmy.app`, and the forms on `dimmy.app`. It says
what each part does with personal data, why, where it goes and for how long.
The website at <https://dimmy.app/privacy/> shows the same content.

---

## Short version

- Dimmy can run fully offline when you choose local models. If you choose a
  cloud speech or AI provider, your audio or text goes to that provider, with
  your own key, under that provider's terms.
- Dictation audio is not kept unless you turn on audio in history. Meeting
  recordings (`audio.wav`) are always saved on your computer, because that is
  what a meeting recording is.
- Usage statistics and crash reports are **off until you say yes**. Two
  separate switches. They never contain your audio, your text or your
  prompts. They are linked to a random installation ID, which is
  pseudonymous, not anonymous.
- API keys are stored in a local file encrypted with AES-256-CTR and
  HMAC-SHA256, under a key derived from your user name and computer name.
- The licence service stores a hash of your e-mail, your licence, and the
  devices you activate. Payment is handled by Stripe.

---

## Who is responsible

The controller is **Konrad Dalla**, Via La Piera 67, Bologna, Italy, who develops and sells Dimmy.
Contact: **hi@dimmy.app**. Security issues: security@dimmy.app.

There is no data protection officer; the law does not require one at this
size. Write to the address above for any request in this policy.

---

## What happens on your computer

Dimmy keeps its data in one folder:

- **Windows:** `%APPDATA%\dimmy\`
- **macOS:** `~/Library/Application Support/dimmy/`
- **Linux:** `~/.config/dimmy/`

In that folder:

| File / folder | What it holds |
|---|---|
| `config.json` | Your settings. |
| `keys.enc` | API keys and tokens, encrypted (AES-256-CTR + HMAC-SHA256). The key is derived from your user name and computer name, so the file cannot be read on another computer. It does not protect against malware that runs under your own account. |
| `history.db` | Dictation history: the raw and the cleaned text, time, the name of the app you dictated into, style and language. |
| `history_audio/` | Dictation audio, **only if** you turn on audio in history. Off by default. When on, files older than 30 days or beyond 5 GB are deleted (both limits can be changed). |
| `meetings/` | One folder per meeting: `audio.wav` and the separate microphone and system tracks, `transcripts.txt`, `recap.md`, `meta.json`, your notes. You can choose another folder in Settings. |
| `consent.jsonl` | A local log of the recording-consent steps (shown, accepted, announced, …), so you can prove what you did. |
| `models/` | Speech and language models you downloaded. |
| `dimmy.log` | The app log. It contains technical messages, not your transcripts. |
| `analytics_id` | The random installation ID (see telemetry). |
| `license.json` | Your signed licence token, if you activated one. |
| Telegram session | If you connect Telegram: the session, sealed with the machine key. |

Nothing in this folder is sent to us. **To delete it**, quit Dimmy and delete
the folder (and your meetings folder, if you moved it). Uninstalling the app
does not delete it. You can also delete single history items and meetings
inside the app.

---

## What leaves your computer, and when

| Destination | When | What |
|---|---|---|
| The cloud speech / AI providers you pick (Groq, OpenAI, Anthropic, Google Gemini, Deepgram, OpenRouter, Together, Fireworks, Kimi Code, …) | Only if you choose a cloud provider | Audio for transcription, text for cleanup, command mode and recaps, with your own key. |
| Claude Code, Codex, Gemini CLI or Kimi Code CLI on your computer | Only if you choose a subscription backend | The prompt is passed to the CLI, which sends it to its vendor under your account. Dimmy never reads the CLI's credential files. |
| Notion, Confluence, Telegram | Only if you connect them | Recaps or messages you send, or that you set to be sent automatically. |
| Hugging Face (`huggingface.co`), GitHub (`github.com`) | When you download a model or an engine | A normal download request. |
| GitHub (`github.com`, `api.github.com`) | Update checks | A request for the latest release. |
| `license.dimmy.app` (our licence service) | Trial, activation, licence refresh, "I'd buy this" | See "The licence service" below. |
| PostHog EU, Sentry EU | **Only if you said yes** to statistics or crash reports | See "Usage statistics and crash reports" below. |

Each provider you choose acts on your own account and under its own terms.
For which providers can use API data for training, see
<https://dimmy.app/data-and-ai/>.

---

## Usage statistics and crash reports (telemetry)

### Off until you say yes

At first launch Dimmy asks whether it may send usage statistics and crash
reports. The two answers are equal, and until you answer nothing is sent.
You can change each one at any time in **Settings → Privacy**:

- **Send anonymous usage data**: usage events to PostHog EU.
- **Send crash reports**: errors and crashes to Sentry EU.

A change takes effect immediately. Events already sent cannot be recalled;
write to us to have them deleted.

Versions up to 0.7.13 started with both switches on. The consent question
arrived with 0.7.14.

### The installation ID

A random UUIDv4 created on first launch and stored in `analytics_id`. It is
not derived from your name, e-mail, account or hardware, but it lets us
recognise the same installation across events, so it is a **pseudonymous**
identifier. Reset it in **Settings → Privacy → Reset anonymous ID**.
A second random ID is created at each launch and never stored, to group the
events of one session.

### On every event

App version, operating system family (`windows` / `macos` / `linux`), CPU
architecture (`x86_64` / `aarch64`). The IP address is not sent: every
PostHog event carries `$ip: null`, and Sentry EU drops IP addresses at intake.

### Never in telemetry

Your audio, the text of a transcription, any prompt, any LLM output, file
names, contacts, the names of apps you dictate into, microphone device names,
API keys or tokens, your e-mail, user name or computer name, calendar
attendees, licence or device identifiers.

### The full list of events

Values are categories (a provider name, a bucket such as `30_120` seconds),
counts, durations or true/false. Free text is never sent.

**App and onboarding**
- `app.started`: the app launched; cold-start time.
- `app.session_ended`: the app closed; session length, number of transcriptions.
- `app.update_check`: an update check ran; current and available version.
- `app.update_applied`: an update was installed; from and to version.
- `onboarding.started`, `onboarding.step_completed`, `onboarding.completed`, `onboarding.abandoned`: progress through the first-run setup; step name, path, duration.

**Settings changes**
- `config.stt_mode_changed`: local or cloud speech.
- `config.cloud_provider_changed`: which cloud provider (category).
- `config.llm_enabled_changed`: AI cleanup on or off.
- `config.llm_style_changed`: which style.
- `config.shortcut_changed`: a shortcut changed (not which keys).
- `config.preprocessing_changed`: audio preprocessing on or off.
- `config.input_gain_changed`: the input gain value.
- `config.autostart_changed`: launch at login on or off.
- `config.recap_model_changed`: the recap model, as a bucket.

**Dictation and transcription**
- `transcription.completed`: local or cloud, provider and engine (category), source (hotkey, file, meeting, pill), audio length, processing time, word count (the number only), language code, whether filler removal and AI cleanup ran, which pipeline (batch or streaming).
- `transcription.failed`: provider and error category (for example `401`, `timeout`).
- `transcription.cancelled`: audio length up to the cancel.
- `dictation.capture_ratio`: how much of the recording time was actually captured (bucket) and the capture source.
- `llm.applied`: AI cleanup ran; provider, style, tone, processing time, success.
- `llm.failed`: provider and error category.
- `feature.hotkey_triggered`: the global hotkey started a recording.
- `feature.api_key_set`: an API key was saved; scope (`stt` / `llm`) and provider. Never the key.

**Meetings and files**
- `meeting.started`, `meeting.stopped` (length bucket, word-count bucket, whether a recap ran), `meeting.paused`, `meeting.resumed`.
- `meeting.stop_timeout`: stopping a meeting took too long.
- `meeting.capture_ratio`: recorded audio against real time (bucket).
- `meeting.transcription_behind`: transcription could not keep up; engine, and how far into the meeting (bucket).
- `audio.loopback_rate_mismatch`: system audio arrived at a different sample rate than announced (two rate labels).
- `meeting.recap_completed`: a recap finished; provider, model family, length bucket, success, error category.
- `meeting.imported_from_file`: a file was turned into a meeting.
- `feature.meeting_shortcut`: a meeting was started from the shortcut or a menu (which one).
- `consent.logged`: a recording-consent step (`shown` / `accepted` / `cancelled` / `announced` / `declined` / …).
- `file_load.started`, `file_load.completed`: a file was transcribed; drop or picker, length, time and word buckets, success.

**Errors and performance**
- `error.cloud_stt`, `error.cloud_llm`: a cloud call failed; provider, HTTP status, error category.
- `error.local_stt`, `error.local_llm`: a local model failed; model name, error category.
- `error.gpu_crash`: after a GPU crash; backend and call site (paths scrubbed).
- `error.audio_health`: an audio-device error code.
- `perf.startup_ms`: start-up time.
- `perf.gpu_status`: which GPU backend, whether it fell back to the CPU.
- `perf.transcribe_overhead_pct`: processing overhead as a percentage.

**Licence**
- `license.activated` (plan), `license.activation_failed` (error category), `license.refreshed`, `license.refresh_failed`, `license.scope_denied` (which feature was locked), `license.device_deactivated` (this device or another), `license.interest_registered` (which plan; never the e-mail).

**Other features**
- `user_dict.word_added` (where from), `user_dict.word_removed`, `user_dict.size_snapshot` (size bucket): custom dictionary use. Never the words.
- `notion.connected`, `notion.disconnected`, `notion.recap_sent` (automatic or manual, success).
- `app_rules.evaluated` (whether a rule matched, rule-count bucket), `app_rules.added`, `app_rules.removed`, `app_rules.reordered`. Never the rule or the app name.
- `pill.visibility_toggled`, `pill.style_scrolled`, `pill.language_scrolled`, `pill.context_menu_opened`.
- `update.channel_changed` (stable / prerelease), `update.apply_deferred`.
- `permission.granted`, `permission.denied`: macOS permissions (microphone, accessibility, input monitoring).
- `claude_code.*`, `codex.*`, `gemini_cli.*`, `kimi_cli.*` (`status_probed`, `login_spawned`, `login_completed`, `invocation`): state of the subscription backends; ready / not logged in / not installed, login outcome, call site, time bucket, error category. Never the prompt, the answer, the CLI path or its output.

### Counters and profile properties

PostHog also keeps running totals for the installation ID
(`total_transcriptions`, `total_transcription_failures`,
`total_transcriptions_cancelled`, `total_llm_uses`, `total_llm_failures`,
`total_sessions`) and properties: first and latest seen time, app version,
OS and architecture, latest speech mode and provider, latest LLM provider.

### Crash reports (Sentry)

If you said yes to crash reports, an error or a crash sends: the error message
(cut to 4 KB, with secret-shaped strings replaced by `<redacted>`), a stack
trace, OS, architecture, app version and build, and the installation ID.
Environment variables, server name, host name and user name are removed. Some
error messages name a file; your account name is removed from the path before
sending (`C:\Users\<USER>\...`).

Errors that Dimmy handles without crashing are also sent, so that they can be
fixed: for example a download that fails, a meeting file that cannot be
finished, a microphone stream that stops, or a recap that fails. Each one
sends the part of the app (`meeting`, `audio`, `download`, …), a fixed error
type, the provider or engine name, and the error message with the same
filters. Dimmy sends each type of error at most three times per session. It
never sends the microphone name or the text of a transcript, a prompt or a
recap.

If the Windows or macOS app itself crashes, stops answering for 5 seconds, or
has an error in an important task (update, recap), it sends the error type,
where it happened and the stack trace (function names and source file names).
It does not send the error message, because that text can contain file
names or your text.

If Dimmy stops inside native code (a speech or language engine, a GPU
driver), there is no error message to send. On Windows Dimmy then writes a
small crash file (a "minidump", usually under 2 MB) in its settings folder,
in `crashes`. It holds the list of loaded program files (names only, no
folders) and the call stack of each thread: the return addresses and the
other numbers that look like memory addresses, nothing else. It holds no
memory of the program's data, no open-file list and no environment
variables. A stack can still hold a few small values that look like
addresses by chance; they are not text or audio. At the next launch:

- If you said yes to crash reports, Dimmy sends the file to Sentry EU with
  the app version, OS and the installation ID, then deletes it.
- If you did not, Dimmy asks once: "Dimmy closed unexpectedly last time.
  Send the crash report so we can fix it?". **Send** sends only that one
  report, even with crash reports off. **Don't send**, or closing the
  window, deletes it. The box "Always send crash reports" turns crash
  reports on, the same as the switch in Settings. On macOS the same
  question is asked after a native abort.

A crash file that is not sent is deleted after 7 days.

### Feedback

**Settings → Send feedback** sends the text you type to Sentry, and your
e-mail only if you type it. It uses the crash-report channel: if that is off,
the form says so and offers to turn it on and send. Nothing is sent before
you click.

### Check what is sent

Every telemetry call is written to `dimmy.log` with the prefix
`[telemetry]`. The log also prints the start of the embedded project keys
(`key-diag`), so you can check where events go.

---

## The licence service (license.dimmy.app)

The licence service runs on Cloudflare Workers with a Cloudflare D1
database. It stores:

- **Licences:** a SHA-256 hash of your e-mail (not the e-mail itself), the
  plan, start and end dates, the device limit, status, and the Stripe
  customer, session and subscription IDs.
- **Devices:** for each activated device, a random device ID, the device
  name (by default your computer name; you can change it before activating),
  first and last seen time, the app version and the `User-Agent` of the last
  licence refresh.
- **Activation codes:** one-time codes for the e-mail link, valid 10 minutes.
- **Audit log:** what happened to a licence (created, activated, refunded,
  deleted, …), with the e-mail hash and, for activations, the device ID and
  name.
- **Leads:** when you start a purchase or sign up for news, a hash of the
  e-mail, the plan and the language. When you ask to be written to ("I'd buy
  this" while sales are closed, or the Linux / mobile waitlist) and tick the
  consent box, the **clear e-mail** and the time of your consent are kept too,
  only to write to you about that.
- **Rate limits:** a counter keyed by your IP address, to stop abuse.

To send you activation links and receipts, your e-mail address passes through
Resend (e-mail delivery) and Stripe. Cloudflare, as the host, also processes
your IP address to deliver each request.

## Payment

Purchases are processed by **Stripe**, through **Stripe Managed Payments**:
Stripe is the merchant of record. It collects the card or other payment data,
your billing address and e-mail, calculates and collects taxes, and issues
receipts and invoices. We never see your card number. Stripe may offer
**Link**, its saved-payment service; using it is between you and Stripe.

## The website

dimmy.app is hosted on Cloudflare. It sets no cookies and runs no analytics.
To show the page it loads code and images from `unpkg.com`,
`cdn.simpleicons.org`, `api.github.com` and Product Hunt, which receive your
IP address like any web request. The forms (buy, waitlist, newsletter,
withdrawal) send what you type to `license.dimmy.app`.

---

## Why we process data (legal basis, GDPR art. 6)

| Processing | Legal basis |
|---|---|
| Licence, trial, activation, devices, payment, receipts, withdrawal requests | Performance of the contract (art. 6(1)(b)). Keeping an ended licence for the card-dispute window, and an expired trial's e-mail hash: our legitimate interest in defending payment disputes and preventing repeated free trials (art. 6(1)(f)). |
| Usage statistics and crash reports | Your consent (art. 6(1)(a); ePrivacy art. 5(3), Codice privacy art. 122). |
| "I'd buy this", waitlist and newsletter e-mails | Your consent (art. 6(1)(a)). |
| Feedback you send | Your consent, given by sending it. |
| Rate limits, abuse prevention, the audit log, security | Our legitimate interest in keeping the service safe and in proving what we did (art. 6(1)(f)). |

You can withdraw consent at any time. This does not affect what was done
before.

---

## Who processes data for us

- **Stripe** (Stripe Payments Europe / Stripe, Inc.): payment, tax, receipts, as merchant of record.
- **Cloudflare, Inc.**: hosting of the website and the licence service (Workers, D1).
- **Resend**: delivery of activation and confirmation e-mails.
- **PostHog** (EU cloud, Frankfurt): usage statistics, only with your consent.
- **Sentry** (Functional Software, EU region, Frankfurt): crash reports and feedback, only with your consent.

Only Konrad Dalla has access to these accounts.

The speech and AI providers you pick, the subscription CLIs (Claude Code,
Codex, Gemini CLI, Kimi Code), Telegram, Notion and Confluence are **not** our
processors: Dimmy connects to them on your own account, with your own key or
login, and they act under their own terms with you.

## Transfers outside the EU

Some of these companies are based in the United States (for example Stripe,
Cloudflare, Resend). Where data is transferred to the US, the transfer relies
on the EU-US Data Privacy Framework where the company is certified, or on the
European Commission's Standard Contractual Clauses.

---

## How long we keep it

| Data | Kept for |
|---|---|
| Rate-limit rows (IP address) | 48 hours. |
| Activation codes and account-deletion codes | 24 hours after they expire. |
| Active licence and its devices | As long as the licence is active. |
| Licence that ended (expired, refunded or revoked, withdrawn, deleted) and its devices | 120 days after the end (the window for card disputes), then deleted. |
| Expired trial that never became a paid licence | Only the hash of your e-mail, for 12 months after the trial ended, so that the same address cannot start a second free trial. Then deleted. Device names and User-Agents are removed when the trial ends. |
| Audit log | 12 months. |
| Stripe event IDs (each payment event is processed once) | 90 days. |
| Withdrawal declarations | 12 months after we receive them. |
| Leads, "I'd buy this" and waitlist e-mails | 12 months, or earlier when you ask or delete your account. |
| Usage statistics (PostHog) | Up to 12 months. |
| Crash reports (Sentry) | Up to 90 days. |
| Data on your computer | Until you delete it. |

A daily job on the licence service deletes the rows when their period ends.
Stripe (including Link) keeps your payment data as its own controller, under
its own privacy policy. We keep Stripe's monthly self-billed invoices for 10
years, as Italian law requires for accounting records (art. 2220 of the Civil
Code). These invoices do not contain your personal data.

---

## Your rights

Under the GDPR you can ask for access to your data, correction, deletion,
restriction, portability, and you can object to processing based on
legitimate interest. Write to **hi@dimmy.app**. We answer within
one month.

You can also lodge a complaint with the Italian supervisory authority,
the **Garante per la protezione dei dati personali**
(<https://www.garanteprivacy.it>), or with the authority of the EU country
where you live.

### Delete your account

The licence service has a deletion request at `POST /api/account/delete`:
you send your e-mail, we send you a one-time code, and you confirm with it.
On confirmation the licence is marked deleted, the e-mail hash is replaced by
a random placeholder, any subscription is cancelled, and your lead and
waitlist rows are removed. The audit log, which holds the e-mail hash, stays, to
prove the request was honoured. Records that tax law makes us keep are kept
for that period. If you prefer, write to us and we do it for you.

Telemetry is linked only to the installation ID. To have it deleted, send us
that ID (in the `analytics_id` file), then reset it.

---

## AI transparency

Dimmy uses AI models: speech-to-text, text cleanup, command mode and meeting
recaps. Their output can be wrong.

- Every recap saved to `recap.md` carries a machine-readable marker
  (`<!-- dimmy-ai-generated: true; by: Dimmy; model: … -->`), and the app
  shows "AI-generated summary. Review it before sharing." above it.
- Some things happen without a review step, because you set them up that
  way: dictated text is pasted into the app in front of you, history is
  saved, a recap runs when a meeting stops, and a recap can be sent to Notion
  automatically if you turn that on. Review AI output before you share it.
- We do not train models on your data. Dimmy does not identify people from
  their voiceprint, infer emotions or do biometric categorisation. Speaker
  labels in meeting transcripts ("Speaker 1", "Speaker 2") only separate
  voices inside one recording.

---

## Changes to this policy

Material changes are announced in the release notes and the date at the top
changes. `git log PRIVACY.md` shows every change.

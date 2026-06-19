# Module reference

One page per core module. For the top-down picture, read [`../ARCHITECTURE.md`](../ARCHITECTURE.md) first.

## `provider.rs` — the provider enum

```rust
enum Provider { Groq, OpenAI, OpenRouter, Gemini, Deepgram, Anthropic, Custom, Local }
```

- Cloud providers are detected from URL via `Provider::from_url()`.
- `Local` is set explicitly by setting `stt_mode = "local"` in config. It does not have a URL.
- Every provider has `max_file_bytes()` — the cap that decides whether `transcribe.rs` should chunk the WAV before sending. `Local` returns `usize::MAX` (no network, no cap).
- URL validation: HTTPS required (localhost is the only exception). See `Provider::validate_url`.

## `transcribe.rs` — STT routing

Cloud providers have three wire formats; the module dispatches on the `Provider` enum:

| Format | Providers | How |
|---|---|---|
| OpenAI-compatible multipart | Groq, OpenAI, OpenRouter, Custom | `reqwest::multipart` with the WAV as a `file` part |
| Raw body | Deepgram | `reqwest` with raw WAV bytes, content-type `audio/wav` |
| Base64 JSON | Gemini | inline base64 in the request body |
| Direct | Local | calls `local_stt::transcribe_local()` — no HTTP |

**Chunking** happens when the WAV exceeds the provider's `max_file_bytes()`. `split_at_silence()` searches backwards in the last 25% of a chunk for an RMS < 0.01 window of 300ms. If none found, force-split at the max boundary. Post-condition: total samples in == total samples out (asserted).

**Timeouts** scale with payload: `30s + wav_bytes / (1024 * 1024)`, capped at 600s. Applied to all three network paths.

## `local_stt.rs` — whisper-rs integration

- Feature-gated behind `local-stt`. GPU variants add `local-stt-metal`, `local-stt-vulkan`, `local-stt-cuda`.
- Models are GGML format, downloaded on demand from HuggingFace to `dirs::data_dir()/dimmy/models/`.
- **Default model:** `ggml-base-q8_0.bin` (78 MB). Tiny (42), Base (78), Small (181), Medium (514) are catalogued.
- Input: f32 16 kHz mono samples. The preprocessing pipeline in `preprocess.rs` produces 48 kHz; `to_wav_payload()` downsamples to 16 kHz for whisper.
- **Context cache:** the loaded `WhisperContext` stays in VRAM across recordings. Invalidated on model change or `dimmy_shutdown()`.
- **Sticky known-bad GPU marker:** if a GPU path aborts once (e.g. `ggml-vulkan` init crash), a fingerprint is written so the next run falls back to CPU without reattempting the same combo. See [`../dev/known-bugs.md`](known-bugs.md) for GPU crash recovery context.
- **Downloads go through the shared `download.rs` module (below)** — resumable + SHA-256/magic verified (whisper `.bin` magic is `ggml`/`GGUF`).
- FFI: `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`.

## `local_llm.rs` — llama.cpp integration (optional)

- Feature-gated behind `local-llm`. GPU variants: `local-llm-metal`, `local-llm-vulkan`, `local-llm-cuda`.
- Uses the **forked `llama-cpp-4`** dependency — `KonradDallaOrg/llama-cpp-rs`.
- **`dynamic-link` is enabled on the GPU builds that ship — Vulkan (Windows) and Metal (macOS)** (`local-llm-vulkan` and `local-llm-metal` both pull `llama-cpp-4/dynamic-link`; `local-llm-cuda` does not). llama's ggml ships as separate DLLs/dylibs next to `dimmy_lib` instead of being statically linked in. This is load-bearing: `whisper-rs-sys` AND `llama-cpp-sys-4` each vendor ggml, and a static link deduplicates them to ONE (`/FORCE:MULTIPLE`, `LNK4006`) — fine while their ggml revisions matched, but after the llama.cpp fork bump the June-2026 ggml diverged from whisper's and silently broke local STT (0 chars + crash). Dynamic-link keeps each module's ggml private (per-module symbol resolution). See `feedback_whisper_llama_shared_ggml_collision` in the session memory.
  - **Windows:** `ggml*.dll` + `llama*.dll` are copied next to `dimmy_lib.dll` by the C# `CopyLlamaDlls` MSBuild target (and into the installer); they are loaded at runtime, so they MUST sit beside the DLL.
  - **macOS:** dylibs bundled into `Dimmy.app/Contents/Frameworks/` and codesigned.
  - **Linux:** built but not wired into the default AppImage (CPU-only for portability).
- **Generation hygiene** (small chat-templated models, e.g. Gemma/Phi): stop at the turn-end marker and `strip_special_tags` removes `<think>…</think>`, `<|im_end|>`, `<start_of_turn>` etc. Thinking mode stays OFF — with it on, models emit 300-500 hidden tokens first (see [`local-llm-feasibility.md`](local-llm-feasibility.md)). `DEFAULT_LLM_MODEL` is Phi-4 Mini (Gemma E2B drifts "playful" on short prompts).
- **Translation prompt** uses the language NAME via `crate::llm::lang_name` (small models ignore "translate to en" but follow "translate to English").
- **Downloads go through the shared `download.rs` module (below)** — resumable + SHA-256/magic verified.
- FFI: `dimmy_list_llm_models`, `dimmy_download_llm_model`, `dimmy_llm_model_exists`.

## `download.rs` — resumable, integrity-checked model downloads

One place for every multi-GB model download (LLM GGUF, whisper ggml/GGUF, parakeet ONNX bundle) so they all survive a mid-flight kill and never install a corrupt file — shared core, so Win/Mac/Linux behave identically.

- **`download_resumable(client, url, dest, accept_magics, on_progress)`** (async) — used by `local_llm` and `local_stt`. Writes to `<dest>.part`, then atomically renames.
- **`verify_file(path, accept_magics, expected_sha)`** (sync) — magic + streaming SHA-256; reused by `parakeet`'s blocking per-file bundle download.
- **Resume:** an existing `.part` continues via `Range: bytes=N-`. `206` → append; `200` (server ignored the range, or `If-Range` says the file changed) → truncate + restart; `416` → discard the stale `.part`. The starting ETag is persisted in a `<file>.part.etag` sidecar so a later resume can send `If-Range`.
- **Integrity:** HuggingFace LFS serves each file's SHA-256 as the (`X-Linked-`)`ETag` → captured and compared after download (streamed in 1 MiB chunks, never buffered whole), plus optional magic-byte prefixes. On ANY integrity/size failure the `.part` is DELETED so the retry restarts clean instead of resuming corruption.
- `sha2` is a **non-optional** dependency so the check runs in every build (incl. the frozen Windows feature set, which has no `license-client`).

## `llm.rs` — post-processing router

- Two entry points: `process_text` (dictation enhancement — style + tone + translate, wraps the text in `[TRANSCRIPTION]` and applies `build_system_prompt`) and `process_raw_prompt` (command mode + meeting recap — sends the caller's prompt verbatim). Local mirrors live in `local_llm.rs`.
- Dispatches on `style` (Off, Correct, Summarize, Elaborate, Comprehensible, Professional, Prompt, Gen-Z, Boomer, Emoji, Acronyms, Imbruttito, Custom).
- Two wire formats: **OpenAI-compatible chat completions** (Groq, OpenAI, OpenRouter, Gemini, Together, Custom) and **Anthropic Messages API** (Claude).
- `llm_mode` config field routes to cloud or local (when `local-llm` feature is enabled).
- **PREAMBLE enforces "keep same language as the input"**. Small local models need this reinforcement or they default to English.
- **Translation directive** uses `lang_name(code)` → the English language NAME + an imperative ("Then translate the ENTIRE result into English…"). The old bare ISO code (`"…to en."`) was silently ignored even by capable models (Claude Haiku kept Italian). Codes accepted via `SUPPORTED_TRANSLATE_LANGS`.
- **`strip_output_scaffolding`** runs on cloud output: drops prompt scaffolding a weak model echoes (`[TRANSCRIPTION]`, `[SPOKEN]`/`[SELECTION]`, ChatML tokens) AND the whole `<think>…</think>` reasoning trace (qwen3 via Groq leaked it).
- **OpenAI gpt-5 / o-series** use `openai_reasoning_shape` → `max_completion_tokens` (no `temperature`) **floored at `.max(8192)`** in both `process_text` and `process_raw_prompt`. Without the floor the internal reasoning trace consumes the whole budget and the content comes back EMPTY.
- **Key resolution** (in `ffi.rs`, not here): the cloud/command dispatch reads `KeyringScope::Llm(vendor)` first, then falls back to the SAME vendor's `KeyringScope::Stt(vendor)` key — vendor is derived from `llm_url`, so it can never pull a different provider's key. One key per provider works for STT + LLM + command.
- **Live verification:** [`core/tests/llm_flows.rs`](../../core/tests/llm_flows.rs) — the `#[ignore]` flow matrix + catalog sweep. See [`llm-flows-testing.md`](llm-flows-testing.md).

## `keystore.rs` — API key storage

- **Always uses local AES-256-GCM encrypted file** at `~/.config/dimmy/keys.enc` (or `%APPDATA%\dimmy\keys.enc`).
- Key derivation: `SHA-256(username + hostname + salt)`. Machine-specific — copying `keys.enc` to another machine yields unreadable blob.
- **No OS popups, no admin prompts, no keyring prompts.** The `use_keyring` config field is forced to `false` in the core; the toggle is removed from all platform UIs.
- OS keyring (macOS Keychain, Windows Credential Manager, Secret Service on Linux) is kept as **read-only fallback** for migrating users from the pre-v0.4 era.

## `history.rs` — transcription history

- SQLite + FTS5 virtual table for full-text search.
- DB file: `~/.config/dimmy/history.db` (Linux/macOS) or `%APPDATA%\dimmy\history.db` (Windows).
- Auto-saves after every successful transcription. No opt-out by design (the feature is the feature).
- FFI: `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`.

## `filler.rs` — filler word removal

- Post-transcription pass. Regex with word-boundary matching, case-insensitive.
- 6 languages: Italian (`ehm`, `cioè`, `allora`), English (`um`, `uh`, `like`, `basically`), Spanish, French, German, Portuguese.
- Applied to both local and cloud transcriptions when `filler_removal_enabled: true`.
- Runs BEFORE the LLM post-processing stage, so the LLM sees clean text.

## `audio.rs` — mic capture

- `cpal` for cross-platform audio input. 48 kHz mono f32.
- Emits `AudioCommand::Start` / `AudioCommand::Stop` via an mpsc channel from the hotkey thread.
- Safe shutdown: `dimmy_shutdown()` sends `Stop`, waits 50 ms, then clears the model cache.
- **Audio health check** at startup: `dimmy_check_audio_health()` opens a short probe stream on the default device before the first real recording. If the mic is broken / missing / muted, the UI surfaces the failure immediately instead of showing the pill and silently transcribing nothing.

## `preprocess.rs` — VAD, AGC, highpass, clamp

Full reference: [`audio-pipeline.md`](audio-pipeline.md). Headline:

- Clamp → 80 Hz highpass (biquad) → VAD (nnnoiseless) → AGC (dagc) → clamp.
- **Never feed zero-amplitude samples to dagc** — it produces all-NaN permanently. The VAD grace period delays the `in_speech → false` transition but must NOT emit silence frames. (AUDIO-001.)
- `process_buffer()` calls `process()` ONCE for the full recording. The whole buffer goes through a single VAD+AGC pass.

## `hotkey.rs` — global hotkey

- Platform-specific. On Windows: low-level keyboard hook (`SetWindowsHookEx`) with 7 FFI functions for the C# UI to bind arbitrary combinations. On macOS: `CGEventTap`. On Linux: X11 (`XDotool`) and Wayland (portal, where supported).
- **macOS symbol gotchas:** `objc_msgSend` must NOT be declared variadic — stack-based args on ARM64 crash with PAC failure at runtime (CI cross-compile doesn't catch it). Declare as `fn objc_msgSend()` and `transmute` to typed pointers. `kCFTypeDictionaryKeyCallBacks` must be `static ... : [u8; 0]`. See `known-bugs.md` MACOS-001/002.
- **macOS framework links** must be explicit in `hotkey.rs`: CoreGraphics + CoreFoundation.

## `ffi.rs` — the C surface

- ~76 functions, snapshotted in `core/tests/fixtures/abi_exports.txt`
  and diff-tested by `abi_snapshot.rs`. Stable ABI.
- Global `OnceLock<AppState>` owns the process-wide singleton. The
  `MEETING` static (separate slot) owns the meeting session so its
  lifecycle is decoupled from any UI window.
- Every function asserts preconditions: non-null pointers, valid UTF-8, bounds, finite floats.
- All JSON blobs are validated on entry (malformed JSON returns an error, does not panic).
- String outputs: caller allocates the buffer and passes its size; function writes UTF-8 + NUL and returns the bytes written. If the buffer is too small, returns `-1` and the required size is discoverable via a separate `_size()` function.
- **Return-code conventions** vary by family. The notable ones a UI
  must handle exactly:
  - `dimmy_start_recording` returns **-7** when a meeting is active
    (silent no-op — must not surface as an error).
  - `dimmy_meeting_pause` / `_resume` / `_is_paused` return **1**
    (state flipped), **0** (no-op / no meeting), **-1** (lock
    failure).
  - `dimmy_transcribe_file` returns **-1..-8** for distinct error
    classes (file missing, unsupported codec, model missing,
    over-limit cloud chunk, etc.). The rc table is contractual —
    do NOT renumber.

## `error.rs` — typed error hierarchy

- Central `DimmyError` enum, variants per subsystem (`Audio`, `Transcribe`, `Llm`, `Keystore`, `History`, `Model`, `Config`, `Io`).
- Error messages are truncated to 200 chars at the FFI boundary — prevents leaking API keys, PII, or oversized provider responses into logs.
- Display impl is the canonical user-facing string. Debug impl is for logs.

## `meeting.rs` — long-form meeting mode

- `MeetingSession::start(...)` spawns a worker thread that drains the
  shared audio buffer in chunks (default 15 s, configurable via
  `meeting_chunk_secs`), streaming-writes `audio.wav` (16 kHz mono int16,
  ~115 MB / hour) and appending one line per chunk to `transcripts.txt`.
- On-disk layout per meeting (`<config>/meetings/<id>/`): `audio.wav`,
  `transcripts.txt`, `meta.json` (start_ts, sample_rate, last_chunk_ts),
  `recap.md` + `actions.json` (post-stop), `.recording` marker (deleted
  on clean stop; presence at startup → "recover meeting?"). UUIDs avoid
  same-second collisions.
- **STT routing**: chunks go through the SAME backend the dictation
  pipeline would use — cloud (`transcribe.rs`) or local (whisper /
  Parakeet / parakeet_fluid). Hardcoding Parakeet here was the 2026-05-07
  bug that broke meeting transcripts on builds without
  `local-stt-parakeet`.
- **Pause/resume**: `MeetingSession::pause()` / `.resume()` flip an
  `Arc<AtomicBool>`; cpal callbacks keep filling buffers but the worker
  skips drain / WAV write / STT chunks. On resume the worker advances
  `samples_written` + `last_processed` to current `buf_len_now` so the
  paused window is excluded from `audio.wav` AND from the chunked
  timeline. A `[paused] (resumed after N ms)` line lands in
  `transcripts.txt`. Idempotent (second `pause()` returns false).
- **Post-process** (`save_post_process`): UI-side `MeetingPostProcessService`
  (Win + Mac mirrors) reads `transcripts.txt`, formats the 11-section
  structured-recap prompt, calls `llm::process_raw_prompt` (recap-model
  override before URL heuristic), parses the response into `recap.md` +
  `actions.json`. Anthropic Opus 4.7+ / Sonnet 5+ use
  `thinking.type=adaptive` (no `budget_tokens`).
- FFI: `dimmy_meeting_start`, `_stop`, `_save_post_process`,
  `_list_orphans`, `_is_active`, `_pause`, `_resume`, `_is_paused`.

## `aec.rs` — acoustic echo cancellation (Mix mode)

- Pure-Rust port of WebRTC AEC3 via the `aec3 = 0.2` crate.
- Operates on 10 ms frames at 48 kHz mono (480 samples). cpal callbacks
  push to two ring buffers — mic (`capture`) and loopback (`render`,
  the reference signal); the worker drains 480-sample frames and runs
  them through `aec3::pipelines::linear`.
- **Bounded rings** (`MAX_RING_SAMPLES = 48_000`, 1 s headroom): if a
  callback stalls, oldest samples are dropped and AEC resyncs via its
  delay estimator. Better than unbounded growth.
- **Always-mix safety**: when loopback is silent / disabled / routed
  away (BT meeting in HFP, no default output, headset unplugged), the
  worker zero-pads the ref ring rather than blocking — the
  `worker_processes_mic_when_ref_ring_empty` test guards this.
- Idle: rings empty → 5 ms sleep tick, zero CPU.

## `dfn.rs` — DeepFilterNet noise suppression (DEFERRED)

- Module wired upstream of AEC for ML-based noise suppression. Currently
  a no-op gate behind `local-dfn` cargo feature.
- Activation deferred until either the upstream `deep_filter` crate
  publishes a `tract`-feature build, or we swap to `deepfilter-rt`
  riding the existing `ort` runtime. See `Cargo.toml` comment.

## `process_loopback.rs` — per-process WASAPI loopback (Win-only, Phase 5a SCAFFOLDING)

- Why: standard WASAPI loopback captures whatever's routed to a specific
  OUTPUT device. When a meeting app puts a Bluetooth headset into HFP/SCO
  for bidirectional voice, the audio bypasses the normal render endpoint
  → loopback is silent. Per-process capture asks Windows for the audio
  the meeting app *itself* is producing, before routing.
- API surface: `list_meeting_processes()` enumerates known meeting exes
  (`ms-teams.exe`, `zoom.exe`, `discord.exe`, `slack.exe`, `webex.exe`,
  `googlemeet.exe`, …) via `Toolhelp32` snapshot. `auto_detect_meeting_pid()`
  picks one heuristically.
- **Status: SCAFFOLDING.** `spawn_process_capture` returns `Err` for
  now. The real implementation (`ActivateAudioInterfaceAsync` +
  `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` + an
  `IActivateAudioInterfaceCompletionHandler` impl) is the next commit.
- Mac/Linux: stub returning empty enumeration + Err from
  `spawn_process_capture`. Caller falls back to default-output loopback.

## `chunked_stt.rs` — realtime chunked Parakeet worker

- Spawns a worker that, while the audio capture thread fills the shared
  PCM buffer, periodically slices off the most recent N seconds (+ small
  overlap), runs them through `parakeet::transcribe`, dedups against the
  running cumulative text (last-3-words match), and emits a callback so
  the FFI layer can fan out events to the native UI.
- Default 5 s chunks for the realtime path (interactive cadence). The
  benchmark-tuned 30 s window with 500 ms overlap is for the offline
  driver. Pattern proven on WSL CPU at 8.7× realtime over 272 min of
  LibriVox audio (see `docs/dev/stt-benchmark-parakeet-local-2026-05-05.md`).
- Worker downsamples to 16 kHz per chunk. **No preprocessing** is
  applied per chunk — Parakeet is robust to mic-level noise on its own;
  highpass/VAD/AGC are tuned for end-of-recording silence trim, not
  streaming.

## `parakeet.rs` — Parakeet TDT v3 FP32 local STT (ONNX Runtime)

- Pure-Rust port of `onnx_asr.models.nemo.NemoConformerTdt`. Bundle:
  `nemo128.onnx` (waveform → 128-bin mel) + `encoder-model.onnx` +
  external `.data` (~2.4 GB) + `decoder_joint-model.onnx` + `vocab.txt`
  (8193 tokens BPE-style with `▁` word marker), downloaded from
  `istupakov/parakeet-tdt-0.6b-v3-onnx` to `<config>/parakeet-fp32/`
  on first use (~2.5 GB).
- Pipeline: 16 kHz f32 PCM → mel features `[1, 128, T_mel]` → encoder
  `[1, 1024, T_enc] + lens` → greedy TDT (LSTM state `[2, 1, 640]` × 2;
  per-frame argmax of token + duration) → vocab → text.
- Feature gates: `local-stt-parakeet` (CPU, default for Win/Linux),
  `local-stt-parakeet-cuda` (Win NVIDIA), `local-stt-parakeet-coreml`
  (Mac, currently no-win — see `parakeet_fluid.rs` for the production
  Mac path).
- Word-level timestamps via `transcribe_with_word_timestamps` for the
  history-v2 schema.
- **Lifecycle gotcha**: `Box::leak`s the `Mutex<Option<Inner>>` so
  Rust's static-destructor pass never drops the cached ort `Session`s
  — drops at process exit hit a torn-down onnxruntime mutex and abort.
  See `known-bugs.md` STT-002.

## `parakeet_fluid.rs` — Parakeet via FluidAudio CoreML (Mac ANE)

- Wraps `fluidaudio-rs` (Swift bridge built by `build.rs` at compile
  time; needs Xcode CLT with Swift 5.10+) to run Parakeet on the Apple
  Neural Engine. Documented RTF on M-series: 100–300×; first
  `init_asr()` downloads the ~3 GB CoreML bundle into
  `~/.cache/fluidaudio/` and ANE-compiles it (~20–30 s one-time);
  warm reloads ~1 s.
- Gated by `local-stt-parakeet-fluid` feature. Mutually exclusive with
  the ONNX path on Mac at build time; `transcribe.rs` and
  `chunked_stt.rs` dispatch on the active feature without knowing
  which is live.
- Only available on `aarch64-apple-darwin` — `#![cfg(...)]` at module
  top so non-Mac / Intel-Mac builds skip the file entirely.
- `transcribe_samples` only landed upstream after FluidAudio v0.12.6,
  so the bridge writes a temp WAV per call (~320 KB I/O for a 5 s
  chunk, negligible vs ANE inference time).

## `app_rules.rs` — per-app LLM style override

- Resolves the captured app id (process name on Win, bundle id on Mac,
  X11 `WM_CLASS` on Linux/X11) against the user's rule list. First match
  wins. Produces a per-transcription override of `llm_style` and
  `llm_translate_to` without mutating the user's saved defaults.
- `MatchType` discriminator is set at rule-creation time based on the
  user's OS so the matcher can dispatch (case-insensitive process name,
  case-sensitive bundle id, case-insensitive WM_CLASS). Wayland is
  unsupported (compositor security model) — rules with `WmClass`
  silently no-op on Wayland sessions.
- **Privacy invariant**: app identifiers ARE user data (could leak a
  sensitive app name) — they MUST NOT leave the machine via telemetry.
  Only emit `app_rule_matched` as a boolean signal. See `CLAUDE.md`
  privacy hard rules.
- FFI: `dimmy_set_app_context`, `dimmy_clear_app_context`. Win captures
  the foreground HWND + focus drift in `Helpers/AppContextCapture.cs`.

## `autostart.rs` — launch-at-login toggle

- Cross-platform wrapper around the `auto-launch` crate. Win:
  `HKCU\…\Run\Dimmy`. Mac: `~/Library/LaunchAgents/dimmy.plist`. Linux:
  `~/.config/autostart/dimmy.desktop` (XDG spec; honoured by GNOME /
  KDE / XFCE / …).
- All three are user-scope, reversible, survive reboots. `is_enabled()`
  is cheap (one stat or registry read) — caching not needed.
- FFI: `dimmy_autostart_set_enabled`, `dimmy_autostart_is_enabled`.
  Failure surface is non-load-bearing — losing the entry just means
  "next reboot, the user has to launch Dimmy manually".

## `gpu_health.rs` + `gpu_diag.rs` — GPU crash recovery

- **Sentinel** (`.gpu_init_in_progress`): short-lived. Written
  immediately before a GPU-backed model load, deleted immediately after
  the call returns (only a hard `abort()` leaves it on disk). Lets the
  next process detect that the previous one died inside ggml-vulkan.
- **Known-bad** (`.gpu_known_bad`): sticky across sessions. Written
  when the sentinel fires AND recovery succeeds. Stores a driver
  fingerprint so the next launch can compare: same → keep CPU mode,
  different → driver/ICD changed, retry GPU once. Without this, the
  user paid one crash + relaunch on every cold start. Manual override:
  `clear_known_bad` (UI button).
- `gpu_diag.rs` registers ggml log callbacks on whisper + llama so the
  C++ stderr (invisible on Windows GUI apps) lands in `dimmy.log` —
  the last-words-before-crash become visible post-mortem. Plus a
  one-shot Vulkan environment snapshot (vulkan-1.dll path + size,
  per-device VRAM, driver version best-effort).

## `license.rs` — Ed25519 license verification (offline)

- Verifies Ed25519-signed license tokens **offline** against a
  build-time-embedded public key (`DIMMY_LICENSE_PUBKEY` env var).
- Reads/writes `~/.config/dimmy/license.json`. Tracks last successful
  online refresh so it can enforce a soft offline grace window
  (`max_offline_days` from the token).
- **Privacy invariants**: plain email is never persisted on disk, only
  its SHA-256 hash with stable salt. Private signing key is never in
  the client binary — only the public key.
- Open-source carve-out: when `DIMMY_LICENSE_PUBKEY` is empty/unset
  (source-build path), `check_status()` returns `Unrestricted` and
  licensing is a no-op. Build-it-yourself, run-it-free.
- Token format: JWT-like, `header_b64.payload_b64.sig_b64` (alg=EdDSA,
  typ=DLT). See `Claims` for the payload schema.
- Feature-gated: `license-cli` (CLI testing client) + `license-client`
  (verification + file I/O for the production cdylib, default ON).
  Server-side signing happens in a Cloudflare Worker (TypeScript) — no
  Rust signer ships.

## `telemetry/` — PostHog + Sentry pipeline

- `telemetry/events.rs` defines the `Event` enum — every event variant
  IS the wire format. Adding a new event requires a unit test plus
  updates to `docs/dev/telemetry-implementation.md` and `PRIVACY.md` if
  a new category of data is collected.
- `telemetry/sentry_pipeline.rs` wraps the `sentry` crate (gated by
  `telemetry-sentry` feature, default ON). User feedback uses a
  manually-built envelope (`type=feedback`) so reports land in Sentry's
  Feedback tab, not Issues.
- **Hard privacy rules** — never include user content (transcribed
  text, prompt text, custom LLM prompt, mic device name, file paths,
  hostname, username, IP) in any property or message. The
  `looks_like_secret` filter is a safety net, not a substitute for
  review. Provider names (groq/openai/anthropic/…) are categorical
  enums and OK to send. See `PRIVACY.md` for the public surface.

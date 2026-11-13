# Module reference

One page per core module. For the top-down picture, read [`../ARCHITECTURE.md`](../ARCHITECTURE.md) first. For the **end-to-end as-built behavior + freeze invariants** of the load-bearing features that these modules implement (meeting/system-audio capture, recap, shortcuts, API/providers, dictation), see [`known-good-baseline.md`](known-good-baseline.md).

## `provider.rs` — the provider enum

```rust
enum Provider { Groq, OpenAI, OpenRouter, Gemini, Deepgram, Anthropic, Fireworks, Together, Kimi, Custom, Local }
```

- Cloud providers are detected from URL via `Provider::from_url()`. An empty URL gives `Groq`; an unknown host gives `Custom`.
- `Local` has no URL and `from_url()` never returns it. Local STT is selected by the config field `stt_mode = "local"`, which the dispatch in `ffi.rs` reads directly.
- Every provider has `max_file_bytes()` — the cap that decides whether `transcribe.rs` should chunk the WAV before sending: Deepgram 2 GB, Gemini 20 MB, Fireworks / Together 100 MB, every other cloud provider 25 MB. `Local` returns `usize::MAX` (no network, no cap).
- URL validation: HTTPS required (`localhost`, `127.0.0.1` and `::1` are the only exceptions). See `Provider::validate_url`.
- Other lookups: `from_model_id` (claude-* / gpt-* / o-series / gemini-* prefixes), `from_catalog_model_id` (exact match in the embedded model catalog), `from_tag` / `as_str` (vendor tag), `default_llm_url`, `supports_stt` (Groq, OpenAI, Gemini, Deepgram, Fireworks, Together).
- `KeyringScope { Stt(p), Llm(p), Recap(p), NotionToken, ConfluenceToken }` also lives here; `entry_name()` gives the keystore entry (`api-key-groq`, `llm-key-anthropic`, `recap-key-openai`, ...).

## `transcribe.rs` — STT routing

Cloud providers have three wire formats; the module dispatches on the `Provider` enum:

| Format | Providers | How |
|---|---|---|
| OpenAI-compatible multipart | every URL that is not Deepgram or Gemini (Groq, OpenAI, OpenRouter, Fireworks, Together, Custom) | `reqwest::multipart` with the WAV as a `file` part. `stt_field_style(model)` picks the language field: `language` (whisper-1, gpt-4o-transcribe, third parties) or `languages[]` (gpt-transcribe, gpt-live-transcribe) |
| Raw body | Deepgram | `reqwest` with raw WAV bytes, content-type `audio/wav`; model and language are query parameters (`compose_deepgram_url`) |
| Base64 JSON | Gemini | inline base64 in a `generateContent` request body |
| Direct | Local | no HTTP. One function per engine: `transcribe_audio_local` (whisper, calls `local_stt::transcribe_local()`), `transcribe_audio_local_parakeet`, `_qwen`, `_whistle`, `_nemotron` (+ `_words` variants) |

**Local engine choice:** `resolve_local_backend(selected, EnginesReady)` returns the engine to run (`whisper`, `parakeet`, `qwen`, `whistle`, `nemotron`). When the selected engine has no model on disk and whisper has one, it falls back to whisper and sets `fell_back`.

**Chunking** (`transcribe_chunked`) happens when the estimated WAV exceeds the provider's `max_file_bytes()`. The chunk length comes from `cloud_chunk_samples` (80% of the limit, never under 5 s). `ProcessedAudio::split_at_silence()` (in `audio.rs`, via `first_silence_split`) takes the latest 300 ms window with RMS < 0.01 in the last 25% of a chunk. If none found, force-split at the max boundary. Post-condition: total samples in == total samples out (asserted).

**Timeouts** scale with payload: `30s + wav_bytes / (1024 * 1024)`, capped at 600s. Applied to all three network paths.

## `local_stt.rs` — whisper-rs integration

- Inference is feature-gated behind `local-stt`. GPU variants add `local-stt-metal`, `local-stt-vulkan`, `local-stt-cuda`; `local-stt-coreml` moves the encoder to the Apple Neural Engine (see `coreml_encoder.rs`).
- Models are GGML format, downloaded on demand from HuggingFace to `dirs::data_dir()/<config namespace>/models/` (`model_directory()`; the namespace is `dimmy` unless `DIMMY_CONFIG_NAMESPACE` is set at build time).
- **Default model:** `DEFAULT_MODEL` = `ggml-base-q8_0.bin` (78 MB). `AVAILABLE_MODELS` has 11 entries: Tiny q8 (42 MB), Base q8 (78), Small q5_1 (181), Medium q5_0 (514), Large-v3-Turbo q5 (574) / q8 (874), Large-v3 q5 (1104) / q8 (1580) / f16 (2952), Distil-Large-v3.5 q8 (818) / q5 (538, English only). See [`model-inventory.md`](model-inventory.md) for the sources.
- Input: f32 16 kHz mono samples. The capture pipeline produces 48 kHz; `transcribe::stt_input_16k()` calls `preprocess::downsample_to_16k()` before whisper (and applies GTCRN when it is switched on).
- **Context cache:** the loaded `WhisperContext` stays in VRAM across recordings. Cleared by `clear_model_cache()` on model change, on `dimmy_model_delete`, and in `dimmy_shutdown()`.
- **Sticky known-bad GPU marker:** one abort during GPU init forces the CPU for that session only. A second consecutive abort on the same driver fingerprint (`gpu_health::record_strike`) writes the sticky marker, and later runs use the CPU until the fingerprint changes. `announce_cpu_fallback` tells the host. See `gpu_health.rs` below and [`../dev/known-bugs.md`](known-bugs.md) for GPU crash recovery context.
- **Downloads go through the shared `download.rs` module (below)** — resumable + SHA-256/magic verified (accepted magics: `lmgg`, the on-disk bytes of a ggml file, and `GGUF`).
- FFI: `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`.

## `local_llm.rs` — llama.cpp integration (optional)

- Feature-gated behind `local-llm`. GPU variants: `local-llm-metal`, `local-llm-vulkan`, `local-llm-cuda`.
- Uses the `llama-cpp-4` dependency from upstream `eugenehp/llama-cpp-rs` (git, `default-features = false`). The `KonradDallaOrg/llama-cpp-rs` fork is no longer used; `core/Cargo.toml` gives the reason.
- Linking:
  - **Windows (Vulkan):** `local-llm-vulkan` turns on `llama-cpp-4/dynamic-link`: llama.cpp and ggml are built as shared libraries (`llama.dll`, `ggml*.dll`), so whisper keeps its own static ggml inside `dimmy_lib.dll`. `local-stt-qwen` adds `mtmd.dll`.
  - **macOS (Metal):** `dynamic-link` — dylibs (`libllama`/`libggml*`) bundled into `Dimmy.app/Contents/Frameworks/` and codesigned.
  - **Linux:** the release workflow builds `platforms/linux` with `--features license-client` only, so the AppImage has no local LLM.
- **Generation hygiene** (small chat-templated models, e.g. Gemma/Phi): `strip_special_tags` removes `<think>…</think>`, `<start_of_turn>`, ChatML tokens etc.; `strip_leaked_prompt` cuts an echoed prompt. Thinking stays OFF: for a template with an `enable_thinking` switch, `skip_thinking_suffix` pre-fills an empty think block (see [`local-llm-feasibility.md`](local-llm-feasibility.md)). `DEFAULT_LLM_MODEL` is Gemma 4 E2B QAT (`gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf`); Phi-4 Mini was the default from 2026-05-18 until the prompt-format fix of 2026-10-22 (see the constant's comment). `AVAILABLE_LLM_MODELS` has 18 entries.
- Entry points: `process_text_local` (dictation enhancement) and `process_raw_prompt_local` (command mode + recap), the local mirrors of `llm.rs`.
- **Translation prompt** uses the language NAME via `crate::llm::lang_name` (small models ignore "translate to en" but follow "translate to English").
- **Downloads go through the shared `download.rs` module (below)** — resumable + SHA-256/magic verified.
- FFI: `dimmy_list_llm_models`, `dimmy_download_llm_model`, `dimmy_llm_model_exists`.

## `download.rs` — resumable, integrity-checked model downloads

One place for every model download (LLM GGUF, whisper ggml/GGUF, parakeet ONNX bundle, Qwen3-ASR, Whistle, Nemotron, the diarization model, the Core ML encoder zip) so they all survive a mid-flight kill and never install a corrupt file — shared core, so Win/Mac/Linux behave identically.

- **`download_resumable(client, url, dest, accept_magics, on_progress)`** (async) — used by `local_llm`, `local_stt`, `parakeet`, `qwen_asr`, `whistle`, `nemotron`, `diarize` and `coreml_encoder`. Writes to `<dest>.part`, then atomically renames. Progress is reported every 200 ms, not per chunk.
- **`verify_file(path, accept_magics, expected_sha)`** (sync) — magic + streaming SHA-256. Called inside `download_resumable`, and again by `whistle`, `nemotron` and `diarize` against the SHA-256 they pin in source.
- **Resume:** an existing `.part` continues via `Range: bytes=N-`. `206` → append; `200` (server ignored the range, or `If-Range` says the file changed) → truncate + restart; `416` → discard the stale `.part`. The starting ETag is persisted in a `<file>.part.etag` sidecar so a later resume can send `If-Range`.
- **Integrity:** HuggingFace serves each file's SHA-256 as `X-Linked-ETag` on its own 302. `probe_linked_etag` reads it with a separate HEAD that does not follow the redirect (the CDN's `ETag` is a Xet content hash, not the SHA-256). The hash is compared after download (streamed in 1 MiB chunks, never buffered whole), plus optional magic-byte prefixes. On an integrity failure the `.part` and its sidecar are DELETED so the retry restarts clean. A short download (fewer bytes than announced) keeps the `.part` so the next try resumes.
- `sha2` is a **non-optional** dependency so the check runs in every build, with or without `license-client`.

## `download_center.rs` — one queue for every on-device model

- FFI: `dimmy_model_download_enqueue(id)` (0 queued / 1 already active / -1 unknown id), `_cancel(id)`, `_snapshot_json(buf,len)`. Ids are the On-device row ids: whisper `.bin`, LLM `.gguf`, `parakeet:fp32`, `whistle`, `nemotron`, `nemotron-f32`, `qwen:<file>` — anything outside the catalogs is refused (the id becomes a path). `whistle`, `nemotron` and `nemotron-f32` are known only where an engine build exists for the platform.
- One worker thread, FIFO, exits when idle. Each job: presence check → the existing per-kind download fn (so `download.rs` resume applies) → up to 3 retries (2 s / 8 s / 30 s). Cancel drops the running future via `tokio::select!`; the `.part` stays for the next try.
- Emits `model_download` `{"id","state","done","total","error"?}` on every state change and at most once per percent (per MiB when the size is unknown). Emitted outside the state lock, serialized by `emit_order`, so a host may read the snapshot from inside its callback.
- Shares `ffi::model_download_slot(id)` with the per-page download FFIs, so the two paths can never append to one `.part` from different offsets.
- `dimmy_model_delete(id)` (0 removed / 1 nothing on disk / 2 queued-or-downloading / -1 unknown / -2 could not remove / -3 not deletable): drops a loaded copy first (whisper, LLM, Qwen caches — llama.cpp maps its file and Windows refuses to delete an open one), then removes the model, its `.part` + `.part.etag`, and on Mac the CoreML GPU alias + encoder bundle when no other quantisation shares it. Blocking; emits state `deleted`. Parakeet ORT keeps its session for the process, so a loaded Parakeet on Windows answers -2 until restart; a loaded Whistle or Nemotron engine library does the same. Qwen on the Neural Engine is -3: FluidAudio owns those folders.
- `with_emit` builds a center that reports to a caller's function instead of the host event channel; `dimmy-setup` (`setup_cli.rs`) uses it to print progress lines.
- Hosts: Win `Services/ModelDownloadCenter.cs` (fed by `AppViewModel.HandleEvent`), Providers page On-device rows. Mac: the `model_download` event in `DimmyCore.swift` / `AppState.swift`.

## `llm.rs` — post-processing router

- Two entry points: `process_text` (dictation enhancement — style + tone + translate, wraps the text in `[TRANSCRIPTION]` and applies `build_system_prompt`) and `process_raw_prompt` (command mode + meeting recap — sends the caller's prompt verbatim). Local mirrors live in `local_llm.rs`.
- Dispatches on `style` (Off, Correct, Summarize, Elaborate, Comprehensible, Professional, Prompt, Gen-Z, Boomer, Emoji, Acronyms, Imbruttito, Custom).
- Two HTTP wire formats: **Anthropic Messages API** when `Provider::from_url(api_url)` is Anthropic, and **OpenAI-compatible chat completions** for every other URL (Groq, OpenAI, OpenRouter, Gemini, Together, Fireworks, Kimi, Custom).
- Four **subscription (local CLI) branches** run before the HTTP path, in this order, in both entry points: Gemini CLI (`gemini-cli://` URL, or `auth_method == "subscription"` with a Gemini URL), Codex (`codex://`, or subscription with an OpenAI URL), Kimi Code (`kimi-cli://`, or subscription with a Kimi URL), Claude Code (any other `subscription`, or the legacy `claude-code://` URL).
- `llm_mode` config field routes to cloud or local (when `local-llm` feature is enabled).
- `emit_recap_stream_event` sends `llm_stream` events `{phase, delta, job_id, purpose}`; `StreamPurpose` (`text` / `recap` / `command`) is set by the FFI entry (`dimmy_llm_call_raw`, `dimmy_command_transform`).
- **PREAMBLE enforces "keep same language as the input"**. Small local models need this reinforcement or they default to English.
- **Translation directive** uses `lang_name(code)` → the English language NAME + an imperative ("Then translate the ENTIRE result into English…"). The old bare ISO code (`"…to en."`) was silently ignored even by capable models (Claude Haiku kept Italian). Codes accepted via `SUPPORTED_TRANSLATE_LANGS`.
- **`strip_output_scaffolding`** runs on cloud output: drops prompt scaffolding a weak model echoes (`[TRANSCRIPTION]`, `[SPOKEN]`/`[SELECTION]`, ChatML tokens) AND the whole `<think>…</think>` reasoning trace (qwen3 via Groq leaked it).
- **OpenAI gpt-5 / o-series** use `openai_reasoning_shape` → `max_completion_tokens` (no `temperature`) **floored at `.max(8192)`** in both `process_text` and `process_raw_prompt`. Without the floor the internal reasoning trace consumes the whole budget and the content comes back EMPTY.
- **Key resolution** (in `ffi.rs::load_key_any_scope`, not here): the dispatch reads the primary scope first (`KeyringScope::Llm(vendor)` for the LLM, `Stt(vendor)` for STT), then falls back to the SAME vendor's other scopes (`Stt`, `Llm`, `Recap`) — vendor is derived from the URL, so it can never pull a different provider's key. One key per provider works for STT + LLM + command.
- **Live verification:** [`core/tests/llm_flows.rs`](../../core/tests/llm_flows.rs) — the `#[ignore]` flow matrix + catalog sweep. See [`llm-flows-testing.md`](llm-flows-testing.md).

## `claude_code.rs` — Anthropic subscription LLM (local `claude` CLI)

- Routes LLM calls through the user's Claude Pro/Team/Max subscription instead of an API key, by shelling out to the locally-installed `claude` binary. A synthetic `claude-code://default` URL selects this path in `llm.rs`.
- `status()` → `Ready { binary_path }` / `NotLoggedIn { binary_path }` / `NotInstalled`. `spawn_login()` runs `claude /login` in a visible terminal (browser OAuth → `~/.claude/credentials.json`); `run_blocking(prompt, model, timeout, quick)` invokes `claude --print` with the prompt on **stdin** (no argv leakage, stderr captured separately). The caller sets the timeout: 60 s for a dictation rewrite, 600 s for a raw prompt (recap / command), 15 s for the ping. `quick` turns thinking down for short calls.
- `should_prewarm` / `prewarm(model)` start a `claude` process at dictation start so the rewrite does not pay the CLI start-up.
- **Privacy:** Dimmy never reads the credentials file; `ClaudeCodeError::Display` strips spawn/stdout text so transcript fragments can't leak into logs/telemetry.
- FFI: `dimmy_claude_code_status`, `_spawn_login`, `_ping`, `_recheck`, `_diagnostics`, `_binary_path`, `_node_status`. See [`claude-code-backend.md`](claude-code-backend.md).

## `codex.rs` — OpenAI/ChatGPT subscription LLM (local `codex` CLI)

- Sibling of `claude_code.rs` for ChatGPT Plus/Pro/Team: detects the `codex` CLI, `codex login` for OAuth, invokes `codex exec -` with the prompt on stdin. Synthetic `codex://default` URL selects it in `llm.rs`.
- Same status enum + privacy posture (stdin only, stderr captured, Display redacts spawn text). Credentials in `$CODEX_HOME` (`~/.codex/auth.json`).
- FFI: `dimmy_codex_status`, `_spawn_login`, `_ping`, `_recheck`, `_diagnostics`, `_binary_path`. See [`codex-backend.md`](codex-backend.md).

## `gemini_cli.rs` — Google account LLM (local `gemini` CLI)

- Third sibling of `claude_code.rs` and `codex.rs`: detects the `gemini` CLI, `spawn_login()`, `run_blocking(prompt, model, timeout)`. Credentials are cached by the CLI under `~/.gemini/`; Dimmy does not read them. Synthetic `gemini-cli://default` URL selects it in `llm.rs`.
- FFI: `dimmy_gemini_cli_status`, `_spawn_login`, `_ping`, `_recheck`, `_diagnostics`, `_binary_path`, `_last_error`.

## `kimi_cli.rs` + `kimi_mcp.rs` — Kimi Code membership

- `kimi_cli.rs`: LLM calls through the `kimi` binary over the Agent Client Protocol (`kimi acp`, ndjson JSON-RPC on stdio), because `kimi -p` takes the prompt from argv only. `run_blocking`, `list_models`, `spawn_login`, `install_command`, `git_bash_present`. Synthetic `kimi-cli://default` URL selects it in `llm.rs`. See [`kimi-backend.md`](kimi-backend.md).
- `kimi_mcp.rs`: registers `dimmy-mcp` in `~/.kimi-code/mcp.json` (`$KIMI_CODE_HOME`). It changes one key, `mcpServers.<ours>`; a file it cannot parse is left alone.
- FFI: `dimmy_kimi_status`, `_spawn_login`, `_ping`, `_recheck`, `_diagnostics`, `_binary_path`, `_install_command`, `_git_bash_present`, `_models_json`, and `dimmy_kimi_mcp_status` / `_install` / `_uninstall`.

## `cli_runner.rs` + `win_paths.rs` — running a local CLI safely

- `cli_runner.rs` runs a local CLI for `claude_code`, `codex`, `gemini_cli` and `kimi_cli`: stdin is written and stdout + stderr are drained on their own threads, the deadline holds, and a timeout kills the whole process tree (a Job Object on Windows, where an npm `.cmd` shim makes the child `cmd.exe`). Main functions: `spawn`, `feed_stdin`, `drain`, `run`, `wait_until`.
- `win_paths.rs` (Windows only): finds a CLI binary installed after Dimmy started, which the process's launch-time PATH cannot see. Used by `claude_code`, `codex`, `kimi_cli` and `setup_cli`.

## `claude_desktop.rs` — Claude Desktop MCP bridge

- Wires Dimmy into the Claude Desktop app as an **extension**: detects the install (Win MSIX/Squirrel, Mac `/Applications/Claude.app`), then `install_extension` copies the `dimmy-mcp` binary, a manifest and an icon into `<Claude data dir>/Claude Extensions/<id>/` and sets the enable flag under `Claude Extensions Settings/`. The id is `dimmy`, or `dimmy-<namespace>` for a side-by-side install.
- It no longer adds a `mcpServers.dimmy` entry to `claude_desktop_config.json`; install and uninstall remove a leftover entry from that era.
- `refresh_extension_if_stale` updates an installed copy that is older than the running app. It does not install for a user who never connected.
- The MCP server (`mcp-server/`) exposes 6 tools: `dimmy_get_recent_meetings`, `dimmy_search` (full-text search over meetings), `dimmy_get_meeting`, `dimmy_save_recap`, `dimmy_get_recent_dictations`, `dimmy_get_recap_template`.
- FFI: `dimmy_claude_desktop_status`, `_install`, `_refresh`, `_uninstall`.

## `catalog.rs` — embedded cloud-model catalog

- `include_str!`s `assets/model-catalog.json` (schema v2) into the binary as the single source of truth for cloud STT/LLM/recap providers + models. Win (System.Text.Json) and Mac (Codable) deserialize the SAME bytes → no per-OS model drift.
- Only cloud models. The catalog lists nine providers: `openai`, `anthropic`, `kimi`, `gemini`, `groq`, `deepgram`, `openrouter`, `together`, `fireworks`. The local lists are separate constants in the core: `local_stt::AVAILABLE_MODELS`, `local_llm::AVAILABLE_LLM_MODELS` and the `qwen_asr` catalog (FFI `dimmy_list_local_models`, `dimmy_list_llm_models`, `dimmy_qwen_asr_models_json`). Unit tests validate schema version + non-empty arrays + per-task endpoint URLs.
- `Provider::from_catalog_model_id` reads the same JSON to map a model id to its vendor.
- FFI: `dimmy_model_catalog_json`. Validate against live `/models` endpoints with `scripts/dev/check-model-ids.py` before a release.

## `notion.rs` — Notion integration

- REST client for the user's OWN internal Notion integration token (no Dimmy server in between). Sends a recap to a chosen page/database via `POST /v1/pages` with server-side markdown parsing.
- Token stored in the AES keystore under `KeyringScope::NotionToken`. API version pinned (`Notion-Version: 2026-03-11`), 30 s timeout; Notion allows ~3 req/s and the module does no automatic retry.
- `ping()` → `WorkspaceInfo`; `search()` → `NotionSearchResult { id, object, title, parent_label, url }`; `send_meeting_recap()` with page/database routing.
- FFI: `dimmy_notion_has_token`, `_set_token`, `_test_connection`, `_search`, `_send_recap`.

## `confluence.rs` — Confluence Cloud integration

- Sibling of `notion.rs`: sends a meeting recap to a Confluence wiki page. Auth is the user's email + API token over HTTP Basic, stored under `KeyringScope::ConfluenceToken`.
- `normalize_site`, `ping()` → `SiteInfo`, `spaces()` (destination picker), `md_to_wiki` (markdown → wiki markup), `send_meeting_recap()`. 30 s timeout.
- FFI: `dimmy_confluence_has_token`, `_set_token`, `_test_connection`, `_spaces`, `_send_recap`.

## `calendar.rs` — which appointment a meeting was

- Gives a recap its roster. Ranks calendar events against the recording's time span (`rank_candidates`, `rank_in_progress`, `rank_for_meeting`), builds the attendee text for the recap prompt (`roster_for_prompt`), and stores the choice in the meeting folder (`save_candidates`, `save_assignment`, `load_assignment`). It holds no credential: `probe_connector` and `fetch_day` ask the user's own `claude` CLI, which carries the claude.ai calendar connectors, for the day's events as JSON (`FETCH_TIMEOUT_SECS` = 120). It works only on the Claude subscription backend; every entry point degrades to "no context", not to an error. Attendee data never goes to telemetry.
- FFI: `dimmy_calendar_status`, `_spawn_setup`, `_candidates`, `_pending`, `_assign`, `_assignment`. Win: `MeetingWindow.Calendar.cs`, `SettingsWindow.Calendar.cs`.

## `telegram.rs` — Telegram inbox source

- Logs into the user's own Telegram account with `grammers` (MTProto) and downloads audio the user shared to Saved Messages. Its job ends at a local file path; the host runs the normal file-load pipeline on it.
- The client code needs the `telegram` cargo feature (`is_compiled()`); the release workflows for Windows and macOS pass it. Runtime is also gated by the `telegram_enabled` config field.
- FFI: `dimmy_telegram_status`, `_set_enabled`, `_start_login`, `_start_qr_login`, `_submit_code`, `_submit_password`, `_cancel_login`, `_logout`, `_pending`, `_process`, `_mark_processed`, `_dismiss`, `_reply`.

## `call_detector.rs` — meeting auto-detection state machine

- Pure state machine (no threads/IO): the host polls audio-session state and feeds signals; the core decides whether to nudge "start a meeting?" and when to suggest stopping. App-agnostic (a browser call works like Teams/Zoom).
- Debounce + per-app cooldown + exclusion list + a silence-based stop-suggestion (`has_tracked_origin` gates it to a watched pid to stop flapping). Emits each transition exactly once.
- FFI: `dimmy_call_signal`, `_signal_sys`, `_signal_session_ended`, `_signal_response`, `_set_tracked_origin`, `_meeting_started_external`, `_mic_confirmed_free`, `_detector_state`. Win: `CallDetectionService.cs` + `CallNudgeWindow`. Mac: `CallDetectionManager`.

## `call_media.rs` — is the call still connected

- Tells a hang-up from a headset change before an auto-recorded meeting is ended: a headset change leaves the call's media sockets (RTP over connected UDP) open, a hang-up closes them. `live_media_sockets(pid)` counts them (`count_media_sockets` parses `lsof` output, ignoring ports 53, 443, 5353); `end_verdict(...)` returns an `EndVerdict`; `NO_EVIDENCE_GRACE_SECS` = 90.
- FFI: `dimmy_call_live_media`, `dimmy_call_end_verdict`. Written for the Mac host (see the module header); Win has `Services/CallMediaSockets.cs`.

## `consent.rs` — recording-consent gate

- One cross-platform source for what the recording-consent notice SAYS and THAT it happened. `modal_text(lang)` + `announcement_text(lang, cloud_processing)` localize to it/es/fr/de/pt (+ English fallback); the announcement discloses local-only vs cloud processing (a material GDPR fact). `cloud_processing(...)` decides that flag from the STT, LLM and recap settings.
- The spoken announcement has `ANNOUNCEMENT_VARIANTS` = 3 wordings; `announcement_audio` returns the matching pre-recorded MP3 compiled in from `core/assets/consent/`.
- `append_event(kind, lang)` writes an append-only `<config_dir>/consent.jsonl` audit trail (best-effort, never blocks recording); `has_event(kind)` reads it. The host decides WHEN to show the notice.
- FFI: `dimmy_consent_text`, `dimmy_consent_announcement`, `dimmy_consent_audio`, `dimmy_consent_audio_muted`, `dimmy_consent_log_event`, `dimmy_consent_has`.

## `deepgram_stream.rs` — streaming dictation (Deepgram WebSocket)

- True realtime STT (vs `chunked_stt.rs`'s chunked approximation): one persistent Deepgram WS, PCM16 LE 16 kHz pushed as cpal captures it, interim+final results streamed back.
- Reuses the chunked callback contract `(delta, cumulative, is_final)` so the host injects `delta` at the cursor and shows `cumulative` as a live caption. TLS via `tokio-tungstenite` + `native-tls` (rustls 0.23+ panics under Velopack).
- **Engine choice** (in `dimmy_start_recording`, when `streaming_dictation` is on): cloud STT with an OpenAI URL and an OpenAI STT key → `openai_stream`; any other cloud provider with a saved Deepgram key → this module; local STT with the Nemotron backend → `nemotron::Streamer`; other local backends → the chunked path in `chunked_stt.rs`.
- No new public FFI — driven by the `STREAMING` static (`ActiveStreamer::{Deepgram, OpenAi, Nemotron}`) in `ffi.rs`, emits `stt_chunk` events.

## `openai_stream.rs` + `stream_bounds.rs` — streaming dictation (OpenAI Realtime)

- `openai_stream.rs`: sibling of `deepgram_stream.rs` over OpenAI's Realtime WebSocket (`wss://api.openai.com/v1/realtime?intent=transcription`), same `(delta, cumulative, is_final)` contract. It sends 24 kHz audio. `OpenAiStreamer::start` / `stop`, `compose_session_update`, `parse_oai_message`.
- `stream_bounds.rs`: the time limits both cloud streamers share, so Stop returns when the network does not: `CONNECT_TIMEOUT` 10 s, `SEND_TIMEOUT` 5 s, `STOP_JOIN_TIMEOUT` 10 s, `LOCAL_STOP_JOIN_TIMEOUT` 30 s (used by `chunked_stt` and `nemotron`). Functions `connect`, `send`, `join_worker`.

## `dfn3.rs` — DeepFilterNet v3 noise suppression

- Sibling of `dfn.rs`: `Dfn3Processor` wraps the `deep_filter` crate (DFN3 model + tract ONNX; the dependency is the `KonradDallaOrg/DeepFilterNet` fork, branch `fix/tract-ndarray-port`). Gated on the `local-dfn` cargo feature; `process_mono`/`process_frame` operate on 10 ms hops (480 @ 48 kHz).
- `local-dfn` is not in the default feature set. The macOS build lines of all three release workflows and the Windows line of `staging-tester.yml` pass it, so the module is compiled there. No capture or transcription path calls it; its only caller is the compare tool `core/src/bin/denoise_offline.rs`.

## `keystore.rs` — API key storage

- **Always uses local AES-256-CTR + HMAC-SHA256 encrypted file** `keys.enc` in the config dir: `~/.config/dimmy/` (Linux), `~/Library/Application Support/dimmy/` (macOS), `%APPDATA%\dimmy\` (Windows).
- Key derivation: `SHA-256(username + ":" + hostname + ":dimmy-local-key-v1")`. Machine-specific — copying `keys.enc` to another machine yields unreadable blob.
- **No OS popups, no admin prompts, no keyring prompts.** The `use_keyring` config field is forced to `false` in the core; the toggle is removed from all platform UIs.
- OS keyring (macOS Keychain, Windows Credential Manager, Secret Service on Linux) is kept as **read-only fallback** for migrating users from the pre-v0.4 era.

## `history.rs` — transcription history

- SQLite + FTS5 virtual table for full-text search.
- DB file: `history.db` in the config dir (`crate::config_dir_path()`): `~/.config/dimmy/` (Linux), `~/Library/Application Support/dimmy/` (macOS), `%APPDATA%\dimmy\` (Windows).
- v2 schema (`save_v2`, added by an idempotent migration): enhanced text, audio path, app process, word timestamps. The FTS5 index covers raw + enhanced text. `prune_audio_dir` applies the audio retention.
- Auto-saves after every successful transcription. No opt-out by design (the feature is the feature).
- FFI: `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`, `dimmy_history_update_enhanced`, `dimmy_history_update_audio`, `dimmy_history_update_word_timestamps`.

## `filler.rs` — filler word removal

- Post-transcription pass. Regex with word-boundary matching, case-insensitive.
- 6 languages: Italian (`ehm`, `cioè`, `allora`), English (`um`, `uh`, `like`, `basically`), Spanish, French, German, Portuguese.
- Applied to both local and cloud transcriptions when `filler_removal_enabled: true`.
- Runs BEFORE the LLM post-processing stage, so the LLM sees clean text.

## `audio.rs` — mic + loopback capture

- `cpal` for cross-platform audio input. The shared buffers hold mono f32 at `MEETING_CANONICAL_RATE` (48 kHz); the cpal callback resamples from the device's native rate.
- `AudioSource { Mic, System, Mix }`: dictation starts capture with `Mic`, a meeting with `Mix` (mic + system loopback, echo-cancelled by `aec.rs`). On macOS the Swift host captures system audio and pushes it through `dimmy_push_loopback_audio`.
- The audio thread takes `AudioCommand` (`Start { device_name, source }`, `Stop`, `PushLoopback`, `DevicesChanged`) over an mpsc channel (`AppState.audio_tx`); `ffi.rs` sends them.
- `ProcessedAudio::split_at_silence` / `first_silence_split` (used by the cloud chunking in `transcribe.rs`) and `encode_wav` live here.
- Dictation guardrail: `DICTATION_SOFT_WARN_SECS` = 300 and `DICTATION_HARD_STOP_SECS` = 600 of buffered audio.
- Device follow (`loopback_target`, `mic_decision`, 5 s heartbeat, `DEVICE_SETTLE_MS`): see `device_events.rs` and `capture_sessions.rs` below.
- Safe shutdown: `dimmy_shutdown()` sends `Stop`, waits 50 ms, then clears the whisper and LLM model caches and saves the config.
- **Audio health check** at startup: `dimmy_check_audio_health()` opens a short probe stream on the default device before the first real recording. If the mic is broken / missing / muted, the UI surfaces the failure immediately instead of showing the pill and silently transcribing nothing.

## `preprocess.rs` — VAD, AGC, highpass, clamp

Full reference: [`audio-pipeline.md`](audio-pipeline.md). Headline:

- Full pipeline: clamp → 80 Hz highpass (biquad) → VAD (nnnoiseless) → AGC (dagc) → clamp.
- **Route** (`preprocess_route(preprocessing_enabled, stt_mode)`): `Raw` when preprocessing is off; `Full` for local STT, run through `process_buffer_guarded` (falls back when `preprocess_made_it_worse`); `HighpassOnly` for cloud STT.
- **Never feed zero-amplitude samples to dagc** — it produces all-NaN permanently. The VAD grace period delays the `in_speech → false` transition but must NOT emit silence frames. (AUDIO-001.)
- `process_buffer()` calls `process()` ONCE for the full recording. The whole buffer goes through a single VAD+AGC pass.
- File load uses `process_buffer_for_file_load*` / `FileLoadHighpass` (highpass only). Realtime chunks use `process_chunk_vad_only` (VAD trim, no AGC) with `chunk_gate_fallback`; the speech decision comes from `silero.rs` when its model is present.
- Resampling: `downsample_to_16k`, `downsample_to`, `StreamingDownsampler16k`, `StreamingDownsampler`.

## `hotkey.rs` — global hotkey

- Platform-specific. On Windows: low-level keyboard hook (`SetWindowsHookExW`, `WH_KEYBOARD_LL`). On macOS: `CGEventTap` (listen-only). On Linux the core has a no-op stub; the GTK host has its own `platforms/linux/src/hotkey.rs` (xdg-desktop-portal GlobalShortcuts on Wayland, X11 `XGrabKey` with the `x11-fallback` feature).
- Three bindings run on the same hook: dictation, command mode and meeting (`Which::{Dictation, Command, Meeting}`). A combo is two modifier groups, or modifiers plus one key.
- FFI: `dimmy_hotkey_install`, `dimmy_hotkey_set`, `dimmy_hotkey_set_command`, `dimmy_hotkey_set_meeting`, `dimmy_hotkey_combos_conflict` (plus the four recorder entries below).
- **macOS symbol gotchas:** no `objc_msgSend` or `kCFTypeDictionaryKeyCallBacks` declaration is in `core/src` today. The rules for adding one (never declare `objc_msgSend` variadic; `kCFTypeDictionaryKeyCallBacks` as `static ... : [u8; 0]`) are in `known-bugs.md` MACOS-001/002.
- **macOS framework links** must be explicit in `hotkey.rs`: CoreGraphics + CoreFoundation.

## `hotkey_recorder.rs` — recording a new shortcut

- Behind the "set shortcut" dialog (Win `ShortcutEditor` / Mac `ShortcutRecorderSheet`), used by Settings, the setup wizards and onboarding alike. FFI: `dimmy_hotkey_recorder_start(target, current_json)` / `_stop` / `_is_active` / `_key`. Each key-down that changes the held keys emits ONE `hotkey_recorder` event: keys to draw, combo in the core grammar, `can_save`, `error`, `warnings`.
- **Who feeds it:** Windows, the keyboard hook in `hotkey.rs`, which swallows the keys while the recorder is open AND a Dimmy window is in front (so Win+... never reaches the shell and the current shortcuts can't fire; a dialog left behind another app never eats typing). macOS, the sheet's `NSEvent` local monitors via `dimmy_hotkey_recorder_key`; the Swift event taps step aside while `dimmy_hotkey_recorder_is_active() == 1`.
- **The rules live here, once:** target-aware validity (the dictionary hotkey needs a real key; two modifiers alone are fine on the hook; Windows binds at most two modifier groups; Fn only for macOS dictation, without a key), conflicts against the other three shortcuts (`combos_conflict`, subset rule), and warnings: taken by the system (Windows: a `RegisterHotKey` probe; macOS: `CopySymbolicHotKeys`), Ctrl+Alt = AltGr (Windows) / Option+key types characters (macOS).
- Tab, Enter and Esc on their own pass through to the dialog. The verdict is computed on a worker thread, never inside the hook: Windows silently removes a slow low-level hook.
- `hotkey::install` is idempotent: the Windows dialog calls it, because during onboarding it opens before the hotkey service exists.

## `ffi.rs` — the C surface

- 230 `extern "C" fn dimmy_*` entries in `ffi.rs` at v0.7.15. Two are gated: `dimmy_inject_pcm_for_test` (`test-ffi`) and `dimmy_license_set_server_url` (debug builds or `test-ffi`).
- The golden list `core/tests/fixtures/abi_exports.txt` holds 221 symbols and is compared by `core/tests/abi_snapshot.rs`. No workflow runs that test (`e2e-tests.yml` excludes it), and the fixture is behind the code: it lacks `dimmy_consent_has`, `dimmy_meeting_import`, `dimmy_meeting_recover_interrupted`, `dimmy_meeting_set_recap_status`, `dimmy_meeting_set_title`, `dimmy_onboarding_done`, `dimmy_onboarding_set_done`.
- Global `OnceLock<AppState>` (`GLOBAL_STATE`) owns the process-wide singleton. The
  `MEETING` static (separate slot) owns the meeting session so its
  lifecycle is decoupled from any UI window. Other statics: `EVENT_CALLBACK`,
  `CHUNKED`, `STREAMING`, `DICTATION_GEN`, `CALL_DETECTOR`.
- Functions check their arguments at entry: a null pointer or invalid UTF-8 returns an error code.
- All JSON blobs are validated on entry (malformed JSON returns an error, does not panic).
- String outputs (`write_to_buf`): caller allocates the buffer and passes its size; the function writes UTF-8 + NUL and returns the bytes written. A null or zero-length buffer returns `-1`. A buffer that is too small gets the text cut at a character boundary; there is no `_size()` function. The embedded-text entries (`dimmy_model_catalog_json`, `dimmy_license_text`, `dimmy_third_party_notices`, `dimmy_legal_document`; helper `write_static_text`) return the TOTAL byte length whatever the buffer is, so a host calls once with a null buffer to size and again to read. A few entries document their own "buffer too small" code (for example `-2` or `-3`).
- Events go to the host through `emit_event(name, payload_json)` → the callback set by `dimmy_set_event_callback`, as `{"event": ..., "payload": ...}`.
- **Return-code conventions** vary by family. The notable ones a UI
  must handle exactly:
  - `dimmy_start_recording` returns 0, **-1** (no API key), **-2**
    (already recording), **-3** (lock poisoned), **-7** when a meeting
    is active (silent no-op — must not surface as an error).
  - `dimmy_meeting_start` returns 0, **-1** (session already active),
    **-2** (audio capture failed), **-3** (filesystem error), **-4**
    (a dictation is recording).
  - `dimmy_meeting_pause` / `_resume` return **1**
    (state flipped), **0** (no-op / no meeting), **-1** (lock
    failure). `_is_paused` returns 1 or 0.
  - `dimmy_transcribe_file` returns the transcript length, or: **-1**
    invalid arguments, **-2** file open / decode failure or zero
    samples, **-5** local backend failed, **-6** cloud config
    incomplete (url / key / model), **-7** runtime could not start,
    **-8** cloud transcription failed. The code no longer returns -3
    or -4 (its doc comment still lists them). The rc table is
    contractual — do NOT renumber.

## `error.rs` — typed error hierarchy

- Central `DimmyError` enum with variants `Audio(AudioError)`, `Transcribe(TranscribeError)`, `Llm(LlmError)`, `Config`, `InvalidState`, `Platform`. `TranscribeError`: `NoApiKey`, `Api { status, body }`, `Empty`, `Network`, `InsecureUrl`, `LocalModel`. `LlmError` adds `Refusal` and `Truncated`.
- Provider error bodies are cut to 200 bytes with `crate::truncate_utf8` where the error is built (`transcribe.rs`, `llm.rs`, `download.rs`, `ffi.rs`) — prevents leaking API keys, PII, or oversized provider responses into logs.
- Display impl is the canonical user-facing string. Debug impl is for logs.

## `lib.rs` — config, state, shared helpers

- Module declarations, the config struct and its defaults / JSON load and save (the only writer of `config.json`), `AppState`, and helpers used by every module: `config_dir_name()` (build-time `DIMMY_CONFIG_NAMESPACE`, default `dimmy`), `config_dir_path()`, `set_config_dir_override` (for `dimmy-setup --config-dir`), `write_atomic`, `truncate_utf8`, `log`, the onboarding marker (`.onboarding_done`), and the compiled-in `LICENSE_TEXT` / `THIRD_PARTY_NOTICES`.
- Under `cfg(test)` or the `test-ffi` feature, `config_dir_path()` never returns the real user dir: it uses `DIMMY_TEST_CONFIG_DIR` or a per-process temp dir.
- `main.rs` is a stub that prints "This binary is not used" and exits 1; the hosts load the library.

## `meeting.rs` — long-form meeting mode

- `MeetingSession::start(...)` spawns **two threads** (THE AUDIO RULE, see
  [`meeting-audio-durability.md`](meeting-audio-durability.md)):
  - `worker_loop` (capture): reads the shared buffers, writes the track
    sinks and hands each window to the transcriber through a bounded
    queue with `try_send` (`STT_QUEUE_DEPTH` = 4; 120 for Nemotron). It
    never waits; a full queue drops the window and the host gets one
    `meeting_transcription_behind`.
  - `stt_thread_loop` (transcription): owns STT, `transcripts.txt` and
    the host events (`meeting_chunk`, `meeting_partial`).
- Window length: default 15 s (`DEFAULT_CHUNK_SECS`), configurable via
  `meeting_chunk_secs` (clamped to 5-60 s), 500 ms overlap. Nemotron uses
  0.5 s windows (`NEMOTRON_WINDOW_SECS`) because it streams.
- **Audio tracks** (`TrackSink`), at the capture rate (48 kHz), not
  16 kHz: `audio` (mix of mic + system), `audio_mic` (echo-cancelled
  mic), `audio_system` (raw loopback, Mix mode only). Each is
  Ogg/Vorbis (`.ogg`, quality 0.8) on Windows and macOS, with a
  fallback to int16 WAV (`.wav`) when the encoder cannot start; Linux
  writes WAV. Meetings from older builds have `.wav`.
- On-disk layout per meeting (`<config>/meetings/<id>/`): the three
  tracks, `transcripts.txt`, `meta.json`, `speakers.json` (when speakers
  are labeled, see `diarize.rs`), `recap.md` + `actions.json`
  (post-stop), `.recording` marker (deleted on clean stop; presence at
  startup → "recover meeting?"). UUIDs avoid same-second collisions.
- **STT routing**: `SttSnapshot`, taken at start, sends chunks to the
  SAME backend the dictation pipeline would use — cloud
  (`transcribe.rs`) or local (whisper / Parakeet / Qwen / Whistle /
  Nemotron). Hardcoding Parakeet here was the 2026-05-07 bug that broke
  meeting transcripts on builds without `local-stt-parakeet`.
- **Pause/resume**: `MeetingSession::pause()` / `.resume()` flip an
  `Arc<AtomicBool>` and `audio::MEETING_CAPTURE_GATED`, so every capture
  append is skipped while paused (the streams stay open). On resume the
  worker moves its cursors past the paused window, so it is excluded
  from the tracks AND from the chunked timeline. A
  `[paused] (resumed after ...)` line lands in `transcripts.txt`.
  Idempotent (second `pause()` returns false).
- **Stop**: the sinks are finalized before the transcriber is joined;
  `stop()` joins the worker with a 120 s bound (`join_bounded`).
- **Post-process**: the UI-side `MeetingPostProcessService` (Win + Mac
  mirrors) reads `transcripts.txt`, formats the 11-section
  structured-recap prompt and calls `dimmy_llm_call_raw`; `ffi.rs`
  resolves `recap_model_override` and calls `llm::process_raw_prompt`.
  The host then calls `dimmy_meeting_save_post_process`
  (`save_post_process`), which writes `recap.md` (with the AI-generated
  marker, `mark_ai_generated`) + `actions.json` and sets the title from
  the recap's first H1. Anthropic Opus 4.7+ / Sonnet 5+ use
  `thinking.type=adaptive` (no `budget_tokens`).
- **Import**: `import_from_file` turns an audio file into a meeting
  folder of the same shape (`dimmy_meeting_import`).
- FFI: `dimmy_meeting_start`, `_stop`, `_save_post_process`,
  `_list_orphans`, `_is_active`, `_active_dir`, `_pause`, `_resume`,
  `_is_paused`, `_set_title`, `_set_recap_status`,
  `_recover_interrupted`, `_import`, `_retranscribe`, `_rename_speaker`,
  and `dimmy_meetings_dir`. `_start` returns -4 while a dictation holds
  the capture.
- **`meta.json` (schema 2)** has one writer, `meeting::update_meta`
  (read, change, `write_atomic`). Written at start: `schema`, `stage`,
  `id`, `started_at`, `device_sample_rate`, `chunk_secs`, `overlap_ms`.
  Core-owned fields: `schema`, `stage`
  (`recording` → `stopped` → `recapped`, or `interrupted` after a crash),
  `transcript` {status `ok|silent|failed|empty|unknown`, engine, reason,
  windows_text/silent/failed}, `recap` {status, at}. Hosts rename through
  `dimmy_meeting_set_title`. A folder without `schema` is from an older
  build: derive its stage from its files and treat its transcript as
  unknown, never as silence.

## `capture.rs` — who owns the shared capture buffers

- `Owner { Idle, Dictation{id}, Meeting{id}, MeetingDraining{id} }`, one
  `apply()` (transition table test), one lock. The meeting worker reads its
  claim through `meeting_owns(id)`, a single atomic load: THE AUDIO RULE
  holds.
- A dictation is refused (-7) while a meeting records or drains; a meeting
  is refused (-4) under a dictation; a new meeting may claim from
  `MeetingDraining`. The meeting worker calls `meeting_released` right
  after its audio is final, before the transcriber join, and that release
  frees the buffers under the lock, so a later dictation is never wiped.
- `dimmy_start_recording` / `_stop_recording` emit `dictation_phase`
  {phase: `recording|finishing|idle`, id}: the hosts' pill rests on it.

## `transcriber_health.rs` — is a meeting's transcriber producing text

- `Health { Live, Unavailable(Reason) }`, owned by `stt_thread_loop`
  (never the capture worker). An engine that cannot start (Nemotron
  refused for memory) or two failed windows in a row → one
  `meeting_transcription_unavailable` {dir, engine, reason, elapsed_secs};
  text afterwards → one `meeting_transcription_recovered`.
- Reason codes (`not_enough_memory`, `model_missing`, `no_api_key`, `auth`,
  `network`, `rate_limit`, `engine_error`) are what hosts turn into
  sentences (Win `Helpers/TranscriptHealthText.cs`, Mac
  `TranscriptHealthText` in DimmyCore.swift). The user's engine choice
  stands: no automatic fallback to another engine.

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

## `dfn.rs` — RNNoise denoiser (not in the capture path)

- `DfnProcessor` wraps `nnnoiseless` (the RNNoise port), 480-sample
  frames at 48 kHz, with the `DENOISE_ENABLED` switch. The module is
  always compiled; it has no cargo feature.
- It ran upstream of AEC3 in the `aec.rs` worker until 2026-08-10, when
  that stage was removed (see the comment in `aec.rs`: one suppressor,
  GTCRN, on the 16 kHz STT input, and the archived audio stays
  unfiltered). No capture or transcription path calls it now; its only
  caller is the compare tool `core/src/bin/denoise_offline.rs`.
- The DeepFilterNet backend is `dfn3.rs` (feature `local-dfn`).

## `device_events.rs` — wake the audio thread on device events

- Device follow (default in/out, device list, sample rate, which app holds
  a mic) reacts to OS events instead of a timer. Windows:
  `IMMNotificationClient` (default change, add/remove, state, and
  `PKEY_AudioEngine_DeviceFormat` only — other endpoint properties churn)
  plus the microphone privacy registry key (`RegNotifyChangeKeyValue`, the
  same source as `MicUsageWatcher.cs`; any app, packaged or not). macOS:
  CoreAudio listeners on the default input/output, the device list and
  the default input's nominal sample rate. Linux: none.
- The callback only sends `AudioCommand::DevicesChanged`; the audio
  thread reads and decides. Watching exists only while recording
  (registered at each Start, dropped at Stop).
- The heartbeat is the net: 5 s, 1 s while a change is settling (tests:
  1 s). A new device/rate must hold `DEVICE_SETTLE_MS` (900 ms, a
  DURATION — events can arrive ms apart) before streams are rebuilt: the
  macOS 26 HAL-wedge guard.
- The mic callback sends one `DevicesChanged` when a run of digital zeros
  crosses 4 s (no OS event exists for that).

## `capture_sessions.rs` — who is on a call, on which devices (Win-only)

- "The call" = any process other than Dimmy with an ACTIVE capture
  session (no app list). `call_devices()` returns the inputs those
  processes capture from and the outputs they, or a relative in their
  process tree (ToolHelp snapshot; shells like explorer/svchost excluded),
  play to. Measured: a process holding the Realtek mic and playing to the
  Realtek speakers came back as exactly that, and nothing once it hung up.
- `audio.rs::loopback_target`: the endpoint loopback moves to the call's
  output when it is a single device other than the bound one (Teams' own
  speaker menu, 2026-11-02); otherwise it follows the system default.
- `audio.rs::mic_decision` reads the inputs when our mic goes to digital
  zeros.
- Why not process loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`):
  tried 2026-11-02 and REMOVED. It caught other apps' audio on any output,
  but a live Teams call came through as digital zeros (also targeting the
  Teams process tree), while endpoint loopback recorded it at the same
  moment.

## `chunked_stt.rs` — realtime chunked transcription worker (backend-agnostic)

- `ChunkedTranscriber::start(...)` spawns a worker that, while the audio
  capture thread fills the shared PCM buffer, periodically slices off
  the most recent N seconds (+ overlap), runs them through a
  caller-supplied `TranscribeFn` (Parakeet, whisper, Qwen or Whistle;
  `ffi.rs` builds it), dedups against the running cumulative text
  (`dedup_last_3_words`, which matches an overlap of 2 to 6 words), and
  emits a callback so the FFI layer can fan out `stt_chunk` events to
  the native UI. Nemotron does not use this worker: it streams
  (`nemotron::Streamer`).
- Chunk length comes from `chunk_secs_for_backend(backend,
  visible_output)`: 5 s for Parakeet and Whistle when the text is shown
  live, 15 s for the other backends and whenever nothing is shown until
  the end. The 30 s window with 500 ms overlap is the offline benchmark
  setting: 8.7× realtime over 272 min of LibriVox audio on WSL CPU,
  measured for `docs/dev/stt-benchmark-parakeet-local-2026-05-05.md`.
- Worker downsamples to 16 kHz per chunk. The only per-chunk
  preprocessing is optional: with `vad_trim` (the caller passes
  `preprocess_route(..) == Full`) each window goes through
  `preprocess::process_chunk_vad_only`. There is no highpass or AGC per
  chunk.

## `parakeet.rs` — Parakeet TDT v3 FP32 local STT (ONNX Runtime)

- Pure-Rust port of `onnx_asr.models.nemo.NemoConformerTdt`. Bundle:
  `nemo128.onnx` (waveform → 128-bin mel) + `encoder-model.onnx` +
  external `.data` (~2.4 GB) + `decoder_joint-model.onnx` + `vocab.txt`
  (8193 tokens BPE-style with `▁` word marker), downloaded from
  `istupakov/parakeet-tdt-0.6b-v3-onnx` (`resolve/main`) to
  `<config>/parakeet-fp32/` (~2.5 GB, `BUNDLE_SIZE_MB` = 2500) by
  `download_bundle`, one `download::download_resumable` call per file.
- `transcribe`, `transcribe_with_word_timestamps`, `warmup`,
  `active_bundle_present`, `download_active_bundle` are the entry points
  for both backends: on macOS arm64 with `local-stt-parakeet-fluid` they
  use `parakeet_fluid` when its bundle is on disk, and the ONNX path
  otherwise.
- The ONNX path cuts long audio into windows of at most
  `MAX_ORT_WINDOW_SECS` = 120 s (the encoder fails on a 10-minute input;
  see the constant's comment).
- Pipeline: 16 kHz f32 PCM → mel features `[1, 128, T_mel]` → encoder
  `[1, 1024, T_enc] + lens` → greedy TDT (LSTM state `[2, 1, 640]` × 2;
  per-frame argmax of token + duration) → vocab → text.
- Feature gates: `local-stt-parakeet` (ONNX Runtime on CPU; in the
  Windows release feature line), `local-stt-parakeet-cuda` (`ort/cuda`),
  `local-stt-parakeet-coreml` (`ort/coreml`; in the macOS release
  feature line, but the ONNX path stays on the CPU on Mac — see
  `parakeet_fluid.rs` for the production Mac path). Without
  `local-stt-parakeet` the ONNX functions return an error; the bundle
  download and presence helpers are always compiled.
- Word-level timestamps via `transcribe_with_word_timestamps` for the
  history-v2 schema. The FluidAudio path returns `"[]"` (no timestamps).
- FFI: `dimmy_parakeet_bundle_present`, `_download_bundle`, `_warmup`,
  `_transcribe`.
- **Lifecycle gotcha**: `Box::leak`s the `Mutex<Option<Inner>>` so
  Rust's static-destructor pass never drops the cached ort `Session`s
  — drops at process exit hit a torn-down onnxruntime mutex and abort.
  See `known-bugs.md` STT-002.

## `parakeet_fluid.rs` — Parakeet via FluidAudio CoreML (Mac ANE)

- Wraps `fluidaudio-rs` (Swift bridge built by `build.rs` at compile
  time; needs Xcode CLT with Swift 5.10+) to run Parakeet on the Apple
  Neural Engine. Documented RTF on M-series: 100–300×; first
  `init_asr()` downloads the CoreML bundle and ANE-compiles it
  (~20–30 s one-time); warm reloads ~1 s. The bundle lives in
  `~/Library/Application Support/FluidAudio/Models/parakeet-tdt-0.6b-v3-coreml/`
  (`parakeet_bundle_dir()`), and `BUNDLE_SIZE_MB` = 466 is its measured
  size on disk. (The module header and `Cargo.toml` still say ~3 GB and
  `~/.cache/fluidaudio/`.)
- Gated by `local-stt-parakeet-fluid` feature. The macOS release
  feature line compiles it together with the ONNX path
  (`local-stt-parakeet-coreml`). Callers (`transcribe.rs`, `ffi.rs`)
  call `parakeet::transcribe`, which uses this module when
  `bundle_present()` is true and the ONNX path otherwise.
- A recording under 1 s (`ENGINE_MIN_SAMPLES`) is padded with silence
  to 1.5 s (`PADDED_SAMPLES`), because FluidAudio rejects shorter input.
- Only available on `aarch64-apple-darwin` — `#![cfg(...)]` at module
  top so non-Mac / Intel-Mac builds skip the file entirely.
- `transcribe_samples` only landed upstream after FluidAudio v0.12.6,
  so the bridge writes a temp WAV per call (~320 KB I/O for a 5 s
  chunk, negligible vs ANE inference time).

## `nemotron.rs` — Nemotron 3.5 ASR Streaming (fifth local STT backend)

Real-streaming local STT through NeMo-Speech.cpp's C ABI (`asr.h` v1), loaded in-process with `libloading` from a project-built library (ggml static). Model, diarizer and engine archives are downloaded and SHA-256-pinned here, like Whistle's engine.
- `app_recognizer(speakers)` / `default_spec(speakers)`: GPU build when the hardware is there, CPU build as fallback; two cached recognizers (with / without the diarizer, since a loaded diarizer runs on every stream).
- `Streamer` (dictation, one never-reset stream, whole words only), `Transcript::apply` (corrections as `backspace` within the last 8 words), `MeetingTrack` (fed by `meeting.rs` in 0.5 s windows, `NEMOTRON_WINDOW_SECS`; speaker turns via `turns`; `drop_speakers` when the load measured in `meeting.rs` passes its limit, 0.9 on a CPU and 1.0 on a GPU; `speaker_list` for `speakers.json`).
- Pinned downloads: the Q8_0 model (`nvidia/nemotron-3.5-asr-streaming-0.6b` @ `ea30d66`, 742 MB), the diarizer (`nvidia/Nemotron-3-Diarization` @ `f667ed7`, 107 MB), the optional F32 model (`KonradDallaOrg/dimmy-nemotron-3.5-asr-gguf` @ `76599da`, 2.58 GB, GPU builds only) and the engine archives from the GitHub release `engine-nemotron-66d50d9` (Windows and Linux x86_64: cpu + cuda; macOS arm64: metal).
- FFI: `dimmy_nemotron_status` (1 / 0 / -1 no engine here), `_size_mb`, `_download` (blocking, `nemotron_download_progress`), `_full_status`, `_full_size_mb`, `_full_download`, `_restart_needed`.
- Full write-up, measurements and engine rebuild steps: [`nemotron.md`](nemotron.md).

## `qwen_asr.rs` + `qwen_fluid.rs` — Qwen3-ASR local STT (`local_stt_backend = "qwen"`)

- `qwen_asr.rs`: Qwen3-ASR through llama.cpp's multimodal path (mtmd); inference needs the `local-stt-qwen` cargo feature. The catalog has four variants: 0.6B and 1.7B as GGUF Q8_0 + mmproj (`ggml-org/Qwen3-ASR-*-GGUF`, `Runtime::LlamaCpp`) and 0.6B int8 / f32 on the Apple Neural Engine (`Runtime::NeuralEngine`). `find`, `bundle_present`, `download_bundle`, `transcribe(pcm, variant, language)`, `clear_model_cache`, `strip_asr_scaffolding`.
- `qwen_fluid.rs`: the Neural Engine variants through FluidAudio's CoreML pipeline. `supported()` checks macOS 15 at runtime; it is false on other platforms, and the catalog then hides those variants.
- FFI: `dimmy_qwen_asr_models_json`, `_bundle_present`, `_download`. Download-center ids `qwen:<file>`. See [`qwen3-asr-2026-09-09.md`](qwen3-asr-2026-09-09.md).

## `whistle.rs` — Whistle local STT (`local_stt_backend = "whistle"`)

- Cactus Compute's 16.9 MB CPU-only model (`whistle.cact`). The engine is a shared library (`needle` C API) taken out of the Cactus Python wheel for the platform and opened with `libloading`. Model and wheel are pinned to a revision and a SHA-256 in this file. No cargo feature.
- `LANGUAGES`: en, de, fr, es, it, nl, pl. Audio is cut into chunks of 16 to 24 s (`MIN_CHUNK`, `MAX_CHUNK`). `engine_available`, `bundle_present`, `download_bundle`, `transcribe(pcm, language, keywords)`, `transcribe_words`.
- FFI: `dimmy_whistle_status`, `_size_mb`, `_download`. Download-center id `whistle`.

## `diarize.rs` — speaker diarization

- "Who spoke when" with Nemotron-3-Diarization (ONNX int8, `joosthel/Nemotron-3-Diarization-ONNX` pinned to a revision, two files with pinned SHA-256, `MODEL_SIZE_MB` = 104), run through `ort`. The speaker cache that carries identity between chunks is ported here. Up to `NUM_SPEAKERS` = 8. `diarize()` needs the `local-stt-parakeet` feature (it brings `ort`); without it the function returns an error.
- Two callers, never the capture worker: `diarize()` over a finished recording inside `dimmy_meeting_retranscribe`, and `LiveSpeakers` (built on `StreamDiarizer`) on the meeting's transcription thread while it records (`meeting.rs::stt_thread_loop`); a live run that broke is thrown away and the stop does the full pass. `label_bands`, `lines_by_dominant_speaker`, `load_speakers` / `save_speakers` (`speakers.json`), `rename_speaker`.
- FFI: `dimmy_diarization_model_present`, `dimmy_diarization_download`, `dimmy_meeting_rename_speaker`. See [`diarization.md`](diarization.md).

## `coreml_encoder.rs` — whisper's Core ML encoder bundle (Mac)

- With `local-stt-coreml`, whisper.cpp looks next to the `.bin` for `<name>-encoder.mlmodelc` and then runs the encoder on the Apple Neural Engine; a missing bundle only costs a log line. The bundle is chosen by model architecture, not by quantisation; Distil models have none.
- `download` (a zip from `ggerganov/whisper.cpp`, through `download.rs`), `bundle_present`, `prepare_in_background` / `prepare_blocking` / `run_deferred` (compile ahead of use; deferred while a meeting records), `artifacts` (what a delete removes).
- FFI: `dimmy_coreml_encoder_status`, `dimmy_coreml_encoder_download`, `dimmy_coreml_prepare`.

## `silero.rs` — Silero VAD for the realtime chunk gate

- Answers "is there speech in this window?" with Silero VAD v6.2.0 (`ggml-silero-v6.2.0.bin`, bundled next to the executable; the model directory is a fallback). Needs `local-stt`. `speech_present(pcm16k)` returns `None` when the model is absent, and the caller keeps the RNNoise + energy path.
- Callers: `preprocess::process_chunk_vad_only` and `whistle.rs`.

## `gtcrn.rs` — GTCRN denoiser (feature `denoise-gtcrn`)

- Streaming speech enhancement over `ort` with the bundled `gtcrn_simple.onnx` (535 KB). `maybe_denoise_16k` sits on the 16 kHz STT input (`transcribe::stt_input_16k`); the `live` submodule denoises during capture and hands the result over with `take_prepared`.
- Compiled into the Windows release builds but OFF at runtime: `denoise_enabled()` is true only when `DIMMY_GTCRN=1`. The comment on that function has the A/B numbers behind the switch.

## `lang_detect.rs` — which language a recording is in

- Reads whisper's language probabilities (`whisper_lang_auto_detect`, through `whisper-rs`'s `raw-api`) over `WINDOWS` = 5 windows of 30 s and takes a majority (`majority_verdict`, at least 3 agreeing). Uses the first of `ggml-tiny-q8_0.bin` / `ggml-base-q8_0.bin` found on disk. Advisory: an empty answer is normal.
- FFI: `dimmy_detect_audio_language`. The host uses the answer to name the recap language.

## `file_stream.rs` — streaming file load

- Decodes an audio file (Symphonia, plus libopus through `codecs()`), downmixes to mono, applies the file-load highpass and cuts chunks one block at a time, so a file of any length costs one decoded packet plus one chunk of memory. `FileAudioStream`, `FileChunker`, `for_each_chunk`, `for_each_window`.
- Callers: `dimmy_transcribe_file` in `ffi.rs` and `lang_detect.rs`.

## `dictation_session.rs` — one state machine for dictation input

- Hosts hand every press, release and click to `input(Input)` and do what the returned `Action` says; `phase()` gives the current `Phase`. `MIN_HOLD_MS` = 150, `TOGGLE_BOUNCE_MS` = 300.
- FFI: `dimmy_dictation_input(input, kind, mode)` (0 nothing, 1 start, 2 finish, ...), `dimmy_dictation_phase`.

## `hardware.rs` — what the machine can do

- `detect()` → `GpuInfo`, `assess` → `LocalFitness`, `summary_line`, `read_nvidia`. Feeds the onboarding choice between local and cloud, and the memory checks in `local_llm`, `meeting`, `nemotron`, `qwen_asr` and `setup`.
- FFI: `dimmy_hardware_json`. Win: `Helpers/HardwareInfo.cs`.

## `legal_doc.rs` — licence and notices as blocks

- Parses the compiled-in `LICENSE_TEXT` and `THIRD_PARTY_NOTICES` once with `pulldown-cmark` into a list of `Block`s (headings, paragraphs, tables, licence texts) that Windows and macOS draw natively. `from_markdown`, `from_plain`, `json`.
- FFI: `dimmy_legal_document(kind)` (0 licence, 1 notices). The raw texts: `dimmy_license_text`, `dimmy_third_party_notices`; `dimmy_terms_version`.

## `setup.rs` + `setup_cli.rs` + `setup_prefs.rs` — agent setup (`dimmy-setup`)

- `setup.rs` (pure): which settings an agent may set, strict validation of a patch (`validate_patch`), the "few answers → consistent settings" profile (`profile_patch`, `recommended_profile`), machine facts, log redaction.
- `setup_cli.rs`: argument parsing and command dispatch (`run`). Every command prints one JSON object on stdout. Exit codes: 0 ok, 1 failed, 2 bad usage or invalid settings, 3 Dimmy is running and the command would write.
- `setup_prefs.rs`: the settings the app stores outside `config.json` (Windows `<config_dir>/ui_prefs.json`, macOS UserDefaults), with one hotkey grammar for both systems.
- FFI: `dimmy_setup_cli(argc, argv)`. The `dimmy_setup` binary (feature `setup-launcher`) loads `dimmy_lib` from its own folder and calls it; macOS runs the same entry through `Dimmy --setup`. Design: [`agent-setup.md`](agent-setup.md).

## `win_qos.rs` — no EcoQoS throttling during local inference

- Windows schedules a background process onto the efficiency cores. `NoThrottle::for_local_inference()` returns a guard that exempts the process while it is alive (nested guards are counted).
- Callers: `local_stt`, `parakeet`, `diarize`, `gtcrn`.

## `sim_audio.rs` — meeting audio from files (feature `sim-audio`)

- Local test builds only, never in a release feature line. Feeds a meeting's mic and system tracks from WAV files in 10 ms blocks at hardware pace, entering where the real streams do (`AudioCommand::PushLoopback` for the system track).

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
  The telemetry event `Event::AppRulesEvaluated` (`app_rules.evaluated`)
  carries only `matched` (bool) and a bucketed rule count. See `CLAUDE.md`
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
  on the SECOND consecutive abort during GPU init on the same driver
  fingerprint (`record_strike`, `.gpu_first_strike`); the first abort
  forces the CPU for that session only, because the sentinel cannot tell
  a driver abort from a killed process. Stores a driver
  fingerprint so the next launch can compare: same → keep CPU mode,
  different → driver/ICD changed, retry GPU once. Without this, the
  user paid one crash + relaunch on every cold start. A sentinel left
  by another program (`foreign_marker_present`) is cleared and ignored.
  Manual override: `clear_known_bad` (UI button, FFI
  `dimmy_gpu_clear_known_bad`; status via `dimmy_gpu_get_status`).
  The decision itself is `local_stt::compute_gpu_backend_status`.
- `gpu_diag.rs` registers ggml log callbacks on whisper + llama so the
  C++ stderr (invisible on Windows GUI apps) lands in `dimmy.log` —
  the last-words-before-crash become visible post-mortem. Plus a
  one-shot Vulkan environment snapshot (vulkan-1.dll path + size,
  per-device VRAM, driver version best-effort).

## `license.rs` — Ed25519 license verification (offline)

- Verifies Ed25519-signed license tokens **offline** against a
  build-time-embedded public key (`DIMMY_LICENSE_PUBKEY` env var).
- Reads/writes `license.json` in the config dir
  (`crate::config_dir_path()`). Tracks last successful
  online refresh so it can enforce a soft offline grace window
  (`max_offline_days` from the token). Also the HTTP client for the
  licensing server: `request_trial`, `redeem_activation_code`,
  `refresh_token`, `list_devices`, `deactivate_device`,
  `create_checkout`, `change_plan`, `billing_portal_url`,
  `fetch_sales_open`, `register_interest`.
- **Privacy invariants**: plain email is never persisted on disk, only
  its SHA-256 hash with stable salt. Private signing key is never in
  the client binary — only the public key.
- Open-source carve-out: when `DIMMY_LICENSE_PUBKEY` is empty/unset
  (source-build path), or the build has no `license-client` feature,
  `check_status()` returns `Unrestricted` and
  licensing is a no-op. Build-it-yourself, run-it-free.
- Token format: JWT-like, `header_b64.payload_b64.sig_b64` (alg=EdDSA,
  typ=DLT). See `Claims` for the payload schema.
- Feature-gated: `license-cli` (CLI testing client) + `license-client`
  (Ed25519 verification for the production cdylib). `license-client` is
  NOT in the crate's `default` features (`local-stt`,
  `telemetry-sentry`), although a comment in `Cargo.toml` says "Default
  ON"; the release workflows pass it explicitly. Types, file I/O and the
  HTTP client compile without it.
  Server-side signing happens in a Cloudflare Worker (TypeScript) — no
  Rust signer ships.
- FFI: `dimmy_license_status_json`, `_refresh`, `_clear`, `_has_scope`,
  `_request_trial`, `_redeem`, `_devices_list`, `_device_deactivate`,
  `_checkout_url`, `_plan_change`, `_billing_portal_url`, `_sales_open`,
  `_register_interest`, `_text`.

## `telemetry/` — PostHog + Sentry pipeline

- `telemetry/events.rs` defines the `Event` enum — every event variant
  IS the wire format. Adding a new event requires a unit test plus
  updates to `docs/dev/telemetry-implementation.md` and `PRIVACY.md` if
  a new category of data is collected.
- `telemetry/sentry_pipeline.rs` wraps the `sentry` crate (gated by
  `telemetry-sentry` feature, default ON). User feedback uses a
  manually-built envelope (`type=feedback`) so reports land in Sentry's
  Feedback tab, not Issues.
- Other files: `client.rs` (PostHog HTTP client over `reqwest`, no SDK,
  best-effort), `identity.rs` (anonymous id, one random UUID per
  install in `analytics_id`), `sanitize.rs` (the helpers every property
  goes through, incl. `looks_like_secret` and the bucket functions),
  `report.rs` (the one way a handled failure reaches Sentry),
  `crash_dump.rs` (Windows minidump into `<config>/crashes/`, sent on
  the next launch), `abort_report.rs` (a marker for native aborts,
  reported on the next launch), `crash_prompt.rs` (what happens to a
  crash from the previous launch when crash reports are off: the host
  asks once; FFI `dimmy_crash_report_prompt` / `_resolve` / `_text`).
- **Hard privacy rules** — never include user content (transcribed
  text, prompt text, custom LLM prompt, mic device name, file paths,
  hostname, username, IP) in any property or message. The
  `looks_like_secret` filter is a safety net, not a substitute for
  review. Provider names (groq/openai/anthropic/…) are categorical
  enums and OK to send. See `PRIVACY.md` for the public surface.

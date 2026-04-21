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
- FFI: `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`.

## `local_llm.rs` — llama.cpp integration (optional)

- Feature-gated behind `local-llm`. GPU variants: `local-llm-metal`, `local-llm-vulkan`, `local-llm-cuda`.
- Uses the **forked `llama-cpp-4`** dependency — `KonradDallaOrg/llama-cpp-rs`. The fork patches `llama-context.cpp` for Gemma 4 (FGDN patch). When bumping the llama.cpp submodule upstream, re-apply the FGDN patch.
- Platform differences:
  - **Windows:** static link (`--features local-llm-vulkan`), linker needs `/FORCE:MULTIPLE`.
  - **macOS:** `dynamic-link` feature enabled (pulled in via `local-llm-metal`). dylibs (`libllama.dylib`, `libggml.dylib`, ...) bundled into `Dimmy.app/Contents/Frameworks/` and codesigned.
  - **Linux:** built but not wired into the default AppImage (CPU-only for portability).
- **Thinking mode must be OFF** for Gemma 4. See [`local-llm-feasibility.md`](local-llm-feasibility.md) — with thinking on, models generate 300-500 hidden tokens before answering (20+ seconds).
- FFI: `dimmy_list_llm_models`, `dimmy_download_llm_model`, `dimmy_llm_model_exists`.

## `llm.rs` — post-processing router

- Dispatches on `style` (Off, Correct, Summarize, Elaborate, Comprehensible, Professional, Prompt, Gen-Z, Boomer, Emoji, Acronyms, Imbruttito, Custom).
- Two wire formats:
  - **OpenAI-compatible chat completions** — Groq, OpenAI, OpenRouter, Gemini, Custom
  - **Anthropic Messages API** — Claude
- `llm_mode` config field routes to cloud or local (when `local-llm` feature is enabled).
- **PREAMBLE enforces "keep same language as the input"**. Small local models need this reinforcement or they default to English.

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

- 30+ functions. Stable ABI.
- Global `OnceLock<AppState>` owns the process-wide singleton.
- Every function asserts preconditions: non-null pointers, valid UTF-8, bounds, finite floats.
- All JSON blobs are validated on entry (malformed JSON returns an error, does not panic).
- String outputs: caller allocates the buffer and passes its size; function writes UTF-8 + NUL and returns the bytes written. If the buffer is too small, returns `-1` and the required size is discoverable via a separate `_size()` function.

## `error.rs` — typed error hierarchy

- Central `DimmyError` enum, variants per subsystem (`Audio`, `Transcribe`, `Llm`, `Keystore`, `History`, `Model`, `Config`, `Io`).
- Error messages are truncated to 200 chars at the FFI boundary — prevents leaking API keys, PII, or oversized provider responses into logs.
- Display impl is the canonical user-facing string. Debug impl is for logs.

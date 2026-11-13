# Streaming dictation (Deepgram WebSocket) — prototype

Status, checked against the code on 2026-11-11:

- `streaming_dictation` (default off, `lib.rs:1208`; "Realtime typing" in the
  Windows Settings) picks its engine from the STT mode, in `dimmy_start_recording`
  (`ffi.rs:1216-1390`). One `STREAMING` slot holds one `ActiveStreamer`: `Deepgram`,
  `OpenAi` or `Nemotron` (`ffi.rs:66`).
- Cloud STT mode: the OpenAI Realtime engine (`openai_stream.rs`, model
  `gpt-live-transcribe` from `realtime_model_for`) when the selected provider is
  OpenAI. Every other cloud provider gets Deepgram (`deepgram_stream.rs`, `nova-3`)
  when a Deepgram key is saved. With no such key nothing streams: the dictation is
  one batch transcription at stop.
- Local STT mode: a saved Deepgram key is not used. Nemotron streams for real
  (`nemotron::Streamer`). Whisper, Parakeet, Qwen and Whistle type from fixed chunks
  (`chunked_stt.rs`: 5 s for Parakeet and Whistle, 15 s for whisper and Qwen, 500 ms
  overlap), with `engine` = `local-stream`.
- An empty language setting sends `language=multi` to Deepgram
  (`compose_deepgram_ws_url`) and no `languages` field to OpenAI
  (`compose_session_update`).
- Windows and macOS both type the text at the cursor, for the engines `deepgram`,
  `openai` and `local-stream` (`AppViewModel.cs:669`, `DimmyCore.swift:1702`).
- A socket that fails in the middle of a dictation emits `stt_stream_failed`, and
  the stop transcribes the whole recording with batch STT (`emit_stream_failed`,
  `live_final_to_keep`). An empty live result also falls back to batch STT.

The text below is the note of the first prototype (branch
`feat/streaming-dictation`, Deepgram only, Windows only at that time).

Branch: `feat/streaming-dictation`. Status then: Windows prototype, end-to-end.
Goal: dictate and watch the text appear at the cursor as you speak, the
"Claude dictation" feel, using a realtime WebSocket STT backend.

## What it does

When `streaming_dictation` is on, the STT mode is cloud, AND a Deepgram STT key
is saved (and the selected provider is not OpenAI, which gets the OpenAI Realtime
engine), the
dictation capture path streams your microphone audio live to Deepgram over
a WebSocket. Interim results scroll in the live caption; each finalised
segment is typed at the cursor as you speak. At stop the final paste is
suppressed (the segments are already in), and the full transcript still
flows to history.

It is the true-streaming twin of the existing chunked Parakeet path: it
reuses the exact same `stt_chunk` event contract (`delta` / `cumulative` /
`is_final`), so the live caption + history pipeline did not change.

## Why Deepgram first

Per the June 2026 landscape sweep, Deepgram (Nova-3 / Flux) is the
pragmatic first target: ~150-300 ms partial latency, a documented realtime
WS protocol, BYO-key, and it is what Claude's own dictation appears to use.
The architecture is backend-agnostic (one `StreamCallback` contract), so a
local backend (Kyutai STT 1B over its Rust WS server, or streaming
Parakeet) can be added behind the same seam later.
Added since, in the same slot: OpenAI Realtime (cloud) and Nemotron (local, with
its own callback that also carries `backspace`). Kyutai and a streaming Parakeet
were not added; the other local backends type from `chunked_stt.rs`.

## How to try it

1. Settings > Providers: save a **Deepgram** STT key. Keep the STT mode on
   cloud, with a provider other than OpenAI.
2. Settings > Voice input, Advanced view > "Realtime typing": on (Windows).
3. Dictate with your normal hotkey. Watch the text land at the cursor
   segment by segment; the live caption shows the interim words.

## Architecture

- `core/src/deepgram_stream.rs` — `DeepgramStreamer` (start/stop lifecycle,
  gemello del `ChunkedTranscriber`). Owns a single-thread tokio runtime,
  taps the shared PCM buffer via a moving cursor, downsamples 48k->16k,
  ships PCM16 LE to `wss://api.deepgram.com/v1/listen`, parses interim +
  final results, emits the `stt_chunk` callback. Pure helpers
  (`compose_deepgram_ws_url`, `pcm_f32_to_i16le`, `parse_dg_message`,
  `drain_new_samples`) are unit-tested (the file has 12 tests, one of them for
  `stop()` against a server that never finishes the handshake).
- `core/src/ffi.rs` — `STREAMING` static slot (an `ActiveStreamer`: Deepgram,
  OpenAI or Nemotron); the Deepgram streamer is spawned in
  `dimmy_start_recording` when `streaming_dictation` is on, the STT mode is cloud,
  the provider is not OpenAI and a Deepgram key is present
  (takes priority over chunked); drained in `dimmy_stop_recording`, its text
  flows through the same history/transcript tail as the chunked result.
- Config: `streaming_dictation` bool (lib.rs struct + default + save/load +
  GlobalState + snapshot; ffi.rs init + get/set). Round-trips through C#
  `SettingsViewModel` and the Swift `AppState`.
- Windows host:
  - `TextInjectionService.TypeUnicodeText` — SendInput KEYEVENTF_UNICODE,
    no clipboard churn, used to inject each finalised segment.
  - `AppViewModel` — `stt_chunk` handler reads `engine`/`delta`; for
    `engine == "deepgram"`, `"openai"` or `"local-stream"` it sets
    `StreamingDictationActive` and raises
    `StreamingSegmentFinalized(delta, backspace)`.
  - `App.xaml.cs` — subscribes and types each segment; suppresses the final
    paste when a streaming session ran (the check is in `App.xaml.cs`,
    `StreamingDictationActive`).
- macOS host: `DimmyCore.swift` (`stt_chunk`, same three engine names) and
  `TextInjector.typeStreamed`.

## TLS note (do not switch to rustls)

`tokio-tungstenite` uses the **native-tls** feature on purpose. The cdylib
standardised on native-tls (schannel on Windows) because rustls 0.23+
panics in the Velopack/WinAppSDK load path without
`CryptoProvider::install_default()` (the 0xc0000409 crash class). A rustls
WS client would reintroduce it.

## Known prototype gaps (follow-ups, not shipped)

The gaps as listed for the prototype, each with what the code does today.

- **Connection errors are logged, not surfaced.** A failed WS connect
  yields an empty transcript with a `[dg-stream]` log line, no user-facing
  error toast. Wire an `error` event for v2.
  Today: a failed connect is still only logged, but the empty live result makes
  the stop run batch STT on the recording, so the text is not lost. A send that
  fails mid-dictation emits `stt_stream_failed`, which both hosts handle.
- **Mac/Linux parity not done.** This is Windows-only so far. Mac would use
  the same Rust core (already cross-platform) + a Swift segment-injector via
  CGEvent unicode, mirroring `TypeUnicodeText`.
  Today: macOS is done (`DimmyCore.swift:1702`, `TextInjector.typeStreamed`). The
  Linux UI has no `stt_chunk` handler.
- **No telemetry event** for streaming sessions yet.
  Today: `transcription.completed` carries `engine` (`deepgram_stream`,
  `openai_stream`, `local_stream`, `chunked_caption` or `batch`; `DICTATION_ENGINE`
  in `ffi.rs`).
- **Injection is segment-chunky by design** (the chosen UX: stable segments
  at the cursor + interim preview in the pill), not per-word backspace
  correction. This is the robust path for arbitrary external apps.
  Today: still true for Deepgram and OpenAI. Nemotron sends whole words and a
  `backspace` count for corrected words.

# Streaming dictation (Deepgram WebSocket) — prototype

Branch: `feat/streaming-dictation`. Status: Windows prototype, end-to-end.
Goal: dictate and watch the text appear at the cursor as you speak, the
"Claude dictation" feel, using a realtime WebSocket STT backend.

## What it does

When `streaming_dictation` is on AND a Deepgram STT key is saved, the
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

## How to try it

1. Settings > Providers: save a **Deepgram** STT key.
2. Settings > Advanced > "Streaming dictation (Deepgram, realtime)": on.
3. Dictate with your normal hotkey. Watch the text land at the cursor
   segment by segment; the live caption shows the interim words.

## Architecture

- `core/src/deepgram_stream.rs` — `DeepgramStreamer` (start/stop lifecycle,
  gemello del `ChunkedTranscriber`). Owns a single-thread tokio runtime,
  taps the shared PCM buffer via a moving cursor, downsamples 48k->16k,
  ships PCM16 LE to `wss://api.deepgram.com/v1/listen`, parses interim +
  final results, emits the `stt_chunk` callback. Pure helpers
  (`compose_deepgram_ws_url`, `pcm_f32_to_i16le`, `parse_dg_message`,
  `drain_new_samples`) are unit-tested (11 tests).
- `core/src/ffi.rs` — `STREAMING` static slot; spawned in
  `dimmy_start_recording` when `streaming_dictation` + Deepgram key present
  (takes priority over chunked); drained in `dimmy_stop_recording`, its text
  flows through the same history/transcript tail as the chunked result.
- Config: `streaming_dictation` bool (lib.rs struct + default + save/load +
  GlobalState + snapshot; ffi.rs init + get/set). Round-trips through C#
  `SettingsViewModel`.
- Windows host:
  - `TextInjectionService.TypeUnicodeText` — SendInput KEYEVENTF_UNICODE,
    no clipboard churn, used to inject each finalised segment.
  - `AppViewModel` — `stt_chunk` handler reads `engine`/`delta`; for
    `engine == "deepgram"` it sets `StreamingDictationActive` and raises
    `StreamingSegmentFinalized(delta)`.
  - `App.xaml.cs` — subscribes and types each segment; suppresses the final
    paste (App + PillWindow stop paths) when a streaming session ran.

## TLS note (do not switch to rustls)

`tokio-tungstenite` uses the **native-tls** feature on purpose. The cdylib
standardised on native-tls (schannel on Windows) because rustls 0.23+
panics in the Velopack/WinAppSDK load path without
`CryptoProvider::install_default()` (the 0xc0000409 crash class). A rustls
WS client would reintroduce it.

## Known prototype gaps (follow-ups, not shipped)

- **Connection errors are logged, not surfaced.** A failed WS connect
  yields an empty transcript with a `[dg-stream]` log line, no user-facing
  error toast. Wire an `error` event for v2.
- **Mac/Linux parity not done.** This is Windows-only so far. Mac would use
  the same Rust core (already cross-platform) + a Swift segment-injector via
  CGEvent unicode, mirroring `TypeUnicodeText`.
- **No telemetry event** for streaming sessions yet.
- **Injection is segment-chunky by design** (the chosen UX: stable segments
  at the cursor + interim preview in the pill), not per-word backspace
  correction. This is the robust path for arbitrary external apps.

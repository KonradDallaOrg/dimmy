# Nemotron — the fifth local STT backend (`local_stt_backend = "nemotron"`)

Code: `core/src/nemotron.rs`, the Nemotron parts of `core/src/meeting.rs`
(`NemoTracks`, `LineSink`, `NEMOTRON_WINDOW_SECS`) and `core/src/ffi.rs`
(`ActiveStreamer::Nemotron`, `dimmy_nemotron_status/_size_mb/_download`).
Engine build: `.github/workflows/engine-nemotron.yml`.
All figures below were measured 2026-10-29/30 on an i7-12700H + NVIDIA T600,
Italian speech.

## What and why

NVIDIA Nemotron 3.5 ASR Streaming 0.6B: a cache-aware streaming
FastConformer-RNNT. 40 languages from one checkpoint, language **auto-detected
by default**, and text that grows while the person is still speaking.

Why it exists next to whisper / Parakeet / Qwen / Whistle:

- Real streaming (not chunked): dictation types as you speak, and meetings get
  a live transcript with live speaker labels.
- Mixed Italian/English meetings work. Whisper with a forced `it` turns the
  English parts into made-up Italian; Nemotron needs no forced language.
- Speakers come from the same engine (Nemotron-3-Diarization), word by word.

| File | Size | Source |
|---|---|---|
| ASR model, GGUF Q8_0 | 742 MB | `nvidia/nemotron-3.5-asr-streaming-0.6b` @ `ea30d66` |
| Diarizer, GGUF Q8_0 | 107 MB | `nvidia/Nemotron-3-Diarization` @ `f667ed7` |
| Engine library (per OS/backend) | see release | `engine-nemotron-66d50d9` assets |

F16 gives the same text as Q8_0 at 3x the cost: not offered. INT8 ONNX was worse
on technical terms: not used. Both models are OpenMDW-1.1 (credited in
`scripts/dev/third-party-manual.md`).

## Architecture

### Engine build and loading

The runtime is NVIDIA's NeMo-Speech.cpp, C ABI `asr.h` v1 (opaque handles, POD
structs that start with `size` and only grow), loaded **in-process** with
`libloading`. Built by this project from NVIDIA commit `66d50d9` with ggml
linked **statically** (`BUILD_SHARED_LIBS=OFF`): NVIDIA's own release ships
`ggml.dll` / `llama.dll`, the same names `local-stt-qwen` and `local-llm` load
into the same process, and two DLLs with one name in one process is a crash.
The engine is therefore two libraries with names nothing else uses.

- `GGML_NATIVE=OFF`, AVX2 baseline (portable CPU build); the AVX-512 gate
  (`windows-ci.md` I11) checks it like `dimmy_lib`.
- Builds: Windows cpu + cuda, Linux cpu + cuda, macOS metal. CUDA uses the
  cuBLAS shim and static cudart. A GPU build is used only when the hardware is
  there (`has_nvidia_gpu`) and falls back to the CPU build when it will not load
  (old driver).
- It is **not a cargo feature and not in the installer** (like Whistle): the
  library is downloaded with the models into `nemotron-engine/<flavor>/`.

### Two recognizers

A recognizer that carries the diarizer runs it on **every** stream, used or
not (dictation 2.6x -> 1.4-1.6x realtime on CPU). So `default_spec(speakers)`
builds two specs, cached in `RECOGNIZERS`: without the diarizer (dictation, and
the mic track on a call) and with it (tracks that need speakers).
`app_recognizer(speakers)` picks one.

## Dictation

`ActiveStreamer::Nemotron` in the `STREAMING` slot (`ffi.rs`), started by
`nemotron::Streamer::start`. Real streaming, not `chunked_stt.rs`.

- **One stream per dictation, never reset.** A reset lost the first word and the
  detected language.
- Audio is pushed at the captured 48 kHz (`push_at`), in 100 ms steps; the
  engine resamples.
- **Whole words only.** A word still forming is held back; hosts append one
  space per delta.
- **Corrections.** If the engine revises a word it already typed, `stt_chunk`
  carries `"backspace": N` (characters to delete before typing `delta`). Only
  within the last 8 words of the open sentence (`MAX_REVISE_WORDS`). Windows
  deletes only inside the window it last typed into.
- `engine` is `"local-stream"` (typing) or `"nemotron"` (captions only).

## Meetings

`NemoTracks` runs on the **transcription thread** only; `worker_loop` is
untouched (THE AUDIO RULE, `docs/dev/meeting-audio-durability.md`).

- 2 s windows (`NEMOTRON_WINDOW_SECS`), one stream per track, fed the new part
  of each window. The two tracks are fed in parallel (`thread::scope`).
- `LineSink::write` appends `[HH:MM:SS] [label] text` to `transcripts.txt`
  and emits the chunk event, like every other engine. Time = the turn's first
  word.
- Tracks: the system track carries the live diarizer. The mic keeps its own
  label on a call; in a **one-room meeting** (no system track) the mic is
  diarized instead.

### Speakers

- The system track with the live diarizer writes `[HH:MM:SS] [Speaker N] text`
  per turn; flicker turns under 3 words (`MIN_TURN_WORDS`) are folded into the
  neighbour.
- **Load guard.** Seconds of compute per second of window, smoothed
  (0.8 old + 0.2 new). Above **0.9** the live speakers are dropped for the rest
  of the meeting (`drop_speakers`, event `meeting_speakers_deferred`) and the
  stop-time speaker pass labels the turns instead. Dropping labels is correct;
  falling behind is not.
- **Skipping the second pass.** When live labels cover the whole meeting (and no
  window was lost), `speakers.json` is written from them
  (`diarize::save_speakers`) and the stop pass is skipped.
- Live labels only run when the `diarization_enabled` setting is on
  (`speaker_labels` in the meeting's STT context).

## Download and pinning

`dimmy_nemotron_status` (1 ready / 0 not downloaded / -1 no engine for this
platform: hosts hide it), `_size_mb` (model + diarizer + the engine builds this
machine wants), `_download` (blocking; progress event
`nemotron_download_progress {"downloaded","total"}`). Download-center id
`nemotron`. Everything is pinned in `nemotron.rs`: HF URLs by revision, SHA-256
and size per file, magic bytes; engine archives by SHA-256. A changed file
fails the download and never loads. An engine entry with an empty `sha256` is
unpublished and offered as nothing.

## Measurements

| Case | Result |
|---|---|
| Dictation, CPU | 2.6-4.4x realtime |
| Dictation, CPU, recognizer carrying the diarizer | 1.4-1.6x (was 2.6x without it) |
| GGUF Q8_0, CUDA (T600) | 6.1x realtime |
| FP32 ONNX, CUDA | 3.4x |
| 2-track meeting + live speakers, CPU, before the two-recognizer + parallel-tracks fix | load 1.56 (over the 0.9 guard) |
| Same, after the fix | TODO(nemotron.md): re-measure with `nemotron_meeting_bench` and write the number here |

## Known limits

- **Vulkan is blocked** upstream: NVIDIA/NeMo-Speech.cpp issue #48
  (`GGML_ASSERT ne3 == ne13`). No Vulkan build is offered.
- **Live speakers on CPU are tight.** Weaker CPUs trip the 0.9 guard and fall
  back to stop-time labelling; the transcript is unaffected.
- Technical English terms are weaker than whisper turbo.

## Rebuilding the engine

1. Bump `NEMO_COMMIT` / `NEMO_SHORT` (and `LLAMA_COMMIT` if needed) in
   `.github/workflows/engine-nemotron.yml`. Read `windows-ci.md` and ask before
   touching a workflow.
2. Push a tag `engine-nemotron-<short sha>` (or run the workflow dispatch with
   `publish`). It publishes a **pre-release, never Latest**, with no Velopack
   manifest, so no updater or download button sees it. The tag is outside
   `check-release-tag.sh`'s `v*`.
3. Copy every archive's SHA-256 and size from the release's `SHA256SUMS` into
   `ENGINES` in `core/src/nemotron.rs`, and update `ENGINE_RELEASE` and the
   archive names.
4. Re-run `nemotron_smoke` on the new library before shipping.

## Dev bins (`core/src/bin/`)

- `nemotron_smoke --lib <dll> --model <gguf> [--diar <gguf>] [--gpu N] [--lang it] [--keywords ...] [--oneshot] file.wav`:
  drives the engine in-process as the app would (100 ms pushes); prints deltas,
  final text, speaker turns and the realtime factor.
- `nemotron_meeting_bench <meeting dir> [--no-speakers] [--lang it]`: replays
  `audio_mic.ogg` / `audio_system.ogg` in live 2 s windows and prints the load
  and lines. This is the tool for the TODO above.
- `stt_race`: per-window transcription speed across local backends. (There is
  no `stt_ab` bin in the tree.)

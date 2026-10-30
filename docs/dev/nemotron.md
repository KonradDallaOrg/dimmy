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
| ASR model, GGUF F32 (optional, GPU only) | 2.58 GB | converted by this project, `KonradDallaOrg/dimmy-nemotron-3.5-asr-gguf` |

F16 gives the same text as Q8_0 at 3x the cost: not offered. INT8 ONNX was worse
on technical terms: not used. All models are OpenMDW-1.1 (credited in
`scripts/dev/third-party-manual.md`).

**F32.** NVIDIA publishes no F32 GGUF and its converter refuses `--outtype f32`
for ASR, so it is produced with a one-line local patch of `conversion/registry.py`
and `conversion/asr.py` at the pinned commit, from the same checkpoint revision.
The file on disk is the setting: download-center id `nemotron-f32`, delete it to
go back. It runs only on a GPU build, and only with 10.4 GB of commit charge free
(`pick_model`); otherwise Q8_0 does the job, which is why Q8_0 stays in the bundle.

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

### Recognizers, and why a GPU has only one

A recognizer that carries the diarizer runs it on **every** stream, used or
not (dictation 2.6x -> 1.4-1.6x realtime on CPU). So on CPU `default_spec(speakers)`
builds two specs, cached in `RECOGNIZERS`: without the diarizer (dictation, and
the mic track on a call) and with it (tracks that need speakers).

On a GPU there is **one**, always with the diarizer. A GPU has the headroom and
not the memory: the process commits 7.5 GB at Q8_0 and 8.7 GB at F32, mostly the
CUDA runtime, and a second F32 recognizer aborted the process
(`GGML_ASSERT(ctx->mem_buffer != NULL)`) or, when it fit, spilled out of the 4 GB
of VRAM: load 1.08 with two against 0.41 with one.

**The engine aborts when an allocation fails**, and in a meeting the process is
the recording. So `recognizer()` checks `setup::available_memory_bytes` (Windows:
the commit charge left, not free RAM) and returns an error instead, which falls
back from GPU to CPU. Keep this check in front of any new load.

Q8_0 and F32 are two rows of the model picker (`nemotron_model`: `q8` or `f32`).
The file on disk no longer decides. A model already on the GPU stays until the
next launch (`nemotron::restart_needed` tells the host), because it could not be
freed for the other one. Benchmarks without a config use `DIMMY_NEMOTRON_F32=1`.

`scripts/dev/nemotron-memory-test.ps1` runs a meeting and the pass after it at
fixed levels of free commit, held by `commit_hog`. Run it after any change to these
checks, with other programs closed. Results on T600, 45 s meeting, 2026-10-30:

| Free commit | Engine | Live text | Speakers | Pass after the meeting |
|---|---|---|---|---|
| 12 GB, 9 GB | GPU, Q8_0 | both tracks | live | ok, 5 s |
| 12 GB, F32 selected | GPU, F32 | both tracks | live | ok, 17 s |
| 6 GB, 5 GB | CPU, Q8_0 | both tracks | at the pass | ok, 16 s |
| 3 GB | none | none, the host is told | none | refused (rc -5) |

Audio was complete in every run. Live peak commit is 7.0 GB at Q8_0 and 8.6 GB at
F32, which the GPU check covers. The pass adds about 4 GB (11 GB and 14.4 GB) and
has no check of its own when the recognizer is already loaded, so it is the path
left where an allocation can still abort.

A CPU recognizer commits 3.1 GB at Q8_0 without the diarizer and 4.4 GB with it,
so a meeting on CPU needs about 7.5 GB. The CPU engine always runs Q8_0, also when
F32 is on disk. `RECOGNIZERS` changes only after a new recognizer exists, because
what leaves it is never freed: clearing it before a refused GPU load made every
call reload the CPU models and leak the old ones (2026-10-30, a staging install
with 26 GB already committed by other programs: 15 GB, then the abort above, three
times in an hour). With no track open at all the core emits
`meeting_transcription_behind`; the audio is recorded regardless. The engine's
aborts never reach Sentry, which only sees Rust panics: read the Windows crash
dumps in `%LOCALAPPDATA%\CrashDumps`.

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

- 0.5 s windows (`NEMOTRON_WINDOW_SECS`), one stream per track, fed the new part
  of each window, one track after the other. Parallel gained nothing (0.65 against
  0.62): each stream already uses the engine's thread pool.
- **Endpointing is on** (`RecognizerConfig.endpointing`, 800 ms). The engine
  default is off, and then its only final comes when the stream closes: a whole
  meeting appeared at stop. A speaker who never pauses is closed by
  `force_endpoint` at the next sentence end past 30 words, or at 60 regardless.
- The open sentence goes to the hosts as `meeting_partial {dir, track, text}`,
  empty text when it closes. It is not written anywhere; the line is.
- The queue holds 120 windows (`NEMOTRON_QUEUE_DEPTH`), a minute: a stream catches
  up after a slow patch, so a backlog costs lag, not text. Past that a window is
  dropped as for every engine, the word times stay right (`clock_to_track`), and
  the speaker pass runs at stop since the labels have a hole.
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
- **Load guard.** Seconds of compute per second of window, averaged over about
  ten seconds and starting from zero, so a heavy first window means nothing.
  Above **0.9** the live speakers are dropped for the rest of the meeting
  (`drop_speakers`, event `meeting_speakers_deferred`) and the stop-time speaker
  pass labels the turns instead. Dropping labels is correct; falling behind is not.
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

A real 4-minute two-track Italian meeting, 0.5 s windows, i7-12700H + T600 (4 GB).
"Behind" is how far the text trails the voice at the worst moment.

| Engine, model, live speakers | Load | Behind (max) | Flush at stop |
|---|---|---|---|
| GPU Q8_0, system track | 0.24 | 0.2 s | 0.1 s |
| GPU Q8_0, both tracks | 0.38 | 2.0 s | 0.2 s |
| GPU F32, system track | 0.41 | 0.3 s | 0.2 s |
| GPU F32, both tracks | 0.54 | 1.7 s | 0.3 s |
| CPU Q8_0, none | 0.62 | 0.4 s | 0.2 s |
| CPU Q8_0, none, tracks in parallel | 0.65 | 0.5 s | 0.2 s |
| CPU Q8_0, system track | **1.17** | **50 s** (and growing) | 0.8 s |
| CPU Q8_0, system track, load guard on | 0.65 | 1.2 s | 0.3 s |

So: a GPU as small as the T600 holds everything live. A CPU of this class holds the
text on both tracks but not the speakers, which the guard drops after 43 s.

The same meeting through the real `MeetingSession` on a wall clock
(`nemotron_meeting_replay`), with the mirror upload and other work running:

| Engine, model | Recorded | Stop | Speakers ready at stop | Line after its first word (median / max) | Windows lost |
|---|---|---|---|---|---|
| GPU Q8_0 | 246.5 of 246 s | 0.4 s | yes | 3.5 / 14.0 s | 0 |
| GPU F32 | 246.7 of 246 s | 0.6 s | yes | 3.5 / 17.8 s | 0 |
| CPU Q8_0 | 246.6 of 246 s | 0.5 s | no, dropped by the guard | 5.5 / 20.7 s | 0 |

Dictation: 2.6-4.4x realtime on CPU, 6.1x with Q8_0 on the T600.

## Known limits

- **Vulkan is blocked** upstream: NVIDIA/NeMo-Speech.cpp issue #48
  (`GGML_ASSERT ne3 == ne13`). No Vulkan build is offered.
- **Live speakers do not hold on a CPU of this class** (table above): the guard
  drops them and the stop-time pass labels the turns; the transcript is unaffected.
- A fresh sentence shows as a partial straight away, but a closed **line** arrives
  at the pause that ends it: 3.5 s after its first word typically, 12 s for a long
  one.
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
- `nemotron_meeting_bench <meeting dir> [--no-speakers] [--mic-speakers] [--parallel] [--adaptive] [--window 0.5] [--seconds N] [--lang it]`:
  replays `audio_mic.ogg` / `audio_system.ogg` in live windows on a simulated clock
  and prints load, lag and lines. Fast, for comparing configurations.
- `nemotron_meeting_replay <meeting dir> [--no-speakers] [--seconds N]`: the real
  `MeetingSession`, capture and transcription threads, events and stop, on a wall
  clock. Takes as long as the meeting. Removes the meeting it creates.
- `stt_race`: per-window transcription speed across local backends.

`DIMMY_FORCE_CPU=1` forces the CPU engine, `DIMMY_NEMOTRON_Q8=1` ignores F32.

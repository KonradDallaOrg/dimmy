# Audio Pipeline — Detailed Reference

This audio pipeline is shared across all native UI platforms (Windows WinUI3, macOS SwiftUI, Linux GTK4). The Rust core modules are identical; only the integration layer differs per platform.

## Pipeline Overview — dictation

```
Mic (cpal; the callback resamples to 48 kHz = MEETING_CANONICAL_RATE)
    │
    │  Dictation is Mic-only (`AudioSource::Mic`, set in
    │  `dimmy_start_recording`): mic samples go straight to audio_buffer.
    │  No loopback, no AEC ring, no AEC worker. (Mix mode — AEC3 with the
    │  loopback as reference — is the MEETING capture, see below.)
    ↓ dimmy_stop_recording
RawAudio { samples, sample_rate = 48 kHz }
    ↓ preprocess_route(preprocessing_enabled, stt_mode)
    │     Raw          → untouched            (preprocessing off — the default)
    │     Full         → process_buffer_guarded       (on + local)
    │     HighpassOnly → process_buffer_for_file_load (on + cloud)
ProcessedAudio { samples, sample_rate }
    │
    ├── stt_mode == "local":
    │       ↓ transcribe::stt_input_16k(): downsample_to_16k(), then GTCRN
    │         denoise only when DIMMY_GTCRN=1 (off by default)
    │       f32 16 kHz mono samples
    │       ↓ whisper / parakeet / qwen / nemotron / whistle
    │         (per `local_stt_backend`)
    │       String (transcript)
    │
    └── stt_mode == "cloud":   (all inside transcribe_chunked())
            ↓ estimate_wav_size() → chunking decision
            ↓ to_wav_payload() or split_at_silence() + to_wav_payload()
            WavPayload { data: Vec<u8> }
            ↓ transcribe_audio(), once per chunk
            String (transcript)
    │
    ↓ filler::remove_fillers() (if enabled)
    ↓ history save_v2() (auto-save, v2 schema)
String (transcript) → returned to the host
    ↓ separate FFI call: dimmy_process_with_llm
    │     app_rules::resolve(current app context) — optional style override
    │     optional LLM post-processing
String (final text) → paste; host calls dimmy_history_update_enhanced
```

A non-empty transcript from a live engine (Deepgram / OpenAI realtime
streaming, Nemotron streaming, or the local `chunked_stt` worker) is used
instead of the batch STT step above; an empty one falls back to it.

## Pipeline Overview — meeting mode (long-form)

The meeting always captures `AudioSource::Mix` (`dimmy_meeting_start`).
Two threads share the work (`core/src/meeting.rs`): `worker_loop` only
writes audio, `stt_thread_loop` transcribes. See
[`meeting-audio-durability.md`](meeting-audio-durability.md).

```
Mic (cpal → 48 kHz) → aec_mic_ring ─┐
Loopback (Windows: cpal WASAPI loopback, resampled to 48 kHz;
          macOS/Linux: host push via dimmy_push_loopback_audio)
    ├→ aec_ref_ring ────────────────┤  aec.rs worker: 480-sample frames,
    │                               │  WebRTC AEC3 (mic = capture,
    │                               │  loopback = render). Empty ref ring:
    │                               │  zero-pad the ref, never block.
    │                               ↓
    │                        audio_buffer            (cleaned mic, 48 kHz)
    └→ audio_buffer_secondary                        (raw system, 48 kHz)
    ↓ worker_loop (every 100 ms, POLL_INTERVAL)
    │
    │  align_secondary: pad the system buffer with zeros up to the mic
    │  buffer; mic_lag_padding: pad the mic when it trails by > 2 s.
    │  Pause check: if paused, skip write/queue this tick (capture
    │  appends are gated by MEETING_CAPTURE_GATED). On resume: move
    │  samples_written + last_processed past the paused window so it is
    │  excluded from the tracks AND from the chunked timeline; queue a
    │  [paused] line for transcripts.txt at the seam.
    ↓
3 track sinks at 48 kHz: audio (mix, soft-limited), audio_mic,
  audio_system — Ogg/Vorbis on Windows + macOS, int16 WAV on Linux or
  when the encoder cannot start
+ every `meeting_chunk_secs` (default 30 s, clamp 5–60; Nemotron: 0.5 s)
  one window per track → bounded queue (STT_QUEUE_DEPTH = 4, try_send;
  a full queue drops the window, never the audio)
+ buffers drained once written (DRAIN_THRESHOLD_SAMPLES = 10 s)
    ↓ stt_thread_loop
chunk gate (process_chunk_vad_only, only when preprocessing on + local)
→ downsample_to_16k → same backend as dictation (cloud or local)
→ transcripts.txt appended (one line per track per window)
→ `meeting_chunk` event
    ↓ stop: sinks finalized FIRST, then the transcriber is joined (90 s cap)
meta.json finalized (duration_secs, chunk_count, transcript health,
                     stage "stopped"); .recording marker deleted by
                     MeetingSession::stop
    ↓ host
LLM raw post-process (dimmy_llm_call_raw → process_raw_prompt with the
                      host-built 11-section structured-recap prompt;
                      recap_model_override first, URL heuristic fallback;
                      Anthropic Opus 4.7+ uses thinking.type=adaptive)
    ↓
recap.md + actions.json written (dimmy_meeting_save_post_process)
```

The meeting `MEETING` static lives in `ffi.rs` independently of any UI
window — closing the UI doesn't stop the recording. UIs probe
`dimmy_meeting_is_active` on open and re-attach.

## Pipeline Overview — file load

`dimmy_transcribe_file` (drag-drop / picker → transcribe) uses a
**different preprocess path**: the file-load highpass
(`preprocess::FileLoadHighpass`, the streaming form of
`preprocess::process_buffer_for_file_load` — same output, sample for
sample). The file is streamed by `file_stream::FileAudioStream`, so the
whole file is never in memory:

```
audio file (WAV via hound; m4a/mp4/aac/mp3/flac/ogg via Symphonia)
    ↓ FileAudioStream: decode one block → mono f32 + sample_rate
    ↓ FileLoadHighpass::process(block)
    │   - Clamp to [-1.0, 1.0] (NaN/Inf → 0.0)
    │   - 80 Hz highpass (skipped if sample_rate < 1 kHz)
    │   - NO VAD
    │   - NO AGC
    │   - Final NaN/Inf guard + clamp
    ↓ file_stream::for_each_chunk — chunks end at a pause
    │   (audio::first_silence_split)
    │   local: 30 s max per chunk (CHUNK_MAX_SECS)
    │   cloud: transcribe::cloud_chunk_samples (whole file if it fits the
    │          provider limit, else 80 % of the limit)
    ↓ local backend (through stt_input_16k) or cloud transcribe_audio
String (transcript)
```

**Why no AGC for files**: dagc emits NaN on long-silence stretches
(meetings have many of them) and the post-AGC clamp turns NaN into 0.
Once dagc's internal gain state is corrupted, every subsequent sample
outputs NaN forever. Burned 2026-05-08: 97 % of a 95-min WAV became
silent zeros, Parakeet emitted empty for 186 of 191 chunks. Fix in
commit `0ed682b`. See `known-bugs.md` AUDIO-001 for the dictation-side
counterpart.

## Type Safety

Three distinct types (`core/src/audio.rs`) name the pipeline stages:
- **RawAudio**: Raw f32 samples from cpal. Methods: `preprocess(enabled)` and `to_wav()` (debug dump).
- **ProcessedAudio**: After preprocessing. Can estimate WAV size, split at silence, or encode to WAV.
- **WavPayload**: Ready-to-send WAV bytes (16 kHz, 16-bit PCM).

Only `ProcessedAudio::to_wav_payload()` produces a `WavPayload`, so a cloud
upload is always downsampled. The ordering is a convention, not a hard
compile-time guarantee: the struct fields are public, and
`dimmy_stop_recording` builds `ProcessedAudio` directly for the `Full` and
`HighpassOnly` routes (`RawAudio::preprocess` runs only for `Raw`).

## Preprocessing (preprocess.rs)

### Pipeline Steps (in order)
1. **Clamp**: Input clamped to [-1.0, 1.0]. NaN/Inf → 0.0.
2. **Highpass**: 80Hz Butterworth 2nd order (biquad). Removes DC offset + low-frequency rumble.
3. **VAD**: nnnoiseless voice probability with hysteresis state machine. Runs only at 48 kHz; at any other rate the VAD is skipped and steps 4–5 still run.
4. **AGC**: dagc MonoAgc adaptive gain (`TARGET_RMS = 0.05` ≈ -26 dBFS, `AGC_DISTORTION = 0.001`), then `limit_peak`: if a peak exceeds `PEAK_CEILING = 0.89` the whole buffer is scaled down by one factor (no clipping). The target was 0.2 until 2026-08-04 (commit `8f4bc73d`); with it every one of 38 measured captures came out hard-clipped.
5. **Clamp**: Output clamped to [-1.0, 1.0]. NaN → 0.0 (safety net for dagc bugs).

This is `AudioPreprocessor::process` (`AudioPreprocessor::new`). It runs
only on the `Full` route (preprocessing on + local STT), see "Route-aware
preprocessing" below. `preprocessing_enabled` is **off by default** since
2026-08-31 (`AppConfig::default` in `core/src/lib.rs`); a `config.json`
that already carries a value keeps it.

### VAD State Machine

```
IDLE ──[voice_prob > onset OR energy_override, 3 consecutive]──→ SPEECH
  ↑                                                                 │
  │                                                                 ↓
  └──[silence_frames > GRACE (300)]────────────────────────── GRACE ←┘
                                                    [voice_prob < offset
                                                     AND rms < ENERGY_FLOOR]
```

Constants:
- `VAD_ONSET_THRESHOLD = 0.5` (first onset)
- `VAD_OFFSET_THRESHOLD = 0.3` (offset + re-onset after has_spoken)
- `MIN_SPEECH_FRAMES = 3` (30ms at 48kHz/480 frame size)
- `SILENCE_GRACE_FRAMES = 300` (3 seconds)
- `ENERGY_FLOOR = 0.015` (RMS floor for energy override)

### CRITICAL: dagc NaN Bug

**dagc::MonoAgc produces ALL NaN when fed zero-amplitude samples.**

This is not a theoretical concern — it caused real user-facing bugs (see known-bugs.md AUDIO-001).

Rules:
1. NEVER emit silence/zero-energy frames to the output that will reach AGC
2. Grace period must NOT emit silence — only delay state transition
3. Hysteresis branch must check RMS > ENERGY_FLOOR before emitting
4. Always have the NaN→0.0 safety clamp after AGC, but don't rely on it

If dagc is ever replaced, verify the replacement handles:
- Zero input for extended periods
- NaN propagation
- Very quiet input (< 0.001 amplitude)

### process_buffer() Architecture

`process_buffer()` creates a FRESH preprocessor, calls `process()` ONCE with ALL samples, then appends `flush()` (the up-to-479-sample tail left in the VAD frame buffer, emitted only when still in speech). This means:
- The entire recording goes through a single VAD→AGC pass
- All output (from first speech to last) is in ONE Vec
- AGC processes this Vec in one shot

Consequence: if silence frames end up in the output Vec (e.g., from grace period), AGC sees them in the same pass as speech. dagc NaN corruption from those silence frames destroys all subsequent speech in the same Vec.

### Route-aware preprocessing (dictation stop)

The full VAD+AGC pipeline is NOT applied uniformly. `dimmy_stop_recording`
picks a route via the pure `preprocess::preprocess_route(preprocessing_enabled,
stt_mode)` (single source of truth, unit-tested so the mapping can't drift):

| Config | Route | Path |
|---|---|---|
| `preprocessing_enabled = false` | `Raw` | passthrough, untouched |
| enabled + `stt_mode == "local"` | `Full` | `process_buffer_guarded` (VAD+AGC, guarded) |
| enabled + cloud (anything else) | `HighpassOnly` | `process_buffer_for_file_load` (80 Hz highpass only) |

With preprocessing off (`Raw`) and local STT, `dimmy_start_recording` also
arms `gtcrn::live` so a GTCRN denoise can run during capture — a no-op unless
`DIMMY_GTCRN=1` (see "Noise suppression" below).

### Chunk paths: a VAD gate, never a trim (trim 2026-07-31, gate since 2026-08-05, Silero first since 2026-08-06)

The table above is the route for the **batch** buffer at `dimmy_stop_recording`.
The realtime chunk workers — `chunked_stt.rs` (dictation; a window of 5 s for
parakeet/whistle with text on screen, else 15 s, from
`chunked_stt::chunk_secs_for_backend`, plus 500 ms overlap) and the
transcription thread in `meeting.rs` (one `meeting_chunk_secs` window per
track, default 30 s) — used to hand whisper the raw buffer, silence included,
which is the input it hallucinates YouTube sign-offs on ("Grazie", "Thank you").
They now call `preprocess::process_chunk_vad_only` whenever the same
`preprocess_route(..)` returns `Full`, i.e. preprocessing on AND local STT.
With preprocessing off (the default) or cloud STT the window goes to the model
ungated. The Nemotron streaming paths do not use this gate.

That function is a **gate**: it returns the window WHOLE and untouched (no
highpass, no trim, no AGC) or returns it empty, and an empty window skips the
model call. The first version (2026-07-31) trimmed the silence inside each
window; measured 2026-08-05, whisper then received sub-second fragments and
hallucinated in the other direction (Italian came back as invented Spanish, a
9 s dictation as "Yeah. Yeah. Yeah."), so commit `0891209d` made it
all-or-nothing. Never AGC here: dagc is adaptive, so a per-chunk instance
settles on a different gain per window, and it NaNs on all-silence input
(AUDIO-001) — which is exactly what an idle chunk is.

Who answers "is there speech in this window?" (48 kHz input only; any other
rate passes through):

- **Silero first** (`core/src/silero.rs`, since 2026-08-06, commit
  `2e393abd`): the window is downsampled to 16 kHz and
  `silero::speech_present` runs the bundled `ggml-silero-v6.2.0.bin` at the
  fixed `THRESHOLD = 0.5`. Speech → whole window; no speech → empty.
- **Fallback** `preprocess::chunk_gate_fallback` when Silero gives no answer
  (model file absent, load failure, or a build without `local-stt`): an
  `AudioPreprocessor::new_vad_trim` pass (highpass + RNNoise VAD + a per-frame
  energy floor, no AGC) is used only to MEASURE how much speech the window
  holds; its output is discarded.

Three rules of the fallback gate, paid for on 2026-07-31 with a real meeting:

1. **A frame must clear `CHUNK_GATE_ENERGY_FLOOR` to count as speech.**
   nnnoiseless is trained to be indifferent to level, so a keyboard click at
   -60 dBFS opens a speech window. Measured 2026-07-31: an idle mic track with
   median level 0.00028 still produced a "Grazie" every 15 s.

   (Do not repeat the claim, which stood here until 2026-08-05, that RNNoise
   "scores spectral shape, not level". It is wrong: RNNoise keeps the first
   cepstral coefficient — energy — and deliberately skips cepstral mean
   normalisation, so its features are level-DEPENDENT. The indifference comes
   from training on "audio at all realistic levels". Source: Valin,
   jmvalin.ca/demo/rnnoise. The distinction matters: the energy term exists to
   compensate for a *trained* behaviour, which is why the industry instead
   thresholds a level-robust model score — Silero at a fixed 0.5 — and carries
   no energy term at all. See the note below.)

   **The floor is NOT `ENERGY_FLOOR`.** Measured 2026-08-05 over 38 real
   captures with confirmed transcripts (~17k speech frames): `ENERGY_FLOOR`
   (0.015) sits 12 dB INSIDE the speech distribution and rejected 37% of real
   speech frames. Two genuine quiet dictations carried 10 ms and 0 ms of
   qualifying audio in their entire length and would have been discarded whole.
   `CHUNK_GATE_ENERGY_FLOOR` (0.0025) is the geometric mean of the gap between
   the noisiest room (0.0018) and the quietest speech (0.0037) in that dataset.
   Re-measured with `chunk_vad_ab` afterwards: 17 windows, 3 dropped, and all
   three were genuinely empty (whisper on the whole window produced "" or
   "Grazie."). Median retention 100%.

   **Known limit.** That margin is only ~6 dB and it has to span SESSIONS —
   within any one recording speech stands ~24 dB over its own noise. A fixed
   absolute energy threshold is the fragile part of this design. Adaptive
   noise-floor tracking (Martin's minimum statistics, Cohen's MCRA) is the
   textbook answer but its documented weakness is adaptation lag of one to two
   search windows, and the windows carry no state between them, so it
   would be wrong at the start of every dictation. The industry answer is to
   stop thresholding energy at all and threshold a level-robust model score
   instead: faster-whisper and whisper.cpp both ship Silero with a FIXED 0.5
   probability threshold and no energy term. That is what the code does now:
   Silero answers first (see above), and this energy floor remains only in the
   fallback gate. Measured with the `vad_ab` bin over 300 windows of 12 real
   meeting mic tracks: 62 hallucinations passed by the RNNoise+energy gate
   against 19 by Silero, zero words lost by either.
2. **`preprocess_made_it_worse` is NOT used here.** It treats "retained < 5 %"
   as a collapse, which on a 15 s window is any utterance shorter than 750 ms.
   The gate has its own rule instead: if the VAD found at least
   `CHUNK_MIN_SPEECH_MS` (200 ms) of speech, the whole window passes.
3. **When the VAD finds less than 200 ms, `sustained_energy_fraction` decides.**
   Fraction of 10 ms frames over the floor: under 10 %
   (`CHUNK_SUSTAINED_ENERGY_FRACTION`) it is transients (clicks, a chair) and
   the chunk is dropped; at or over 10 % the VAD probably misjudged real speech
   and the whole window is handed over. Measured 2026-07-31: real speech
   40-60 %, the offending idle mic 4 %.

**Whisper's own `no_speech_probability()` does NOT work as a filter here — do
not re-add it.** It is the standard second net (OpenAI's reference decoder and
faster-whisper both threshold it at 0.6), and it was wired up and measured on
2026-07-31: over 45 segments of two real meetings it never exceeded 0.00002,
and the hallucinated "Grazie a tutti" segments reported exactly 0.00000.
Whisper is most confident precisely when it is inventing, so a confidence
filter cannot see hallucinations. It was removed as dead code. `suppress_nst`
is still set (one free line). Silence has to leave the AUDIO before whisper
sees it.

A/B measured the same evening (2026-07-31, with the trim version of the chunk
path that the gate later replaced), same machine, same model, six minutes
apart, only the `preprocessing_enabled` toggle differing:

| | toggle ON | toggle OFF |
|---|---|---|
| silent chunks blocked before whisper | 21 | 0 |
| phantom "Grazie a tutti" in the transcript | 0 | 3 |

The `sustained_energy_fraction` fallback never fired in either run, i.e. the
VAD never emptied a window that actually held speech.

**Why cloud is highpass-only (BUG B):** Groq/OpenAI/Deepgram run their own
VAD + normalization server-side. Ours is redundant and can only degrade —
on a quiet mic our VAD trimmed the speech and dagc amplified the residual
noise to clipping, so a 45 s dictation transcribed to "Ah!". Cloud now uses
the same safe path as file-load. See known-bugs.md AUDIO-004.

**Make-it-worse guard (LOCAL path).** `process_buffer_guarded` runs the full
pipeline, then checks `preprocess_made_it_worse(input, output)`: if a
clearly-speech input (`rms > ENERGY_FLOOR`) collapsed to near-nothing (empty,
< 5 % of samples retained, or output RMS < 5 % of input RMS) it falls back to
highpass-only. This is the user's rule — *preprocessing must HELP, never make
audio worse than raw* — enforced in production, not just in tests. It is
deliberately conservative: a normal 40–60 % VAD trim never trips it, so it
does NOT alter validated dictations; it is a floor, not a quality knob.

### Capture-ratio invariant (BUG A)

At dictation stop, `dimmy_stop_recording` compares captured audio seconds
(`buffer.len() / rate`) to elapsed recording seconds (from a start `Instant`).
Mic-mode dictation should land ~100 %; a low ratio means the capture path
silently dropped samples (the Mix AEC ring dropped ~60 % — see AUDIO-004).

- Runs when elapsed > 3 s, no streaming session was active (Deepgram, OpenAI
  realtime or Nemotron: `streaming_final.is_none()`) and no meeting is active.
  Telemetry
  (`dictation.capture_ratio`, bucketed `lt_50` / `50_85` / `85_95` / `ge_95`)
  is sent for EVERY such stop; on ratio < 0.85 a **WARN log** is added. It is
  intentionally **WARN, not `assert!`** — a shortfall is device/load-dependent
  (a slow BT-HFP mic on a busy box), not a logic bug, and crashing over it
  would be worse than the drop.
- The guard needs the start `Instant` that `dimmy_start_recording` stores
  (`RECORDING_STARTED_AT`); without it nothing runs, so it is inert in the
  `test-ffi` injection harness. The bucket arithmetic
  (`telemetry::sanitize::bucket_capture_ratio`) is unit-tested in
  `telemetry::events` (`capture_ratio_bucket_boundaries`).
- Meetings have the sibling guard at stop in `meeting.rs::worker_loop`
  (`meeting.capture_ratio`, active time > 5 s, same 0.85 WARN line).

## Downsampling

- Source: 48kHz (or device rate)
- Target: 16kHz (Whisper's internal rate)
- Method: Anti-aliasing lowpass at 7kHz (Butterworth, 0.4375 × target rate) + linear interpolation (`preprocess::downsample_to`; `downsample_to_16k` is the 16 kHz wrapper)
- Cloud: happens in `ProcessedAudio::to_wav_payload()` via `downsample_to_16k()`
- Local batch: happens in `transcribe::stt_input_16k()`; the live chunk workers (`chunked_stt.rs`, `meeting.rs`) call `downsample_to_16k()` per window

## Chunked Transcription (transcribe.rs)

For recordings that exceed provider file size limits:

1. `estimate_wav_size()` — O(1) calculation without encoding
2. If under limit → single request (pass-through)
3. If over limit → `split_at_silence(max_chunk_samples)`
4. Each chunk transcribed sequentially, results joined with spaces
5. The progress callback makes `dimmy_stop_recording` emit a `chunk_progress` event (`{"current","total"}`) on the event callback

`max_chunk_samples` comes from `transcribe::cloud_chunk_samples`: 80 % of the
provider limit (at 32 000 bytes/s of 16 kHz WAV), never under 5 s.

### Provider File Limits (`Provider::max_file_bytes`)
- Deepgram: 2 GB (basically unlimited)
- Gemini (inline): 20 MB
- Fireworks, Together: 100 MB
- Every other provider (Groq, OpenAI, OpenRouter, Anthropic, Custom, …): 25 MB

### split_at_silence()
- Searches backwards in last 25% of chunk for silence boundary (RMS < 0.01, 300ms window)
- Force-splits at max boundary if no silence found
- Post-condition assertion: total samples in = total samples out

### Timeout Scaling
- Formula: `30s + wav_data.len() / (1024 * 1024)`, capped at 600s
- Applied by `timeout_for_payload` on every provider path (OpenAI-compatible, Deepgram incl. its word-timestamp variant, Gemini)

## Audio Debug

When audio debugging is enabled (`audio_debug_enabled`), each dictation saves to `{Dimmy config dir}/audio_debug/{YYYY-MM-DD_HH-MM-SS}/` (`create_debug_session_dir` in `lib.rs`, written by `dimmy_stop_recording`):
- `raw.wav` — Raw audio from the capture buffer (before preprocessing)
- `processed.wav` — After the preprocess route (at the capture sample rate, NOT downsampled)
- `metadata.json` — timestamp, device, sample rate, raw sample count, peak amplitude, preprocessing flag, input gain, STT mode / provider / model, language, LLM style and tone, the transcript or the error, duration

Old sessions are pruned by the background prune thread that `dimmy_init` starts: `prune_audio_debug_dirs(dir, 7, 500 MB)` (7 days, 500 MB total).

The processed WAV is at the capture sample rate (48 kHz), not 16kHz. Downsampling only happens in `to_wav_payload()` (cloud) or `stt_input_16k()` (local), which create the data sent to the STT engine.

## nnnoiseless Requirements

- Requires exactly 48kHz input (if device rate differs, VAD is skipped)
- Frame size: 480 samples (10ms)
- Input range: [-32768.0, 32767.0] (we scale from [-1.0, 1.0])
- Returns voice probability [0.0, 1.0] per frame
- RNN-based: state can drift on very long recordings (hence energy floor fallback)

## Capture source: Mic for dictation, Mix for meetings

Pre-2026-05, the user could pick `AudioSource = Mic | System | Mix`.
Post-`3eddac3`, the pill + meeting paths forced `AudioSource::Mix`
unconditionally. **That was walked back for DICTATION on 2026-07-01**
(BUG A, see known-bugs.md AUDIO-004): dictation now captures
`AudioSource::Mic` — the mic callback writes straight to `audio_buffer`,
no AEC ring, no worker, no drop. **Meeting mode keeps `AudioSource::Mix`**
(it genuinely needs AEC to cancel the loopback far-end echo).

Why the split: dictation has no loopback echo to cancel, so Mix bought
nothing and cost ~60 % of the audio when the AEC ring overflowed under
load (measured 2026-07-01: 48 kHz stereo mic + the NN denoiser that then
still ran upstream of AEC3). Meetings do mix in remote
participants, so they need the AEC3 path. The `AudioSource` enum is kept
on disk for backward-compat with old `config.json`; the Rust runtime
picks the source per path (`dimmy_start_recording` = Mic,
`dimmy_meeting_start` = Mix; the stored `audio_source` is ignored by both).

Where the meeting's two streams come from:

- **System audio.** Windows: cpal WASAPI endpoint loopback on the output the
  call app plays to, else the default output (`audio.rs::loopback_target` +
  `resolve_loopback_device`, call devices from `capture_sessions.rs`).
  macOS / Linux: the host pushes samples through `dimmy_push_loopback_audio`
  (`AudioCommand::PushLoopback`). Both are resampled to 48 kHz and written to
  `audio_buffer_secondary` and to the AEC reference ring.
- **Mic.** Follows the system default input. It leaves that device only when
  it delivered digital zeros for `MIC_SILENCE_MS` (4 s), no other app captures
  from it, and exactly one other device is being captured
  (`audio.rs::mic_decision`). A pinned device is only reopened.

**Failure-mode safety** (load-bearing — guarded by
`worker_processes_mic_when_ref_ring_empty`): if the loopback ring stays
empty (no default output, BT routed away in HFP profile, headset
unplugged mid-meeting), the AEC worker zero-pads the reference frame
rather than blocking on lockstep mic+ref drain. Pre-`3eddac3` this
class of setup hung the audio buffer forever.

## AEC3 in Mix mode

See `core/src/aec.rs` for the implementation. Headline:

- 10 ms frames @ 48 kHz mono (480 samples). cpal callbacks PUSH samples
  into mic + ref ring buffers; the AEC worker DRAINS in lockstep.
- Bounded rings (`MAX_RING_SAMPLES = 48_000`, 1 s headroom). Overflow
  drops oldest samples and AEC resyncs via its delay estimator —
  better than unbounded growth.
- Pipeline: `aec3::pipelines::linear` (HPF + NS + AGC + linear AEC
  filter). Output is mic minus speaker echo.
- No neural denoiser runs in this worker. One sat upstream of AEC3 (RNNoise
  via nnnoiseless, or DeepFilterNet3 under `local-dfn`) until 2026-08-10
  (commit `a7eb2594`), when it was removed. `dfn.rs` / `dfn3.rs` and the
  `dfn::DENOISE_ENABLED` flag are still in the tree, but only the
  `denoise_offline` compare tool uses them; there is no FFI setter and no
  Settings toggle for it.
- `initial_delay_ms(120)`; if the pipeline cannot be built the worker falls
  back to `run_passthrough` (mic forwarded unchanged, no echo cancellation).

## Noise suppression (GTCRN) — compiled in, off by default

`core/src/gtcrn.rs` (cargo feature `denoise-gtcrn`) is the only noise
suppressor left. It works on the 16 kHz STT input, never on the capture
chain, so the audio on disk is never filtered.

- **Switch:** the environment variable `DIMMY_GTCRN`. Exactly `1` turns it on;
  unset, empty or any other value means off (`gtcrn::denoise_enabled_from`).
  There is no config field. It was default-on from 2026-08-10 to 2026-08-31.
- **Where it runs when on:** `transcribe::stt_input_16k`, i.e. the batch local
  paths (dictation stop, file load, meeting re-transcribe). The live chunk
  workers (`chunked_stt.rs`, the meeting transcription thread) do not call it.
- **Live variant:** `gtcrn::live` denoises during capture; armed by
  `dimmy_start_recording` only for local STT on the `Raw` route.
- **Failure mode:** model file missing or an error → the audio passes through
  unchanged (`maybe_denoise_16k`). Model: `gtcrn_simple.onnx`, looked up next
  to the executable, then in the model directory.
- Measured 2026-08-31 on 70 real dictations (`core/src/bin/denoise_ab.rs`):
  38 transcripts identical; over the other 32 the denoised audio lost 33 % of
  the punctuation and 53 % of the capitalisation. See `gtcrn::denoise_enabled`.

## Taskbar amplitude — dual-source (Win)

`TaskbarService` polls both `dimmy_get_amplitude()` (mic) and
`dimmy_get_loopback_amplitude()` (system) at 12 Hz and draws
`max(mic, sys)` on the taskbar progress bar. So the bar reacts to
remote-participant audio even when the local mic is silent — the
free VU meter that's visible when the pill is hidden.

## Voice processing chain: what is applied, and when

**The first diagram is MEETING capture (Mix mode).** The mic runs through
the AEC worker (`aec.rs`), whose output is the "cleaned mic" buffer
(`audio_buffer`). Wave glyphs below are stylised:
`∿`=voice, `····`=background noise, `▂▂`=low rumble, `▁▁`=quiet, `██`=too loud.

```
 ┌─ SOURCES (MEETING — captured in "Mix mode") ─────────────────────────────┐
 │   MIC (voice + room noise)                  SYSTEM loopback (PC audio)    │
 │   ∿∿∿ ···· ▂▂                               ∿∿∿                          │
 └──────┬───────────────────────────────────────────┬──────────────────────┘
        │ aec_mic_ring                               │ aec_ref_ring + RAW copy
        ▼                                            │  (audio_buffer_secondary)
 ╔══ AEC WORKER (thread always running; fed only in Mix mode, aec.rs) ══╗
 ║                              echo reference ◄──────────────────╨─ (system)
 ║ (1) HIGH-PASS   cut below ~80 Hz (rumble/pop)    ▂▂∿∿ -> ∿∿
 ║ (2) AEC3        subtract speaker echo from mic (ref = system loopback)
 ║ (3) NS          WebRTC noise suppression (residual)
 ║ (4) AGC2        auto gain to target level        ▁▁->▅▅ ; ██->▆▆
 ╚════════════════════════════════╤══════════════════════════════╝
                                  ▼
                       ┌──────────────────────┐
                       │  "CLEANED MIC" buffer │  (output of 1-4)
                       └──────────┬───────────┘
                                  ▼
 MEETING : CLEANED MIC(1-4) + RAW system  ->  MIX (soft-limit) + 3 tracks
           (the Ogg/WAV tracks on disk are never preprocessed. With
            preprocessing on + local STT, each transcription window passes
            the chunk gate first: whole window or nothing, see "Chunk paths")
```

**DICTATION** (Mic-only since 2026-07-01) skips steps 1-4 entirely:

```
 MIC -> audio_buffer (raw, 48 kHz)
        ├─ LIVE (while speaking), one of:
        │    cloud streaming (Deepgram / OpenAI realtime WebSocket)
        │    Nemotron streaming
        │    chunked_stt worker: a 5 s or 15 s window + 500 ms overlap;
        │      chunk gate when preprocessing on, else no filtering
        │    -> text typed live at the cursor / live captions
        └─ AT STOP: (5) PREPROCESS  route-aware (preprocessing_enabled, def OFF)
             off   -> raw
             cloud -> high-pass ONLY
             local -> VAD+AGC (guarded)
             -> STT -> final text

 FILE LOAD: no capture  ->  high-pass ONLY (no AGC, protects long files)  -> STT
```

Net effect since 2026-07-01: **dictation skips steps 1-4 entirely** (Mic-only
capture, no AEC worker) and applies only the route-aware step 5 at stop —
off (the default) gets raw audio, cloud gets highpass-only, local gets guarded
VAD+AGC. Steps 1-4 apply to MEETING capture (Mix mode). This removed the old
double-processing on dictation (two NS + two AGC + two high-passes) that could
over-pump quiet speech. Step 5 is gated by `preprocessing_enabled`, the only
user toggle in this chain; the GTCRN denoiser has an environment switch only
(`DIMMY_GTCRN=1`). If meeting voice sounds over-processed/pumped, the suspects
in order are AGC2 (4), then the WebRTC NS stage (3).

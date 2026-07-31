# Audio Pipeline — Detailed Reference

This audio pipeline is shared across all native UI platforms (Windows WinUI3, macOS SwiftUI, Linux GTK4). The Rust core modules are identical; only the integration layer differs per platform.

## Pipeline Overview — dictation

```
Mic (cpal, 48 kHz mono)        → mic ring buffer
+ Loopback (cpal, native rate) → loopback ring buffer  [Mix mode, Win-only]
    │
    │  Mix mode: aec.rs worker drains 480-sample frames in lockstep,
    │            runs WebRTC AEC3 (mic = capture, loopback = render),
    │            pushes mic - speaker_echo to audio_buffer.
    │            If loopback ring is empty: zero-pad ref, never block.
    │  Mic-only mode: mic samples go straight to audio_buffer.
    ↓ stop_recording
RawAudio { samples, sample_rate }
    ↓ preprocess(enabled)
ProcessedAudio { samples, sample_rate }
    │
    ├── stt_mode == "local":
    │       ↓ downsample_to_16k()
    │       f32 16 kHz mono samples
    │       ↓ whisper-rs / parakeet / parakeet_fluid (Mac)
    │       String (transcript)
    │
    └── stt_mode == "cloud":
            ↓ estimate_wav_size() → chunking decision
            ↓ to_wav_payload() or split_at_silence() + to_wav_payload()
            WavPayload { data: Vec<u8> }
            ↓ transcribe_audio() or transcribe_chunked()
            String (transcript)
    │
    ↓ filler::remove_fillers() (if enabled)
    ↓ app_rules::resolve(captured_app_id) — optional style override
    ↓ optional LLM post-processing
    ↓ history::save() (auto-save, v2 schema)
String (final text) → paste
```

## Pipeline Overview — meeting mode (long-form)

The meeting worker runs alongside the live capture and consumes the
same `audio_buffer`, but with different chunking + persistence:

```
Mic + Loopback rings (same as dictation, AEC if Mix mode)
    ↓
audio_buffer (process-wide, ring-style)
    ↓ meeting worker (every ~100 ms)
    │
    │  Pause check: if paused, skip drain/write/STT this tick.
    │  Otherwise drain `meeting_chunk_secs` worth of samples
    │  (default 15 s). On resume: advance samples_written +
    │  last_processed past the paused window so it's excluded
    │  from audio.wav AND from the chunked timeline; emit a
    │  [paused] line in transcripts.txt at the seam.
    ↓
streaming WAV write (16 kHz mono int16) → audio.wav
+ chunked transcribe (same backend as dictation: cloud or local)
+ meta.json updated with last_chunk_ts
+ transcripts.txt appended (one line per chunk)
    ↓ stop or pill Stop while meeting active
LLM raw post-process (process_raw_prompt with the 11-section
                      structured-recap prompt; recap_model_override
                      first, URL heuristic fallback;
                      Anthropic Opus 4.7+ uses thinking.type=adaptive)
    ↓
recap.md + actions.json written
.recording marker deleted
```

The meeting `MEETING` static lives in `ffi.rs` independently of any UI
window — closing the UI doesn't stop the recording. UIs probe
`dimmy_meeting_is_active` on open and re-attach.

## Pipeline Overview — file load

`dimmy_transcribe_file` (drag-drop / picker → transcribe) uses a
**different preprocess path** (`preprocess::process_buffer_for_file_load`):

```
WAV / file bytes
    ↓ hound decode → f32 samples + sample_rate
    ↓ process_buffer_for_file_load(samples, sample_rate)
    │   - Clamp to [-1.0, 1.0] (NaN/Inf → 0.0)
    │   - 80 Hz highpass (skipped if sample_rate < 8 kHz)
    │   - NO VAD
    │   - NO AGC
    │   - Final NaN/Inf guard
    ↓
ProcessedAudio
    ↓ chunk if > provider limit, or single-shot to whisper/parakeet
    ↓
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

Three distinct types enforce pipeline ordering at compile time:
- **RawAudio**: Raw f32 samples from cpal. Can only go to `preprocess()`.
- **ProcessedAudio**: After preprocessing. Can estimate WAV size, split at silence, or encode to WAV.
- **WavPayload**: Ready-to-send WAV bytes. Immutable.

You cannot skip steps or mix types. This is enforced by the Rust type system.

## Preprocessing (preprocess.rs)

### Pipeline Steps (in order)
1. **Clamp**: Input clamped to [-1.0, 1.0]. NaN/Inf → 0.0.
2. **Highpass**: 80Hz Butterworth 2nd order (biquad). Removes DC offset + low-frequency rumble.
3. **VAD**: nnnoiseless voice probability with hysteresis state machine.
4. **AGC**: dagc MonoAgc adaptive gain (target RMS 0.2, distortion 0.001).
5. **Clamp**: Output clamped to [-1.0, 1.0]. NaN → 0.0 (safety net for dagc bugs).

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

`process_buffer()` creates a FRESH preprocessor and calls `process()` ONCE with ALL samples. This means:
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

### Chunk paths: VAD trim only (since 2026-07-31)

The table above is the route for the **batch** buffer at `dimmy_stop_recording`.
The realtime chunk workers — `chunked_stt.rs` (dictation) and `meeting.rs` (one
15 s window per track) — used to hand whisper the raw buffer, silence included,
which is the input it hallucinates YouTube sign-offs on ("Grazie", "Thank you").
They now call `preprocess::process_chunk_vad_only` whenever the same
`preprocess_route(..)` returns `Full`, i.e. preprocessing on AND local STT.

That function is highpass + VAD, **never AGC**: dagc is adaptive, so a
per-chunk instance settles on a different gain per window (adjacent chunks come
out at different levels), and it NaNs on all-silence input (AUDIO-001) — which
is exactly what an idle chunk is.

Three chunk-only rules, all paid for on 2026-07-31 with a real meeting:

1. **A frame must clear `ENERGY_FLOOR` to count as speech.** nnnoiseless scores
   voice likelihood from spectral shape, not level, so a keyboard click at
   -60 dBFS opens a speech window. Measured: an idle mic track with median
   level 0.00028 (50x under the floor) still produced a "Grazie" every 15 s.
2. **`preprocess_made_it_worse` is NOT used here.** It treats "retained < 5 %"
   as a collapse, which on a 15 s window is any utterance shorter than 750 ms —
   a short "sì" would be handed to the model with all its surrounding silence.
   If the VAD kept anything, trust it.
3. **When the VAD empties a chunk, `sustained_energy_fraction` decides.**
   Fraction of 10 ms frames over the floor: under 10 % it is transients (clicks,
   a chair) and the chunk is dropped; at or over 10 % the VAD probably misjudged
   real speech and the untrimmed window is handed back. Real speech measures
   40-60 %, the offending idle mic measured 4 %.

**Whisper's own `no_speech_probability()` does NOT work as a filter here — do
not re-add it.** It is the standard second net (OpenAI's reference decoder and
faster-whisper both threshold it at 0.6), and it was wired up and measured on
2026-07-31: over 45 segments of two real meetings it never exceeded 0.00002,
and the hallucinated "Grazie a tutti" segments reported exactly 0.00000.
Whisper is most confident precisely when it is inventing, so a confidence
filter cannot see hallucinations. It was removed as dead code. `suppress_nst`
is still set (one free line). Silence has to leave the AUDIO before whisper
sees it.

A/B measured the same evening, same machine, same model, six minutes apart,
only the `preprocessing_enabled` toggle differing:

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

- On ratio < 0.85 (and elapsed > 3 s, not a meeting): **WARN log + telemetry**
  (`dictation.capture_ratio`, bucketed). It is intentionally **WARN, not
  `assert!`** — a shortfall is device/load-dependent (a slow BT-HFP mic on a
  busy box), not a logic bug, and crashing over it would be worse than the drop.
- The guard is inert in the `test-ffi` injection harness (no real capture
  timing); its arithmetic is unit-tested in `telemetry::sanitize` /
  `telemetry::events`.

## Downsampling

- Source: 48kHz (or device rate)
- Target: 16kHz (Whisper's internal rate)
- Method: Anti-aliasing lowpass at 7kHz (Butterworth) + linear interpolation
- Happens in `ProcessedAudio::to_wav_payload()` via `downsample_to_16k()`

## Chunked Transcription (transcribe.rs)

For recordings that exceed provider file size limits:

1. `estimate_wav_size()` — O(1) calculation without encoding
2. If under limit → single request (pass-through)
3. If over limit → `split_at_silence(max_chunk_samples)`
4. Each chunk transcribed sequentially, results joined with spaces
5. Progress callback emits `final-chunk-progress` Tauri events

### Provider File Limits
- Deepgram: 2 GB (basically unlimited)
- Gemini (inline): 20 MB
- Groq, OpenAI, OpenRouter, Anthropic, Custom: 25 MB

### split_at_silence()
- Searches backwards in last 25% of chunk for silence boundary (RMS < 0.01, 300ms window)
- Force-splits at max boundary if no silence found
- Post-condition assertion: total samples in = total samples out

### Timeout Scaling
- Formula: `30s + wav_data.len() / (1024 * 1024)`, capped at 600s
- Applied to all 3 provider paths (OpenAI-compatible, Deepgram, Gemini)

## Audio Debug

When audio debugging is enabled, each recording saves to `{config_dir}/dimmy/audio_debug/{timestamp}/`:
- `session_raw.wav` — Raw audio from cpal (before preprocessing)
- `session_processed.wav` — After preprocessing (at original sample rate, NOT downsampled)
- `metadata.json` — Sample rate, duration, device, chunk count

The processed WAV is at the original sample rate (typically 48kHz), not 16kHz. Downsampling only happens in `to_wav_payload()` which creates the data sent to the STT API.

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
load (48 kHz stereo mic + heavy NN denoise). Meetings do mix in remote
participants, so they need the AEC3 path. The `AudioSource` enum is kept
on disk for backward-compat with old `config.json`; the Rust runtime
picks the source per path (`dimmy_start_recording` = Mic, meeting worker
= Mix).

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
- DFN noise suppression upstream of AEC is wired but DEFERRED — see
  `dfn.rs` for activation criteria.

## Taskbar amplitude — dual-source (Win)

`TaskbarService` polls both `dimmy_get_amplitude()` (mic) and
`dimmy_get_loopback_amplitude()` (system) at 12 Hz and draws
`max(mic, sys)` on the taskbar progress bar. So the bar reacts to
remote-participant audio even when the local mic is silent — the
free VU meter that's visible when the pill is hidden.

## Voice processing chain: what is applied, and when

**This diagram describes MEETING capture (Mix mode).** DICTATION since
2026-07-01 is Mic-only: it skips the whole AEC worker (steps 1-5) and feeds
raw mic straight to `audio_buffer`, then applies the route-aware step 6 at
stop (highpass-only for cloud, guarded VAD+AGC for local). Meeting capture is
Mix, so the mic runs through the AEC worker (`aec.rs`), whose output is the
shared "cleaned mic" buffer. Wave glyphs below are stylised:
`∿`=voice, `····`=background noise, `▂▂`=low rumble, `▁▁`=quiet, `██`=too loud.

```
 ┌─ SOURCES (MEETING — captured in "Mix mode"; dictation = Mic-only) ────────┐
 │   MIC (voice + room noise)                  SYSTEM loopback (PC audio)    │
 │   ∿∿∿ ···· ▂▂                               ∿∿∿                          │
 └──────┬───────────────────────────────────────────┬──────────────────────┘
        │ mic_ring                                   │ ref_ring + RAW copy
        ▼                                            │  (buffer_secondary)
 ╔══ AEC WORKER (always ON while recording, aec.rs) ═════════════╗
 ║                              echo reference ◄──────────────────╨─ (system)
 ║ (1) DENOISE NN  RNNoise (nnnoiseless)   toggle DENOISE_ENABLED (def ON)
 ║       ∿∿ ····  ->  ∿∿        [DeepFilterNet3 if `local-dfn`, deferred]
 ║ (2) HIGH-PASS   cut below ~80 Hz (rumble/pop)    ▂▂∿∿ -> ∿∿
 ║ (3) AEC3        subtract speaker echo from mic (ref = system loopback)
 ║ (4) NS          WebRTC noise suppression (residual)
 ║ (5) AGC2        auto gain to target level        ▁▁->▅▅ ; ██->▆▆
 ╚════════════════════════════════╤══════════════════════════════╝
                                  ▼
                       ┌──────────────────────┐
                       │  "CLEANED MIC" buffer │  (common output of 1-5)
                       └───┬───────────────┬───┘
        ┌──────────────────┘               └──────────────────┐
        ▼ LIVE (while speaking)                                ▼ AT STOP (dictation)
   REALTIME / CHUNKED / DEEPGRAM                    (6) PREPROCESS  route-aware (def ON)
   reads buffer ~every 3 s                              cloud -> high-pass ONLY
   NO extra filtering                                   local -> VAD+AGC (guarded)
        │                                                    │
        ▼                                                    ▼
   text typed live at cursor                          STT -> final text

 MEETING : CLEANED MIC(1-5) + RAW system  ->  MIX (soft-limit) + 3 tracks
           (the WAV/Ogg tracks on disk are still raw — "preprocessing" does
            not change what is recorded. Since 2026-07-31 it DOES change what
            each 15 s chunk hands to the model: a VAD trim, see below)
 FILE LOAD: no capture  ->  high-pass ONLY (no AGC, protects long files)  -> STT
```

Net effect since 2026-07-01: **dictation skips steps 1-5 entirely** (Mic-only
capture, no AEC worker) and applies only the route-aware step 6 at stop —
cloud gets highpass-only, local gets guarded VAD+AGC. Steps 1-5 apply to
MEETING capture (Mix mode). This removed the old double-processing on
dictation (two NS + two AGC + two high-passes) that could over-pump quiet
speech. Step 6 is gated by `preprocessing_enabled`; DENOISE (1) is the only
other user toggle. If meeting voice sounds over-processed/pumped, the suspects
in order are AGC2 (5), then the two NS stages.

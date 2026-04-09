# Audio Pipeline — Detailed Reference

This audio pipeline is shared across all native UI platforms (Windows WinUI3, macOS SwiftUI, Linux GTK4). The Rust core modules are identical; only the integration layer differs per platform.

## Pipeline Overview

```
Mic (cpal, 48kHz mono) → Raw samples buffer
    ↓ stop_recording
RawAudio { samples, sample_rate }
    ↓ preprocess(enabled)
ProcessedAudio { samples, sample_rate }
    │
    ├── stt_mode == "local":
    │       ↓ downsample_to_16k()
    │       f32 16kHz mono samples
    │       ↓ whisper-rs transcribe_local()
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
    ↓ optional LLM post-processing
    ↓ history::save() (auto-save)
String (final text) → paste
```

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

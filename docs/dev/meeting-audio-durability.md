# Meeting audio durability — why capture and transcription are separate threads

> **The rule:** nothing may stand between captured audio and the disk.
> Everything else in `core/src/meeting.rs` follows from that sentence.
> Pinned in `CLAUDE.md` under "THE AUDIO RULE"; guarded by
> `core/src/meeting.rs` `mod audio_never_blocked`.

## Why the rule exists

Recording is the only step that cannot be redone. A transcript regenerates
from the file, a recap re-runs, a summary gets rewritten. Audio that was
never written is gone, and the meeting with it.

## The incident (2026-09-02)

A real 34-minute meeting produced an 11-minute file.

```
meta.json duration_secs = 2057.2   (34.3 min)   ← what was recorded
audio.ogg               =  662.7   (11.0 min)   ← what reached the disk
transcripts.txt ends at =  660     (11.0 min)   ← consistent with the file
ratio                   = 0.32
```

The transcript was not truncated: it covered 100 % of the audio that
existed. **23 minutes of conversation were never written.**

### Timeline, from `dimmy.log`

```
15:41:35  whisper_full (samples=248000)          normal, 16 s
15:41:51  whisper_full returned Ok               last successful chunk
15:41:51  [Meeting/diag] elapsed=718.7  samples_written=19,695,840  chunks=33
          ...  22 minutes with NO diag line (it prints every 5 s)  ...
16:00:27  [Audio] Loopback diag: total_samples=84,497,568 peak=0.61   capture is FINE
16:00:32  (stop requested)
16:02:32  [Meeting] stop join TIMED OUT — worker wedged
16:02:32  [Meeting] stopped duration=0.0s chunks=0 err="stop timed out"
16:04:10  [Meeting/diag] elapsed=2056.9 samples_written=31,808,640  chunks=33
```

### What the numbers prove

1. **The worker loop stopped iterating for 22 minutes.** The diag prints
   every 5 s; there are exactly two lines 22 minutes apart.
2. **It was not stuck in whisper.** No `whisper_full` was logged in that
   window at all — the next one is at 16:09:44. *This is the key fact:*
   "make STT faster" would not have prevented it, and neither would a
   timeout around the model call.
3. **Capture was healthy throughout.** The audio thread kept logging real
   signal (`peak=0.61`) until stop.
4. **`stop()` gave up.** Its bounded join expired after 120 s and returned
   a partial result. The audio the worker had not yet written was still in
   RAM, and the buffer was gone by the time the worker came back.
5. `last_processed` advanced by exactly one chunk (720,000 samples) across
   the whole stall — one loop iteration, 22 minutes.

The exact cause of that one slow iteration was never identified, and
**does not matter**: the loop wrote the audio *and* ran transcription *and*
called back into the host. Anything slow in any of those stalls all three.

### Why nothing warned

The capture-ratio guard exists (`ratio < 0.85` → WARN + telemetry) and
would have flagged 0.32 — but it only runs at stop, and stop had already
timed out. In the whole log that line appears twice, both for dictations,
never for a meeting.

## The design

```
   cpal callbacks
        │  append
        ▼
   ┌──────────────────────┐
   │ audio_buffer(s)      │  Arc<Mutex<Vec<f32>>>
   └──────────┬───────────┘
              │ read + copy out
              ▼
   ┌──────────────────────────────────────────┐
   │ worker_loop        (capture thread)      │
   │  • writes audio.ogg / _mic / _system     │
   │  • advances samples_written              │
   │  • pause gate, diagnostics               │
   │  • extracts a window, hands it away      │
   │                                          │
   │  NEVER: a model, the network, the host   │
   └──────────┬───────────────────────────────┘
              │ sync_channel(STT_QUEUE_DEPTH), try_send
              ▼
   ┌──────────────────────────────────────────┐
   │ stt_thread_loop    (transcription)       │
   │  • VAD, downsample, whisper/parakeet/    │
   │    cloud                                 │
   │  • transcripts.txt, dedup, accumulators  │
   │  • emit_event("meeting_chunk") → host    │
   │                                          │
   │  May block, wedge or die. Nothing above  │
   │  this line waits for it.                 │
   └──────────────────────────────────────────┘
```

### The four properties

| Property | Mechanism |
|---|---|
| Capture never waits on transcription | `try_send`, never `send` |
| A slow machine costs transcript, not audio | Full queue → window dropped + user told |
| A wedged transcriber cannot grow memory | `STT_QUEUE_DEPTH = 4` (~24 MB of PCM) |
| A wedged transcriber cannot cost the recording | Sinks are **finalized before** the join |

### Stop ordering — the part that matters most

```rust
// 1. loop breaks on `cancelled`
// 2. writer.finalize() / writer_mic / writer_system   ← audio is now a valid file
// 3. drop(stt_tx)                                     ← transcriber drains and exits
// 4. join_bounded(stt_handle, 90 s)                   ← abandoned if wedged
// 5. read transcripts.txt, build the result
```

Step 2 before step 4 is the whole guarantee. The Ogg trailer / WAV header
rewrite is what makes the recording seekable and complete; doing it first
means a wedged transcriber costs the tail of the transcript and nothing
else.

### The final window is capped

On cancel the worker used to transcribe *everything* remaining in one
slice. If the transcriber had fallen minutes behind, that is a
hundreds-of-MB allocation handed to a model that cannot use it anyway. It
is now capped at `4 × chunk_samples`; the excess is logged and the user is
pointed at Regenerate transcript. The cap never triggers when the
transcriber is keeping up.

## When the machine cannot keep up

This is not treated as a fault. The core emits, **once per meeting and
while it is still running**:

- telemetry `meeting.transcription_behind { engine, elapsed_bucket }`
- event `meeting_transcription_behind { engine, elapsed_secs }`

Both hosts turn it into a toast that names the actual choice:

| engine behind | suggestion |
|---|---|
| `whisper` | Parakeet, or a smaller model |
| `parakeet` | a cloud provider |
| `cloud` | check the connection, or go local |

The message says the recording is safe and continues, because it is — and
because the alternative reading ("Dimmy is broken") is what makes people
stop a meeting that was recording perfectly well.

Cost model worth knowing: whisper pads any input under 30 s to a full 30 s
encoder window, so the per-call cost is nearly fixed. A 15 s chunk costs
about what a 30 s one does, and Mix mode runs two calls per window. On a
machine where each call takes ~16 s, the worker is already at ~2× realtime
deficit and only survives on the silence gaps.

## FREEZE invariants

- `worker_loop` may **not** call a model, the network, or
  `crate::ffi::emit_event`. If you are adding something to it, the
  question is not "is this fast?" but "can this ever block?" — anything
  short of a provable no belongs on the transcription thread.
- The handoff stays `try_send` on a **bounded** channel. Switching to
  `send`, or to an unbounded channel, silently restores the coupling
  (the first blocks, the second turns a slow machine into an OOM).
- Audio sinks are finalized **before** the transcriber is joined.
- The transcription thread owns `transcripts.txt` outright. Two writers on
  that file would interleave lines mid-meeting.

## Regression guards

`core/src/meeting.rs` `mod audio_never_blocked` drives the real primitive
(`sync_channel` + `try_send`) against a consumer that is slow, wedged, or
dead:

| test | pins |
|---|---|
| `a_wedged_consumer_never_blocks_the_producer` | 500 windows in < 1 s behind a consumer stuck for 30 s |
| `a_dead_consumer_never_blocks_the_producer` | a panicked transcriber does not stall or kill capture |
| `a_consumer_that_keeps_up_loses_nothing` | the happy path is unchanged: 200/200 delivered, in order |
| `dropping_the_sender_ends_the_consumer` | stop's unblock mechanism, independent of the join timeout |
| `the_queue_is_bounded_and_small` | memory stays bounded under a wedge |

## Still open

- The capture-ratio guard only runs at stop, so it cannot fire when stop
  itself times out. A periodic version during recording would have caught
  this incident in its first minute.
- `primary_len` was observed shrinking from 31.8 M to 1.3 M samples across
  the stall. No code path in `meeting.rs` shortens that buffer, and
  neither `Start command received` nor `device-change auto-recovery`
  appears in the log for that meeting. Unexplained; it does not change the
  design above, but it is a loose thread.
- Nothing yet re-runs transcription automatically over the audio when
  windows were dropped. Today the user must press Regenerate transcript.

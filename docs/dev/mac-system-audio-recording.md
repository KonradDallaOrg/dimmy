# macOS system-audio recording & saving — as-built mechanism (FREEZE)

> **Why this doc exists.** The macOS meeting/system-audio path broke twice in
> hard-to-diagnose ways (Tahoe HAL freeze; the "voce 3× accelerata / a tratti"
> Bluetooth-HFP rate bug — recovered offline 2026-07-21, fixed 2026-07-22). This
> file pins the **exact mechanism by which Dimmy captures and saves meeting audio
> on macOS**, and the **two alarms** that catch a regression in the field. Read it
> before touching `SystemAudioProcessTap.swift`, `core/src/audio.rs` (loopback
> path), or `core/src/meeting.rs` (worker). Diff against this when Mac audio
> misbehaves. Companion to [`audio-pipeline.md`](audio-pipeline.md),
> [`known-good-baseline.md`](known-good-baseline.md) and
> [`known-bugs.md`](known-bugs.md) (MACOS-001/002/003).

## The shape in one paragraph

A macOS meeting captures **two independent sources** and writes **three WAVs**.
The **mic** is captured by the Rust core via cpal (device-native rate →
`LinearResampler` → 48 kHz). The **system audio** (the other participants) is
captured on the Swift side by a CoreAudio **process tap** and pushed into the
core as loopback frames. Both streams are canonicalised to **48 kHz** and the
meeting worker writes them in **lockstep** to `audio_mic.wav` (mic),
`audio_system.wav` (system, Mix mode only) and `audio.wav` (the mixed playback
track). The single thing that makes or breaks this on macOS is that the tap must
**actually deliver 48 kHz** — Windows gets that for free from WASAPI shared-mode;
macOS does not, which is the whole story below.

## 1. Capture — two sources

| Source | Who captures | Path | Rate handling |
|---|---|---|---|
| **Mic** | Rust core (cpal) | `core/src/audio.rs` | device-native SR → `LinearResampler` → 48 kHz; a default-input change triggers a heartbeat rebuild at the new rate |
| **System audio** | Swift (CoreAudio tap) | `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift` | tap → aggregate pinned to 48 kHz → `dimmy_push_loopback_audio` |

Dictation captures **mic only**; meeting captures **Mix** (mic + system). That
split is deliberate — see AUDIO-004 in `audio-pipeline.md`.

## 2. The macOS system-audio tap (the load-bearing part)

`SystemAudioProcessTap.start()` (`SystemAudioProcessTap.swift`):

1. Picks a single audio-active process and builds a **process tap**
   (`AudioHardwareCreateProcessTap`, `~L134-147`). No active process ⇒
   `.deferred` and the rescan listeners self-promote when audio appears
   (`~L120-129`).
2. Reads the tap stream format (`~L150-156`) and creates a **private aggregate
   device** anchored to the current default output (`~L173-210`). Two invariants,
   both paid in blood:
   - `kAudioAggregateDeviceTapAutoStartKey: false` + an explicit `AudioDeviceStart`
     (`~L188`, `~L272`). On Tahoe (26.x) `TapAutoStart=true` registers the
     aggregate but the IO proc **never fires** (`samples=0`). Chromium uses `false`
     for the same reason.
   - Sub-tap **drift compensation** on, anchored to the default output as the clock
     (`~L188-197`). A tap-only aggregate (empty sub-device list) is created fine
     but its IO proc never fires.
3. **Pins the aggregate to 48 kHz** — THE fix for the 3×/chopped bug
   (`~L212-241`):
   ```
   kAudioDevicePropertyNominalSampleRate = 48_000  on the aggregate
   ```
   A Bluetooth-HFP output only clocks at **16 kHz**; a tap anchored to it delivered
   16 kHz of content while `readTapFormat` still reported the nominal **48 kHz**.
   The core trusted the claim, did passthrough, and the meeting worker **zero-filled
   the 2/3 shortfall** → audio 3× fast AND gated "a tratti", participants never
   transcribed. Setting the aggregate's nominal rate makes CoreAudio **SRC the HFP
   sub-device up to 48 kHz internally**, so the IO proc actually delivers 48 kHz and
   the core never guesses. If the set fails we keep the tap-format rate (previous
   behaviour) rather than abort — a WARN, not a crash.
4. Publishes the (now canonical) rate to the core with
   `dimmy_set_loopback_sample_rate(sampleRate)` **before** the first buffer
   (`~L241`), so the meeting worker sizes the `audio_system.wav` header + STT
   downsample against what we actually deliver.
5. IO proc forwards every frame to `onSamples` → `dimmy_push_loopback_audio`
   (the realtime block captures by value, no `self`, no alloc on the mono fast
   path, `~L245-264`).

**Rebuild on device change.** The rescan listeners
(`kAudioProcessPropertyIsRunning` + default-output change, `~L293-355`,
`~L441-469`) tear down and re-`start()` the tap when the audio-active set or the
default output flips (headphones plug/unplug). Because the 48 kHz pin lives
**inside `start()`**, every rebuild **re-pins to 48 kHz** — that is what makes the
fix survive mid-meeting device juggling. Teardown runs off-thread
(`teardownQueue`, `~L615-627`) — the MACOS Tahoe HAL-freeze fix (v0.6.66).

## 3. Rate handoff to the core

- `dimmy_set_loopback_sample_rate(sr)` — the tap's claimed source rate; published
  once per (re)build.
- `dimmy_push_loopback_audio(ptr, len)` — f32 PCM frames; each push also carries
  the current rate (the **rate on the push wins** if it ever differs from the
  pre-published one).
- In `core/src/audio.rs`, `AudioCommand::PushLoopback` handles the frames; the
  loopback stream is canonicalised to `MEETING_CANONICAL_RATE = 48_000`.

## 4. The meeting worker — lockstep save (`core/src/meeting.rs`)

The worker (`fn run…`, `~L659+`) writes source-rate samples and tracks how far
each track has been flushed with `samples_written` (`~L662`). Per tick
(`~L964-999`):

- `audio_mic.wav`  = AEC-cleaned mic @ primary device rate — full meeting
  duration.
- `audio_system.wav` = raw loopback @ system rate (Mix only) — full duration,
  **zeros while paused**.
- Both windows are copied with **`slice_or_zeros(buf, samples_written,
  buf_len_now)`** (`~L515-532`): it copies exactly the `[samples_written,
  buf_len_now]` window and **zero-fills only a short tail** if the buffer hasn't
  caught up. Both tracks are the same canonical rate so they stay sample-aligned.
- `audio.wav` is the mixed playback track.
- Pause/resume advances `samples_written` past the paused window so the gap
  doesn't stretch the timeline (`~L261`, `~L899-937`).

Save layout on disk (one dir per meeting, `<config>/meetings/<id>/`):

```
audio.wav          mixed playback track
audio_mic.wav      mic (post-AEC)
audio_system.wav   system/participants (Mix only)
transcripts.txt    [ts ms] [speaker] text   (one line per STT chunk)
meta.json          duration, chunk_count, …
recap.md / actions.json   (post-process output)
```

Playback resolution tries `audio*.ogg` then `audio*.wav` for bases
`audio` / `audio_mic` / `audio_system` — so shipping WAVs is fine; do **not**
leave a stale `.ogg` next to a corrected `.wav` (the `.ogg` wins).

## 5. The two regression alarms (how we catch a silent break)

The mechanism is defended by **measured-vs-claimed** checks that fire in the
field instead of silently shipping distorted audio:

1. **Meeting capture-ratio guard** — `meeting.rs` `~L1255-1287`, at finalize.
   Compares audio actually written (`samples_written / rate`) against the **real
   active recording time** (elapsed − paused). Healthy ≈ 1.0; the HFP 3× bug is
   ~0.33. `< 0.85` ⇒ `[Meeting] WARN capture ratio …%` + telemetry
   `Event::MeetingCaptureRatio { ratio_bucket }` (`meeting.capture_ratio`,
   buckets `lt_50|50_85|85_95|ge_95`). WARN-not-assert: rate drift is
   device-dependent, not a logic bug, and crashing at stop would lose the whole
   recording. Gated on `> 5 s` active so startup jitter can't mis-bucket.

2. **Loopback-rate mismatch alarm** — `audio.rs` (`reconcile_loopback_rate`,
   alarm-only). Measures delivered samples vs claimed rate over a window; if they
   disagree it logs a WARN and emits `Event::LoopbackRateMismatch { claimed,
   measured }` (`audio.loopback_rate_mismatch`) **once per window**. It does
   **not** override the rate (the host-side 48 kHz pin owns the correction) — it
   is the canary that the pin failed on some device.

**What to watch in the log** after each device change during a meeting:
`[SystemAudio/tap] aggregate pinned to 48000 Hz` should reappear, and neither
`[Audio/loopback] WARN … MEASURED …` nor `[Meeting] WARN capture ratio …` should
fire.

## 6. FREEZE invariants — do not break without a reproduced bug + test

- The aggregate **must** be pinned to 48 kHz inside `start()` (so it re-applies on
  every rebuild). Removing it reintroduces the BT-HFP 3× bug.
- `TapAutoStart=false` + explicit `AudioDeviceStart` — do not "simplify" to
  auto-start (Tahoe: zero samples).
- Publish the rate (`dimmy_set_loopback_sample_rate`) **before** the first push,
  and again after the pin.
- The worker writes with `slice_or_zeros` in lockstep; both tracks share the
  canonical 48 kHz. Do not desync them.
- The two alarms are the field safety net — keep them and their telemetry events.
- Teardown stays off the main thread (Tahoe HAL freeze).

## 7. Tests (present + to extend)

Present: `slice_or_zeros` unit tests (`meeting.rs` `~L1588-1614`), the capture-ratio
bucket + `MeetingCaptureRatio`/`LoopbackRateMismatch` telemetry unit tests
(`core/src/telemetry/events.rs`), and `reconcile_loopback_rate` pure-fn tests
(`audio.rs`). The Swift tap itself is not unit-testable off a Mac — its guard is
`SelfTests.runAtLaunch` + the two runtime alarms above.

To add later (tracked): a fixture-driven meeting-worker test that feeds a
16 kHz-labelled-as-48 kHz loopback stream and asserts the capture-ratio guard
buckets it `lt_50` (reproduce the 3× bug at the worker level, host-independent).

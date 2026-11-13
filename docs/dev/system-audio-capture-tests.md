# Test coverage — `feat/system-audio-capture` branch

> Hardening pass added 2026-05-09 after the branch landed a sequence of
> regression-grade bugs in production. Every category here corresponds
> to a real shipped issue (or near-miss) — read the **Why this exists**
> notes before deleting/relaxing any of these.

## What this branch changed (one line each)

The branch evolved over three weeks and 50+ commits. The risky surface
area, in dependency order:

| Area | Module(s) | Risk |
|---|---|---|
| Always-mix capture (today: the meeting always opens `AudioSource::Mix`; dictation opens `AudioSource::Mic`, `ffi.rs::dimmy_start_recording`) | `audio.rs`, `aec.rs` | cpal multi-stream COM apartment, AEC ref starvation |
| File-load preprocess | `preprocess.rs::process_buffer_for_file_load` | AGC NaN on long silence stretches (97 % of audio destroyed) |
| Meeting pause/resume | `meeting.rs`, `ffi.rs` | atomic state, paused-window gap exclusion |
| Local STT routing | `meeting.rs::stt_thread_loop` (was in `worker_loop` when the branch landed) | hardcoded backend silently produced empty transcripts |
| Anthropic adaptive thinking | `llm.rs::process_raw_prompt` | 400 invalid_request when sending budget_tokens to Opus 4.7 |
| Pill ↔ Meeting lifecycle | `App.xaml.cs`, `PillWindow.xaml.cs`, `MeetingWindow.xaml.cs` | UI feedback / state ownership / recap pipeline plumbing |
| FFI surface additions | `dimmy_meeting_pause/_resume/_is_paused`, `dimmy_start_recording` rc=-7 | C# host depends on these |
| Settings UI dropdown | `SettingsWindow.xaml`, `SettingsViewModel` | recap-model field round-trip |

## Tier 1 — Rust unit tests (`cargo test --lib`)

### `preprocess::tests::file_load_*` — 8 tests

Lives in `core/src/preprocess.rs`. Regression coverage for the 2026-05-08
file-load AGC NaN bug.

- `file_load_empty_input_returns_empty`
- `file_load_preserves_sample_count` — no VAD trim, no AGC expansion
- `file_load_replaces_nan_inf_with_zero`
- `file_load_clamps_extreme_values` — input outside `[-1, 1]` clamped
- `file_load_handles_zero_sample_rate` — assert! must panic
- **`file_load_long_silence_does_not_corrupt_subsequent_audio`** — the
  exact bug AGC introduced
- `file_load_skips_highpass_for_low_sample_rates`
- `file_load_works_at_common_sample_rates` — 8k/16k/22.05k/44.1k/48k/96k

A ninth `file_load_*` test was added to the same module after this pass:
`file_load_highpass_streamed_matches_whole_buffer`.

**Why this exists:** before commit `0ed682b` the live preprocess pipeline
(highpass + AGC) ran over the entire file load buffer. dagc emits NaN on
near-zero stretches, the post-AGC clamp turns NaN into 0, and from that
point the AGC's *internal gain state* is corrupted and outputs NaN
forever. End result: 97 % of a 95-min meeting WAV became silent zeros,
Parakeet emitted empty transcripts for 186 of 191 chunks.

### `llm::tests` model-dispatch tests — 7 tests

Lives in `core/src/llm.rs`. Coverage for the model-id → API-shape dispatch
that decides whether a call uses Anthropic adaptive thinking or the
legacy `budget_tokens` form, and whether extended thinking auto-enables
on Gemini Pro / Gemini 3.x.

- `gemini_native_url_detection`
- `anthropic_thinking_dispatch_flagship_models` — Opus / Sonnet 4 / Sonnet 5 / Fable 5
- `anthropic_thinking_dispatch_skips_haiku_and_sonnet3`
- `anthropic_adaptive_thinking_only_for_new_models` — Opus 4.7 / 4.8, Sonnet 5+, Fable 5
- `anthropic_dispatch_combinations_match_routing_rule` — sanity invariant
- `gemini_thinking_dispatch_pro_and_3x`
- `case_insensitive_model_matching` — the helpers match lower-case ids
  only; the caller lowercases

**Why this exists:** before commit `9729ca4` the recap path sent
`thinking.type=enabled` + `budget_tokens` to Opus 4.7, which now requires
`thinking.type=adaptive`. Anthropic returned 400
`invalid_request_error: "thinking.type.enabled is not supported for this
model"`. The dispatch helpers `anthropic_uses_adaptive_thinking` /
`anthropic_wants_thinking` are the load-bearing decision; if a future
model pin ever drifts the rules, these tests catch it.

### `aec::tests` — 6 tests

Lives in `core/src/aec.rs`.

- `push_to_ring_caps_at_max` — unbounded growth guard
- `drain_frame_returns_false_when_short`
- `drain_frame_consumes_exact_size`
- **`worker_processes_mic_when_ref_ring_empty`** — load-bearing for
  always-mix safety; before commit `3eddac3` the worker blocked on
  lockstep mic+ref drain, so any setup without active loopback (no
  default output, BT routed away, silent system) hung the audio buffer
  forever
- `worker_processes_mic_with_ref_present` — symmetric case
- `worker_honours_shutdown_signal` — promptly exits, doesn't hang

### `ffi::tests::meeting_pause_no_op_without_meeting` + `start_recording_blocked_when_meeting_active` — 2 tests

Lives in `core/src/ffi.rs`. FFI return-code contract:

- `dimmy_meeting_pause` / `_resume` / `_is_paused` return 0 (no-op) when
  no meeting is active — NOT -1 (reserved for lock failure).
- `dimmy_start_recording` returns -7 when a meeting is in flight,
  preventing pill dictation from corrupting the meeting capture.

## Tier 1.5 — Rust integration tests (`cargo test --test`)

### `core/tests/meeting_pause_resume.rs` — 5 tests

The whole file is gated on the `test-ffi` cargo feature
(`#![cfg(feature = "test-ffi")]`): without it the file compiles to no tests.

End-to-end coverage for the pause/resume feature:

- `pause_resume_no_op_when_no_meeting_active` — FFI surface
- `pause_resume_idempotency_via_session` — `MeetingSession::pause()`
  returns true on first call (state flipped), false on second
  (already paused); same for `resume()`. Includes 5-cycle stress.
- `ffi_signatures_callable` — links + invokable; defends against
  signature drift between Rust pub fn and C# p-invoke.
- `stop_while_paused_does_not_deadlock` — `stop()` returns within
  2 s when called on a paused session, and clears the capture gate.
- `a_meeting_that_lost_the_buffers_never_touches_them_again` — added
  later: a stopping meeting whose worker wakes up after the next meeting
  claimed the capture buffers must leave them alone.

### `core/tests/parakeet_long_file.rs` — 1 test

`diagnose_long_file_chunks`, gated on the `local-stt-parakeet` feature.
Diagnostic-style test that reproduces `dimmy_transcribe_file` on a
real long WAV. Skips cleanly when the user's local fixture isn't
available; logs per-chunk RMS / peak / NaN counts. Used 2026-05-08 to
prove the AGC NaN bug; survives in tree as a regression early-warning
for future preprocess changes.

## Tier 2 — C# tests (xUnit, `dotnet test`)

### `Dimmy.Windows.Tests/ViewModels/SettingsViewModelTests.cs`

Existing test class. The recap-model and audio-source sections hold 8
test methods today (23 xUnit cases, because three of them are `[Theory]`):

- Recap-model override — 6 test methods for the recap-model picker
  round-trip through `LoadFromJson` ↔ `ToJson`:
  `RecapModelOverride_defaults_to_empty`,
  `LoadFromJson_parses_recap_model_override`,
  `LoadFromJson_recap_model_override_missing_keeps_empty`,
  `ToJson_includes_recap_model_override`,
  `RecapModelOverride_round_trips_through_json` (10 cases) and
  `LoadFromJson_migrates_stale_gemini_recap_ids` (5 cases). Catches the
  2026-05-08 dropdown regression where the field was momentarily stripped
  during a debug revert dance.
- `AudioSource_*` — 2 test methods (`AudioSource_default_does_not_break_load`,
  `AudioSource_round_trips_through_json` with 3 cases) for the `audio_source`
  config field. The Rust core still stores it and returns it in the config
  JSON, but does not use it to pick the capture source: a meeting always
  opens `AudioSource::Mix` and a dictation always opens `AudioSource::Mic`
  (`ffi.rs`). The view-model still reads/writes it for backward-compat with
  old config.json files.

## Tier 3 — Manual / FlaUI

The following surfaces aren't covered by automated tests yet — pre-PR
manual sweep:

| Area | What to verify |
|---|---|
| App rules drag-reorder | Open Settings → App rules → grab the grip (E76F glyph, leftmost column) → drag up / down. Source row dims, 2-px insertion line tracks slot boundary, ghost popup follows cursor with row snapshot, near-edge auto-scroll. **NB: built-in `CanReorderItems` is disabled — replaced with manual pointer-driven impl because WinUI 3 v3.1.7 hard-crashes the renderer on the built-in path. See [`known-bugs.md`](known-bugs.md) WIN-003.** |
| Meeting pause UI | Start meeting → click Pause → icon flips to play, label "Resume" → click → icon flips back. `transcripts.txt` contains a `[HH:MM:SS] [paused] (resumed after HH:MM:SS)` line at the seam. |
| Pill stop = meeting recap | Start meeting → close MeetingWindow → click pill Stop → pill shows Transcribing spinner ~10-30 s → recap.md and actions.json land in the meeting dir. If MeetingWindow is reopened during the recap, sidebar auto-selects the new row. |
| Sidebar Delete | MeetingWindow sidebar history → click trash icon on a row → confirms → meeting dir deleted from disk + row removed. |
| Taskbar amp dual-source | Run a meeting with system audio playing through speakers → taskbar progress bar reacts to the loopback signal even when mic is silent. |
| File-load Parakeet on long WAV | Drag a > 30-min WAV onto Settings → File load → all chunks transcribe (not just the first 5). |

## Running the suite

From `core/`:

```bash
# Unit tests (lib level)
cargo test --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan -- --test-threads=1

# Integration tests (meeting_pause_resume needs test-ffi; the command is the one in the file's header)
cargo test --test meeting_pause_resume --features test-ffi
cargo test --release --test parakeet_long_file --features local-stt-parakeet -- --nocapture --test-threads=1
```

From `platforms/windows/Dimmy.Windows.Tests/`:

```powershell
dotnet test --filter "FullyQualifiedName~SettingsViewModelTests"
```

## What is NOT covered (and why)

- **`MarkdownRenderer` / `TranscriptRenderer`** — depend on
  `Microsoft.UI.Xaml.Documents` types that require the WinUI runtime.
  Testable only inside the FlaUI tier or after extracting parsing into a
  pure helper.
- **`MeetingPostProcessService.RunRecapAsync`** — still calls into
  `MeetingWindow` (`PickRecapModelInternal`, `CalendarRosterLine`, on a
  partial XAML class) and the FFI layer, and has no test. Manual smoke
  covers the happy path. The static helpers (`BuildStructuredRecapPrompt`,
  `ParseStructuredRecap`) have since moved to
  `Helpers/MeetingRecapHelpers.cs` and are covered by
  `Dimmy.Windows.Tests/Helpers/MeetingRecapHelpersTests.cs`.
- **WinUI 3 ListView drag-reorder COM bug** — environmental
  (`combase.dll +0x37fc4 E_UNEXPECTED`), not addressable in our test
  pyramid. Tracked in `docs/dev/known-bugs.md` as WIN-003.
- **Windows-specific FFI rc=-7 path under real cpal recording** — the
  pill-block test uses an injected MEETING slot rather than firing a
  real audio thread. End-to-end coverage with cpal would require a
  virtual audio source (VB-CABLE) and is parked at tier 3.

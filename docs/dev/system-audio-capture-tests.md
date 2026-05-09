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
| Always-mix capture | `audio.rs`, `aec.rs` | cpal multi-stream COM apartment, AEC ref starvation |
| File-load preprocess | `preprocess.rs::process_buffer_for_file_load` | AGC NaN on long silence stretches (97 % of audio destroyed) |
| Meeting pause/resume | `meeting.rs`, `ffi.rs` | atomic state, paused-window gap exclusion |
| Local STT routing | `meeting.rs::worker_loop` | hardcoded backend silently produced empty transcripts |
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

**Why this exists:** before commit `0ed682b` the live preprocess pipeline
(highpass + AGC) ran over the entire file load buffer. dagc emits NaN on
near-zero stretches, the post-AGC clamp turns NaN into 0, and from that
point the AGC's *internal gain state* is corrupted and outputs NaN
forever. End result: 97 % of a 95-min meeting WAV became silent zeros,
Parakeet emitted empty transcripts for 186 of 191 chunks.

### `llm::tests::anthropic_*` / `gemini_*` — 6 tests

Lives in `core/src/llm.rs`. Coverage for the model-id → API-shape dispatch
that decides whether a call uses Anthropic adaptive thinking or the
legacy `budget_tokens` form, and whether extended thinking auto-enables
on Gemini Pro / Gemini 3.x.

- `gemini_native_url_detection`
- `anthropic_thinking_dispatch_flagship_models` — Opus / Sonnet 4 / 5 / 6
- `anthropic_thinking_dispatch_skips_haiku_and_sonnet3`
- `anthropic_adaptive_thinking_only_for_new_models` — Opus 4.7 / Sonnet 5+
- `anthropic_dispatch_combinations_match_routing_rule` — sanity invariant
- `gemini_thinking_dispatch_pro_and_3x`
- `case_insensitive_model_matching`

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

### `core/tests/meeting_pause_resume.rs` — 4 tests

End-to-end coverage for the pause/resume feature:

- `pause_resume_no_op_when_no_meeting_active` — FFI surface
- `pause_resume_idempotency_via_session` — `MeetingSession::pause()`
  returns true on first call (state flipped), false on second
  (already paused); same for `resume()`. Includes 5-cycle stress.
- `ffi_signatures_callable` — links + invokable; defends against
  signature drift between Rust pub fn and C# p-invoke.
- `stop_while_paused_does_not_deadlock` — `stop()` returns within
  2 s when called on a paused session.

### `core/tests/parakeet_long_file.rs` — 1 test

Diagnostic-style test that reproduces `dimmy_transcribe_file` on a
real long WAV. Skips cleanly when the user's local fixture isn't
available; logs per-chunk RMS / peak / NaN counts. Used 2026-05-08 to
prove the AGC NaN bug; survives in tree as a regression early-warning
for future preprocess changes.

## Tier 2 — C# tests (xUnit, `dotnet test`)

### `Dimmy.Windows.Tests/ViewModels/SettingsViewModelTests.cs`

Existing test class, extended with 13 new tests covering:

- `RecapModelOverride_*` — 6 tests for the recap-model picker round-trip
  through `LoadFromJson` ↔ `ToJson`. Catches the 2026-05-08 dropdown
  regression where the field was momentarily stripped during a debug
  revert dance.
- `AudioSource_*` — 2 tests for the now-dead `audio_source` config field.
  The Rust runtime ignores it post-`3eddac3` (always-mix), but the
  view-model still reads/writes it for backward-compat with old
  config.json files.

## Tier 3 — Manual / FlaUI

The following surfaces aren't covered by automated tests yet — pre-PR
manual sweep:

| Area | What to verify |
|---|---|
| App rules drag-reorder | Open Settings → App rules → drag a row up / down. **Known: WinUI 3 v3.1.7 + SentinelOne EDR can crash mid-drag (combase E_UNEXPECTED) — environment issue, not branch regression. Reboot resets COM state.** |
| Meeting pause UI | Start meeting → click Pause → icon flips to play, label "Resume" → click → icon flips back. `transcripts.txt` contains `[paused] (resumed after N ms)` line at the seam. |
| Pill stop = meeting recap | Start meeting → close MeetingWindow → click pill Stop → pill shows Transcribing spinner ~10-30 s → recap.md and actions.json land in the meeting dir. If MeetingWindow is reopened during the recap, sidebar auto-selects the new row. |
| Sidebar Delete | MeetingWindow sidebar history → click trash icon on a row → confirms → meeting dir deleted from disk + row removed. |
| Taskbar amp dual-source | Run a meeting with system audio playing through speakers → taskbar progress bar reacts to the loopback signal even when mic is silent. |
| File-load Parakeet on long WAV | Drag a > 30-min WAV onto Settings → File load → all chunks transcribe (not just the first 5). |

## Running the suite

From `core/`:

```bash
# Unit tests (lib level)
cargo test --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan -- --test-threads=1

# Integration tests
cargo test --release --test meeting_pause_resume --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan -- --test-threads=1
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
- **`MeetingPostProcessService.RunRecapAsync`** — calls into MeetingWindow's
  internal helpers (which live on a partial XAML class) and the FFI
  layer. The seams are too coupled to mock cleanly without a refactor.
  Manual smoke covers the happy path; the static helpers
  (`BuildStructuredRecapPrompt`, `ParseStructuredRecap`) are eligible
  for extraction in a follow-up.
- **WinUI 3 ListView drag-reorder COM bug** — environmental
  (`combase.dll +0x37fc4 E_UNEXPECTED`), not addressable in our test
  pyramid. Tracked in `docs/dev/known-bugs.md` if/when it becomes
  reproducible cleanly.
- **Windows-specific FFI rc=-7 path under real cpal recording** — the
  pill-block test uses an injected MEETING slot rather than firing a
  real audio thread. End-to-end coverage with cpal would require a
  virtual audio source (VB-CABLE) and is parked at tier 3.

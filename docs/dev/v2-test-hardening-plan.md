# v2 Test Hardening Plan — Phase 7+8 + meeting + waveform + icons

> **Status, checked against the code on 2026-11-11:** the plan below is
> mostly not implemented. Phase by phase:
> - **Phase 1 (`Invariant.Require` helper): not done.** No `Invariant`
>   class exists under `platforms/windows/` or `platforms/macos/`.
> - **Phase 2 (Tier 0 assertions): not done on the C# surfaces.**
>   `WavPeaks.cs::ReadPeaks` still returns `Array.Empty<float>()` for a
>   missing file or `bucketCount <= 0`; `IconExtractor.cs` and
>   `Win32DropTarget.cs` have no invariant checks (`Win32DropTarget`
>   only throws `ArgumentException` on a null hwnd). On the Rust side,
>   the `dimmy_transcribe_file` cloud branch returns `-6` / `-7` instead
>   of asserting and has no chunk-offset assertion;
>   `parakeet.rs::transcribe_with_word_timestamps` asserts only that the
>   PCM is finite (no JSON round-trip postcondition); the peaks code in
>   `ffi.rs` (`wav_peaks_streaming`, `compute_peaks_streaming`) asserts
>   `buckets > 0`.
> - **Phase 3 (Tier 1 unit tests): none of the planned tests exist.**
>   `platforms/windows/Dimmy.Windows.Tests/` has no WavPeaks,
>   IconExtractor (the file is only linked into the project),
>   Win32DropTarget, return-code or length-cache test;
>   `platforms/macos/DimmyTests/` has no WavPeaks test;
>   `core/src/parakeet.rs` has 4 unit tests (window size, bundle dir,
>   vocab) and none for the BPE word splitter.
> - **Phase 4 (Tier 2 integration tests): not done.** There is no
>   `core/tests/file_load_cloud.rs` and no `core/tests/parakeet_ts.rs`;
>   no `httpmock` dependency (the HTTP mock in `core/Cargo.toml` is
>   `wiremock`); no `parakeet_jfk_word_timestamps_align_with_audio`; no
>   synthetic `WM_DROPFILES`, icon-extraction or concurrent-appender
>   test. The fixtures `synth_24bit_stereo.wav`, `synth_f32_mono.wav`
>   and `truncated.wav` do not exist. What does exist:
>   `core/tests/fixtures/jfk_16k_mono.wav`;
>   `v2_ffi.rs::transcribe_file_rejects_cloud_mode_without_credentials`
>   (checks only that the return code is negative);
>   `parakeet_e2e.rs` (JFK transcript, no word timestamps);
>   `repro_peaks_crash.rs` (manual repro of `dimmy_compute_audio_peaks`,
>   skips without `DIMMY_REPRO_OGG`).
> - **Phase 5 (Tier 3 FlaUI): not done.**
>   `platforms/windows/Dimmy.Windows.UiTests/` holds only
>   `OnboardingSmokeTests.cs` (5 tests) and `LegalDialogTests.cs`
>   (2 cases). There is no drop, file-pick, app-rule icon or
>   meeting-caption test, and no `DIMMY_FAKE_MIC` hook. Those tests run
>   in the `windows-ui-smoke` job of `e2e-tests.yml`.
>
> The statements below that describe the code as it was when the plan
> was written are corrected in place where they are now wrong.

> **Goal:** turn the Phase 7+8 hot-fix surfaces (icons, drop, waveform,
> file-load, meeting captions, word timestamps) into a tested, asserted,
> regression-proof foundation BEFORE we layer more features on top.
>
> **Philosophy:** mandatory per `CLAUDE.md`:
> - Negative-space programming — `assert!()` (NOT `debug_assert!()`) preconditions
>   and postconditions in production code
> - TDD — failing test first, then minimal fix
> - Cross-platform parity — every assertion holds on Win + Mac + Linux
>
> **Reference:** `docs/dev/testing.md` (tier definitions, fixture layout).

## Scope

Six new surfaces shipped in Phase 7+8 with zero coverage:

| # | Surface | Code | Risk |
|---|---------|------|------|
| 1 | WAV peaks reader | `Helpers/WavPeaks.cs` (Win; `ReadPeaksAny` now asks the core via `dimmy_compute_audio_peaks`), `Views/Meeting/WavPeaks.swift` (Mac) | Wrong peaks = wrong waveform; integer overflow on >2GB files; corrupt WAV crashes |
| 2 | Icon extraction | `Helpers/IconExtractor.cs` (Win) — SHGetFileInfo + GetDIBits + PNG | Cache poisoning, alpha loss, resource leaks (HICON/HBITMAP/HDC) |
| 3 | Win32 drop target | `Helpers/Win32DropTarget.cs` — WM_DROPFILES + UIPI bypass | Subclass-chain leaks, GCHandle-pinned delegate lifetime, COM init/uninit imbalance |
| 4 | File-load FFI cloud branch | `core/src/ffi.rs::dimmy_transcribe_file` | Cloud config leakage in error path, tokio runtime panic, chunk offset drift |
| 5 | Word timestamps | `core/src/parakeet.rs::transcribe_with_word_timestamps` | Frame index off-by-one, BPE word boundary edge cases, JSON injection |
| 6 | Meeting live captions | `MeetingWindow.xaml.cs::OnMeetingChunkReceived` (Win, event-driven; `OnPollTick` no longer exists), `Views/Meeting/MeetingRecordingView.swift` + `MeetingViewModel.swift` (Mac; there is no `MeetingView.swift`) | File-share race with Rust appender (Win reads through `ReadWhileWritten`, `FileShare.ReadWrite \| Delete`), dir mis-detection |

## Test tiers (this plan's own numbering)

`docs/dev/testing.md` uses a different scheme (unit, tier 1 FFI, tier 1.5,
tier 2 Windows UI, tier 3 full install); the tiers below are not those.

- **Tier 0** — production assertions (`assert!()`, never `debug_assert!()`)
- **Tier 1** — pure-function unit tests (no I/O, no FFI)
- **Tier 2** — integration tests at FFI boundary, with file-system fixtures
- **Tier 3** — end-to-end UI: FlaUI on Win, XCTest UI on Mac

Per-surface coverage matrix:

| Surface | Tier 0 | Tier 1 | Tier 2 | Tier 3 |
|---------|--------|--------|--------|--------|
| WavPeaks | ✅ assert non-null path, sample-rate > 0, bucket > 0 | ✅ all 4 sample formats × stereo/mono | ✅ JFK fixture + 24-bit + IEEE float | ⏸ |
| IconExtractor | ✅ assert exe path exists, cache dir writable, png produced | ✅ cache key extraction, version sentinel eviction | ✅ extract from real exe (powershell, notepad) | ✅ FlaUI: settings → app rules → row image is non-default |
| Win32DropTarget | ✅ assert hwnd non-zero, OleInit returns S_OK | ⏸ (heavy COM mocking) | ✅ subclass install/remove leak count, message filter applied | ✅ FlaUI: shell SendInput drag → "X words. Saved" status |
| File-load cloud | ✅ assert provider URL non-empty when stt_mode != local | ✅ return-code mapping (-6/-7/-8) | ✅ fake provider via httpmock crate, chunk offsets, history.save called | ⏸ |
| Word timestamps | ✅ assert tokens.len > 0, frame indices monotonic | ✅ vocab → words splitter, JSON encoding edge cases | ✅ Parakeet bundle JFK fixture, end-to-end FFI round trip | ⏸ |
| Meeting captions | ✅ assert poll interval > 0, meetings dir exists | ✅ length-cache short-circuit logic | ✅ file-write race with concurrent appender (FileShare.ReadWrite) | ✅ FlaUI: start meeting → 20s of synthetic audio → caption text grew |

✅ = required, ⏸ = stretch, future PR.

---

## Surface-by-surface plan

### 1. `WavPeaks` (peaks reader)

#### Production assertions to add
```csharp
// In ReadPeaks(path, bucketCount):
assert(!string.IsNullOrEmpty(path));
assert(bucketCount > 0);
assert(File.Exists(path));      // currently silent-returns []
assert(channels > 0);
assert(bitsPerSample > 0);
assert(frameSize > 0);
// Postcondition before return:
assert(peaks.All(p => p >= 0f && p <= 1f));
```

`assert` is C# `Debug.Assert` only by default — wrap in a small
`Invariant.Require(cond, msg)` helper that throws
`InvariantViolationException` in Release too. Match Rust's `assert!()`.

#### Tier 1 — pure tests (no I/O)
- `ReadSample` (private) — round-trip 16-bit, 24-bit, 32-bit float, 8-bit unsigned
- Bucket math: `framesPerBucket = totalFrames / bucketCount` correctness
  for `total < bucketCount` (must produce ≤ totalFrames distinct buckets,
  never panic)

#### Tier 2 — fixture tests
Fixtures under `core/tests/fixtures/` (`jfk_16k_mono.wav` is used by
`parakeet_e2e.rs`; the FFI tests download `jfk.wav` into
`core/target/test-fixtures/` instead):
- `jfk_16k_mono.wav` (16-bit PCM)
- New: `synth_24bit_stereo.wav` (5 sec sine)
- New: `synth_f32_mono.wav`
- New: `truncated.wav` (intentionally cut RIFF header)

Test asserts:
- `ReadPeaks(jfk, 200).Length == 200`
- All values in `[0, 1]`
- Truncated header → empty result, no exception
- Concurrent reads (10 threads) produce identical output

#### Mac parity
Same fixtures, Swift port at `platforms/macos/Dimmy/Views/Meeting/WavPeaks.swift`.
XCTest exercises identical assertions.

---

### 2. `IconExtractor` (Win-side; Mac uses `NSWorkspace`)

#### Production assertions
```csharp
// EnsureCachedFromExePath(exePath):
Invariant.Require(!string.IsNullOrEmpty(exePath), "exePath empty");
Invariant.Require(File.Exists(exePath), $"exePath not found: {exePath}");
Invariant.Require(Directory.Exists(CacheDir), "cache dir gone");

// SaveHbitmapAsPng(hbmp, pngPath):
Invariant.Require(hbmp != IntPtr.Zero, "hbmp null");
Invariant.Require(width > 0 && height > 0, $"bitmap zero-sized {width}x{height}");
// Postcondition:
Invariant.Require(File.Exists(pngPath), "PNG write reported success but file absent");
Invariant.Require(new FileInfo(pngPath).Length > 0, "PNG is zero bytes");
```

Plus a **resource-leak check** wrapper: track HICON/HBITMAP/HDC handle
counts via `GetGuiResources`, assert post-extraction = pre-extraction.

#### Tier 1
- `StripExe(name)` — case + extension variants
- `CACHE_VERSION` sentinel eviction logic — `EvictStaleCacheVersion`
  with mocked `IFileSystem`

#### Tier 2 — real Win32 calls
- Extract from `cmd.exe`, `notepad.exe`, `explorer.exe` (always present)
- Verify the produced PNG:
  - Has alpha channel (`Bitmap.GetPixel(0,0).A < 255` somewhere on the
    edge of a typical app icon)
  - Is 256×256 or smaller (bound check)
  - Is parseable PNG (`Image.FromFile` doesn't throw)
- Idempotency: call `EnsureCachedFromExePath` 100×, assert PNG written ONCE

#### Tier 3 — FlaUI
- Open Settings → App rules
- Row for `cmd` (which is in default rules) renders an `Image`, not `FontIcon`
- After a fresh wipe of cache: warm-up renders rows within 5s

---

### 3. `Win32DropTarget`

#### Production assertions
```csharp
// Register():
Invariant.Require(_rootHwnd != IntPtr.Zero, "root hwnd null");
Invariant.Require(_subclassed.Count == 0, "Register called twice");

// WndProcHook (static):
Invariant.Require(hwnd != IntPtr.Zero, "wndproc hwnd null");

// Drop callback:
Invariant.Require(paths != null);
```

Resource-leak check: `_subclassed.Count` after `Register()` matches the
number of HWNDs in the chain; after `Unregister()`, all GCHandles
freed (`_subclassed.IsAllocated == false` for each, `_subclassProc == null`).

#### Tier 2 — synthetic message-pump test
Spin up a hidden HWND via `CreateWindowEx`, install our Win32DropTarget,
post a synthetic `WM_DROPFILES` message with a fake `HDROP` (built via
`DragQueryFile`-compatible test stub). Assert `_onDrop` fires with the
expected path list.

This is the right tier to catch:
- POINTL marshaling regressions (relevant if we ever swap WM_DROPFILES
  back for IDropTarget)
- GCHandle-pinned delegate lifetime
- `ChangeWindowMessageFilterEx` actually applied (verify via
  `GetWindowMessageFilterEx` after Register)

#### Tier 3 — FlaUI
Full E2E via:
```csharp
[Fact]
public async Task DropWavOntoSettings_TranscribesAndSaves()
{
    using var app = await Launch();
    var settings = app.OpenSettings();
    settings.WaitForFileLoadCard();
    Shell.DragAndDrop("tests/fixtures/jfk_16k_mono.wav",
                       settings.WindowRect.Center);
    var status = await settings.WaitForStatus(timeout: 30s);
    Assert.Contains("Saved to History", status);
}
```

`Shell.DragAndDrop` uses Win32 `SendInput` mouse-down+move+up sequence
on real explorer.exe → real Dimmy. Catches UIPI regressions, OLE
shim regressions, parser bugs.

---

### 4. File-load FFI cloud branch

#### Production assertions (as written in the plan; in the code today an incomplete cloud config returns `-6` and a tokio runtime failure `-7`, they do not assert)
```rust
assert!(!api_url.is_empty(), "cloud branch: api_url empty");
assert!(!api_key.is_empty(), "cloud branch: api_key empty");
assert!(total_secs >= 0.0);
// Postcondition:
assert!(transcript.is_finite());  // already
```

Add:
```rust
// Each chunk's offset must align with file timeline:
assert!(chunk_offset_secs <= total_secs,
    "chunk_offset_secs {} exceeds total_secs {}",
    chunk_offset_secs, total_secs);
```

#### Tier 1
- Return-code mapping: each `-6`, `-7`, `-8` arm produces the right
  error string in the C# layer (parse `dimmy_transcribe_file` rc → status)

#### Tier 2 — fake cloud provider
Use a localhost mock server that returns canned Whisper-JSON (the
dev-dependency available is `wiremock`; there is no `httpmock`). Test:
- Single-chunk happy path
- 3-chunk path (force chunking via small `max_wav_bytes`)
- 401 response → `-8` error code propagated
- Network timeout → `-8`

#### Tier 3 — none (would need a real provider key in CI)

---

### 5. Word timestamps (Parakeet)

#### Production assertions (today only an `assert!` that the input PCM is finite exists)
Add postcondition before returning JSON:
```rust
let parsed: serde_json::Value = serde_json::from_str(&json)
    .expect("self-emitted JSON must round-trip");
assert!(parsed.is_array());
let arr = parsed.as_array().unwrap();
for entry in arr {
    let s = entry["start"].as_f64().expect("start missing");
    let e = entry["end"].as_f64().expect("end missing");
    assert!(s >= 0.0 && e >= s, "invalid timestamp pair {} → {}", s, e);
}
```

#### Tier 1
- BPE word splitter on a synthetic `Vec<(token, frame)>`:
  - Single word `▁hello` at frame 10 → `[("hello", start=0.8s, end=total)]`
  - Continuation: `▁hello`, `▁world` at frames 10, 25 → 2 words with
    word boundaries at 0.8s and 2.0s
  - Special tokens `<unk>` etc. skipped, no orphan timestamp
- JSON escape: vocab pieces with `"` and `\` produce valid JSON

#### Tier 2 — Parakeet fixture
`#[ignore]` test (parakeet bundle 2.5GB, only run locally):
```rust
#[test]
#[ignore = "requires parakeet-fp32 bundle"]
fn parakeet_jfk_word_timestamps_align_with_audio() {
    let pcm = load_jfk_16k();
    let (text, json) = parakeet::transcribe_with_word_timestamps(&pcm).unwrap();
    let words: Vec<TimedWord> = serde_json::from_str(&json).unwrap();
    assert!(words.len() >= 5, "JFK transcript should have many words");
    // First word's start should be < 0.5s (JFK starts speaking immediately)
    assert!(words[0].start < 0.5);
    // Last word's end should be ≈ total duration
    let total = pcm.len() as f64 / 16000.0;
    assert!((words.last().unwrap().end - total).abs() < 0.5);
}
```

---

### 6. Meeting live captions

> **Out of date:** the 2 s polling timer (`OnPollTick`, `_pollTimer`,
> `_lastTranscriptLen`) this section is written against was replaced by
> the core's `meeting_chunk` event (`AppViewModel.MeetingChunkReceived` →
> `MeetingWindow.OnMeetingChunkReceived`); no length-cache short-circuit
> exists. The file-share concern survives in `ReadWhileWritten`
> (`FileShare.ReadWrite | FileShare.Delete`) for the initial seed. Read
> the items below as the original intent only.

#### Production assertions
```csharp
// OnPollTick:
Invariant.Require(_pollTimer != null, "tick fired with null timer");
Invariant.Require(_startedAt != default, "_startedAt unset");

// Inside file-read:
Invariant.Require(fi.Length >= _lastTranscriptLen,
    "transcripts.txt shrank — corruption or wrong dir");
```

#### Tier 1 — length-cache short-circuit
Pure unit: `ShouldRefresh(currentLen, lastLen) == currentLen != lastLen`.
Trivial; mostly a guard against accidental `>` instead of `!=`.

#### Tier 2 — concurrent appender
Spin up a `Task.Run` that appends to `transcripts.txt` every 200ms while
the polling timer reads it. Assert:
- No `IOException` ("file in use")
- Read content always parses as a sequence of `[N ms] text` lines

#### Tier 3 — FlaUI synthetic meeting
- Start meeting via tray menu
- Pipe synthetic 30s of speech audio into the system's default mic
  (or stub the audio capture via env var `DIMMY_FAKE_MIC=...`)
- Wait for `TranscriptText` to grow past placeholder
- Stop, verify recap was generated

---

## Implementation order

Sequenced so each phase de-risks the next:

1. **Add `Invariant.Require` helper** (Win + Mac) — replace `Debug.Assert`
   in production-critical paths. ~30 min.
2. **Tier 0 assertions across all 6 surfaces** — production guards.
   Ships behavioral changes (some `return ""` paths become throws). ~2h.
3. **Tier 1 unit tests** — pure logic only. WavPeaks bucket math, BPE
   splitter, return-code mapping, length-cache. ~3h.
4. **Tier 2 integration tests** — fixtures + httpmock + synthetic
   message pump. ~6h. The Win32DropTarget and IconExtractor tests are
   the highest-value tier for catching real regressions.
5. **Tier 3 FlaUI** — drop, file-pick, app-rule icons rendered, meeting
   captions visible. ~1 day. Run on every push to staging.

**Total estimate:** ~3 days of focused work, split between Win and
core. Mac side mirrors via XCTest.

## Acceptance criteria

- `cargo test --lib --features local-stt,local-llm` passes the unit
  tests; the integration tests are separate targets
  (`cargo test --test v2_ffi --features local-stt,test-ffi`, also run by
  the `integration-suites` job of `e2e-tests.yml`). Planned but not
  existing: `parakeet_ts.rs`, `file_load_cloud.rs`
- `dotnet test` from `Dimmy.Windows.Tests/` runs all tier 1+2 C# tests.
  CI gates on it (already does today, just expanding scope)
- FlaUI smoke test job covers the 3 tier-3 scenarios above. The job is
  `windows-ui-smoke` in `.github/workflows/e2e-tests.yml` (there is no
  `win-ui-smoke.yml`); it runs only the onboarding and legal-dialog
  tests today
- Every `assert!()` in production has a paired test that exercises the
  failure path (verifies the assertion *can* fire, then the test
  installs a fix path or marks `[ShouldFail]`)

## Out of scope (explicit non-goals)

- Mocking IShellItemImageFactory — too heavy; integration test is the
  right tier
- Mocking AVPlayer / MediaPlayerElement — same
- Mac Phase 7+8 UI testing (tracked separately in
  `2026-05-06-mac-phase7-8-handoff.md` — Mac engineer adds when wiring up)
- Testing the SVG/PNG visual quality (subjective; rely on user review)

## Tracking

This plan maps to TaskCreate items #114 onwards. Each surface gets
its own task; a task is "done" only when ALL three of (Tier 0
assertions, Tier 1 unit, Tier 2 integration) are green in CI.

# Mac parity handover — realtime typing + meeting track selector (2026-06-06)

Audited against `feat/local-realtime-typing` (commit `8f019de`) + recent `staging`.
That branch touched ONLY `core/` + Windows files, so the UI-layer features are
unmirrored on macOS. The Rust core is cross-platform: anything below the FFI
boundary the Mac dylib gets for free on rebuild.

**Do NOT redo these on Mac (already done or core-only):**

- **Meeting audio quality** (Vorbis q0.8 + soft-limit mix, `core/src/meeting.rs`)
  and **Gemma local prompt/sampling fix** (`core/src/local_llm.rs`) — pure core.
  Mac gets them automatically. No Swift work.
- **Local-model download progress** — Mac already handles `model_download_progress`
  / `llm_model_download_progress` (`DimmyCore.swift` ~1005/1012 → AppState; bars in
  `MacVoicePage.swift`, `MacOutputPage.swift`). AppState is a singleton so progress
  already survives a Settings reopen. ✅
- **Claude brand-mark fallback on the MCP card** — `MacIntegrationsPage.swift` already
  extracts the installed icon and falls back to bundled `Image("ClaudeMark")`. ✅
- **Settings live-apply toggles** — Mac already calls `persistConfig()` on toggle
  (not save-on-close). ✅
- **Tray pin (IsPromoted)** + **download-row layout** — Win-tray-specific. macOS uses
  the always-present menu-bar `StatusBarController`; no analog. N/A.
- **Audio device-change loopback-follow**, **recording-consent**, **stop-suggestion
  flapping**, **per-process tap** — already landed on Mac (commits 13272e5 / 4f5b3a5
  / 7e485da / PR #106). (MEMORY.md may still flag device-change as TODO — it's done.)

---

## GAP A + B — Realtime dictation typing at the cursor (HIGH, do these together)

**This is the whole point of the branch and is 100% missing on Mac** — neither the
older Deepgram cloud streaming nor the new local (whisper/Parakeet) typing types at
the cursor on macOS today; Mac only shows the caption overlay and pastes the whole
text at stop.

### Core contract (already shipped, cross-platform)
`dimmy_start_recording` (core/src/ffi.rs) emits `stt_chunk` events carrying
`{ delta, cumulative, is_final, engine }`. `engine` is one of:
- `"deepgram"` — cloud WS streaming (streaming_dictation ON + Deepgram key + STT is cloud)
- `"local-stream"` — local whisper/Parakeet typing (streaming_dictation ON + STT local)
- `"parakeet"` / `"whisper"` — plain chunked captions (display only, NOT typed)

`delta` is append-only/stable for the two typing engines (`local-stream` deltas are
already space-prefixed by the core, mirroring Deepgram).

### Windows reference (what to mirror)
- `ViewModels/AppViewModel.cs` stt_chunk handler: if `engine ∈ {deepgram, local-stream}`
  → set `StreamingDictationActive = true` and fire `StreamingSegmentFinalized(delta)`.
- `App.xaml.cs` `OnStreamingSegmentFinalized` → `TextInjectionService.TypeUnicodeText(seg)`
  (Unicode SendInput, NOT clipboard — avoids clobbering the user's clipboard per segment).
- Final-paste suppression: at the stop/paste site, `if (StreamingDictationActive) skip PasteText`.
- `StreamingDictationActive` reset to false on `recording_started`.

### Mac current state
- `Managers/DimmyCore.swift` `case "stt_chunk"` (~1042) reads only delta/cumulative/is_final
  → `appState.liveCaption*`. **Ignores `engine`; never injects.**
- `Managers/TextInjector.swift` is **clipboard-paste only** (setString + Cmd-V). Reusing it
  per-segment would thrash the clipboard — need a Unicode-keystroke path.
- Final paste fires unconditionally in `Managers/HotkeyManager.swift` (~585 and ~702) — no
  streaming suppression.
- `streaming_dictation` config key is **absent everywhere** on Mac (zero grep hits).

### Mac TODO
1. **Config (gates everything):** add `@Published var streamingDictation: Bool` to
   `State/AppState.swift`; read `config["streaming_dictation"] as? Bool` in the config
   snapshot apply, and emit it in the config-to-JSON path (next to `chunk_streaming_enabled`
   ~1352 / `live_captions_enabled` ~1312).
2. **Settings toggle:** add a "Realtime typing" toggle to `Views/Settings/MacVoicePage.swift`
   Advanced section (near the Chunk streaming / Live captions toggles), `persistConfig()` on
   change. (Match the Win labels: Accelerate transcription / Live captions (on screen) /
   Realtime typing.)
3. **Unicode typist:** add `func typeUnicode(_ text: String)` to `TextInjector.swift` using
   `CGEvent(keyboardEventSource:)` + `keyboardSetUnicodeString` posted to `.cghidEventTap`
   (parity with `TypeUnicodeText`). Chunk long strings if needed (CGEvent unicode buffer limit).
4. **Inject on chunk:** in `DimmyCore.swift` stt_chunk handler read `engine`; when it's
   `deepgram` or `local-stream`, set `appState.streamingDictationActive = true` and, for a
   non-empty `delta`, call `TextInjector.shared.typeUnicode(delta)` on the main actor.
   Leave caption-only engines (`parakeet`/`whisper`) flowing to the caption overlay as today.
5. **Suppress final paste:** guard both paste sites in `HotkeyManager.swift` with
   `if !appState.streamingDictationActive { ...inject final... }`.
6. **Reset:** `appState.streamingDictationActive = false` in the `recording_started` handler.

Caveat to surface in UI copy (already in Win tooltips): Parakeet is realtime; whisper keeps
up only on GPU/ANE. On Apple Silicon, Parakeet runs via FluidAudio (ANE, very fast) — local
realtime typing should feel great with Parakeet on Mac.

---

## GAP C — Meeting playback track selector (MEDIUM, small wiring)

Mac plays the **mix track only**; can't switch playback source. (It already draws the richer
dual-band mic/system waveform — keep that.)

- Win ref: `Views/MeetingWindow.xaml` `DoneTrackSelector` (Mix/Voice/System) +
  `MeetingWindow.xaml.cs DoneTrackSelector_SelectionChanged` swaps `DoneAudioPlayer.Source`
  between `audio.ogg` / `audio_mic.ogg` / `audio_system.ogg`.
- Mac: `Views/Meeting/MeetingDoneView.swift` (~449) `AudioPlaybackBar(url: vm.doneAudioURL,
  micURL:…, systemURL:…)`. VM already exposes `doneAudioMicURL` (~892) / `doneAudioSystemURL`
  (~896). `AudioPlaybackBar.load(url:)` already supports reloading.
- TODO: add a 3-way Picker/segmented control above the player that rebinds the playback `url`
  to mix/mic/system (default Mix). Rationale (same as Win): without headphones the mix carries
  the AEC/NS/AGC-processed mic on top of the clean loopback, so the system-only track sounds
  cleaner for system-audio content.

---

## Pre-flight reminder
Touching `platforms/macos/**` ⇒ run `scripts/dev/preflight-mac.sh` (builds + LAUNCHES the
app so `SelfTests` fire). If you add a new config field, check nothing in SelfTests pins the
toggle count. Adding the `streaming_dictation` field to AppState is config-only (no SelfTests
pin expected) but verify.

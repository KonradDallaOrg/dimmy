# Changelog

All notable changes to Dimmy are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Meetings: choose your own storage folder.** You can now set a custom
  destination directory for meeting recordings in Settings → Meetings.
  Every part of the app (the meeting list, playback, recap, Notion send,
  file-load-to-meeting, and the Claude Desktop MCP bridge) reads from the
  folder you pick. Existing meetings stay where they are — only new ones
  go to the new location. _Thanks to Ricca for the request._

## [0.6.37] - 2026-05-12

Mac side of the auto-update feature. The Win build has shipped
Velopack-driven silent updates since 0.6.33; macOS now reaches
feature parity via Sparkle 2.

### Added

- **Mac auto-update (Sparkle 2).** Integrated `Sparkle` via Swift
  Package Manager. New `UpdateService.swift` mirrors the Win UX:
  background check 5 s after launch, re-check every 6 h, silent
  download, install-at-quit prompt. About page picks up a real
  "Check for updates" button and a Stable / Prerelease channel
  picker. Channel preference persisted in `UserDefaults` under
  `dimmy.update_channel`. Replaces the placeholder
  `.disabled(true)` Toggle / Picker that shipped from 0.6.25 onward.
- **Release pipeline: Sparkle DMG signing + appcast.xml.**
  `release.yml` resolves the Sparkle SPM checkout, locates
  `sign_update`, signs the macOS DMG with the `SPARKLE_PRIVATE_KEY`
  GitHub secret, writes `appcast.xml` (with `<sparkle:channel>` set
  to `prerelease` for prereleases), and uploads it alongside the
  DMG. Graceful degradation: if the secret is unset (bootstrap or
  fork builds), the workflow warns and skips signing without
  failing the release.
- **Mac app version sync from `core/Cargo.toml`.** The Xcode project
  previously hard-coded `MARKETING_VERSION = 0.1.0`, so every shipped
  Mac DMG reported as v0.1.0 to the OS. `release.yml` now passes
  `MARKETING_VERSION` + `CURRENT_PROJECT_VERSION` from Cargo at build
  time. Without this, Sparkle would have offered every release as an
  "update" because it compares the running `CFBundleShortVersionString`
  against the appcast — and 0.1.0 < every published version.
- **`docs/RELEASING.md` — Mac auto-update bootstrap section.** Step
  by step for generating the Sparkle EdDSA keypair, populating
  `Info.plist` with the public key, and uploading the private key as
  a GitHub secret. One-time setup per repo.

### Fixed

- **Mac auto-update channel reset on change.** Switching the channel
  picker calls `SPUUpdater.resetUpdateCycle()` so the next scheduled
  check re-evaluates the appcast under the new channel filter,
  instead of inheriting the prior filter until the 6 h timer fires.

## [0.6.32] - 2026-05-10

Cross-platform Notion integration lands as the headline feature, plus
a stack of Win app-rules / meeting-state fixes that piled up on
`staging` after 0.6.31.

### Added

- **Notion integration (core + Win + Mac).** Send meeting recaps to a
  Notion page or database via the official REST API. Token stored in
  the existing `keys.enc` (AES-256-GCM); never written to
  `config.json`. Five new FFI exports — `dimmy_notion_set_token`,
  `_has_token`, `_test_connection`, `_search`, `_send_recap`.
  Settings → Integrations gains a summary card + 3-step Connect
  wizard (prepare → token → destination). The Done view picks up a
  Send-to-Notion button; if `notion_auto_send=true` the upload fires
  automatically after recap on both platforms.
- **Win theme centralization.** Single source of truth for accent
  colour + dark/light tokens, theme-aware popup menus, jump-list
  rename. Replaces the per-page inline `<Setter>` jungle.
- **Event-driven meeting state on pill (Win + Mac).** The pill no
  longer polls `dimmy_meeting_is_active` / `_is_paused` every 500 ms
  — the meeting worker now posts a Mac NotificationCenter notification
  / Win event each time state flips, and the pill subscribes. Same
  visual behaviour, zero idle CPU.
- **Mac no-FFI pollTick.** `MeetingPostProcessService` extracts the
  recap prompt + parser into `MeetingRecapHelpers`, mirrored by 16
  xUnit cases on Win. Mac `pollTick` no longer touches the FFI from
  the main run loop.
- **Win app-rules manual drag-reorder + 20 xUnit cases.** The reorder
  math is extracted into a pure function with deterministic tests so
  pointer-tracking math can't regress silently.

### Fixed

- **Win app-rules drag-reorder dead in WinUI.** The built-in
  `CanReorderItems` interaction silently no-ops inside the page-level
  `ScrollViewer` that hosts AppRulesListView. Replaced with a manual
  pointer-tracking implementation that handles drag-begin /
  drag-during / drag-end + drop-target hit detection itself. Twenty
  xUnit cases cover the index-shift math.
- **Win app-rules drag edge-scroll did nothing.** Auto-scroll
  introduced in PR #47 walked DOWN the visual tree from the ListView
  and found the ListView's own (disabled) inner `ScrollViewer`. Now
  walks UP via `FindFirstAncestor` to the page-level scroller and
  tests the cursor against `ViewportHeight` in scroller-local coords.
- **Win Notion `has_notion_token=false` after boot.** The FFI snapshot
  read missed the token-presence field on `dimmy_get_config_json`,
  so the summary card always showed "not connected" until the user
  re-pasted. Snapshot read now includes `has_notion_token` from the
  core state, not from the on-disk config.
- **STT chunked dedup overlap drift.** The old greedy character-match
  produced false positives on long meetings (chunk N+1 starting with
  a different word but containing fragments of chunk N). Rewritten
  as longest suffix-prefix overlap with an offset tolerance — single
  pass, deterministic, covered by new unit tests.
- **Mac Notion auto-send broke strict-concurrency.** `runRecap` runs
  from `DispatchQueue.global` / `Task.detached` but read
  `AppState.shared.notionAutoSend` (an `@MainActor @Published`
  property) directly. Now passed in as a `notionAutoSend: Bool`
  parameter snapshotted on MainActor by each of the three callers.
  Honest threading contract; build green.
- **Mac Done view toolbar visual weight.** The Regen / Recap / Copy /
  Notion / Folder buttons were bordered pills with 13pt monochrome
  SF Symbols — looked cheap. Replaced with borderless icon buttons
  using `symbolRenderingMode(.hierarchical)` + a soft hover fill,
  matching the Mail / Messages toolbar shape in macOS Tahoe.
- **Mac Notion send button now uses the real Notion mark** (from
  `Assets.xcassets/Providers/notion.imageset`) instead of the
  placeholder `link.badge.plus` SF Symbol.
- **Mac AudioPlaybackBar waveform chunkier + gradient.** 220 → 120
  buckets, bar width ≥ 2pt, gap 2pt, corner radius 2pt. Played /
  unplayed bars fill with vertical gradients (accent → 0.55 /
  secondary 0.55 → 0.25) and the playhead picks up a soft accent
  glow.
- **Mac recording-bar icon weight.** Pause / Stop / Back-to-live and
  the Mic / System waveform labels bumped from 11–12pt to 12–13pt
  with `.medium` / `.semibold` weights — they no longer read as
  system-default cheap glyphs.

## [0.6.31] - 2026-05-09

This release combines two parallel work streams that landed back-to-back:
the Win-side `feat/system-audio-capture` branch (PR #45, merged into
`staging` at `ef2f998`) and the Mac `staging-mac-v2-parity` port (PR #46).

### Added — cross-platform core (`feat/system-audio-capture`)

- **Always-mix capture architecture.** The pill and meeting paths force
  `AudioSource::Mix` everywhere; the Mic / System / Mix enum is dead at
  call sites (kept on disk for forward-compat with old `config.json`).
  AEC worker zero-pads the reference ring when loopback is empty so a
  silent system / no default output / BT routed-away never hangs the
  audio buffer (`core/src/aec.rs::worker_processes_mic_when_ref_ring_empty`).
- **Meeting pause/resume FFI.** Three new exports —
  `dimmy_meeting_pause`, `dimmy_meeting_resume`, `dimmy_meeting_is_paused`.
  Return-code contract: 1 = state flipped, 0 = no-op (already in target
  state, or no meeting active), -1 = lock failure. While paused, cpal
  callbacks keep filling buffers but the worker skips drain / WAV write /
  STT chunks; on resume the paused window is excluded from `audio.wav`
  and the chunked timeline, with a `[paused] (resumed after N ms)` line
  in `transcripts.txt` at the seam.
- **`dimmy_start_recording` rc = -7.** Silent-noop return code emitted
  when a meeting is active, blocking the pill dictation path from
  corrupting an in-flight meeting capture. C# / Swift hosts treat -7 as
  expected, not as an error.
- **WebRTC AEC3 acoustic echo cancellation** (`core/src/aec.rs`,
  Phase 1 of the meeting Mix mode). Pure-Rust `aec3 = 0.2` port of the
  WebRTC AEC3 algorithm. Mic frames as `capture`, loopback frames as
  `render` reference; output = mic − speaker echo. Operates on 10 ms
  frames at 48 kHz mono with bounded ring buffers (1 s headroom);
  worker sleeps in 5 ms ticks when not active (zero CPU when idle).
- **DeepFilterNet noise-suppression scaffolding** (`core/src/dfn.rs`,
  Phase 2). Module wired upstream of the AEC stage; activation deferred
  pending an upstream `deep_filter` crate that exposes the `tract`
  feature (or a swap to `deepfilter-rt` riding the existing `ort`
  runtime). The `local-dfn` feature is currently a no-op gate.
- **Per-process loopback scaffolding** (`core/src/process_loopback.rs`,
  Phase 5a, Windows-only). `list_meeting_processes()` enumerates known
  meeting apps via `Toolhelp32` snapshot; `auto_detect_meeting_pid()`
  picks one heuristically. The actual `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`
  capture path (`spawn_process_capture`) is a stub returning `Err` —
  caller falls back to default-output loopback. Lays the groundwork for
  the BT/HFP meeting-app case where standard loopback returns silence.
- **Anthropic adaptive-thinking dispatch** (`core/src/llm.rs`). Opus 4.7+
  and Sonnet 5+ now use `thinking.type=adaptive` (no `budget_tokens`)
  per Anthropic's API change; Sonnet 4 / 4.5 / 4.6 still get the legacy
  `budget_tokens` form. Helpers extracted for unit-testability:
  `anthropic_wants_thinking`, `anthropic_uses_adaptive_thinking`,
  `gemini_wants_thinking`, `is_gemini_native_url`. Caught a latent bug
  where `sonnet-6` fell through to plain budget mode.
- **File-load preprocess separate code path**
  (`preprocess::process_buffer_for_file_load`). Highpass-only — skips
  VAD + AGC. Fixes the 2026-05-08 file-load bug where dagc emitted NaN
  on long-silence stretches and corrupted 97 % of a 95-min WAV. The
  live mic path keeps full preprocess; only `dimmy_transcribe_file`
  uses the lighter pipeline. (See `known-bugs.md` AUDIO-001.)

### Added — Win UI (PR #45)

- **MeetingWindow lifecycle decoupled from recording.** Closing the
  window no longer calls `dimmy_meeting_stop`; state lives in the Rust
  `MEETING` static. Reopen probes `dimmy_meeting_is_active` and
  re-attaches polling, pause state, and active dir.
- **Pill Stop branches on meeting active.** When a meeting is in flight
  the pill Stop runs the recap pipeline through the new shared
  `MeetingPostProcessService` (transcribing spinner + auto-refresh of
  MeetingWindow if reopened). Otherwise normal dictation Stop runs.
- **Sidebar Delete on past meetings** with `ContentDialog` confirm +
  filesystem cleanup of the meeting dir.
- **Recap-model override dropdown** (Settings → Recap). Curated picker
  (Auto + Anthropic Opus 4.7 / Sonnet 4.6 / Haiku 4.5 + Gemini 3.1 Pro /
  2.5 Pro / 2.5 Flash + GPT-5 / GPT-4o + Custom) bound to
  `recap_model_override`; honoured by `process_raw_prompt` before the
  URL-heuristic fallback.
- **Taskbar amp = max(mic, system)** so the progress bar reacts to
  loopback even when the mic is silent.
- **MarkdownRenderer + TranscriptRenderer.** Block-level markdown for
  the recap card (bullets, numbered lists, sub-headings, blockquotes)
  + selectable transcript with playhead binding.
- **40 net new tests.** 23 Rust unit + 4 Rust integration
  (`core/tests/meeting_pause_resume.rs`) + 13 C# xUnit. Inventory and
  manual-sweep checklist in [`docs/dev/system-audio-capture-tests.md`].
  Caught + fixed the latent Anthropic-adaptive-thinking dispatch bug.

### Added — Mac UI (PR #46)

- **Mac MeetingWindow port.** SwiftUI rewrite of the meeting window with
  the full state machine (idle / recording / processing / done),
  translucent sidebar of past meetings (search + delete with `NSAlert`
  confirm), persistent recording bar (timer / chunks / Pause / Stop /
  Back-to-live), processing spinner with stepwise checkmarks, and the
  seven recap cards (TLDR + Context + Highlights + Narrative + Decisions
  + Topics + Actions + Open Questions + Risks + Next Steps + Followups).
- **Mac block-level markdown renderer.** Bullets (`- ` / `* `), numbered
  lists (`1. `), sub-headings (`### `), and block quotes (`> `) render
  via native SwiftUI shapes; inline (`**bold**`, `*italic*`, `` `code` ``)
  flows through `AttributedString.markdown`.
- **Mac waveform strip with click/drag seek.** New `AudioPlaybackBar`
  + `WavPeaks` (Swift port of Win's `WavPeaks.cs`). 220-bucket centre-
  mirrored amplitude bars in a SwiftUI `Canvas`; played portion fills
  with accent, unplayed stays in `macTextSecondary`. Single
  `DragGesture(minimumDistance: 0)` covers tap-to-jump + scrubbing.
  AVAudioPlayer-backed because AVKit's `VideoPlayer` triggered a
  SwiftUI layout-loop SIGABRT on audio-only assets.
- **Mac recap-model dropdown** in Advanced settings. Same curated list
  as Win, bound to `recap_model_override` in `config.json`.
  `pickRecapModel()` honours the override before URL-heuristic fallback.
- **Mac Pill ↔ Meeting routing.** Stop button on the pill branches:
  when a meeting is active, Stop spins up the recap pipeline through
  the new shared `MeetingPostProcessService`; otherwise the dictation
  toggle stop runs. A 500 ms `NSTimer` in `PillWindowController`
  mirrors `dimmy_meeting_is_active` / `_is_paused` into `AppState`.
- **Mac XCTest target + 69 unit tests.** New `DimmyTests` bundle wired
  into the pbxproj (productType `bundle.unit-test`, ad-hoc signing).
  Coverage: structured-recap prompt + parser + markdown round-trip,
  curated picker list integrity + resolve fallthrough, history-row
  title/subtitle pretty-printing, AppState recap_model_override
  round-trip, plus the existing AppState language/preset tests.

### Changed

- **Mac MeetingWindow lifecycle decoupled from FFI** (mirrors Win).
  Closing the window no longer stops the recording. Reopening probes
  `dimmy_meeting_is_active` and re-attaches.
- **Mac `AppDelegate` single-instance guard bypassed under XCTest.**
  `XCTestConfigurationFilePath` env var indicates the host is a test
  runner; without the bypass the runner would terminate before XCTest
  could attach if a dev-build instance was already running.
- **Cargo.toml dependency scoping fix.** Phase 5a's
  `[target.'cfg(target_os = "windows")'.dependencies]` block was
  silently swallowing `sentry`, `auto-launch`, `ed25519-dalek`, `sha2`
  and `clap` — TOML sections continue to the next header. Moved the
  Windows-only block to the end so cross-platform deps stay in
  `[dependencies]`. Verified with `cargo metadata`.

### Fixed

- **Mac dictation hotkey race vs in-flight meeting.** `HotkeyManager`
  handles `dimmy_start_recording` rc = -7 as a silent no-op instead of
  surfacing an error. Mac already pre-flighted with `meetingIsActive`;
  the rc = -7 branch closes the race where a meeting is started
  between the check and the call.
- **Mac meeting waveform = real audio, not random walk.** The first
  Mac parity push shipped with `MeetingViewModel.amplitudeTick()`
  driving the bars from `CGFloat.random(in:)` because the bridging
  header was missing `dimmy_get_loopback_amplitude` and a TODO had
  been left in place. Fixed: declare the loopback FFI in
  `DimmyFFI.h`, expose it via `DimmyCore.getLoopbackAmplitude()`, and
  rewrite `amplitudeTick` to poll mic + system FFI 12× per second
  through a display-AGC (`min(1, sqrt(raw) * 1.4)` — same formula Win
  uses) and push into a scrolling FIFO. New `DualBandWaveform` view
  renders the history mirrored: mic above the centre line, system
  audio below. 10 unit tests in `MeetingAmplitudeAGCTests` pin the
  formula + FIFO semantics so the regression cannot recur silently.
- **Mac empty-recording → meaningful done state.** Before: stopping a
  meeting that captured no speech showed "(recap not generated)" as
  if recap had been skipped. Now: `MeetingViewModel.stopAndProcess`
  detects the empty transcript and renders an explicit "Nothing was
  recorded" TLDR card with mic/permissions guidance.
- **Mac LLM-not-configured error → actionable message.** Before: recap
  failure surfaced `LlmRawError.notConfigured` raw description. Now:
  `MeetingPostProcessService.Failure.description` translates each rc
  into a user-actionable string ("Open Settings → LLM and add a
  key …", network-error → "check your network and API key", etc.).
- **Mac recap markdown — `####` headings + fenced code blocks.** The
  recap renderer collapsed level-4 headings into paragraph text and
  rendered code fences as literal triple-backticks. Refactored into a
  `MarkdownBlockParser` (state machine + line classifier) covering
  level 1–4 headings, fenced code blocks (with language tags),
  block quotes, dash/star bullets, numbered lists, and unclosed-fence
  recovery. 13 unit tests in `MarkdownBlockParserTests` lock the rules.
- **Mac pill mirrors meeting pause state.** The 500 ms poll in
  `PillWindowController.tickMeetingState` was already mirroring
  `dimmy_meeting_is_paused` into `AppState`, but the pill itself
  didn't consume it — the user paused from the meeting window and
  saw no visual change on the pill. Added an explicit
  `pause.circle.fill` + "Meeting paused" indicator inside the pill
  so the state is visible from any surface.

## [0.6.30] - 2026-05-07

### Added
- **Win MeetingWindow — full UI port from the standalone HTML mockup.**
  Browser-style sidebar of past meetings, idle / recording / processing /
  done state machine, recap cards with playhead-bound transcript, and
  the chrome that the system-audio-capture branch then layered pause/
  resume / sidebar Delete / pill routing on top of.

### Fixed
- Theme persistence across app restarts.
- App-rule override gate when no rule matches the foreground app
  (was applying the previous match).
- Selectable transcript + playhead behaviour.
- Pre-commit hook pipe-mask bug that let unformatted Rust through.

## [0.6.29] - 2026-05-07

### Added
- **CLAUDE.md "v2 surfaces" section** — 8-row table of new modules + FFI
  entries + UI mirrors so a fresh Claude session can find rules /
  history-v2 / file-load / meeting / parakeet / icons / taskbar without
  grep-archaeology.
- **Workflow `paths-ignore`** — doc-only / mockup / markdown commits no
  longer trigger the 30 min installer build (reverts the 0bd2209
  `cancel-in-progress` experiment that killed long-running builds).

### Fixed (PR #44 cherry-picks onto staging)
- **`SendInput` paste populates `wScan` with the scan code.** Without
  it Electron / Chrome / IME apps drop synthetic events that carry only
  `wVk`. Hardware drivers always set both — now we do too.
- **Toggle-mode race auto-recovery.** When the Rust core returns -2
  (already recording), the C# layer resets local state instead of
  bubbling the error.
- **Per-provider LLM key UX.** Badge refreshes when the LLM provider
  dropdown changes, surfacing the right key state for the new provider.
- **Rich AppContextCapture port from PR #44.** HWND tracking + focus
  drift detection so app-rule resolution doesn't flap when the user
  briefly tabs away during dictation.
- **Win Settings UI polish.** Rule-row reorder slim pattern, drop the
  match-type combo, lock model combo widths, strip em-dash AI-slop
  from labels.

## [0.6.28] - 2026-05-07

### Added
- **Onboarding Parakeet preload.** Onboarding now warms the Parakeet
  bundle so the first dictation chunk doesn't pay cold-start latency.

### Fixed
- **Mac v2-unified parity** — app rules + history v2 + file load +
  meeting all wired through Mac's SwiftUI surface (`feat/v2-unified`
  → Mac).
- **`get_config_json` exposes v2 retention + auto_recap fields.**
  C# / Swift / Linux UIs were reading 0 / false on round-trip.
- **Win Tests project missing files** + clippy `unsafe-doc` lint.
- **Parallel CI builds** (Win + Linux + Mac, no Mac gate). Brings Mac
  release into the same flow as the others.

## [0.6.27] - 2026-05-04

### Added
- **Long-form meeting mode (Win).** `core/src/meeting.rs` — streaming
  WAV (~115 MB / hour at 16 kHz mono int16), `transcripts.txt`
  per-chunk lines, `meta.json` with `last_chunk_ts`, `.recording`
  marker (deleted on clean stop) for crash recovery. At stop, the
  full transcript is sent to the LLM once for `recap.md` +
  `actions.json`. Identifiers are RFC4122 v4 UUIDs.
- **File load (drop / picker → transcribe), Win.** `dimmy_transcribe_file`
  with rc table -1..-8. UIPI bypass for elevated drag sources, Win32
  drop-target on the whole HWND tree, large-file confirmation dialog,
  cloud + local branches, `Helpers/WavPeaks.cs` for waveform preview.
- **History v2 schema.** Idempotent `ALTER TABLE` migration adds
  `enhanced_text`, `audio_path`, `app_process`, retention horizon, and
  word timestamps. Detail panel in `SettingsWindow.xaml` shows
  waveform + audio playback for past dictations.
- **App-context rules (Win).** Per-app LLM style / translate-to
  override. `core/src/app_rules.rs::resolve` resolves the captured app
  id (process name, bundle id, or X11 WM_CLASS) against the user's
  rule list. `Helpers/AppContextCapture.cs` captures the foreground
  HWND + focus drift; tray meeting menu + drag-reorder UI in Settings.
- **Parakeet TDT v3 local STT** (`core/src/parakeet.rs`). Pure-Rust
  port over ONNX Runtime — `nemo128.onnx` + `encoder-model.onnx` +
  `decoder_joint-model.onnx` + `vocab.txt`, ~2.5 GB bundle downloaded
  to `<config>/parakeet-fp32/` on first use. Greedy TDT decoder with
  LSTM state (`[2, 1, 640]` × 2). Selectable in Win Settings + bundled
  in the release pipeline (`onnxruntime.dll` next to `dimmy_lib.dll`).
- **Realtime chunked Parakeet** (`core/src/chunked_stt.rs`). Worker
  thread slices the most recent N seconds + overlap, dedups
  last-3-words against the cumulative text, emits FFI events. 5 s
  chunks for the realtime path; benchmarked at 8.7× realtime on WSL
  CPU over 272 min of LibriVox audio.
- **Word-level timestamps end-to-end** (Phase 7.4). `transcribe_with_word_timestamps`
  + history schema.
- **Real app icons via `SHGetFileInfo`** for app rules + history rows
  (Phase 7.1). Phase 8.5 bumped to 256 × 256, Phase 8.6 preserved alpha.
- **Live captions floating window** (Win). Subtitle-style overlay fed
  by chunked Parakeet during recording.
- **Mac Parakeet on the Apple Neural Engine via FluidAudio**
  (`core/src/parakeet_fluid.rs`, feature `local-stt-parakeet-fluid`).
  Documented RTF: 100-300×; first `init_asr()` downloads the ~3 GB
  CoreML bundle to `~/.cache/fluidaudio/`.
- **CI: parallel Mac + Win + Linux release flows** with FluidAudio
  Parakeet path wired through all three.

### Fixed
- Drag/drop registers on the whole HWND tree + correct POINTL→long
  marshal so drops work over child controls.
- LLM raw FFI for the meeting recap path.
- Mac pill scroll-cycle crash + self-contained bundle + FluidAudio
  download flow + onboarding auto-pick.

## [0.6.26] - 2026-04-30

### Added
- **Win taskbar overhaul.** `TaskbarService` (overlay icon + amplitude
  bar via `ITaskbarList3`) + `JumpListService` (right-click submenus
  for Style / Translate to with custom 32 × 32 BGRA glyphs) +
  `CommandPipeServer` (named-pipe IPC for jump-list commands) +
  `UiPreferences` (Win-only `ui_prefs.json` for taskbar-only mode).
- **Sentry user feedback envelope v2.** `capture_feedback` in
  `core/src/telemetry/sentry_pipeline.rs` sends a manually-built
  envelope with `type=feedback` so reports land in Sentry's Feedback
  tab (not Issues). Replaces `capture_message` + tag.

### Fixed
- **Win11 jump list silently failed without matching `.lnk`.** Need
  process AUMI + per-window AUMI (`SHGetPropertyStoreForWindow`) +
  Start-menu shortcut with matching AUMI. Velopack handles it in prod
  via `--packId Dimmy`; dev builds get `JumpListService.EnsureStartMenuShortcut()`.
  CommitList success ≠ menu shows.

## [0.6.25] - 2026-04-30

Internal staging cut between v0.6.20 (last public release before the
v2 / Phase-7 series) and v0.6.26 (taskbar overhaul). Carries the early
Win Parakeet integration commits (`feat(stt/win)`: bundle
`onnxruntime.dll`, Parakeet selectable in Settings, real download
progress, ABI snapshot refresh, unify into the local-model ComboBox)
plus a handful of stability fixes (drain chunked transcriber before
clearing audio buffer, persist Parakeet selection, transparent
`CaptionWindow`).

## [0.6.24] - 2026-04-29

### Added
- **Settings UI redesign — `.scard` Win11 pattern across all panels.**
  Each setting is its own `SettingCard` (icon + label + description +
  control), grouped under uppercase section headers (LANGUAGE,
  SPEECH-TO-TEXT, TELEMETRY, UPDATES, GPU ACCELERATION, …). Replaces
  the v2 redesign which only repositioned controls inside Border
  wrappers — the new pattern matches the design bundle's `.scard`
  semantic the user originally asked for.
- **Home dashboard — three-tile stats card.** Words transcribed,
  total speaking time, and time saved vs typing. ViewModel now exposes
  `SpeakingTimeDisplay` derived from `stats_total_speaking_secs` so the
  middle tile reacts to Rust-side counter updates in real time.
- **About panel — proper Dimmy logo + Anthropic mark.** Bundled
  `Assets/dimmy-logo.png` (the chat-bubble waveform) on the right of
  the hero, paired with a version chip + action buttons. Footer reads
  *"Made with [Anthropic logo] Claude Code"* using the official
  Anthropic SVG path.
- **Pill overlay — LIVE PREVIEW with state switching.** Mock pill that
  reflects the current BorderStyle and WaveformStyle in real time;
  state chips (idle / recording / transcribing / done / error) swap the
  border colour and inner content (bars / line / dots / glyph) so users
  can see what they're configuring without dragging the real pill.
- **Pill overlay — compact 3×2 position grid embedded in SettingCard.**
  Replaces the dropdown with a wallpaper-position-picker style cell
  selector. Now includes **Top Center** (6 positions, was 5) — handled
  in `WindowHelper.PositionByPreset`.
- **Output Style picker — coloured swatch per style.** Each LLM style
  in the combo (off / correct / professional / genz / imbruttito / …)
  gets a distinct dot via `StyleToColorBrushConverter`, so users can
  scan options visually without reading every label.

### Fixed
- **Pill scroll-wheel could orphan the user with no off-state.** The
  pill's translation-cycle list (`LangList`) was sourced from
  `Languages` (6 entries: it/en/es/fr/de/pt) without the "" → "No
  translation" entry. Once translation was engaged, scrolling could
  never bring it back to off. Switched to `TranslateTargets` (now
  public) which includes "" as the first entry.
- **Pill translation indicator vanished when no language selected.**
  On hover, when `llm_translate_to` was empty the language label was
  hidden — leaving the user with no scroll-wheel hit target. Now shows
  an em-dash (`—`) so the area stays clickable / scrollable.
- **Voice input duplicate STT API key field.** The v2 redesign briefly
  shipped two API-key SettingCards (one unconditional in
  SPEECH-TO-TEXT + one inside the Cloud sub-panel). Removed the
  unconditional one; the in-panel card is the only one wired to
  `CloudApiKeyBox` — local mode no longer shows an irrelevant key
  input.

### Changed
- **Settings panel container is now responsive.** Was `MaxWidth=760`
  with `HorizontalAlignment=Left`; now `MaxWidth=1100` with
  `HorizontalAlignment=Stretch` so cards grow with the window up to a
  readable cap on ultrawides.

## [0.6.20] - 2026-04-20

### Fixed
- **`test-install.yml` still demanded `vcruntime140.dll` + `msvcp140.dll`
  in the installed app folder.** v0.6.19 finally produced a clean build
  and packed a Velopack installer, but the clean-install smoke test
  failed verifying bundle integrity — the check list still required
  those two DLLs even though v0.6.12 had already removed the bundling
  (Velopack's `--framework vcredist143-x64` installs them to System32).
  Removed them from the critical-files check.

## [0.6.19] - 2026-04-20

### Fixed
- **Linker gate couldn't find `dumpbin.exe`.** v0.6.18 built
  `dimmy_lib.dll` successfully (cargo finished, toolchain 14.51
  confirmed active) but the post-build gate step called bare
  `dumpbin` via `& dumpbin`, and `dumpbin.exe` wasn't in PATH —
  the previous step's `vcvars64.bat` activation was scoped to its
  `shell: cmd` subshell only. Fix locates dumpbin under
  `$env:VS2026_PATH\VC\Tools\MSVC\<ver>\bin\Hostx64\x64\` and
  invokes via absolute path.

## [0.6.18] - 2026-04-20

### Fixed
- **Explicit `exit 0` at end of VS 2026 install step.** v0.6.17 ran the
  whole script cleanly (printed "DONE") then still exited 1 —
  chocolatey's exit 3010 (reboot-required) leaves `$LASTEXITCODE=3010`,
  and pwsh 7.3+ with `$PSNativeCommandUseErrorActionPreference=$true`
  (the default on GitHub Actions pwsh wrapper) propagates that to the
  step exit code regardless of what runs after. Adding `exit 0` at the
  end of the PowerShell block overrides the inherited code.

## [0.6.17] - 2026-04-20

### Fixed
- **Silent exit 1 after VS 2026 detection.** v0.6.16 reached the
  toolchain verification (`VS 2026 MSVC toolchain: 14.51.36231 at
  C:\Program Files (x86)\Microsoft Visual Studio\18\Insiders`) then
  aborted with exit 1 and no exception text. Replaced the
  `[version]` cast + `echo >> $env:GITHUB_ENV` with explicit regex
  parsing + `Add-Content -Encoding utf8`. Added Write-Host
  breadcrumbs before each potentially-throwing operation to expose
  which line trips next time.

## [0.6.16] - 2026-04-19

### Fixed
- **`vswhere` missing `-prerelease` flag hid the just-installed VS 2026
  preview.** v0.6.15 choco install succeeded (`Chocolatey installed 5/5
  packages`, VS 2026 BuildTools v118.6.0.117102900-preview1 deployed)
  but the post-install `vswhere -version "[18.0,19.0)"` query returned
  nothing — vswhere filters out preview releases by default. Added
  `-prerelease` to every VS 2026 lookup. Also replaced the 3 s sleep
  with a 60 s poll loop because Installer registration can lag the
  choco `installed` report.

## [0.6.15] - 2026-04-19

### Fixed
- **`choco install` of the VS 2026 BuildTools preview needs `--pre`.**
  v0.6.14 failed with `visualstudio2026buildtools-preview not installed.
  The package was not found with the source(s) listed` — chocolatey
  silently omits prerelease packages unless `--pre` is passed. Added
  the flag to both release.yml and staging-native.yml.

## [0.6.14] - 2026-04-19

### Fixed
- **VS 2026 BuildTools install via chocolatey instead of
  `aka.ms/vs/18`.** v0.6.13 tried to download
  `https://aka.ms/vs/18/release/vs_buildtools.exe` but Microsoft has not
  registered the `aka.ms/vs/18/*` short-URLs yet — they all 302-redirect
  to Bing search results, so the "bootstrapper" ended up being ~63 KB
  of HTML and the installer aborted with "The file or directory is
  corrupted and unreadable". Chocolatey hosts
  `visualstudio2026buildtools-preview` (published 2025-12-22), whose
  install script internally fetches the signed bootstrapper from
  Microsoft's CDN. Switching the CI step to `choco install
  visualstudio2026buildtools-preview -y --package-parameters "..."`
  sidesteps the aka.ms gap entirely.

## [0.6.13] - 2026-04-19

### Fixed
- **Windows build succeeds with MSVC 14.50 via side-by-side VS 2026
  BuildTools install.** v0.6.12 hit the expected wall: the pinned
  `windows-2025` runner image ships VS 2022 Enterprise only (MSVC
  14.44), and `setup.exe update` cannot bridge major Visual Studio
  versions, so the pre-build gate aborted. `windows-2025` has no
  path to 14.50 short of installing VS 2026 separately. New Windows
  build step downloads `vs_buildtools.exe` from
  `aka.ms/vs/18/release` and installs the VCTools + VC.Tools.x86.x64
  components to a side-by-side VS 2026 install. The Rust cargo step
  activates that toolchain via `vcvars64.bat` in a cmd shell, scoped
  to the step so subsequent .NET / MSBuild / AppxPackage steps
  continue to use VS 2022 Enterprise (which has the UWP workloads
  VS 2026 BuildTools lacks). The post-build linker-version gate
  from v0.6.12 still enforces `dumpbin /headers` linker >= 14.50,
  so any future regression that drops the toolchain fails CI loudly.
- **Locate VS AppxPackage tools now pins to VS 2022 explicitly.** With
  VS 2026 BuildTools installed alongside, a bare `vswhere -latest`
  would return VS 2026 (newer) which lacks AppxPackage tasks. The
  step now uses `-version "[17.0,18.0)"` to select VS 2022 regardless
  of install ordering.

### Known
- v0.6.12 tag was pushed with the pre-build gate in place but CI
  aborted before producing a Windows build. GitHub release v0.6.12
  exists with Linux + macOS artifacts only. Do not upgrade to v0.6.12
  on Windows; v0.6.13 supersedes it.

## [0.6.12] - 2026-04-19

### Fixed
- **Windows installer crashed on first transcription at
  `whisper_backend_init_gpu` — MSVC 14.44 linker miscompiles whisper.cpp
  Vulkan state init.** v0.6.11 addressed a related-but-wrong ABI theory
  around the bundled VC runtime; removing the bundle reproduced the crash
  identically, proving the runtime wasn't the cause. An empirical DLL swap
  against a locally-built `dimmy_lib.dll` (same source commit, MSVC 14.50
  linker) ran clean end-to-end on the same machine, pinning the bug to
  MSVC 14.44 codegen around `ggml-vulkan`'s per-state backend allocation.
  Fix has three parts:
  1. Windows CI runners pinned to `windows-2025` which ships MSVC 14.50+.
  2. A pre-build step verifies `VC\Tools\MSVC\<newest>` is >= 14.50 and
     invokes the VS Installer to update if not, aborting with a clear
     error message otherwise.
  3. A post-build gate parses the PE header of `dimmy_lib.dll` via
     `dumpbin /headers` and fails the workflow if linker version < 14.50,
     preventing another silent ship of a known-broken build.
- **Stopped co-locating `msvcp140.dll` / `vcruntime140.dll` in the
  installer folder.** Velopack `--framework vcredist143-x64` already
  installs the official Microsoft VC Redist at setup time (lands in
  System32), and Windows DLL search order makes a second co-located copy
  either redundant (when versions match) or actively harmful (when the
  bundled copy shadows a newer System32 ABI — this was the whole v0.6.10
  crash cause). `verify-self-contained.ps1` updated to reflect the
  delegation.

### Details
- `test-install.yml` still only probes 15 s of startup; it does NOT yet
  exercise `dimmy_stop_recording`, which is why both v0.6.10 and v0.6.11
  shipped with this latent break. Extending it to round-trip a synthetic
  WAV through the FFI before ticking the release green is tracked for
  v0.6.13.

## [0.6.11] - 2026-04-19

### Fixed
- **Windows installer crashed in `dimmy_stop_recording` with
  `AccessViolationException`** — ABI mismatch between the Rust DLL and the
  bundled Visual C++ runtime. The CI step that copies `vcruntime140.dll` /
  `msvcp140.dll` into the publish folder walked the entire `VC` tree with
  `Get-ChildItem -Recurse ... | Select-Object -First 1`, which in practice
  returned the oldest redistributable package shipped with Visual Studio
  (e.g. `14.29.30157.0` from 2021). `dimmy_lib.dll` was linked against the
  current compiler toolchain (14.4x+) so imports that existed only in the
  newer `msvcp140.dll` resolved against the older co-located copy — the
  process dereferenced a null vtable entry deep inside whisper.cpp and
  segfaulted. The step now pins to `VC\Tools\MSVC\<newest>\bin\Hostx64\x64`,
  i.e. the exact toolchain the compiler used, so bundled and linked
  runtimes always match. Affected every self-contained installer produced
  by `staging-native.yml` and `release.yml`.

## [0.6.10] - 2026-04-19

### Added
- **Forensic logging across the whisper inference path** — previously the
  log trail ended at `[LocalSTT] Model cached successfully` followed by
  ggml's `whisper_backend_init_gpu: no GPU found`, making it impossible to
  tell whether a subsequent silent process abort happened in
  `create_state`, during `whisper_full`, or in segment extraction. Dimmy
  now emits a line on entry and exit of each of those three phases,
  including `n_threads`, `single_segment`, and sample count. Lines are
  flushed synchronously per the existing `crate::log` semantics, so the
  last line before a C++ abort pins the crash site for post-mortem.

## [0.6.9] - 2026-04-19

### Added
- **Sticky GPU known-bad marker with driver fingerprint (Windows/Linux)** —
  v0.6.8 recovered from a ggml-vulkan abort within a single relaunch, but
  the recovery state was session-scoped: every cold start re-tried the GPU
  path and crashed again before falling back. Dimmy now persists a
  `.gpu_known_bad` record next to the session-scoped sentinel, including a
  fingerprint of the Vulkan loader environment (vulkan-1.dll size +
  registered ICDs on Windows; ICD JSON files on Linux). Subsequent cold
  starts compare the current fingerprint against the recorded one: match →
  stay on CPU without crashing; mismatch (driver/ICD updated) → clear the
  marker and give the GPU another chance automatically. Settings → Debug
  surfaces the status and a "Retry GPU on next launch" button that wipes
  the marker manually. macOS path is a no-op (Metal does not need it).
- **ggml debug logging toggle (Settings → Debug)** — the per-tensor /
  per-layer dumps from whisper + llama load are now suppressed by default,
  cutting cold-start log volume by ~80%. The toggle re-enables them when
  diagnosing a model load. INFO/WARN/ERROR continue to flow at all times.
- **`dimmy_gpu_get_status` / `dimmy_gpu_clear_known_bad` FFI** — JSON status
  reader and one-shot clear for native UIs to surface the GPU recovery
  state and the manual-retry action.

## [0.6.8] - 2026-04-19

### Fixed
- **Local STT/LLM on hosts where ggml-vulkan aborts inside its own init**
  — on dual-boot Windows installs where the same hardware has a partially
  broken driver stack, `WhisperContext::new_with_params` and
  `LlamaBackend::init()` could still abort the process *after* our sentinel
  forced `use_gpu(false)`. Root cause: whisper.cpp/llama.cpp call
  `ggml_backend_init_by_type(CPU)` → `ggml_backend_registry` singleton →
  `ggml_backend_vk_reg()` → `ggml_vk_instance_init()` unconditionally, so
  the `use_gpu` flag on params doesn't prevent Vulkan driver code from
  running. The CPU fallback was effectively identical to the GPU path on
  these machines.

  Fix: when the GPU backend is declared `Unavailable` (sentinel, env var,
  or probe failure), also set `VK_DRIVER_FILES` + `VK_ICD_FILENAMES` to a
  non-existent path. The Vulkan loader then reports zero ICDs, ggml-vulkan
  logs "No devices found" and returns early without touching driver code.
  Re-ordered `llm_cache::generate` so `gpu_backend_status()` runs before
  `LlamaBackend::init()` — the env var must be set before llama.cpp
  triggers its backend registry.

## [0.6.7] - 2026-04-19

### Fixed
- **macOS CI build (v0.6.6 hotfix)** — v0.6.6 assumed Apple clang emitted
  the same `c_int` alias for `ggml_log_level` as MSVC, but Apple clang on
  arm64 actually emits `c_uint` (same as gcc/Linux). Only Windows is the
  outlier. Flipped the cfg so `GgmlLogLevel` is `c_int` on Windows and
  `c_uint` everywhere else.

## [0.6.6] - 2026-04-19

### Fixed
- **CI build on Linux / macOS (v0.6.5 hotfix)** — `ggml_log_level` is a
  bindgen-generated C enum whose Rust alias is `c_int` on Windows (MSVC
  defaults enums to signed int) but `c_uint` on Linux/macOS (gcc and
  Apple clang default to unsigned for non-negative enums). The v0.6.5
  callback signature hard-coded `i32`, which matched Windows only.
  Introduce `GgmlLogLevel` as a cfg-conditional alias so the fn-pointer
  type matches what each platform's bindgen emits.

## [0.6.5] - 2026-04-19

### Added
- **GPU diagnostic logging** — when the GPU backend is first queried, Dimmy now:
  - Installs log callbacks on both `whisper.cpp` and `llama.cpp` so their
    internal ggml messages (including the error text that precedes a
    process abort) land in `dimmy.log` with a `[ggml <LEVEL>]` prefix.
  - Logs a Vulkan environment snapshot: `vulkan-1.dll` path/size, registered
    Vulkan ICDs (from `HKLM\Software\Khronos\Vulkan\Drivers`), and
    `TdrDelay`/`TdrDdiDelay` registry values. Linux logs the `.json` files
    under `/etc/vulkan/icd.d` and `/usr/share/vulkan/icd.d`.
  - Makes post-mortem analysis of GPU crashes possible without extra tooling:
    the last words of ggml before an abort are now in the user's log file.

## [0.6.4] - 2026-04-19

### Fixed
- **GPU crash recovery (Windows/Linux)** — on some machines with Vulkan-capable
  discrete GPUs, `ggml-vulkan` aborts the process during whisper/llama model init
  (not a recoverable Rust error). The app would restart in a loop whenever the
  user chose local STT or local LLM. Added a sentinel file in the config dir that
  is written before any GPU init attempt and deleted on success; if a subsequent
  launch still sees the sentinel, the backend is forced to CPU for that session
  so the app remains usable. Drivers or settings fixed between runs will allow
  the GPU path to be retried automatically.
- **Windows UI clipped on high-DPI displays** — Settings and Onboarding windows
  were resized in raw physical pixels, so at 150%/200% scaling the windows were
  too small and toggles/buttons were cut off. Windows now resize using logical
  DIPs scaled by the monitor DPI (`WindowHelper.ResizeLogical`).
- **Settings "Advanced" toggle overlap** — on narrow window widths the Advanced
  toggle overlapped the scrollable content. The layout now stacks the toggle
  above the content instead of absolute-positioning it over the panel.

## [0.5.2] - 2026-04-12

### Fixed
- **Vulkan GPU auto-detection** — on multi-GPU laptops (e.g. Intel iGPU + NVIDIA dGPU), whisper.cpp defaulted to device 0 (integrated GPU), causing crashes during inference. Now auto-enumerates Vulkan physical devices and selects the first discrete GPU.
- Added Large-v3-Turbo models (Q5, Q8) and Distil-Large-v3.5 models (Q5, Q8) to the local STT model catalogue

### Added
- `preferred_gpu_device()` — Vulkan device enumeration via raw FFI (Windows + Linux), zero new dependencies
- `GGML_VK_DEVICE` env var override for power users / CI to force a specific GPU device index
- Log output lists all Vulkan devices with type (Integrated/Discrete/Virtual/CPU) at first model load

## [0.4.0] - 2026-04-08

### Added
- **Local offline transcription** via whisper.cpp (whisper-rs) — no API keys required
  - macOS: Metal GPU acceleration on Apple Silicon
  - Windows: Vulkan GPU acceleration (NVIDIA, AMD, Intel)
  - CPU fallback on all platforms
  - Default model: Whisper Base Q8 (78 MB, downloaded on first use)
  - 4 model sizes available: Tiny (42 MB), Base (78 MB), Small (181 MB), Medium (514 MB)
- **Transcription history** with full-text search (SQLite + FTS5)
  - Auto-saved after every transcription
  - Searchable from native UI
  - Stats: total words, sessions, duration
- **Filler word removal** for 6 languages (it, en, es, fr, de, pt)
  - Removes "basically", "you know", "cioè", "praticamente", etc.
  - Enabled by default, configurable in settings
- New config fields: `stt_mode` (local/cloud), `local_model`, `filler_removal_enabled`
- FFI functions for model management: `dimmy_list_local_models`, `dimmy_download_model`, `dimmy_model_exists`
- FFI functions for history: `dimmy_history_save`, `dimmy_history_recent`, `dimmy_history_search`, `dimmy_history_delete`, `dimmy_history_stats`
- `Provider::Local` variant for local STT routing
- CI/CD: platform-specific feature flags (Metal on macOS, Vulkan on Windows)
- BACKLOG.md for feature tracking
- CHANGELOG.md (this file)

### Changed
- Default STT mode is "cloud" for safe upgrades (local mode requires model download first)
- `dimmy_start_recording` skips API key check when in local mode
- Filler removal applied to both local and cloud transcriptions
- **API key storage simplified**: always uses local AES-256 encrypted file (no OS popups, no admin needed). OS keyring kept as read-only fallback for migrating existing keys.
- Removed "Use System Keychain" toggle from macOS, Windows, and Linux settings

### Removed
- `enigo` dependency (unused — native UIs handle text injection)
- `arboard` dependency from core (unused — native UIs handle clipboard; Linux UI has its own)

### Fixed
- README.md referenced non-existent `tauri.conf.json` in pre-push checklist
- `.gitignore` contradicted itself on `docs/superpowers/` directory
- Default `stt_mode` changed from "local" to "cloud" to prevent broken first recording on upgrade (model not yet downloaded)

## [0.3.65] - 2026-04-08

### Changed
- Replaced video with pill-states overview image in README
- Removed Tauri remnants, updated README with native pill screenshots

## [0.3.64] - 2026-04-07

### Changed
- Documentation rewrite + security fixes

[0.4.0]: https://github.com/KonradDallaOrg/dimmy/compare/v0.3.65...v0.4.0
[0.3.65]: https://github.com/KonradDallaOrg/dimmy/compare/v0.3.64...v0.3.65
[0.3.64]: https://github.com/KonradDallaOrg/dimmy/releases/tag/v0.3.64

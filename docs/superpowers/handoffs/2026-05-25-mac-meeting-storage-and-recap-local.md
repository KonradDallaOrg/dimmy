# Handover — Mac parity: meeting storage dir + local recap picker

**Date:** 2026-05-25
**Author:** Win session (Opus 4.7)
**Target:** a macOS session to mirror two features that already shipped on Windows.

## TL;DR

Two Win features merged to `staging` today. Both need a Mac mirror. The
**Rust core is already cross-platform and done** — Mac work is SwiftUI +
two Swift re-derivations + (for storage-dir) the sandbox bookmark gotcha.

| Feature | Win status | Mac status |
|---|---|---|
| User-configurable meeting storage dir | ✅ merged `staging` (PR #82, commits `eb63ebd` + `f19459a`); tester build `v0.6.52-staging.2` | ❌ TODO (this doc, Part A) |
| Local LLM models in the recap picker | ✅ PR #84 (branch `feat/recap-local-models`) | ❌ TODO (this doc, Part B) |

Requested by **Ricca** (storage dir) — credit him in the changelog (already done under `[Unreleased]`).

---

## Part A — Meeting storage directory (Mac)

### What's already done (cross-platform core — DO NOT redo)
- `core/src/lib.rs`: `meeting_storage_path` config field; `meetings_dir()`
  now resolves the override (reads `load_config_file().meeting_storage_path`)
  with a writability fallback to the default; helper
  `resolve_meetings_dir` + `meeting_storage_path_usable`. 4 unit tests.
- `core/src/ffi.rs`: field in AppState init + getter (`dimmy_get_config_json`
  emits `meeting_storage_path`) + setter (reads it, trims, `""` = reset) +
  **new FFI `dimmy_meetings_dir(out_buf, len) -> c_int`** returning the
  effective path.
- These are compiled into the Mac dylib already (no Rust work needed).
  **Verify** the Mac frozen feature set still builds (`preflight-mac.sh`).

### Mac work

**1. Swift binding for the new FFI.** `DimmyCore` wraps string FFIs (see
`configDirURL` at `Managers/DimmyCore.swift:393`, which calls
`dimmy_config_dir_name`). Add a `meetingsDirURL: URL?` computed prop that
calls `dimmy_meetings_dir` the same way (read into a buffer, UTF-8).
**Resolve fresh on each access** (not cached) — the user can change it at
runtime; mirror the Win `BuildInfo.MeetingsDirPath` decision.

**2. Replace the two re-derivations** so nobody bypasses the override:
- `Views/Meeting/MeetingViewModel.swift:760` — `meetingsDir()` currently
  returns `DimmyCore.shared.configDirURL?.appendingPathComponent("meetings")`.
  Change to `DimmyCore.shared.meetingsDirURL`.
- `Services/FileLoadToMeetingService.swift:56` — `configDir.appendingPathComponent("meetings")`.
  Change to `DimmyCore.shared.meetingsDirURL`.
- **Audit** these too (they derive `configDirURL/...` — confirm whether
  they touch the meetings root or something else):
  `Services/MeetingPostProcessService.swift:371`, `Utilities/RecapModel.swift:25`.
  If they build a per-meeting path from a passed-in dir, they're fine; if
  they re-derive the meetings *root*, route through `meetingsDirURL`.

**3. Settings UI.** Mirror the Win card (Settings → Meetings). Win shipped
an expanded card: header + description, full-width path box, buttons
bottom-right (Browse primary, Reset). On Mac use **`NSOpenPanel`** with
`canChooseDirectories = true, canChooseFiles = false`. On pick → write
`meeting_storage_path` through the config round-trip (single-writer rule —
never write config.json from Swift; send via the same setter path the Mac
uses for other config). Reset = send `meeting_storage_path: ""`.

**4. 🚨 Sandbox: security-scoped bookmark.** This is the ONE Mac-specific
landmine. If `Dimmy.app` is sandboxed (check the entitlements /
`com.apple.security.app-sandbox`), a raw path the user picks will **not be
writable after relaunch** — the sandbox only grants access to the picked
URL for the current session unless you persist a **security-scoped
bookmark**:
- On pick: `url.bookmarkData(options: .withSecurityScope, ...)`, persist the
  bookmark blob (UserDefaults or a file).
- On launch: resolve the bookmark, call `startAccessingSecurityScopedResource()`
  before any meeting read/write, `stopAccessing...` on teardown.
- If NOT sandboxed (Dimmy may ship non-sandboxed for the global hotkey /
  Accessibility model — verify), a plain path is fine and you can skip
  bookmarks. **Check first; don't assume.**

**5. Validate writability** before committing the pick (mirror Win
`IsDirectoryWritable` — create dir + write a probe file + delete). Show an
inline error if not writable.

**6. Pre-flight (MANDATORY).** `scripts/dev/preflight-mac.sh` — rebuilds
Rust with the Mac frozen set, `xcodebuild`, **and launches the .app 5 s**
so `SelfTests.runAtLaunch` fires. If you add a `SelfTests` assertion that
pins anything new, update it in the same commit. (Burned before — see
CLAUDE.md "Mac pre-flight".)

### Reader audit (must all use `meetingsDirURL`)
The Win side confirmed only 2 root re-derivations; the per-meeting readers
take an absolute dir from the selected row. On Mac, confirm the meeting
list, meta.json, recap.md, transcripts.txt, audio/waveform, notes, delete,
and Notion-send all resolve from `meetingsDirURL` (or a dir derived from
it), not a fresh `configDirURL/meetings`.

---

## Part B — Local LLM models in the recap picker (Mac)

### Context
The core already routes recap to llama.cpp (**Metal** on Mac) via the
`recap_model_override` `local:<filename.gguf>` prefix — `parse_recap_override`
+ `dimmy_llm_call_raw` handle `effective_mode == "local"` →
`process_raw_prompt_local`. So **local recap already works on Mac** if the
user types `local:…` or runs `llm_mode=local` + recap "Auto". The gap is
purely discoverability: the Mac recap picker doesn't list local models.

### Win reference (mirror this — PR #84, `SettingsWindow.xaml.cs`)
- `PopulateRecapLocalModels()` — lists every catalogue local model as
  `Local · <name> (Ready | <MB> — download in LLM)`, inserted before the
  Custom sentinel. Uses the local-LLM catalogue (Win: `ListLocalLlmModels`
  FFI; Mac equivalent: whatever feeds the existing Local-LLM model picker
  in `MacOutputPage` / the LLM section).
- Recap model match is **Tag-based** (`local:<file>`), not index-based.
- `RecapVendorFromModel("local:…")` → `""` so the cloud key/subscription
  UI hides (no auth for a local model).
- Picking a not-yet-downloaded model nudges the user to download it in the
  LLM section.

### Mac work
- `Views/Settings/MacOutputPage.swift` — the recap model picker. Add the
  downloaded/catalogue local models as `local:<filename>` options.
- `Utilities/RecapModel.swift` + `Utilities/ProviderTagging.swift` — wherever
  the recap vendor is derived from the model id, make `local:` → no vendor
  (hide key/subscription), mirroring `RecapVendorFromModel`.
- Reuse the Mac local-LLM catalogue + download flow already present for the
  dictation LLM picker.

### Answering the original question
Yes — a Windows GeForce user (and any Mac with Metal) can run Gemma/Phi for
the recap locally. Win surfaced it in the dropdown (PR #84); Mac needs the
same surfacing. No new core capability required on either side.

---

## Also pending on Mac (separate, pre-existing — not this work)
- #165 upgrade error "An error occurred in retrieving update information"
- #214 v0.6.48 licensing endpoint error + duplicate device detection
- #223 review/merge `fix/mac-meeting-timer-pause` (ffiorentino, db9188c)

## Commits / PRs to read first
- PR #82 (merged) — storage-dir Win+core+MCP. Commits `eb63ebd`, `f19459a`.
- PR #84 — recap-local Win. Branch `feat/recap-local-models`.
- Prior spec: `docs/superpowers/handoffs/2026-05-25-meeting-storage-dir.md`.

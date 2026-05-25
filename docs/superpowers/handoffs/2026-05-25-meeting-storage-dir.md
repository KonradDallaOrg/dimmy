# Handover — User-configurable meeting storage directory

**Requested by:** Ricca (credit him explicitly in the changelog 🎉)
**Branch:** `feat/meeting-storage-dir` (off `origin/staging` @ 91563b4)
**Date:** 2026-05-25
**Status:** spec confirmed, implementation not started

## What the feature is

Today every meeting lives under a fixed `<config_dir>/meetings/` folder. Let
the user pick a **custom destination directory** for meeting recordings
(NOT during onboarding — a Settings control). After that, everything in the
app that reads/writes meetings uses the user-chosen dir.

## Confirmed design decisions (user, 2026-05-25)

| Question | Decision |
|---|---|
| Scope | **Meetings only.** Dictation audio (`history_audio/`) stays put, keeps its existing retention. |
| Retention on meetings | **None.** Manual delete only, as today. (Auto-deleting files in a user-chosen dir — NAS/sync — is risky.) |
| Migration on dir change | **New meetings only go to the new dir.** No move logic. Old meetings stay in the old dir and simply stop appearing in the list until/unless the user points back. Simplest impl, user accepted the UX trade-off. |
| Platform order | **Windows first, then Mac.** |

## The breakage risk (why this needs care)

Core has ONE source — `lib.rs:232 meetings_dir() = config_dir/meetings` — but
**4 places independently re-derive the path** instead of asking core. These
are the only things that break if the base moves; the fix is to centralize:

1. `core/src/lib.rs:232` `meetings_dir()` — the canonical source (make it honor the override)
2. `platforms/windows/Dimmy.Windows/Views/MeetingWindow.xaml.cs:1503` — hardcodes `Path.Combine(BuildInfo.ConfigDirPath, "meetings")`
3. `platforms/macos/Dimmy/Views/Meeting/MeetingViewModel.swift:760` — `configDirURL.appendingPathComponent("meetings")`
4. `platforms/macos/Dimmy/Services/FileLoadToMeetingService.swift:56` — same re-derivation
5. `mcp-server/src/config.rs:42` — `self.config_dir.join("meetings")` (subprocess; can't call FFI → must read config.json)

**Rule for the implementation:** nobody re-derives. Core resolves the
effective path; everyone else asks core (FFI) or — for the MCP subprocess —
reads the config field from `config.json`.

## Implementation plan

### Phase 1 — Core (Rust, shared) — DO FIRST, buildable/verifiable

1. **Config field** `meeting_storage_path: String` (empty = default). Follow the
   `save_audio_in_history` pattern the explorer mapped:
   - struct field: `lib.rs` AppConfig (~line 490 area)
   - default: `String::new()` (~line 631)
   - `get_config` JSON emit (~line 734)
   - `parse_config` read (~line 909) — **identity-field `if-empty-omit` caution**: this is a path identity field; in the C# `ToJson` it MUST follow the *emit-only-when-non-empty* pattern (see CLAUDE.md "Save anything in C# Settings → ToJson") or a transient empty VM wipes it.
   - AppState mutex: `ffi.rs:314` area
   - FFI getter: `ffi.rs:1391` area (add to `dimmy_get_config_json`)
   - FFI setter: `ffi.rs:1790` area (read from `dimmy_set_config_json`)

2. **Make `meetings_dir()` honor the override.** It's a free fn with no AppState
   access. Cleanest: resolve from the persisted config. Add a helper that reads
   the effective dir:
   ```rust
   pub fn meetings_dir() -> Option<PathBuf> {
       // override wins if set + non-empty; else default config_dir/meetings
       if let Some(p) = effective_meeting_storage_override() {
           return Some(p);
       }
       config_dir_path().map(|p| p.join("meetings"))
   }
   ```
   Decide the override source: read it from the loaded config (whatever global/
   AppState path `recap_provider` etc. use) rather than re-reading config.json on
   every call. **Verify which accessor the core already uses for runtime config
   reads outside ffi** before wiring (meeting.rs reads `crate::meetings_dir()`).
   - **Validation:** if the override path doesn't exist or isn't writable, FALL
     BACK to default + `log()` a warning. Never crash (removable/network drive).

3. **New FFI** `dimmy_meetings_dir(out_buf, buf_len) -> c_int` returning the
   effective path as UTF-8 (same out-buf contract as other string FFIs). All UIs
   call this instead of re-deriving.

4. **Tests** (`core/tests` or unit): override set → meetings_dir returns it;
   empty → default; non-existent override → falls back to default.

5. Build: `cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan` (Win frozen set). Mac frozen set on the Mac side.

### Phase 2 — Windows UI

1. Settings → a "Meetings folder" row with current path + **Browse** (folder
   picker — reuse `Helpers/Win32FileDialog.cs` pattern, folder mode) + **Reset to
   default**. On pick → write `meeting_storage_path` via `dimmy_set_config_json`
   (single-writer rule; never write config.json from C#).
2. **Replace the hardcode** at `MeetingWindow.xaml.cs:1503` with a call to the
   new `dimmy_meetings_dir()` FFI (add P/Invoke in `Interop/DimmyNative.cs`).
3. `SettingsViewModel.ToJson` — emit `meeting_storage_path` only when non-empty
   (identity-field pattern, or it wipes on transient empty VM).
4. Validate the picked dir is writable; show inline error if not.
5. Build: `dotnet build ... -p:Platform=x64`. Local preview (launch, set a dir,
   start a meeting, confirm it lands there + shows in the list + plays back +
   recap + Notion + delete all work against the new dir).

### Phase 3 — MCP server

`mcp-server/src/config.rs:42` — instead of always `config_dir.join("meetings")`,
read `meeting_storage_path` from `config.json` (the MCP already reads config.json
for other fields). Empty/missing → default. Keep it a one-shot read at startup
(matches today's behaviour; the meeting tools resolve dir per-call so a stale
value only matters if the user moves mid-Claude-session — acceptable).

### Phase 4 — Mac (handover target)

Mirror Phase 2 in SwiftUI:
- `MeetingViewModel.swift:760` + `FileLoadToMeetingService.swift:56` → call the
  new `dimmy_meetings_dir()` via `DimmyCore` instead of re-deriving.
- Settings: a "Meetings folder" control (NSOpenPanel folder mode) writing
  `meeting_storage_path` through the config round-trip.
- **MANDATORY**: `scripts/dev/preflight-mac.sh` (build + 5s launch for SelfTests)
  before shipping — and update `SelfTests` if it pins anything new.
- Sandbox note: macOS app sandbox + a user-chosen arbitrary dir needs a
  **security-scoped bookmark** (persist the bookmark, resolve+startAccessing on
  launch). This is the one Mac-specific gotcha — a raw path won't survive
  relaunch under sandbox. Check whether Dimmy.app is sandboxed; if yes, bookmarks
  are required.

## Things to manually verify don't break (point-3 readers, all must use the new dir)

Win `MeetingWindow.xaml.cs`: list (1503), meta.json (1511), recap.md (1638),
transcripts.txt (1675), audio/waveform (1682/2108), notes.md (1157), delete
(1406), Notion send (1722). Mac `MeetingViewModel` + `MeetingDoneView`
equivalents. MCP `tools.rs` get_recent_meetings/get_meeting/save_recap.

## Changelog
> **Meetings: choose your own storage folder.** You can now set a custom
> destination directory for meeting recordings in Settings. _Thanks to Ricca
> for the request._

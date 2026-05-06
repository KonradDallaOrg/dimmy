# Handoff — Mac side for `feat/v2-unified` features

> Source: Win-side implementation done on `feat/v2-unified` (off
> `feat/parakeet-stt-local` tip). Each feature already builds + runs
> on Win. The Mac side mirrors the FFI surface but needs Swift
> wrappers + native UI glue. This doc lists exactly what to wire so
> a Mac engineer can pick up without re-reading the whole branch
> history.

## What's in `feat/v2-unified`

```
feat/v2-unified =
  feat/parakeet-stt-local              (Parakeet STT + chunked + captions)
  + feat/app-context                   (foreground-app rules)
  + feat/history-v2                    (schema migration + audio retention)
  + feat/audio-load                    (drop / pick a WAV)
  + feat/meeting-mode                  (long-form recording + recap)
  + Phase 1 fixes                      (LLM raw FFI, drag/picker, app-context diag)
  + Phase 2 fixes                      (placeholder, hotkey gate, update_enhanced, prune)
  + Phase 3 polish                     (drag-reorder rules, category icons, tray meeting menu)
```

The Rust core is **fully cross-platform**. Mac just needs the C#-equivalent
in Swift for each feature.

---

## Feature 1 — App context (foreground-app rules)

### Rust core (already cross-platform)
- `core/src/app_rules.rs::AppContext { process_name, bundle_id, wm_class }`
- `core/src/app_rules.rs::resolve(rules, ctx) → RuleOverride`
- `core/src/ffi.rs::dimmy_set_app_context(json)` — push current foreground
- `core/src/ffi.rs::dimmy_clear_app_context()`
- Resolve is wired into `dimmy_process_with_llm` already

### What Mac needs
Capture the foreground app at hotkey-down and push:

```swift
import AppKit

func captureAppContext() -> String {
    // bundle_id is the primary Mac identifier — used by app_rules
    // matcher when match_type == "bundle_id"
    let bundleId = NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? ""
    let json = """
    {"process_name":"","bundle_id":"\(bundleId)","wm_class":""}
    """
    return json
}

// At hotkey-down, BEFORE dimmy_start_recording:
let json = captureAppContext()
dimmy_set_app_context(json.cString(using: .utf8))
```

### Defaults (Mac bundle_id mapping for the v1 baseline)

The Win-side `AppRulesDefaults.V1Windows` ships 19 rules keyed on
process_name. The Mac equivalent should ship 19 rules keyed on bundle_id:

| App | Bundle ID | Style |
|---|---|---|
| Slack | `com.tinyspeck.slackmacgap` | imbruttito |
| Discord | `com.hnc.Discord` | genz |
| WhatsApp | `net.whatsapp.WhatsApp` | imbruttito |
| Telegram | `ru.keepcoder.Telegram` | imbruttito |
| Teams | `com.microsoft.teams2` | professional |
| Outlook | `com.microsoft.Outlook` | professional |
| Mail | `com.apple.mail` | professional |
| Safari | `com.apple.Safari` | correct |
| Chrome | `com.google.Chrome` | correct |
| Firefox | `org.mozilla.firefox` | correct |
| Brave | `com.brave.Browser` | correct |
| Arc | `company.thebrowser.Browser` | correct |
| VS Code | `com.microsoft.VSCode` | off |
| Cursor | `com.todesktop.230313mzl4w4u92` | off |
| Xcode | `com.apple.dt.Xcode` | off |
| Notes | `com.apple.Notes` | off |
| Word | `com.microsoft.Word` | comprehensible |
| Notion | `notion.id` | comprehensible |
| Obsidian | `md.obsidian` | comprehensible |

### UI

The Win-side Settings → App rules page has:
- ListView with drag-reorder (CanReorderItems)
- Category icon (Segoe Fluent glyph) inferred from bundle_id (chat / mail / etc.)
- Inline edit: pattern + match_type + style + translate + enabled toggle
- "Load defaults (v1)" button
- Empty state hint

Swift equivalent: SwiftUI `List` with `.onMove` for reorder, `Image(systemName: …)`
for category SF Symbols (chat → "message", mail → "envelope", browser → "globe",
etc — already a 1:1 mapping with Segoe glyphs).

---

## Feature 2 — History v2 schema

### Rust core (already cross-platform)
Schema migration runs at `HistoryStore::new`. New columns:
- `enhanced_text TEXT`
- `audio_path TEXT`
- `app_process_name TEXT`
- `app_bundle_id TEXT`
- `llm_style TEXT`
- `llm_translate_to TEXT`
- `size_bytes INTEGER`

FTS5 covers both `text` + `enhanced_text`.

### What Mac needs
Update the Swift History list page to render the v2 fields:
- `enhanced_text` → toggle in detail view between Raw / Enhanced
- `app_bundle_id` → show app name (resolve via `NSRunningApplication`)
- `audio_path` → "play" button when present
- `size_bytes` → small storage indicator

### Audio retention

`save_audio_in_history` config field controls opt-in. When on,
`dimmy_stop_recording` saves a 16 kHz mono int16 WAV to
`<config>/history_audio/<id>.wav` and links via `dimmy_history_update_audio`.

Background prune thread runs in `dimmy_init`; no Swift work needed —
just expose the 3 retention settings (toggle, days, MB) in the Mac
Settings UI. Same JSON shape as Win.

### Update_enhanced post-LLM hook

Win-side `TranscriptionService.cs` does:
```
1. dimmy_stop_recording → raw transcript saved to history
2. dimmy_process_with_llm → enhanced text
3. dimmy_history_recent(1) → get the row id we just inserted
4. dimmy_history_update_enhanced(id, enhanced_text) → backfill column
```

Mirror in Swift: same sequence post-LLM call.

---

## Feature 3 — Audio file load

### Rust core (already cross-platform)
- `core/src/ffi.rs::dimmy_transcribe_file(path, out_buf, len)` — load WAV
  via hound, downmix to mono, run preprocess, route per backend, save to
  history.

### What Mac needs

```swift
import UniformTypeIdentifiers

// Drag-drop on a SwiftUI view:
.onDrop(of: [.audio], isTargeted: nil) { providers in
    for provider in providers {
        provider.loadObject(ofClass: URL.self) { url, _ in
            guard let url = url as? URL,
                  url.pathExtension.lowercased() == "wav" else { return }
            DispatchQueue.global().async {
                let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: 4 * 1024 * 1024)
                let rc = dimmy_transcribe_file(url.path, buf, Int32(4 * 1024 * 1024))
                // parse rc + buf as in Win
            }
        }
    }
    return true
}

// File picker:
NSOpenPanel.runModal with allowedContentTypes = [.audio]
```

MVP scope = WAV only. MP3/M4A via `symphonia` decoder is a follow-up
on both platforms (single shared FFI add).

---

## Feature 4 — Meeting mode

### Rust core (already cross-platform)
- `core/src/meeting.rs::MeetingSession` — streaming WAV writer + chunked
  transcribe + transcripts.txt persistence
- FFI: `dimmy_meeting_start`, `_stop`, `_save_post_process`,
  `_list_orphans`, `_is_active`
- `dimmy_llm_call_raw(prompt, model_override, max_tokens, …)` — used by
  the post-process pipeline (recap + actions) **bypassing dictation
  llm_style**. Critical: without this the recap returns the prompt
  verbatim (the bug we hit in v1).

### What Mac needs

`MeetingWindow` Swift equivalent (full-window or HUD). Win XAML pattern:
- Status row: pulsing red dot when recording, timer (HH:mm:ss), chunk count
- Start / Stop & process buttons
- Generate recap + actions checkbox (default on)
- ScrollView showing live transcript (poll `transcripts.txt` every 2 s)
- Recap + Actions panels collapsed until post-process completes
- Footer: meeting dir path + "Open folder" button

Post-process LLM model picker:
```swift
func pickRecapModel() -> String {
    // Read llm_api_url from config.json
    if url.contains("anthropic.com") { return "claude-opus-4-7" }
    if url.contains("googleapis.com") { return "gemini-2.5-pro" }
    return ""  // fall back to user's llm_api_model (Groq Llama 3.3 etc)
}
```

### Hotkey gate

While `dimmy_meeting_is_active() == 1`, swallow the dictation hotkey.
Both platforms share cpal — parallel recording corrupts the buffer.

### Tray menu meeting toggle

`TrayService` on Win exposes "Open Meeting…" between Show Pill and
Settings. Mac NSStatusItem menu should show the same entry.

---

## Phase 3 polish — what's there + what's TODO

**Done:**
- Drag-to-reorder app rules
- Per-rule category icons (Segoe Fluent on Win → SF Symbols on Mac)
- Tray menu meeting entry
- Real LLM recap call (raw FFI, model auto-pick)

**TODO (acknowledged scope cuts):**
- Word timestamps end-to-end (Whisper segments + Parakeet alignments
  → `word_timestamps_json` column → click waveform to highlight word)
- SVG brand icon library (SimpleIcons + SHGetFileInfo / NSWorkspace
  iconForFile runtime fallback)
- Smart-detect long dictation at stop ("Looks like a meeting — generate
  recap?" toast)
- Audio-load → Meeting view auto-promote when >5 min
- Real progress bar during file-load (chunked engine emits progress)
- Pill right-click meeting toggle
- Jump-list "Start Meeting" entry

---

## Test plan once Mac side is wired

1. **App context**: open Slack, hold hotkey, dictate, release → check
   `dimmy.log` for `[AppRules] resolve ctx(... bundle='com.tinyspeck.slackmacgap' ...)`
   matched against the Slack rule. LLM should produce imbruttito output.
2. **History v2**: Settings → Recordings, verify rows show app name +
   enhanced toggle. Toggle "save audio" on → record → verify WAV in
   `~/Library/Application Support/dimmy/history_audio/<id>.wav`.
3. **File load**: drag a .wav onto Settings → result panel + history row.
4. **Meeting**: tray menu → Open Meeting → Start → talk for 30+ s → Stop &
   process → verify recap + actions appear and are saved as `recap.md` +
   `actions.json` in `<config>/meetings/<uuid>/`.

---

## Branch state

```
b...  feat/v2-unified
├── 5 base merges (parakeet-stt-local + 4 features)
├── fix(meeting/win): missing card + diagnostic logs
├── merge: feat/app-context
├── merge: feat/audio-load
├── merge: feat/history-v2
├── fix(v2): CRITICAL — LLM raw FFI + drag/picker + app-context diag
├── fix(v2): IMPORTANT — placeholder, hotkey gate, update_enhanced, prune
├── feat(v2): Phase 3 — drag-reorder + per-rule icons + tray meeting menu
└── docs(v2): mac handoff
```

When Mac side lands → PR `feat/v2-unified` against `main` → staging build →
prod release.

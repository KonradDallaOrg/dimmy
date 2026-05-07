# Meeting UI port plan — standalone HTML → MeetingWindow

> **Source:** `docs/dev/refs/meeting-standalone.html` (1.6 MB Figma Make
> bundle). The actual rendered UI is React+JSX inside; the file is
> committed unbuilt as a visual reference for the port.

## Standalone analysis (extracted from CSS classes + JSX class names)

### Multi-state single window
The standalone packs **four states into one window**, each with its
own panel layout:

| State | Surface | Wired to |
|---|---|---|
| **Idle** (`.m-idle*`) | Hero greeting + start button + idle hints | `dimmy_meeting_start` |
| **Recording** (`.m-rec-bar`, `.m-live*`) | Live waveform + transcript scroll + bookmark button | poll `transcripts.txt` |
| **Processing** (`.m-proc*`) | Spinner + ordered steps (Stop → Save → LLM → Done) | `dimmy_meeting_stop` + `dimmy_llm_call_raw` |
| **Done** (`.m-done*`, `.m-summary`, `.m-actions`) | Recap sections + action items + audio waveform card | already in `_save_post_process` |

State machine driven by `_meetingState ∈ {Idle, Recording, Processing, Done}`.

### Major panels
- **Title bar** (`.m-titlebar*`) — Win11-style with custom drag region
- **Tabs** (`.m-tabs`, `.m-tab`) — switches between current meeting and history
- **Live transcript** (`.m-live-body`, `.m-live-empty`) — scroll list
- **Recap** (`.m-summary`, `.m-h3`, `.m-bullet`) — sectioned + bulleted
- **Action items** (`.m-actions`) — checkable list with owner/due
- **Q&A pair** (`.m-qa-pair`, `.m-qa-q`, `.m-qa-a`, `.m-qa-prompt`) — user asks the LLM about the meeting
- **History list** (`.m-hist-list`, `.m-hist-item`) — past meetings + search
- **Audio waveform card** (`.m-wave-card`, `.m-wave-meta`) — playback after Done
- **Bookmarks** (`.m-bookmarks`, `.bm-time`, `.bm-label`, `.bm-del`) — markers placed during recording
- **App context** (`.m-app-icon`, `.m-app-name`) — foreground app captured

### Supporting UI elements
- `.m-tb-btn`, `.m-icon-btn` — toolbar / icon buttons
- `.m-rec-time`, `.m-rec-status`, `.m-rec-label` — recording HUD
- `.m-search-bar`, `.m-search-clear` — history search
- `.m-stage`, `.m-section`, `.m-list` — generic layout primitives
- `.m-notes-hint`, `.m-idle-hints` — onboarding inline tips

## What we already have (current `MeetingWindow.xaml`)

- Stop/Start buttons → `dimmy_meeting_start/_stop`
- Live transcript polling → `transcripts.txt` every 2 s with `_lastTranscriptLen` cache + `FileShare.ReadWrite`
- Generate-recap-on-stop checkbox
- Recap + Actions panels (collapsed → expanded after stop)
- Open-folder button
- Status dot (idle / recording / done) + timer
- LLM raw call via `dimmy_llm_call_raw` + `PickRecapModel` (Anthropic→Opus, Gemini→Pro, else user choice)
- `dimmy_meeting_save_post_process` to persist recap.md / actions.json

## Gap matrix — what we port, what we skip

| Standalone feature | Has backend? | Action |
|---|---|---|
| Idle / Recording / Processing / Done state machine | yes | **Port** — split current stack into 4 panels with Visibility binding |
| Live waveform during recording | partial — amplitude FFI exists (`dimmy_get_amplitude`), pill consumes it | **Port** — same 12 Hz poll, draw bars |
| Audio waveform card in Done state | yes — `WavPeaks.cs` already reads peaks | **Port** — show after Stop |
| Recap sectioned bullets | yes — but we currently render as flat text | **Port** + new structured prompt below |
| Action items list | yes — `actions.json` already persisted | **Port** — checkable items |
| App context shown during recording | yes — `dimmy_set_app_context` already fires on hotkey, exposed via foreground capture at meeting start | **Port** — capture on Start, display |
| Bookmarks during recording | NO backend — needs new `bookmarks.json` writer | **Skip** — leave UI shell visible but disabled. Backlog. |
| Q&A panel ("ask the LLM about this meeting") | NO — needs new FFI to seed transcript context + ask | **Skip** — UI shell only, "Coming soon" hint. Backlog. |
| History list of past meetings | partial — meetings dirs exist, no list endpoint | **Port** — enumerate `meetings/` dir, sort by mtime |
| History search | NO — no FTS on meeting transcripts | **Skip** — input visible, filter client-side via title only |
| Tabs (current / history) | NO state, but trivially derivable | **Port** — pure UI |
| Toolbar buttons (open folder, copy, share) | partial | **Port** what we have |

## New structured recap prompt

Current prompt is 2 sections (RECAP + ACTIONS) — minimal. New prompt
mirrors what Notion / Granola produce so the user gets a "wow" recap:

```
You are an expert meeting analyst. Output ONLY the markdown sections
below in the SAME LANGUAGE as the transcript (auto-detect, do not
translate). Use the EXACT marker headings shown so a downstream
parser can split the response.

## ===TLDR===
1-2 sentence executive summary.

## ===KEY_DECISIONS===
Bullet list. Each: "**[topic]** — [decision verbatim or paraphrased]
([owner] decided)".

## ===TOPICS===
Group the discussion into 3-7 topics. For each:
- ### Topic title (1-3 words)
- 2-4 bullet points capturing what was discussed
- Quote the most important sentence verbatim ("> ...") if one exists

## ===ACTIONS===
Numbered list of action items. Each: "N. **[owner]** — [task] (due:
[date / event / 'unspecified'])". Include only actions explicitly
spoken; do NOT invent.

## ===OPEN_QUESTIONS===
Bullet list. Things raised but not resolved. "—" if none.

## ===RISKS===
Bullet list. Risks / blockers / dependencies surfaced. "—" if none.

## ===NEXT_STEPS===
Numbered list of immediate next steps (different from Actions: these
are the meeting's overall trajectory, not assigned tasks).

Hard rules:
- Output the sections in the exact order above.
- Same language as the transcript; if the transcript is mixed,
  pick the dominant one.
- Never invent participants, dates, amounts, project names, or
  technical terms not in the transcript.
- Skip a section entirely (still emit the marker + "—") if the
  transcript has no content for it.
- No filler: "the meeting discussed", "various topics were", etc.
- No em-dashes in prose (`—`) outside the markers — they read as AI
  slop. Use periods or commas.

Transcript:
{transcript}
```

Parser updates: `ParsePostProcessResponse` splits on the new markers.
Each section becomes a separate UI block in the Done state.

## Implementation phases

1. **Theme bug fix** ✅ already done in this branch
2. **State machine + 4-panel layout** — ~3 h XAML + code-behind
3. **Idle hero + history list** — ~2 h
4. **Recording panel** (live waveform + transcript + app context) — ~2 h
5. **Processing panel** — ~30 min
6. **Done panel** (sectioned recap + actions + waveform card) — ~3 h
7. **New recap prompt + parser** — ~1 h
8. **Bookmarks / Q&A as disabled UI shells** — ~1 h
9. **Build + manual test** — 1 h

Total: ~14 h, split over 2-3 sessions. Mac side mirrors via
SwiftUI (separate handoff doc).

## Cross-platform notes

- The standalone has `data-theme="light"` and `"dark"` variants and
  separate `winwin` / `mac-shell` skins. Win-side port focuses on
  `winwin` styling; the SwiftUI port consumes the same prompt + FFI
  surface so the data layer is identical.
- App-icons reuse the existing `IconExtractor` cache.
- Audio peaks reuse `WavPeaks.cs`.
- All FFI entries already exist — no Rust changes needed.

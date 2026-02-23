# Vocino - Product Roadmap

**Date:** 2026-02-23

---

## Release Timeline

```
v0.1  [DONE]    Core app — recording, transcription, auto-paste, overlay
  |
v1.1  [4-6 wk]  AI cleanup + voice commands + smart formatting
  |
v1.2  [2-3 wk]  Noise suppression + clipboard history
  |
v1.3  [4-6 wk]  Local Whisper (offline mode, hybrid architecture)
  |
v1.5  [3-4 wk]  Custom vocabulary + code dictation + translation + PRO launch
  |
v2.0  [6-8 wk]  Live captions + VS Code extension + API + team features
```

---

## v1.1 — AI Enhancement (4-6 weeks)

The highest-ROI release. Transforms Vocino from "yet another Whisper frontend" into an intelligent dictation tool.

### Features

| Feature | Impact | Effort | Details |
|---------|--------|--------|---------|
| AI post-processing | 9/10 | 3/10 | Clean/Professional/Custom modes via LLM |
| Voice commands | 8/10 | 4/10 | "scratch that", "new paragraph", "undo" |
| Smart formatting | 7/10 | 3/10 | Dates, numbers, addresses auto-formatted |

### Technical Approach
- Add LLM processing step between transcription and paste
- New settings: processing mode selector, LLM endpoint (can differ from STT)
- Voice commands: client-side pattern matching on transcription output with delay buffer
- Smart formatting: LLM prompt rules or regex preprocessor

### Competitive Impact
- Matches Wispr Flow's #1 selling feature (AI cleanup) without cloud lock-in
- Voice commands match what Dragon users miss most
- No competitor in the OSS space offers this

---

## v1.2 — Quality & History (2-3 weeks)

Incremental release focused on input quality and "never lose a transcription."

### Features

| Feature | Impact | Effort | Details |
|---------|--------|--------|---------|
| RNNoise integration | 7/10 | 4/10 | Real-time noise suppression via C FFI |
| Clipboard history | 7/10 | 5/10 | SQLite-backed searchable transcription log |

### Technical Approach
- RNNoise: Rust FFI binding, process audio buffer before chunking
- History: SQLite via `tauri-plugin-sql`, new overlay panel, Ctrl+Shift+V hotkey
- Export options: JSON, TXT, CSV

---

## v1.3 — Local Whisper (4-6 weeks)

The marquee feature. Fully offline transcription. The biggest engineering effort but the biggest differentiator.

### Features

| Feature | Impact | Effort | Details |
|---------|--------|--------|---------|
| whisper.cpp integration | 9/10 | 7/10 | Via `whisper-rs` crate |
| Hybrid cloud/local | 8/10 | 3/10 | Auto-fallback, user toggle |
| Model manager | 6/10 | 4/10 | Download, select model size |

### Architecture

```
User speaks
    |
    v
[Audio capture] --> [RNNoise] --> [Router]
                                     |
                          +----------+----------+
                          |                     |
                   [Local whisper.cpp]    [Cloud API]
                          |                     |
                          +----------+----------+
                                     |
                                     v
                          [AI post-processing]
                                     |
                                     v
                              [Auto-paste]
```

### Model Options (shipped)

| Model | Size | VRAM | Quality | Speed |
|-------|------|------|---------|-------|
| tiny | 75MB | <1GB | Basic | Instant |
| base | 142MB | <1GB | Good | Real-time |
| small | 466MB | ~1GB | Better | Real-time |
| medium | 1.5GB | ~2GB | Great | Near real-time |
| large-v3-turbo | 3GB | ~4GB | Excellent | Real-time on GPU |

First-run wizard: auto-detect hardware, recommend model, one-click download.

---

## v1.5 — Pro Launch (3-4 weeks)

Monetization starts. Launch Pro tier alongside power-user features.

### Features

| Feature | Impact | Effort | Details |
|---------|--------|--------|---------|
| Custom vocabulary | 8/10 | 5/10 | Glossary file + Whisper initial_prompt |
| Code dictation mode | 7/10 | 6/10 | Specialized LLM prompt for code |
| Real-time translation | 6/10 | 4/10 | LLM-based, on top of post-processing |
| **Pro tier** | — | 4/10 | Cloud proxy, AI modes, sync, billing |

### Pro Infrastructure
- Simple backend: API proxy + Stripe billing
- Could use Supabase/PocketBase for auth + sync
- Billing: Stripe checkout, manage via customer portal
- Licensing: License key validated on app start (offline grace period)

---

## v2.0 — Platform Expansion (6-8 weeks)

New user segments: accessibility, developers, automation, teams.

### Features

| Feature | Impact | Effort | Details |
|---------|--------|--------|---------|
| Live captions overlay | 8/10 | 5/10 | System audio + customizable captions |
| VS Code extension | 6/10 | 6/10 | WebSocket to local Vocino API |
| Webhook/REST API | 5/10 | 3/10 | Local HTTP server, events |
| Team features | 7/10 | 6/10 | Shared library, roles, SSO |

---

## Feature Impact/Effort Matrix

```
                     LOW EFFORT              MEDIUM EFFORT            HIGH EFFORT
                 |                       |                        |
HIGH IMPACT      | AI Post-Process (v1.1)| Local Whisper (v1.3)   |
                 | Voice Commands (v1.1) | Custom Vocabulary(v1.5)|
                 | Smart Formatting(v1.1)| Clipboard History(v1.2)|
                 | Noise Suppres.  (v1.2)|                        |
                 |                       |                        |
MEDIUM IMPACT    | Translation    (v1.5) | Code Dictation  (v1.5) | Speaker Diarization
                 | Webhook/API    (v2.0) | Live Captions   (v2.0) |   (future)
                 |                       | VS Code Plugin  (v2.0) |
                 |                       |                        |
LOW IMPACT       |                       | Calendar Integration   | Multi-Device
                 |                       |   (not planned)        |   (not planned)
```

---

## Risk Mitigation

| Risk | Probability | Mitigation |
|------|------------|------------|
| Local Whisper quality insufficient | Low | Hybrid fallback to cloud; turbo model is within 1-2% |
| AI post-processing adds latency | Medium | "Paste raw, then refine" mode; Groq LLM <500ms |
| Apple/Microsoft improve built-in dictation | Medium | Modular architecture; OSS community; advanced features (code, vocab) |
| Audio-native LLMs obsolete Whisper pipeline | Medium (18mo) | Swappable STT backend; monitor Voxtral, Parakeet |
| Market too crowded | Low | Nobody owns cross-platform + local + AI. Tauri gives technical moat. |

---

## Key Architectural Decisions

1. **Modular STT backend** — Abstract transcription behind a trait/interface so we can swap Whisper for Parakeet, Voxtral, or future models
2. **LLM post-processing as separate pipeline step** — Not coupled to STT. Works with any transcription source.
3. **Local-first data** — SQLite for history, JSON for config, keyring for secrets. Cloud sync is additive, never required.
4. **Plugin system** (v2.0+) — Expose events/hooks for community extensions. Learn from Obsidian's API design.

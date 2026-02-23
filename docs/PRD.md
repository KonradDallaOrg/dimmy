# Vocino - Product Requirements Document

**Version:** 1.0
**Date:** 2026-02-23
**Author:** Konrad + AI Research Agents
**Status:** Draft

---

## 1. Vision

Vocino is a **cross-platform, privacy-first voice transcription overlay** that lets users dictate text into any application. It aims to be the tool that finally bridges the gap between local privacy, cloud accuracy, and modern AI post-processing — something no competitor currently delivers.

**One-liner:** The Obsidian of voice transcription — open source core, works locally, optional cloud for power features.

---

## 2. Problem Statement

Voice transcription in 2026 is fragmented along three axes:

| Axis | Current State | User Pain |
|------|--------------|-----------|
| **Privacy vs Quality** | Local tools are hard to set up; cloud tools leak data | Users forced to choose |
| **Platform** | Best tools are Mac-only (SuperWhisper, MacWhisper) | Windows/Linux underserved |
| **Raw vs Smart** | Most tools output raw text; few do AI cleanup | Manual post-editing wastes time |

**Key finding from research:** No single product delivers local+cloud hybrid, cross-platform, AI-enhanced dictation. Buzz (OSS) comes closest on privacy/platform but lacks AI. Wispr Flow ($15/mo) has AI but is cloud-only with privacy concerns (800MB RAM idle, screen context collection).

### Target Users (Priority Order)

1. **Knowledge workers** — Writers, journalists, researchers who dictate long-form content
2. **Developers with RSI** — Need voice input but current tools don't understand code
3. **Multilingual professionals** — Switch languages naturally, need accurate transcription in both
4. **Privacy-conscious users** — Corporate/legal/medical environments where cloud is not an option
5. **Accessibility users** — Physical impairments requiring voice as primary input

---

## 3. Current State (v0.1.0)

Vocino already ships:

- Tauri v2 desktop app (Rust backend, ~11MB binary)
- Real-time audio recording via system microphone (cpal)
- Streaming chunk transcription via OpenAI-compatible APIs (Groq free, OpenAI paid)
- Auto-paste transcribed text into active application (clipboard + Ctrl+V)
- Always-on-top minimal overlay with waveform visualization
- Global hotkey (Win+Alt) with toggle and hold modes
- Secure API key storage via OS keyring
- Settings panel with provider/model/language/device selection
- Positions above taskbar dynamically

### Technical Stack

| Layer | Technology |
|-------|-----------|
| Framework | Tauri v2 |
| Backend | Rust |
| Frontend | Vanilla HTML/JS/CSS |
| Audio | cpal 0.15 |
| HTTP | reqwest 0.12 |
| Keyring | keyring 3 (platform-native) |
| Build | cargo-tauri, PowerShell (Windows) |

---

## 4. Product Requirements

### 4.1 Tier 1 — v1.1-v1.2 (High Impact, Low-Medium Effort)

#### FR-1: AI Post-Processing Pipeline
**Priority:** P0
**Effort:** 1-2 weeks

- After transcription, pass text through LLM for enhancement
- Processing modes: Raw (current), Clean, Professional, Custom
  - **Clean:** Fix grammar, punctuation, remove filler words (um, uh, like)
  - **Professional:** Rewrite for formal tone
  - **Custom:** User-defined system prompt
- Use same OpenAI-compatible API already configured (or dedicated LLM endpoint)
- Show processing status in overlay

**Acceptance Criteria:**
- User can select processing mode from settings
- "Clean" mode removes >90% of filler words and adds punctuation
- Processing adds <2s latency on Groq
- Raw mode remains available (zero added latency)

#### FR-2: Voice Command Recognition
**Priority:** P0
**Effort:** 1-2 weeks

Core commands (detected as keyword patterns in transcription output):
- `new paragraph` / `new line` — insert formatting
- `delete last sentence` / `delete last word` / `scratch that` — corrections
- `period` / `comma` / `question mark` — explicit punctuation
- `undo` — revert last action
- `stop listening` / `resume` — pause control

**Acceptance Criteria:**
- Commands are intercepted before paste (not pasted as text)
- Detection works in at least English and Italian
- False positive rate <1% on normal speech
- Commands configurable/disablable

#### FR-3: Smart Formatting
**Priority:** P1
**Effort:** 1 week (piggybacks on FR-1)

- Dates: "january fifth twenty twenty six" -> "January 5, 2026"
- Numbers: "three hundred forty two dollars" -> "$342"
- Emails, phone numbers, addresses spoken naturally
- Handled by LLM prompt in post-processing pipeline

#### FR-4: Noise Preprocessing (RNNoise)
**Priority:** P1
**Effort:** 1-2 weeks

- Integrate RNNoise (C library, Rust FFI) for real-time noise suppression
- Apply before sending audio chunks to transcription API
- Toggle in settings (on/off)
- Immediate accuracy improvement in noisy environments

**Acceptance Criteria:**
- Background noise (fan, AC, keyboard) is suppressed
- Voice quality is not degraded
- CPU overhead <5% on modern hardware

### 4.2 Tier 2 — v1.3-v1.5 (High Impact, Medium Effort)

#### FR-5: Local/Offline Transcription (whisper.cpp)
**Priority:** P0
**Effort:** 4-6 weeks

- Integrate whisper.cpp via `whisper-rs` crate
- Hybrid architecture:
  - **Default:** Cloud transcription (current behavior)
  - **Offline fallback:** Auto-detect internet loss, switch to local
  - **Privacy mode:** Force local-only (user toggle)
- Recommended default local model: whisper-large-v3-turbo (best speed/accuracy balance)
- Auto-detect hardware capabilities (GPU VRAM, CPU cores)
- Let users choose model size

Hardware requirements (from benchmarks):
| Model | VRAM | Speed | Accuracy |
|-------|------|-------|----------|
| large-v3-turbo | ~4GB | 6x real-time | Within 1-2% of cloud |
| medium | ~2GB | Near real-time | Good for dictation |
| small | ~1GB | Real-time on CPU | Acceptable |

**Acceptance Criteria:**
- Works fully offline with no API key
- Model download managed in-app (first run wizard)
- Latency <1s for sentence completion on GPU
- Graceful fallback: GPU -> CPU if no GPU available

#### FR-6: Clipboard History with Search
**Priority:** P1
**Effort:** 2-3 weeks

- Every transcription saved with timestamp and metadata
- Searchable history panel (hotkey: Ctrl+Shift+V or configurable)
- SQLite backend via tauri-plugin-sql
- Exportable (JSON, TXT, CSV)

#### FR-7: Custom Vocabulary
**Priority:** P1
**Effort:** 2-3 weeks

Two layers:
- **Pre-transcription:** Inject via Whisper `initial_prompt` parameter
- **Post-transcription:** LLM glossary in system prompt
- User-editable glossary file (JSON)
- Auto-learn: track user corrections, suggest additions

### 4.3 Tier 3 — v2.0 (Medium Impact, Medium-High Effort)

#### FR-8: Code Dictation Mode
- Specialized LLM prompt converts natural language to code
- Language-aware (detect from active window title)
- Supports common patterns: variable naming conventions, syntax structures

#### FR-9: Live Captions Overlay
- System audio capture (not just microphone)
- Customizable appearance (font size, colors, position)
- Cross-platform (Windows Live Captions is Win11 only)

#### FR-10: Real-Time Translation
- Speak in language A, get text in language B
- Piggybacks on AI post-processing pipeline (FR-1)

#### FR-11: VS Code Extension
- WebSocket connection to Vocino's local API
- Pipe transcriptions to editor at cursor position
- Auto-activate code dictation when code file is active

#### FR-12: Webhook/REST API
- Local REST API for automation integrations
- Events: transcription_complete, command_detected
- Enables Zapier/n8n/home automation integration

---

## 5. Non-Functional Requirements

### NFR-1: Performance
- Audio-to-text latency: <500ms (cloud), <1s (local GPU), <3s (local CPU)
- Binary size: <20MB (excluding local models)
- RAM usage: <100MB idle, <500MB during recording
- No background CPU usage when not recording

### NFR-2: Privacy
- Zero telemetry in open-source core
- All data stays local unless user explicitly configures cloud
- API keys stored in OS keyring only
- HTTPS enforced for remote API calls (already implemented)
- Log files contain no sensitive data

### NFR-3: Security
- CSP enforced (already implemented)
- Minimal Tauri permissions (already audited)
- No eval(), no innerHTML with user data
- Buffer caps to prevent memory exhaustion (already implemented: 30min)
- Log rotation (already implemented: 1MB)

### NFR-4: Compatibility
- Windows 10/11 (primary)
- macOS 12+ (secondary)
- Linux (tertiary, community-driven)
- Works with any OpenAI-compatible transcription API

---

## 6. Success Metrics

| Phase | Metric | Target |
|-------|--------|--------|
| v1.1 Launch | GitHub stars | 500+ in first month |
| v1.3 (Local) | Monthly active users | 5,000 |
| v1.5 (Pro launch) | Paid conversion rate | 2.5-3.7% |
| v2.0 | Monthly active users | 15,000 |
| Year 2 | Monthly recurring revenue | $5,000/month |

---

## 7. Out of Scope (for now)

- Mobile apps (iOS/Android)
- Meeting transcription with speaker diarization (defer to v2.x)
- Calendar integration
- Multi-device transcription (phone calls via desktop)
- App Store distribution (sandbox conflicts with local models)

---

## 8. Open Questions

1. **Brand name:** Vocino is good. Keep it or explore alternatives?
2. **Local model distribution:** Bundle models in installer or download on first run?
3. **Pro tier pricing:** $9/month vs $79 lifetime vs both?
4. **Plugin system:** When to introduce? What API surface to expose?

---

## References

- See `docs/MARKET-RESEARCH.md` for competitive landscape and pain points
- See `docs/BUSINESS-MODEL.md` for monetization strategy and financial projections
- See `docs/ROADMAP.md` for release timeline

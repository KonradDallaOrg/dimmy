<p align="center">
  <img src="src-tauri/icons/icon.png" width="128" alt="Dimmy">
</p>

<h1 align="center">Dimmy</h1>

<p align="center">
  Cross-platform voice transcription overlay. Speak anywhere, text appears everywhere.
</p>

<p align="center">
  <a href="https://github.com/KonradDallaOrg/dimmy/actions/workflows/ci.yml"><img src="https://github.com/KonradDallaOrg/dimmy/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/KonradDallaOrg/dimmy/releases/latest"><img src="https://img.shields.io/github/v/release/KonradDallaOrg/dimmy?label=download&color=34d399" alt="Latest Release"></a>
  <a href="https://github.com/KonradDallaOrg/dimmy/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-818cf8" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-6366f1" alt="Platform">
</p>

---

Dimmy sits as a tiny always-on-top pill on your screen. Press a keyboard shortcut, speak, and the transcribed text is automatically pasted into whatever app has focus. Optionally enhance with AI post-processing (grammar correction, summarization, tone adjustment).

## Features

- **Universal dictation** — works with any application via clipboard paste
- **Always-on-top overlay** — minimal pill UI with waveform visualization
- **Streaming transcription** — real-time chunks via OpenAI-compatible APIs
- **Realtime preview** — optionally send chunks while recording, or wait for final result
- **AI enhancement** — post-process with LLM (correct, summarize, elaborate, 13 styles total)
- **Multiple providers** — Groq, OpenAI, Deepgram, Gemini, Anthropic, or any custom endpoint
- **Anti-hallucination guard** — skips audio chunks with less than 0.5s of speech
- **Per-provider API keys** — encrypted locally by default, optional OS keyring, switch without re-entering
- **Audio preprocessing** — noise filtering + normalization for cleaner input
- **Configurable shortcut** — toggle or hold mode, any 2-modifier combo
- **Multilingual** — auto-detect or select from 12+ languages
- **Privacy-first** — no telemetry, all data local, keys encrypted on device
- **Auto-update** — built-in update checker with one-click install

## Screenshots

<p align="center">
  <img src="docs/screenshots/dimmy-pill-states.png" alt="Dimmy pill states" width="560">
</p>

<details>
<summary><strong>Settings panel</strong></summary>

<p align="center">
  <img src="docs/screenshots/dimmy-settings-transcription.png" alt="Transcription" width="220">
  <img src="docs/screenshots/dimmy-settings-ai.png" alt="AI Enhancement" width="220">
  <img src="docs/screenshots/dimmy-settings-activation.png" alt="Activation" width="220">
</p>
<p align="center">
  <img src="docs/screenshots/dimmy-settings-audio.png" alt="Audio" width="220">
  <img src="docs/screenshots/dimmy-settings-appearance.png" alt="Appearance" width="220">
  <img src="docs/screenshots/dimmy-settings-stats.png" alt="Stats" width="220">
</p>

</details>

## Download

Get the latest release for your platform:

**[Download Dimmy](https://github.com/KonradDallaOrg/dimmy/releases/latest)** — Windows (.msi), macOS (.dmg), Linux (.deb, .AppImage)

## Quick Start

1. Launch Dimmy — a small green dot appears in the corner of your screen
2. Open Settings (click the gear icon or right-click the pill)
3. Enter an API key for transcription (see [Get an API Key](#get-an-api-key) below)
4. Press **Win+Alt** (default) to start recording
5. Speak naturally
6. Press **Win+Alt** again to stop — text is transcribed and pasted into the active app

## Settings Guide

### Get an API Key

Dimmy needs an API key for speech-to-text transcription. Choose a provider:

| Provider | Type | Models | Free Tier | Get Key |
|----------|------|--------|-----------|---------|
| **Groq** (recommended) | STT + LLM | whisper-large-v3, whisper-large-v3-turbo, llama-3.3-70b | Yes (rate limited) | [console.groq.com/keys](https://console.groq.com/keys) |
| **OpenAI** | STT + LLM | gpt-4o-transcribe, gpt-4o-mini-transcribe, whisper-1, gpt-4o-mini | ~$0.006/min | [platform.openai.com/api-keys](https://platform.openai.com/api-keys) |
| **Deepgram** | STT | Nova-3, Nova-2 | $200 free credits | [console.deepgram.com](https://console.deepgram.com/) |
| **Google Gemini** | STT + LLM | gemini-2.5-flash, gemini-2.5-pro | Yes | [aistudio.google.com/apikey](https://aistudio.google.com/apikey) |
| **Anthropic** | LLM only | Claude Haiku 4.5, Claude Sonnet 4 | No | [console.anthropic.com/keys](https://console.anthropic.com/settings/keys) |
| **OpenRouter** | LLM only | Llama 3.3 70B, DeepSeek R1 | Yes (free models) | [openrouter.ai/keys](https://openrouter.ai/keys) |

Paste your key in Settings → **API Key**. Keys are encrypted locally on your device by default (AES-256) — no OS permission popups required. For extra security, enable **OS secure storage** (Keychain / Credential Manager) in Settings → Appearance. You can also use any **custom endpoint** compatible with the OpenAI API format.

### Transcription Settings

| Setting | Description |
|---------|-------------|
| **API URL** | Provider endpoint. Pre-filled for Groq/OpenAI, or enter a custom URL |
| **Model** | Whisper model to use (e.g. `whisper-large-v3-turbo` for Groq) |
| **Language** | Select a language or leave on "Auto-detect" for multilingual use (Deepgram auto-detects natively) |
| **Audio Device** | Choose which microphone to use |
| **Prompt** | Whisper prompt for vocabulary hints (e.g. proper nouns, acronyms) |
| **Preprocessing** | Toggle noise filtering and voice activity detection (recommended on) |
| **Realtime Preview** | Send audio chunks while recording for live preview, or wait for final result |

### Shortcut Settings

| Setting | Description |
|---------|-------------|
| **Mode: Toggle** (default) | Press once to start, press again to stop |
| **Mode: Hold** | Hold to record, release to stop |
| **Custom shortcut** | Click "Record new shortcut" and press any 2-modifier combo (e.g. Ctrl+Shift, Win+Alt) optionally with a regular key |

### AI Enhancement (LLM Post-Processing)

Enable in Settings to send transcriptions through an LLM for cleanup or transformation. Requires a separate LLM API key (or check "Use same key" if your transcription provider also offers chat completions).

| Setting | Description |
|---------|-------------|
| **LLM API URL** | Endpoint for chat completions (Groq, OpenAI, Gemini, Anthropic, or custom) |
| **LLM API Key** | Separate key for the LLM provider, or "Use same key as transcription" |
| **LLM Model** | Chat model to use (e.g. `llama-3.3-70b-versatile` for Groq) |
| **Style** | What the LLM does — scroll wheel on the pill to cycle |
| **Tone** | How it writes — Ctrl+scroll to cycle |
| **LLM Logging** | Save LLM input/output to `~/.dimmy/llm-log/` for debugging |

**Styles:**

| Style | Effect |
|-------|--------|
| Off | No LLM processing |
| Correct | Fix grammar and filler words |
| Summarize | Condense key points |
| Elaborate | Expand with detail |
| Comprehensible | Rewrite clearly |
| Professional | Business tone |
| Prompt | Reshape as LLM prompt |
| Gen-Z | Gen-Z slang rewrite |
| Boomer | Old-school formal rewrite |
| Emoji | Heavy emoji insertion |
| Acronyms | Replace phrases with abbreviations |
| Imbruttito | Milanese grumpy rewrite |
| Custom | Your own system prompt |

<details>
<summary><strong>STT Provider Benchmarks</strong></summary>

Benchmarked on real audio files (LibriVox, public domain). Match% = word overlap vs reference transcript. All files are 16kHz mono WAV.

#### Short audio (5s - 90s)

| Sample | Duration | Groq turbo | Groq v3 | OpenAI whisper-1 | OpenAI 4o-transcribe | OpenAI 4o-mini | Deepgram Nova-3 | Gemini Flash |
|--------|----------|-----------|---------|-----------------|---------------------|---------------|----------------|-------------|
| JFK "Ask not" | 11s | **815ms** 100% | 832ms 100% | 1834ms 100% | 1173ms 100% | 1188ms 100% | 2227ms 100% | 1569ms 100% |
| Micro Machines (fast) | 29s | **838ms** 100% | 1088ms 100% | 3545ms 73% | 5161ms 100% | 2211ms 100% | 3304ms 93% | 2689ms 93% |
| Gettysburg Address | 10s | **823ms** 100% | 716ms 100% | 1875ms 100% | 1563ms 100% | 1245ms 100% | 4008ms 100% | 1669ms 89% |
| Harvard (female, 8kHz) | 33s | **902ms** 100% | 943ms 100% | 1849ms 100% | 2349ms 100% | 1781ms 100% | 4390ms 100% | 2087ms 100% |

#### Medium audio (5 - 11 min)

| Sample | Duration | Groq turbo | Groq v3 | OpenAI whisper-1 | OpenAI 4o-mini | Deepgram Nova-3 | Gemini Flash |
|--------|----------|-----------|---------|-----------------|---------------|----------------|-------------|
| Pinocchio Ch.1 (EN) | 4:53 | **1.5s** 100% | 1.5s 100% | 15.1s 100% | 10.4s 100% | 19.1s 100% | 11.0s 100% |
| Tale of Two Cities | 6:49 | **1.7s** 100% | 2.1s 100% | 22.4s 100% | 14.6s 100% | 26.4s 100% | 10.0s 100% |
| Pride & Prejudice | 10:38 | **4.6s** 100% | 3.3s 100% | 34.1s 100% | 25.3s 100% | 66.0s 77% | 15.3s 100% |
| Pinocchio Cap.1 (IT) | 5:35 | **1.7s** 100% | 2.1s 100% | 16.5s 100% | 14.1s 100% | 40.5s 100% | 8.3s 100% |
| Divina Commedia (IT) | 42:00 | >25MB | >25MB | >25MB | >25MB | 249.2s | >20MB |

#### Key takeaways

- **Groq is 10-20x faster** than all other providers with equal or better accuracy
- **Deepgram** handles large files natively (2GB limit) but is slower on shorter audio
- **Gemini Flash** offers a good balance of speed and quality, especially for medium-length audio
- **OpenAI whisper-1** has good accuracy but is consistently the slowest
- Files over 25MB require chunking for Groq/OpenAI (Dimmy handles this automatically)

*Benchmark date: 2026-03-13. Run `./tests/test_benchmark.sh quick` to reproduce.*

</details>

## Auto-Update

Dimmy checks for updates automatically when you open Settings. The version number and update status appear at the bottom of the settings panel:

- **"Up to date"** — you're on the latest version
- **"Update vX.Y.Z available"** — click to download and install, then restart the app

## Build from Source

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Tauri CLI](https://tauri.app/): `cargo install tauri-cli --version '^2'`

**Linux:**
```bash
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libasound2-dev libxdo-dev
```

**macOS:**
```bash
xcode-select --install
```

**Windows:** Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the C++ workload.

### Build

```bash
cd src-tauri
cargo tauri build
```

> **Note:** `cargo tauri build` requires a signing key for the auto-updater. If you get an error about `TAURI_SIGNING_PRIVATE_KEY`, generate a local key:
> ```bash
> cargo tauri signer generate -w ~/.tauri/dimmy.key
> export TAURI_SIGNING_PRIVATE_KEY=$(cat ~/.tauri/dimmy.key)
> cargo tauri build
> ```
> This is only needed for release builds. For development, use `cargo tauri dev` instead (no key required).

## Development

```bash
cd src-tauri

# Run in dev mode
cargo tauri dev

# Run tests
cargo test

# Format
cargo fmt

# Lint
cargo clippy -- -D warnings
```

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Framework | Tauri v2 |
| Backend | Rust |
| Frontend | Vanilla HTML/JS/CSS |
| Audio | cpal |
| Noise filter | nnnoiseless + biquad |
| Secure storage | AES-256 local (default) + keyring (opt-in) |
| HTTP | reqwest |

## Support

If Dimmy saves you time, consider supporting its development:

- [Buy Me a Coffee](https://buymeacoffee.com/konraddall5)
- [Ko-fi](https://ko-fi.com/konraddalla)

## License

[AGPL-3.0](LICENSE) — free to use, modify, and distribute. If you redistribute or offer it as a service, your code must remain open source under the same license.

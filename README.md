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
  <a href="https://github.com/sponsors/KonradDallaOrg"><img src="https://img.shields.io/badge/sponsor-%E2%9D%A4-ea4aaa" alt="Sponsor"></a>
</p>

---

Dimmy sits as a tiny always-on-top pill on your screen. Press a keyboard shortcut, speak, and the transcribed text is automatically pasted into whatever app has focus. Optionally enhance with AI post-processing (grammar correction, summarization, tone adjustment).

## Features

- **Universal dictation** — works with any application via clipboard paste
- **Always-on-top overlay** — minimal pill UI with waveform visualization
- **Streaming transcription** — real-time chunks via OpenAI-compatible APIs
- **AI enhancement** — post-process with LLM (correct, summarize, elaborate, custom prompts)
- **Multiple providers** — Groq (free), OpenAI, or any custom endpoint
- **Per-provider API keys** — securely stored in OS keyring, switch without re-entering
- **Audio preprocessing** — noise filtering + normalization for cleaner input
- **Configurable shortcut** — toggle or hold mode, any 2-modifier combo
- **Multilingual** — auto-detect or select from 12+ languages
- **Privacy-first** — no telemetry, all data local, keys in OS secure storage
- **Auto-update** — built-in update checker with one-click install

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

| Provider | Get Key | Cost | Notes |
|----------|---------|------|-------|
| **Groq** (recommended) | [console.groq.com/keys](https://console.groq.com/keys) | Free tier available | Fast, free for moderate usage |
| **OpenAI** | [platform.openai.com/api-keys](https://platform.openai.com/api-keys) | Pay-per-use | Original Whisper provider |
| **Custom endpoint** | Your provider's dashboard | Varies | Any OpenAI-compatible API |

In Settings, paste your key in the **API Key** field. Keys are stored securely in your OS keyring (Windows Credential Manager, macOS Keychain, Linux Secret Service) — never in plain text.

### Transcription Settings

| Setting | Description |
|---------|-------------|
| **API URL** | Provider endpoint. Pre-filled for Groq/OpenAI, or enter a custom URL |
| **Model** | Whisper model to use (e.g. `whisper-large-v3-turbo` for Groq) |
| **Language** | Select a language or leave on "Auto-detect" for multilingual use |
| **Audio Device** | Choose which microphone to use |
| **Prompt** | Whisper prompt for vocabulary hints (e.g. proper nouns, acronyms) |
| **Preprocessing** | Toggle noise filtering and voice activity detection (recommended on) |

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
| **LLM API URL** | Endpoint for chat completions (Groq, OpenAI, or custom) |
| **LLM API Key** | Separate key for the LLM provider, or "Use same key as transcription" |
| **LLM Model** | Chat model to use (e.g. `llama-3.3-70b-versatile` for Groq) |
| **Style** | What the LLM does — scroll wheel on the pill to cycle |
| **Tone** | How it writes — Ctrl+scroll to cycle |
| **LLM Logging** | Save LLM input/output to `~/.dimmy/llm-log/` for debugging |

**Styles:**

| Style | Effect |
|-------|--------|
| Correct | Fix grammar and filler words |
| Summarize | Condense key points |
| Elaborate | Expand with detail |
| Comprehensible | Rewrite clearly |
| Professional | Business tone |
| Prompt | Reshape as LLM prompt |
| Custom | Your own system prompt |

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
| Secure storage | keyring (OS-native) |
| HTTP | reqwest |

## Support

If Dimmy saves you time, consider supporting its development:

- [GitHub Sponsors](https://github.com/sponsors/KonradDallaOrg)
- [Buy Me a Coffee](https://buymeacoffee.com/konraddall5)
- [Ko-fi](https://ko-fi.com/konraddalla)

## License

[AGPL-3.0](LICENSE) — free to use, modify, and distribute. If you redistribute or offer it as a service, your code must remain open source under the same license.

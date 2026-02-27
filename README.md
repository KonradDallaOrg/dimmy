# Dimmy

Cross-platform voice transcription overlay. Speak anywhere, text appears everywhere.

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

| Platform | Download |
|----------|----------|
| Windows | [Dimmy_x64.msi](https://github.com/KonradDallaOrg/dimmy/releases/latest) |
| macOS (Apple Silicon) | [Dimmy_aarch64.dmg](https://github.com/KonradDallaOrg/dimmy/releases/latest) |
| macOS (Intel) | [Dimmy_x64.dmg](https://github.com/KonradDallaOrg/dimmy/releases/latest) |
| Linux (Debian/Ubuntu) | [Dimmy_amd64.deb](https://github.com/KonradDallaOrg/dimmy/releases/latest) |
| Linux (AppImage) | [Dimmy_amd64.AppImage](https://github.com/KonradDallaOrg/dimmy/releases/latest) |

## Build from Source

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Tauri CLI](https://tauri.app/): `cargo install tauri-cli --version '^2'`

#### Linux

```bash
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libasound2-dev libxdo-dev
```

#### macOS

```bash
xcode-select --install
```

#### Windows

Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the C++ workload.

### Build

```bash
cd src-tauri
cargo tauri build
```

The binary will be in `src-tauri/target/release/`.

## Quick Start

1. Launch Dimmy — a small green dot appears in the corner of your screen
2. Open Settings (gear icon) and enter an API key (get a free one at [groq.com](https://console.groq.com/keys))
3. Press **Win+Alt** (default) to start recording
4. Speak naturally
5. Press **Win+Alt** again to stop — text is transcribed and pasted into the active app

### Shortcut Modes

- **Toggle** (default) — press once to start, press again to stop
- **Hold** — hold to record, release to stop

### AI Enhancement

Enable in Settings to post-process transcriptions with an LLM:

| Style | Effect |
|-------|--------|
| Correct | Fix grammar and filler words |
| Summarize | Condense key points |
| Elaborate | Expand with detail |
| Comprehensible | Rewrite clearly |
| Professional | Business tone |
| Prompt | Reshape as LLM prompt |
| Custom | Your own prompt |

Scroll wheel on the pill to cycle styles. Ctrl+scroll to cycle tones.

## Auto-Update

Dimmy checks for updates when you open Settings. If an update is available, click the update link to download and install. Restart the app to apply.

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

## License

MIT

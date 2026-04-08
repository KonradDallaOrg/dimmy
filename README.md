<p align="center">
  <img src="docs/screenshots/icon.png" width="128" alt="Dimmy">
</p>

<h1 align="center">Dimmy</h1>

<p align="center">
  Speak instead of typing — up to 3x faster. Native voice dictation for Windows, macOS, and Linux.
</p>

<p align="center">
  <a href="https://github.com/KonradDallaOrg/dimmy/actions/workflows/ci.yml"><img src="https://github.com/KonradDallaOrg/dimmy/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/KonradDallaOrg/dimmy/releases/latest"><img src="https://img.shields.io/github/v/release/KonradDallaOrg/dimmy?label=download&color=34d399" alt="Latest Release"></a>
  <a href="https://github.com/KonradDallaOrg/dimmy/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-818cf8" alt="License"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-6366f1" alt="Platform">
</p>

<p align="center">
  <a href="https://dimmy.app">dimmy.app</a>
</p>

---

## What is Dimmy?

The name is pronounced like "Dimi" — short for *dimmi*, Italian for "tell me".

Dimmy is a cross-platform desktop app that turns your voice into text — instantly, in any application. It runs as a tiny overlay on your screen. Press a hotkey, speak, and your words are transcribed and pasted into whatever has focus: editors, browsers, chat apps, terminals.

Built with **native UIs** for each platform — no Electron, no WebView. A shared Rust core handles all the heavy lifting (audio capture, transcription, AI post-processing), while each platform gets its own native frontend that feels right at home.

### Key Features

- **Universal dictation** — works with any application via clipboard paste
- **Native on every platform** — WinUI 3 on Windows, SwiftUI on macOS, GTK4 on Linux
- **Always-on-top pill overlay** — compact UI with live waveform visualization
- **Multiple STT providers** — Groq (fastest), OpenAI, Deepgram, Gemini, or any custom endpoint
- **AI enhancement** — 13 post-processing styles: correct grammar, summarize, rewrite professionally, and more
- **Privacy-first** — no telemetry, no cloud accounts, all data local, API keys encrypted on device (AES-256)
- **Multilingual** — auto-detect or select from 12+ languages
- **Configurable shortcut** — toggle or hold-to-record mode, any modifier combo
- **Update notifications** — checks GitHub for new releases from Settings > About

## Download

**[Get the latest release](https://github.com/KonradDallaOrg/dimmy/releases/latest)**

| Platform | File | Requirements |
|----------|------|--------------|
| Windows | `Dimmy-windows-x64.zip` | [.NET 8 Desktop Runtime](https://dotnet.microsoft.com/download/dotnet/8.0) |
| macOS | `Dimmy-macos-arm64.dmg` | macOS 12+, Apple Silicon |
| Linux | `Dimmy-linux-x86_64.AppImage` | None (GTK4 bundled) |

<details>
<summary><strong>Installation notes</strong></summary>

**Windows:** Extract the zip, run `Dimmy.Windows.exe`. Install .NET 8 Desktop Runtime if prompted.

**macOS:** Open the DMG, drag Dimmy to Applications. First launch requires right-click > Open, or:
```bash
xattr -d com.apple.quarantine /Applications/Dimmy.app
```

**Linux:** Make the AppImage executable and run:
```bash
chmod +x Dimmy-linux-x86_64.AppImage
./Dimmy-linux-x86_64.AppImage
```

</details>

## Quick Start

1. Launch Dimmy — a small pill appears on your screen
2. Right-click the pill or tray icon to open Settings
3. Enter an API key for transcription (see [STT Providers](#stt-providers))
4. Press **Win+Alt** (default) to start recording
5. Speak naturally, then press again to stop
6. Text is transcribed and pasted into the active app

## The Pill

Dimmy lives as a tiny overlay on your screen — the "pill". It changes shape and color to show what's happening:

<p align="center">
  <img src="docs/screenshots/pill-states.png" alt="Dimmy pill — all states, waveform styles, border colors, LLM style indicators" width="560">
</p>

## Settings

Right-click the pill or tray icon to open Settings. Each platform has its own native settings window with tabs for:

- **General** — language, shortcut mode (toggle/hold), startup behavior
- **Transcription** — STT provider, model, API key
- **AI Enhancement** — LLM provider, post-processing style, custom prompts
- **Audio** — input device, noise filter, gain, clipping detection
- **Overlay** — pill position, size, waveform style, border colors
- **Shortcut** — record hotkey configuration
- **Stats** — transcription count, time saved, audio processed
- **About** — version, update check, links

## STT Providers

Dimmy needs an API key for speech-to-text. Choose a provider:

| Provider | Type | Models | Free Tier | Get Key |
|----------|------|--------|-----------|---------|
| **Groq** (recommended) | STT + LLM | whisper-large-v3, whisper-large-v3-turbo, llama-3.3-70b | Yes (rate limited) | [console.groq.com/keys](https://console.groq.com/keys) |
| **OpenAI** | STT + LLM | gpt-4o-transcribe, gpt-4o-mini-transcribe, whisper-1, gpt-4o-mini | ~$0.006/min | [platform.openai.com/api-keys](https://platform.openai.com/api-keys) |
| **Deepgram** | STT | Nova-3, Nova-2 | $200 free credits | [console.deepgram.com](https://console.deepgram.com/) |
| **Google Gemini** | STT + LLM | gemini-2.5-flash, gemini-2.5-pro | Yes | [aistudio.google.com/apikey](https://aistudio.google.com/apikey) |
| **Anthropic** | LLM only | Claude Haiku 4.5, Claude Sonnet 4 | No | [console.anthropic.com/keys](https://console.anthropic.com/settings/keys) |
| **OpenRouter** | LLM only | Llama 3.3 70B, DeepSeek R1 | Yes (free models) | [openrouter.ai/keys](https://openrouter.ai/keys) |

Keys are encrypted locally on your device (AES-256). For extra security, enable **OS secure storage** (Keychain / Credential Manager) in Settings. You can also use any **custom endpoint** compatible with the OpenAI API format.

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

## LLM Post-Processing

Enable in Settings to send transcriptions through an LLM for cleanup or transformation. Requires a separate LLM API key (or check "Use same key" if your provider also offers chat completions).

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

Scroll wheel on the pill to cycle styles. Ctrl+scroll to cycle tone.

## Architecture

```
+-------------------+   +-------------------+   +-------------------+
|  Windows (WinUI3) |   |  macOS (SwiftUI)  |   | Linux (GTK4/Rust) |
|       C# UI       |   |     Swift UI      |   |   Rust + GTK4     |
+--------+----------+   +--------+----------+   +--------+----------+
         |  P/Invoke             |  C FFI               |  Rust crate
         v                      v                      v
+---------------------------------------------------------------+
|                     Shared Rust Core                           |
|  audio.rs  preprocess.rs  transcribe.rs  llm.rs  provider.rs  |
|  ffi.rs (20+ exported C functions)   keystore   hotkey        |
+---------------------------------------------------------------+
         |                      |                      |
         v                      v                      v
   STT Providers          LLM Providers          OS Audio (cpal)
```

The shared core (`src-tauri/src/`) handles all business logic: audio capture, preprocessing (noise filter + AGC), transcription via multiple STT APIs, optional LLM post-processing, and secure key storage. Windows and macOS call it through C FFI exports. Linux links directly as a Rust crate.

**Test coverage:** 206 Rust core tests + 38 Linux UI tests + 91 C# Windows tests = **335 total tests**.

## Contributing

### Prerequisites

All platforms need:
- [Rust](https://rustup.rs/) (latest stable)
- [Git](https://git-scm.com/)

### Clone & verify

```bash
git clone https://github.com/KonradDallaOrg/dimmy.git
cd dimmy

# Verify the Rust core builds and passes tests
cd src-tauri
cargo test --lib
cargo clippy -- -D warnings
cd ..
```

### Windows

**Additional requirements:**
- [Visual Studio 2022+](https://visualstudio.microsoft.com/) with:
  - .NET Desktop Development workload
  - Windows App SDK / WinUI 3
- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)

```bash
# 1. Build the Rust DLL
cd src-tauri
cargo build --release --lib --target x86_64-pc-windows-msvc
cd ..

# 2. Build the Windows app
cd native-ui/windows/Dimmy.Windows
dotnet restore
dotnet build -c Release
cd ..

# 3. Copy Rust DLL to output
copy ..\..\src-tauri\target\x86_64-pc-windows-msvc\release\dimmy_lib.dll Dimmy.Windows\bin\Release\net8.0-windows10.0.19041.0\

# 4. Run tests
cd Dimmy.Windows.Tests
dotnet test -c Release
```

Or use the build script: `powershell -File build-windows.ps1`

### macOS

**Additional requirements:**
- Xcode 15+ with Command Line Tools

```bash
# 1. Install Xcode CLI tools
xcode-select --install

# 2. Build the Rust static library (Apple Silicon)
cd src-tauri
cargo build --release --lib --target aarch64-apple-darwin
cd ..

# 3. Remove dylib so Xcode links statically
rm -f src-tauri/target/aarch64-apple-darwin/release/libdimmy_lib.dylib

# 4. Open and build in Xcode
open native-ui/macos/Dimmy.xcodeproj
# Build with Cmd+B, Run with Cmd+R
```

### Linux

**Additional requirements:**
- GTK4 and libadwaita development libraries
- pkg-config

```bash
# Ubuntu/Debian 24.04+
sudo apt install libgtk-4-dev libadwaita-1-dev libasound2-dev libxdo-dev \
  libdbus-1-dev pkg-config

# Fedora
sudo dnf install gtk4-devel libadwaita-devel alsa-lib-devel libxdo-devel \
  dbus-devel

# Arch
sudo pacman -S gtk4 libadwaita alsa-lib xdotool dbus

# Build and run
cd native-ui/linux
cargo build --release
./target/release/dimmy-linux
```

### Development Workflow

```bash
cd src-tauri

# Run Rust core tests
cargo test --lib

# Format
cargo fmt

# Lint (CI enforces zero warnings)
cargo clippy -- -D warnings

# Lint the Linux UI (requires GTK4 dev libs)
cd ../native-ui/linux
cargo clippy -- -D warnings
cargo test
```

### Pre-push checklist

- `cargo fmt --check` in `src-tauri/` — clean
- `cargo clippy -- -D warnings` in `src-tauri/` — zero warnings
- `cargo test --lib` in `src-tauri/` — all pass
- Version updated in `src-tauri/Cargo.toml`

### CI/CD

| Workflow | Trigger | What it does |
|----------|---------|-------------|
| `ci.yml` | Push/PR to main or staging | Lint + test Rust core, lint Linux GTK4 crate |
| `staging-native.yml` | Push to staging | Build all 3 platforms, create pre-release |
| `release.yml` | Tag push (`v*`) | Build all 3 platforms, publish GitHub Release |

### Project Structure

```
src-tauri/src/          Shared Rust core (audio, STT, LLM, FFI, keystore)
native-ui/windows/      WinUI 3 / C# (.NET 8) — P/Invoke to dimmy_lib.dll
native-ui/macos/        SwiftUI — FFI bridge via DimmyFFI.h to libdimmy_lib.a
native-ui/linux/        GTK4 + libadwaita (Rust) — direct crate dependency
docs/dev/               Development docs (audio pipeline, known bugs, practices)
.github/workflows/      CI/CD pipeline definitions
```

## Development Philosophy

Dimmy follows **Negative Space Programming**: every function asserts its preconditions and postconditions in production code. Assertions run in release builds — the absence of a crash is the proof of correctness. Combined with TDD (write failing tests before implementation) and strict defensive coding (clamp audio, check for NaN, truncate errors, validate URLs), this keeps the app stable for daily use.

Full development guidelines are in the project's `CLAUDE.md`, which also serves as the AI-assisted development playbook for contributors using Claude Code or similar tools.

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Core | Rust (stable) |
| Windows UI | WinUI 3 / C# / .NET 8 |
| macOS UI | SwiftUI |
| Linux UI | GTK4 + libadwaita / Rust |
| Audio capture | cpal |
| Noise filter | nnnoiseless + biquad highpass |
| AGC | dagc (with NaN safety guards) |
| Secure storage | AES-256 local (default) + OS keyring (opt-in) |
| HTTP | reqwest |
| CI/CD | GitHub Actions |

## Support

If Dimmy saves you time, consider supporting its development:

- [Buy Me a Coffee](https://buymeacoffee.com/konraddall5)
- [Ko-fi](https://ko-fi.com/konraddalla)

## License

[AGPL-3.0](LICENSE) — free to use, modify, and distribute. If you redistribute or offer it as a service, your code must remain open source under the same license.

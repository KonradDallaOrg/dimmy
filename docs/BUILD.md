# Build

> **One page. Every build/test/lint command. Do not add build instructions anywhere else.**
> Release runbook: [`RELEASING.md`](RELEASING.md). CI rules: [`dev/windows-ci.md`](dev/windows-ci.md).

## Common prerequisites (all platforms)

- **Rust** — latest stable via [rustup](https://rustup.rs/)
- **Git**
- **CMake** — required by whisper-rs to compile whisper.cpp from source
  - macOS: `brew install cmake` (or Xcode CLI tools)
  - Windows: `choco install cmake` or via `get-cmake` GitHub Action
  - Linux: `sudo apt install cmake`
- **LLVM / libclang** — required by `whisper-rs` build script on Windows and some Linux distros

## Feature flags (`core/Cargo.toml`)

```
default                    = ["local-stt", "telemetry-sentry"]

# Whisper (whisper.cpp via whisper-rs)
local-stt                  = baseline whisper.cpp (CPU)
local-stt-metal            = whisper.cpp + Metal (macOS Apple Silicon)
local-stt-vulkan           = whisper.cpp + Vulkan (Win/Linux cross-vendor GPU)
local-stt-cuda             = whisper.cpp + CUDA (NVIDIA)

# Parakeet TDT v3 (ONNX Runtime — istupakov/parakeet-tdt-0.6b-v3-onnx)
local-stt-parakeet         = ort + ndarray (CPU; default for Win/Linux)
local-stt-parakeet-cuda    = ort/cuda      (Win NVIDIA)
local-stt-parakeet-coreml  = ort/coreml    (Mac, currently no-win)

# Parakeet via FluidAudio CoreML (Apple Neural Engine)
local-stt-parakeet-fluid   = aarch64-apple-darwin only; pulls Swift bridge

# DeepFilterNet (DEFERRED — upstream `deep_filter` crate needs `tract` feature)
local-dfn                  = no-op gate (module is wired, dependency commented out)

# llama.cpp local LLM
local-llm                  = baseline llama.cpp (CPU)
local-llm-metal            = llama.cpp + Metal + dynamic-link (Mac)
local-llm-vulkan           = llama.cpp + Vulkan (Win/Linux)
local-llm-cuda             = llama.cpp + CUDA  (NVIDIA)

# Telemetry & licensing
telemetry-sentry           = Sentry crash + manual capture pipeline (default ON)
license-cli                = CLI client for the licensing server (clap + ed25519)
license-client             = production cdylib license verification (default ON
                             when DIMMY_LICENSE_PUBKEY is injected at build)

# Test-only
test-ffi                   = test-only FFI entry points that bypass cpal
                             (used by core/tests/ffi_e2e.rs, never in release)
smoke-test                 = libloading-based runtime FFI smoke binary
                             (core/src/bin/dimmy_smoke.rs)
```

**Rule of thumb for CI & local dev:**

| Scenario | Features |
|---|---|
| CI lint + test (ubuntu-22.04) | `--features local-stt,local-llm` |
| macOS release build | `--features local-stt-metal,local-stt-parakeet-fluid,local-llm-metal` |
| Windows release build (FROZEN — see below) | `--features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan` |
| Linux release build | `--features local-stt` (CPU fallback for AppImage portability) |
| Quick local check | `--features local-stt` (skip local-llm to avoid llama.cpp compile) |
| Tier-1 FFI integration | `--features local-stt,test-ffi` |
| Installer FFI smoke | `--features smoke-test` (builds `dimmy_smoke` binary) |

### Windows local DLL build — feature flag set is FROZEN

`cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan` is the canonical local Win build. Dropping any of the three silently breaks production code paths because the user's *runtime config* decides which path runs:

- `local-stt-vulkan` → whisper.cpp Vulkan STT (dictation when `local_stt_backend=whisper`, meeting STT chunks, file-load).
- `local-stt-parakeet` → Parakeet TDT v3 (dictation chunked-stt worker when `local_stt_backend=parakeet`, default for many users; ALSO referenced by meeting follow-ups).
- `local-llm-vulkan` → llama.cpp Vulkan LLM (local recap, local rewrite).

Burned twice in production 2026-05-07 (meeting empty transcript, then dictation empty transcript) — the user has explicitly forbidden dropping a feature on your own initiative. See CLAUDE.md "Windows local DLL build — feature flag set is FROZEN".

## Core (Rust) — everyone runs these

```bash
cd core

# Format — CI fails on non-clean
cargo fmt --check

# Lint — CI uses local-stt,local-llm; zero-warning policy
cargo clippy --features local-stt,local-llm -- -D warnings

# Test — currently ~411 lib tests + ~88 integration tests
cargo test --lib --features local-stt,local-llm
```

**Pre-push checklist — run ALL three before every push.** CI treats warnings as errors; anything clippy catches locally is cheaper than a red CI run.

## Windows

### Prerequisites (additional)
- **Visual Studio 2022+** with *.NET Desktop Development* and *Windows App SDK / WinUI 3*
- **.NET 8 SDK**
- **Ninja** (`choco install ninja`) — required for the fast whisper.cpp build path
- **Vulkan SDK** — required for `local-stt-vulkan` feature
- On a local dev box, **rename `C:\Program Files\Git\usr\bin\link.exe`** so it doesn't shadow the MSVC linker when you're in a MSYS2 shell. CI side-steps this by using `vcvars64.bat` explicitly.

### Build

```bash
# 1. Build the Rust DLL with Vulkan + LLM
cd core
set CMAKE_GENERATOR=Ninja
cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan

# 2. Build the WinUI 3 app
cd ../platforms/windows/Dimmy.Windows
dotnet restore
dotnet build -c Release

# 3. Run C# tests
cd ../Dimmy.Windows.Tests
dotnet test -c Release
```

Or from the repo root: `powershell -File build-windows.ps1` (this is the one-shot script used by contributors; CI uses its own inlined steps for toolchain control).

### Windows CI critical notes
- CI uses `windows-2025` runner + VS 2026 BuildTools (side-by-side with VS 2022) — MSVC linker ≥ 14.50 is required to avoid the `ggml-vulkan` miscompile.
- Do not touch `release.yml`, `staging-auto-update.yml`, or `test-install.yml` without reading **[`dev/windows-ci.md`](dev/windows-ci.md)** first. Every rule there is paid for in blood.

## macOS

### Prerequisites (additional)
- **Xcode 15+** with Command Line Tools (`xcode-select --install`)

### Build

```bash
# 1. Build the Rust static library for Apple Silicon
cd core
cargo build --release --lib --target aarch64-apple-darwin --features local-stt-metal,local-llm-metal

# 2. Remove dylib so Xcode links statically (dynamic-link is enabled for local-llm-metal,
#    but the Xcode target wants libdimmy_lib.a only)
rm -f target/aarch64-apple-darwin/release/libdimmy_lib.dylib

# 3. Open in Xcode and build
cd ..
open platforms/macos/Dimmy.xcodeproj
# Cmd+B to build, Cmd+R to run, Cmd+U for tests
```

**Gotcha:** `local-llm-metal` pulls `dynamic-link` on the `llama-cpp-4` side. The macOS packaging pipeline bundles the required dylibs into `Dimmy.app/Contents/Frameworks/` and codesigns them. If you're running outside Xcode and the dylibs are missing, the build links statically instead and llama.cpp may not find symbols at runtime. Use Xcode for macOS builds.

## Linux

### Prerequisites (additional)

**Ubuntu/Debian 24.04+:**
```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libasound2-dev libxdo-dev \
  libdbus-1-dev pkg-config cmake
```

**Fedora:**
```bash
sudo dnf install gtk4-devel libadwaita-devel alsa-lib-devel libxdo-devel \
  dbus-devel cmake
```

**Arch:**
```bash
sudo pacman -S gtk4 libadwaita alsa-lib xdotool dbus cmake
```

### Build

```bash
# AppImage build uses default feature (local-stt = CPU whisper.cpp) for portability
cd platforms/linux
cargo build --release
./target/release/dimmy-linux

# Lint + test (matches CI)
cargo clippy -- -D warnings
cargo test
```

**Linux does not default to Vulkan.** The AppImage is CPU-only to stay portable across distros with different Vulkan loader availability. A user who wants GPU acceleration on Linux can build from source with `--features local-stt-vulkan`.

## CI workflow matrix

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | Push/PR to `main` or `staging` | Rust core: fmt + clippy + test (ubuntu-22.04, `local-stt,local-llm`). Linux GTK4 crate: clippy + test (ubuntu-24.04). |
| `staging-auto-update.yml` | Push to `staging` | Builds all 3 native UIs in parallel, packages installers, runs `test-install` smoke check on the Windows one, uploads `staging-latest` release. |
| `release.yml` | Tag push (`v*`) | Same as staging but publishes a GitHub Release instead of `staging-latest`. |
| `test-install.yml` | `workflow_call` from staging/release, or manual `workflow_dispatch` | Downloads the Windows Setup.exe, installs it silently on a clean `windows-latest`, launches for 15s, fails if `dimmy_startup.log` contains CRASH or required files are missing. |

## Common build failures

| Symptom | Cause | Fix |
|---|---|---|
| `cannot find -ldwmapi` on Linux | Cross-compile artefact | You're building on Linux with Windows target; use `--target x86_64-unknown-linux-gnu` explicitly |
| `cmake: command not found` | CMake missing | Install per prerequisites above |
| `libclang not found` (Windows) | LLVM missing | `choco install llvm` |
| `link.exe: undefined reference` (Windows local) | Git's `link.exe` shadows MSVC | Rename `C:\Program Files\Git\usr\bin\link.exe` |
| `linker version 14.44.*` gate fails (Windows CI) | VS 2026 BuildTools not active | See `dev/windows-ci.md` I1 |
| `whisper-rs` fails to compile | Ninja missing or CMake too old | `choco install ninja` / update CMake ≥ 3.26 |
| macOS app crashes on launch with SIGSEGV PAC | `objc_msgSend` declared variadic | Fixed in current code; see `dev/known-bugs.md` MACOS-001 |

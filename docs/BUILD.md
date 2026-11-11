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
local-stt-coreml           = whisper encoder on the Apple Neural Engine (Core ML);
                             composable with local-stt-metal
local-stt-vulkan           = whisper.cpp + Vulkan (Win/Linux cross-vendor GPU)
local-stt-cuda             = whisper.cpp + CUDA (NVIDIA)

# Parakeet TDT v3 (ONNX Runtime — istupakov/parakeet-tdt-0.6b-v3-onnx)
local-stt-parakeet         = ort + ndarray (CPU; not in `default`, the Windows
                             build set enables it)
local-stt-parakeet-cuda    = ort/cuda      (Win NVIDIA)
local-stt-parakeet-coreml  = ort/coreml    (Mac, currently no-win)

# Parakeet via FluidAudio CoreML (Apple Neural Engine)
local-stt-parakeet-fluid   = aarch64-apple-darwin only; pulls Swift bridge

# Qwen3-ASR local STT through llama.cpp's multimodal path
local-stt-qwen             = local-llm + llama-cpp-4/mtmd + dynamic-link
                             (produces mtmd.dll / libmtmd, which installers must carry)

# Noise suppression
denoise-gtcrn              = GTCRN on the ort session (compiled in; off at
                             runtime unless DIMMY_GTCRN=1)
local-dfn                  = DeepFilterNet3 backend (core/src/dfn3.rs) via the
                             `deep_filter` git fork; enabled by the macOS builds
                             and by staging-tester.yml's Windows build

# llama.cpp local LLM
local-llm                  = baseline llama.cpp (CPU)
local-llm-metal            = llama.cpp + Metal + dynamic-link (Mac)
local-llm-vulkan           = llama.cpp + Vulkan + dynamic-link (Win/Linux)
local-llm-cuda             = llama.cpp + CUDA  (NVIDIA)

# Telemetry, licensing, integrations
telemetry-sentry           = Sentry crash + manual capture pipeline (default ON)
license-cli                = CLI client for the licensing server (clap + ed25519)
license-client             = production cdylib license verification. NOT in
                             `default`: every shipping workflow passes it
                             explicitly (see RELEASING.md)
telegram                   = Telegram user-account client (grammers); off by
                             default, enabled by the Windows and macOS CI builds

# Test-only / tooling
test-ffi                   = test-only FFI entry points that bypass cpal
                             (used by core/tests/ffi_e2e.rs, never in release)
sim-audio                  = local test builds only: meeting tracks from WAV files
smoke-test                 = libloading-based runtime FFI smoke binary
                             (core/src/bin/dimmy_smoke.rs)
setup-launcher             = dimmy_setup console launcher (dimmy-setup.exe in
                             the Windows install)
```

**Rule of thumb for CI & local dev:**

| Scenario | Features |
|---|---|
| CI lint + test (ubuntu-22.04) | `--features local-stt,local-llm` (`ci.yml`, and `rust-check` in `release.yml`; `staging-auto-update.yml` uses `--features local-stt` only) |
| macOS release build | `--features local-stt-metal,local-stt-coreml,local-llm-metal,local-stt-parakeet-coreml,local-stt-parakeet-fluid,local-dfn,license-client,telegram,local-stt-qwen` (same list in `release.yml` and `scripts/dev/preflight-mac.sh`) |
| Windows release build (FROZEN — see below) | `--features local-stt-vulkan,local-llm-vulkan,local-stt-parakeet,denoise-gtcrn,license-client,telegram,local-stt-qwen` (`release.yml`; `staging-tester.yml` also adds `local-dfn`) |
| Linux release build | `cd platforms/linux && cargo build --release --locked --features license-client` (core comes in with its default features, CPU whisper.cpp, for AppImage portability) |
| Quick local check | `--features local-stt` (skip local-llm to avoid llama.cpp compile) |
| Tier-1 FFI integration | `--features local-stt,test-ffi` |
| Installer FFI smoke | `--features smoke-test` (builds `dimmy_smoke` binary) |

### Windows local DLL build — feature flag set is FROZEN

`cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,denoise-gtcrn,local-stt-qwen` is the canonical local Win build (see `CLAUDE.md`). The CI Windows builds use the same set plus `license-client,telegram` (and `local-dfn` in `staging-tester.yml`). Dropping any of these silently breaks production code paths because the user's *runtime config* decides which path runs:

- `local-stt-vulkan` → whisper.cpp Vulkan STT (dictation when `local_stt_backend=whisper`, meeting STT chunks, file-load).
- `local-stt-parakeet` → Parakeet TDT v3 (dictation chunked-stt worker when `local_stt_backend=parakeet`, default for many users; ALSO referenced by meeting follow-ups).
- `local-llm-vulkan` → llama.cpp Vulkan LLM (local recap, local rewrite).
- `local-stt-qwen` → Qwen3-ASR via llama.cpp's multimodal path; it also produces `mtmd.dll`, which the installer must carry.
- `denoise-gtcrn` → GTCRN noise suppression, compiled in but off at runtime unless `DIMMY_GTCRN=1`.

Burned twice in production 2026-05-07 (meeting empty transcript, then dictation empty transcript) — the user has explicitly forbidden dropping a feature on your own initiative. See CLAUDE.md "Windows local DLL build — feature flag set is FROZEN".

## Core (Rust) — everyone runs these

```bash
cd core

# Format — CI fails on non-clean
cargo fmt --check

# Lint — CI uses local-stt,local-llm; zero-warning policy
cargo clippy --features local-stt,local-llm -- -D warnings

# Test — lib tests (the count changes; read it from the cargo output)
cargo test --lib --features local-stt,local-llm

# Tier-1 FFI integration (also in the CLAUDE.md pre-push checklist)
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1
```

**Pre-push checklist — run ALL of these before every push.** CI treats warnings as errors; anything clippy catches locally is cheaper than a red CI run.

## Windows

### Prerequisites (additional)
- **Visual Studio 2022+** with *.NET Desktop Development* and *Windows App SDK / WinUI 3*
- **.NET 8 SDK**
- **Ninja** (`choco install ninja`) — required for the fast whisper.cpp build path
- **Vulkan SDK** — required for `local-stt-vulkan` feature
- On a local dev box, **rename `C:\Program Files\Git\usr\bin\link.exe`** so it doesn't shadow the MSVC linker when you're in a MSYS2 shell. CI side-steps this by using `vcvars64.bat` explicitly.

### Build

```bash
# 1. Build the Rust DLL with the frozen feature set (see above)
cd core
set CMAKE_GENERATOR=Ninja
cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,denoise-gtcrn,local-stt-qwen

# 2. Build the WinUI 3 app (-p:Platform=x64 is not optional: without it the
#    output lands in bin/Debug/... instead of bin/x64/Debug/..., and the
#    installed exe keeps loading the old Dimmy.Windows.dll)
cd ../platforms/windows/Dimmy.Windows
dotnet restore
dotnet build -c Debug -p:Platform=x64

# 3. Run C# tests
cd ../Dimmy.Windows.Tests
dotnet test -c Release
```

The C# project copies `dimmy_lib.dll`, `ggml*.dll`, `llama*.dll` and `mtmd*.dll` from `core/target/release/`, and the onnxruntime DLLs from `core/target/onnxruntime-win-x64-<ver>/lib/` (`Dimmy.Windows.csproj`), so step 1 must come first. `scripts/download-onnxruntime.ps1` fetches the onnxruntime DLLs once.

From the repo root, `powershell -File build-windows.ps1` restores and builds the C# solution (default `-Configuration Debug -Platform x64`), runs the C# tests, then runs `cargo test --lib`. It does NOT build the Rust DLL. CI uses its own inlined steps for toolchain control.

### Windows CI critical notes
- The Windows build jobs in `release.yml`, `staging-auto-update.yml` and `staging-tester.yml` run on the `windows-2025-vs2026` runner image. Each job makes sure a VS 2026 toolchain is present (installing the BuildTools through chocolatey if `vswhere` does not find one) and pins MSVC 14.50 — linker ≥ 14.50 is required to avoid the `ggml-vulkan` miscompile.
- Do not touch `release.yml`, `staging-auto-update.yml`, `staging-tester.yml`, or `test-install.yml` without reading **[`dev/windows-ci.md`](dev/windows-ci.md)** first. Every rule there is paid for in blood.

## macOS

### Prerequisites (additional)
- **Xcode 15+** with Command Line Tools (`xcode-select --install`)

### Build

```bash
# 1. Build the Rust static library for Apple Silicon
cd core
#    (feature list = release.yml build-macos = MAC_FEATURES in scripts/dev/preflight-mac.sh)
cargo build --release --lib --target aarch64-apple-darwin --features local-stt-metal,local-stt-coreml,local-llm-metal,local-stt-parakeet-coreml,local-stt-parakeet-fluid,local-dfn,license-client,telegram,local-stt-qwen

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
# The Linux crate links core with its default features (local-stt = CPU whisper.cpp) for portability
cd platforms/linux
cargo build --release
./target/release/dimmy-linux

# Lint + test (matches CI; release.yml adds --locked and builds with --features license-client)
cargo clippy -- -D warnings
cargo test
```

**Linux does not default to Vulkan.** The AppImage is CPU-only to stay portable across distros with different Vulkan loader availability. The `platforms/linux` crate has only the features `x11-fallback`, `layer-shell` and `license-client` (`platforms/linux/Cargo.toml`) and passes no GPU feature through to core, so `--features local-stt-vulkan` is not accepted there. GPU acceleration on Linux needs a change to that manifest.

## CI workflow matrix

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | Push/PR to `main` or `staging` | Swift string-literal check + Rust core fmt + clippy (`lint`), tests (`test`) on ubuntu-22.04 with `local-stt,local-llm`. Linux GTK4 crate: clippy + test (ubuntu-24.04). |
| `e2e-tests.yml` | Push to `staging`, PR to `staging`/`main`, manual | Push: every integration suite on `windows-2025` plus the FlaUI UI smoke on `windows-2025-vs2026`. PR: also the 3-OS `ffi-integration` and `ffi-smoke` matrices. Produces no release artifacts. |
| `staging-auto-update.yml` | Push to `staging` (ignores `docs/**`, `mockup/**`, `*.md`, `CLAUDE.md`), manual | Builds all 3 native UIs in parallel, packs Windows as packId `Dimmy-Staging` / channel `staging`, runs `test-install` on the Windows installer, uploads the rolling `staging-latest` pre-release. |
| `staging-tester.yml` | Tag push `v*-staging*` | Signed staging installers (Windows + macOS, no Linux, no `test-install`) against the staging licensing server; Windows is signed by the `sign-windows` job on the self-hosted `dimmy-sign` runner. |
| `release.yml` | Tag push `v*` except `v*-staging*` | Builds all 3 native UIs, signs Windows on the self-hosted `dimmy-sign` runner, runs the `test-install` and macOS-launch gates, and publishes the draft GitHub Release only at the end (`publish-release`). Stable tags also publish the signed Setup.exe to R2. |
| `test-install.yml` | `workflow_call` from `staging-auto-update.yml` / `release.yml`, or manual `workflow_dispatch` | Downloads the Windows Setup.exe from a release tag, installs it silently on a clean `windows-latest`, launches for 15s, fails on a `CRASH` line in `dimmy_startup.log`, a `crash.log` (headless UI), an early exit, or missing required files. |
| `agent-preview.yml` | Tag push `agent-preview.*` | macOS-only test build of the AI-assisted install ("Dimmy Agent.app"). |
| `engine-nemotron.yml` | Tag push `engine-nemotron-*`, manual | Builds the Nemotron STT engine libraries that `core/src/nemotron.rs` loads at run time. Not part of any release. |

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

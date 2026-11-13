# Contributing to Dimmy

> **If you're a human:** welcome. Read the sections below. The goal is to get you productive inside an hour.
> **If you're an AI agent (Claude Code, etc.):** start at [`CLAUDE.md`](CLAUDE.md) — that's your playbook. This file is for humans but the rules here apply to your commits too.

## One-minute tour

Dimmy is a cross-platform voice-transcription overlay: shared Rust core + one native UI per OS (WinUI 3 on Windows, SwiftUI on macOS, GTK4 on Linux). Everything non-UI — recording, STT, LLM, history, key storage — lives in `core/`. UIs call the core through a C FFI.

- **Big picture:** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- **Build anything:** [`docs/BUILD.md`](docs/BUILD.md)
- **Ship a release:** [`docs/RELEASING.md`](docs/RELEASING.md)
- **Writing code rules:** [`docs/dev/development-practices.md`](docs/dev/development-practices.md) — this is mandatory, not aspirational

## First PR in 10 minutes

```bash
git clone https://github.com/KonradDallaOrg/dimmy.git
cd dimmy/core
cargo test --lib --features local-stt,local-llm      # should pass
cargo clippy --features local-stt,local-llm -- -D warnings   # should be clean
```

If both are green, you have a working dev environment. For anything that touches a native UI, see the per-platform section below.

Once per clone, activate the committed git hooks: `./scripts/install-hooks.sh`. The `pre-commit` hook runs `cargo fmt --check` on commits that touch `core/**.rs` and refuses unformatted code. The `pre-push` hook only prompts when the push contains a `v*` tag (Windows code-signing reminder).

## Development rules (not optional)

These are the rules. Skipping them is how Dimmy would turn from "works" to "haunted". The full rationale is in [`docs/dev/development-practices.md`](docs/dev/development-practices.md); here are the headlines:

1. **Negative Space Programming.** Every function asserts its preconditions and postconditions. Use `assert!()` (runs in release), not `debug_assert!()`. The absence of a crash IS the proof of correctness.
2. **Test-Driven Development.** Every bug fix needs a failing test first, then the minimal fix, then verification. Every feature gets a test describing the behaviour.
3. **Cross-platform parity.** Every feature must work identically on Windows, macOS, and Linux. A feature that works on one OS and silently fails on another is a bug, not a limitation.
4. **Defensive DSP.** Clamp audio to `[-1.0, 1.0]`. Check for NaN/Inf after every DSP op. The dagc library can produce all-NaN on zero input — see [`docs/dev/known-bugs.md`](docs/dev/known-bugs.md) AUDIO-001.
5. **Sensitive data hygiene.** Truncate error bodies to 200 chars (prevents key/PII leak). Validate URLs before use (reject non-HTTPS except localhost). Never log API keys.

## Pre-push checklist

Run these commands from `core/` before every push. CI will fail the build otherwise.

```bash
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm
# tier-1 end-to-end integration (Rust, cross-platform, ~3 s after first run):
cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1
```

The tier-1 harness feeds pre-recorded PCM (JFK sample + silence + synthetic noise) through the actual FFI and asserts on transcripts. See [`docs/dev/testing.md`](docs/dev/testing.md) for the full pyramid, what each layer catches, and how to add tests.

If you touched the Linux GTK4 UI:

```bash
cd platforms/linux
cargo clippy -- -D warnings
cargo test
```

If you touched the Windows UI / XAML / onboarding:

```bash
# Build Rust + C# first, then run the FlaUI smoke tests
cd core
cargo build --release --lib --features local-stt
cd ../platforms/windows/Dimmy.Windows
dotnet build Dimmy.Windows.csproj -c Release
cd ../Dimmy.Windows.UiTests
dotnet test -c Debug
```

If you touched `platforms/macos/**` or Mac-facing FFI, run `scripts/dev/preflight-mac.sh` before pushing. It rebuilds the Rust static lib with the Mac feature set, runs `xcodebuild` and launches the app for a few seconds so the `SelfTests.runAtLaunch` assertions fire; compiling alone does not catch a stale `SelfTests` assertion. If you only changed Rust or Windows code, CI handles the macOS build.

## Branches & commits

- Feature branches: `feat/<thing>` or `fix/<thing>`. Push to `staging` when ready (or open a PR against it).
- **Commit messages:** `feat:`, `fix:`, `chore:`, `docs:`, `test:`, `style:`. One concern per commit — don't bundle unrelated changes.
- Merge to `staging` with `--no-ff` so the history shows what was a feature branch.
- `main` is fast-forwarded from `staging` when we cut a release (see [`docs/RELEASING.md`](docs/RELEASING.md)).

## Working on Windows

- Use VS 2022 or VS 2026 (CI uses both side-by-side; see [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md)).
- Install: .NET 8 SDK, Windows App SDK / WinUI 3 workload, CMake, Ninja, LLVM, Vulkan SDK.
- Renaming `C:\Program Files\Git\usr\bin\link.exe` avoids a local build conflict with the MSVC linker.
- Platform-specific docs: [`platforms/windows/README.md`](platforms/windows/README.md).
- Local C# build: `dotnet build platforms/windows/Dimmy.Windows/Dimmy.Windows.csproj -c Debug -p:Platform=x64`. Without `-p:Platform=x64` the output lands in `bin/Debug/...` and the .exe you launch from `bin/x64/Debug/...` keeps loading the old `Dimmy.Windows.dll`.
- The local `dimmy_lib.dll` is built with the frozen feature set from `CLAUDE.md` (`local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,denoise-gtcrn,local-stt-qwen`). Dropping a feature breaks a runtime path silently.
- **Before touching any workflow** in `.github/workflows/`, read [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md). Eleven invariants (I1–I11). Each cost a shipped bug.

## Working on macOS

- Xcode 15+. Apple Silicon is the target (`aarch64-apple-darwin`).
- Platform-specific docs: [`platforms/macos/README.md`](platforms/macos/README.md).
- Things to know: Metal is on, `dynamic-link` is used for llama-cpp-4 on macOS (dylibs bundled + codesigned). FFI quirks are in [`docs/dev/known-bugs.md`](docs/dev/known-bugs.md) (MACOS-001 to MACOS-003; no `objc_msgSend` or `tao` code remains in the repo, so those entries are history).

## Working on Linux

- Ubuntu 24.04 is the reference. See [`docs/BUILD.md`](docs/BUILD.md) for distro-specific packages.
- The AppImage ships CPU-only whisper.cpp for portability; Vulkan is opt-in from source.
- Platform-specific docs: [`platforms/linux/README.md`](platforms/linux/README.md).
- `release.yml` builds the Linux crate with `--locked` (clippy, test, build), so when `core/` gains or bumps a dependency, update `platforms/linux/Cargo.lock` in the same change.
- Status: the GTK4 UI is an experimental prototype. Its global-hotkey registration (`platforms/linux/src/hotkey.rs`) is a stub and the core's Linux keyboard hook is a no-op, so check the Linux README before assuming a feature works there.

## Where to put things

| You want to... | Put it here |
|---|---|
| Add a new cloud STT provider | `core/src/provider.rs` + routing in `transcribe.rs` |
| Add a new LLM post-processing style | `core/src/llm.rs` + UI dropdown in each platform |
| Add a config field | `core/src/lib.rs` (struct) + `ffi.rs` getter/setter + platform UI |
| Fix an audio bug | Reproduce in a test in `preprocess.rs`, THEN fix. Read [`docs/dev/audio-pipeline.md`](docs/dev/audio-pipeline.md). |
| Add a Windows CI step | Read all 11 invariants in [`docs/dev/windows-ci.md`](docs/dev/windows-ci.md) first. |
| Change the release flow | [`docs/RELEASING.md`](docs/RELEASING.md) + edit the relevant workflow. |
| Document a new pattern | Add it to `docs/dev/`. Don't re-document in CLAUDE.md or README.md — link instead. |

## Reporting bugs

Open an issue at <https://github.com/KonradDallaOrg/dimmy/issues>. For audio / transcription bugs, include:

- OS + version
- STT mode (local or cloud provider name)
- Model (for local)
- Pill state when the bug happened (idle, recording, transcribing, done, error)
- `dimmy.log` and `crash.log` if they exist. Both sit in the config dir (`dirs::config_dir()` + `dimmy`):
  - Windows: `%APPDATA%\dimmy\`
  - macOS: `~/Library/Application Support/dimmy/`
  - Linux: `~/.config/dimmy/`

## License

Dimmy is AGPL-3.0. Contributions are accepted under the same license.

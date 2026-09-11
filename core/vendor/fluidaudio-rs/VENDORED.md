# Vendored fluidaudio-rs

Upstream: https://github.com/FluidInference/fluidaudio-rs, tag `v0.12.6`
(commit `d38f95f`), MIT license (see `Cargo.toml`).

Vendored because upstream's bridge exposes Parakeet, VAD and diarization but
not FluidAudio's Qwen3-ASR pipeline, which the pinned FluidAudio 0.12.6
already ships. Dimmy's additions, all marked "Qwen3-ASR" in the source:

- `swift/FluidAudioBridge.swift`: `fluidaudio_qwen3_{supported,models_exist,download,initialize,transcribe}`
- `src/ffi/bridge.rs`: the matching `extern "C"` declarations and safe wrappers
- `src/lib.rs`: `FluidAudio::{qwen3_supported,qwen3_models_exist,qwen3_download,init_qwen3,transcribe_qwen3}`

Nothing upstream was changed. Removed only what a path dependency doesn't
use: upstream's `.github/`, `.cargo/`, `Cargo.lock`.

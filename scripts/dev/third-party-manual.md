## Components that are not Rust crates

These ship inside the Dimmy installers or are linked into them. Licence texts
are printed further down. Versions are the ones pinned in the build files.

| Component | Used for | Licence | Source |
|---|---|---|---|
| whisper.cpp and ggml (built by whisper-rs-sys) | local speech recognition | MIT, Copyright (c) 2023-2026 The ggml authors | https://github.com/ggml-org/whisper.cpp |
| llama.cpp and ggml (llama-cpp-4; ggml, llama and mtmd shared libraries on Windows and macOS) | local language model, Qwen3-ASR | MIT, Copyright (c) 2023-2026 The ggml authors | https://github.com/ggml-org/llama.cpp |
| ONNX Runtime 1.22.0 (onnxruntime.dll, loaded by the ort crate) | Parakeet speech recognition, GTCRN | MIT, Copyright (c) Microsoft Corporation. Its own ThirdPartyNotices.txt ships with the runtime and lists its dependencies | https://github.com/microsoft/onnxruntime |
| Silero VAD (ggml-silero-v6.2.0.bin) | voice activity detection | MIT, Copyright (c) 2020-present Silero Team | https://github.com/snakers4/silero-vad |
| GTCRN (gtcrn_simple.onnx) | optional noise suppression | MIT, Copyright (c) 2024 Rong Xiaobin | https://github.com/Xiaobin-Rong/gtcrn |
| WebRTC AEC3 (Rust port, crate aec3) | echo cancellation | MIT or BSD-3-Clause, plus Google's WebRTC patent grant (the PATENT file of the crate is printed with the crate licences) | https://github.com/RubyBit/aec3-rs |
| NeMo-Speech.cpp (NVIDIA; built by Dimmy with ggml linked statically, downloaded at your request, not in the installer) | Nemotron speech recognition and speaker labels | Apache-2.0 | https://github.com/NVIDIA/NeMo-Speech.cpp |
| Nemotron 3.5 ASR Streaming 0.6B and Nemotron-3-Diarization (GGUF Q8_0 models, downloaded at your request) | Nemotron speech recognition and speaker labels | OpenMDW-1.1 (per their Hugging Face cards) | https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b, https://huggingface.co/nvidia/Nemotron-3-Diarization, https://openmdw.ai/license/1-1/ |
| FluidAudio 0.12.6 (macOS, through fluidaudio-rs) | Parakeet and Qwen3-ASR on the Apple Neural Engine | Apache-2.0 | https://github.com/FluidInference/FluidAudio |
| Swift packages linked by FluidAudio on macOS: swift-asn1, swift-atomics, swift-collections, swift-crypto, swift-huggingface, swift-jinja, swift-nio, swift-system, swift-transformers | model download and tokenisation | Apache-2.0 | https://github.com/apple and https://github.com/huggingface |
| EventSource 1.4.1, yyjson 0.12.0, fluidaudio-rs (vendored in core/vendor) | same | MIT | https://github.com/mattt/EventSource, https://github.com/ibireme/yyjson |
| Sparkle 2.9.1 (macOS) | automatic updates | MIT-style, with bundled components under their own terms (see its text below) | https://github.com/sparkle-project/Sparkle |
| Velopack 0.0.1298 (Windows) | installer and automatic updates | MIT, Copyright 2021 Caelan Sayler, Copyright 2024 Velopack Ltd. | https://github.com/velopack/velopack |
| H.NotifyIcon.WinUI 2.2.0 (Windows) | tray icon | MIT, Copyright (c) 2020 havendv | https://github.com/HavenDV/H.NotifyIcon |
| Markdig 0.40.0 (Windows) | Markdown rendering | BSD-2-Clause, Copyright (c) 2016-2026 Alexandre Mutel | https://github.com/xoofx/markdig |
| CommunityToolkit.Mvvm 8.4.0 (Windows) | view-model helpers | MIT, Copyright (c) .NET Foundation and Contributors | https://github.com/CommunityToolkit/dotnet |
| Windows App SDK 1.7 (Windows, self-contained) | WinUI 3 runtime | Microsoft software licence terms shipped with the Microsoft.WindowsAppSDK package (the SDK source repository is MIT) | https://github.com/microsoft/WindowsAppSDK |
| .NET runtime (Windows, self-contained) | application runtime | MIT, Copyright (c) .NET Foundation and Contributors | https://github.com/dotnet/runtime |

On Linux, GTK 4 and libadwaita (LGPL-2.1-or-later) are loaded from the system
and are not part of the Dimmy download.

Speech and language models that Dimmy downloads at your request are not part of
the Dimmy binaries. Each is governed by its publisher's licence.

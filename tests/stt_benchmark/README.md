# STT benchmark scripts

Python scripts used on 2026-05-03 to compare local Parakeet (INT8 + FP32) and
local Whisper-large-v3-turbo against the same Italian audio samples that drive
the cloud benchmarks. Numbers + interpretation live in
[`docs/dev/stt-benchmark-2026-05-03.md`](../../docs/dev/stt-benchmark-2026-05-03.md).

These are **research scripts**, not committed tests. Hardcoded paths inside
each `.py` reflect the WSL setup of the original session. Adapt before running.

## Install once

```bash
python3 -m pip install --user onnx-asr sherpa-onnx librosa soundfile
```

`onnx-asr` auto-downloads the FP32 Parakeet bundle from HuggingFace
(`istupakov/parakeet-tdt-0.6b-v3-onnx`, ~2.5 GB) on first run.

`sherpa-onnx` needs the INT8 bundle downloaded manually:

```bash
mkdir -p ~/code/pai-voice/.scratch/parakeet
cd ~/code/pai-voice/.scratch/parakeet
curl -LO https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2
tar -xjf sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2
```

Both bundles end up in `.scratch/` which is gitignored — the repo never carries
the ~3.5 GB of model weights.

## Scripts

| Script | What it benchmarks |
|---|---|
| `test_local.py` | Parakeet INT8 via sherpa-onnx, two short Italian samples, cold + warm latency |
| `test_fp32.py` | Parakeet FP32 + Canary 1B v2 + Whisper-large-v3-turbo via onnx-asr |
| `test_chunked.py` | Streaming-style chunked Parakeet on a 176 s audio, with 3 s / 5 s / 10 s windows |

Edit the audio paths at the top of each file before running. The original
samples were Dimmy debug captures under
`%APPDATA%/dimmy/audio_debug/<timestamp>/processed.wav` on Windows or the
equivalent WSL `/mnt/c/...` path.

## Headline numbers

- Parakeet INT8 sherpa-onnx, CPU 4-thread: **~580 ms warm** on 3.5 s audio
- Parakeet FP32 onnx-asr, CPU: **~337 ms warm** (better quality, +1.4 GB disk)
- Whisper-large-v3-turbo onnx-community, CPU: **15-17 s** (unusable without GPU)
- 5 s chunked Parakeet on a 176 s recording: **676 ms avg / 901 ms max** per
  chunk — real-time-safe with 82 % margin

Cloud baselines sit in the 749 ms - 2.9 s range; see the benchmark doc.

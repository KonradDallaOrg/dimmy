# Parakeet TDT v3 FP32 — local STT

> **Branch**: `feat/parakeet-stt-local`
> **Status**: working end-to-end on Windows (UI + chunked live captions
> shipped) and on macOS local dev (CLI smoke + 4 e2e tests green
> against the JFK fixture, see [Mac validation](#mac-validation-2026-05-05)).
> Mac UI (Settings + Onboarding) is wired and the app builds + signs
> with the bundled `libonnxruntime.dylib`. Live mic path on Mac
> awaits a hardware-equipped session.

## Why Parakeet (already validated, branch `feat/stt-providers-expansion`)

Live benchmark on Italian audio (4 May 2026, see
[`stt-benchmark-2026-05-03.md`](stt-benchmark-2026-05-03.md)):

| Backend | Model | Warm latency | Quality |
|---|---|---|---|
| **Local CPU (Parakeet TDT v3 FP32)** | NeMo TDT 0.6 B | **337-547 ms** | ✓ accurate |
| Local CPU (Parakeet INT8) | NeMo TDT 0.6 B | 580-683 ms | drops first phoneme sometimes |
| Local CPU (Whisper-large-v3-turbo ONNX) | OpenAI | 15-17 s ❌ | unusable on CPU |
| Groq cloud | whisper-large-v3-turbo | 749 ms | ✓ |

Chunking 5 s with the same Python pipeline (`tests/stt_benchmark/test_chunked.py`):
avg 676 ms, max 901 ms → **82 % real-time margin on CPU** for arbitrary-length
audio. That's the target of this work.

## Why FP32 not INT8

Same benchmark: INT8 is 1.7× slower in cold path (sherpa-onnx) and
*loses leading phonemes* on Italian audio ("Sì sarebbe" → "Sarebbe",
"Calcolo" → "Alcolo"). FP32 (~2.5 GB) keeps quality with sub-second
latency on consumer CPUs.

## Architecture

```text
                     ┌──────────────────────┐
   16 kHz f32 PCM    │ nemo128.onnx         │   features [1, 128, T]
   (mono) ──────────▶│  (mel preprocessor)  │ ──────────────────────────┐
                     └──────────────────────┘                            │
                                                                         ▼
                                                  ┌──────────────────────┐
                                                  │ encoder-model.onnx   │
                                                  │  (Conformer)         │
                                                  └──────────────────────┘
                                                            │
                              encoded [1, 1024, T']  +  encoded_lengths [1]
                                                            │
                              ┌─────────────────────────────┘
                              ▼
   ┌── greedy TDT loop (per encoder frame t) ──────────────────────────┐
   │                                                                   │
   │  prev_token  = blank_idx (8192)                                   │
   │  state1, state2 = zeros [2, 1, 640]   (LSTM hidden + cell)        │
   │  emitted = 0                                                      │
   │                                                                   │
   │  while t < T':                                                    │
   │    inputs = (encoder_outputs[1, 1024, 1] = enc[t],                │
   │              targets [[prev_token]],                              │
   │              target_length [1],                                   │
   │              input_states_1 = state1,                             │
   │              input_states_2 = state2)                             │
   │    outputs[V+5], state1', state2' = decoder_joint.run(inputs)     │
   │    token = argmax(outputs[..V])           # V = 8193              │
   │    step  = argmax(outputs[V..V+5])        # 0..=4 (TDT-v3)        │
   │    if token != BLANK:                                             │
   │       state1, state2 = state1', state2'                           │
   │       tokens.push(token)                                          │
   │       emitted += 1                                                │
   │    if step > 0:        t += step;     emitted = 0                 │
   │    elif token == BLANK || emitted == 10:                          │
   │                        t += 1;        emitted = 0                 │
   │                                                                   │
   └───────────────────────────────────────────────────────────────────┘
                              │
                              ▼
                        token ids → vocab.txt → text
                        ('▁foo bar' = ' foo' + 'bar', i.e. word starts
                        marked with U+2581; `<…>` skipped as control)
```

Algorithmic notes (verified against the upstream Python reference at
`onnx_asr.models.nemo.NemoConformerTdt` + `_AsrWithTransducerDecoding._decoding`):

- **TDT** = Token-and-Duration Transducer. Adds `NUM_DURATIONS=5` extra
  logits to a normal RNN-T joint output. Each step picks both a token
  and a "skip-this-many-frames" duration → typically 5-10× fewer
  decoder calls than vanilla RNN-T.
- **`max_tokens_per_step = 10`** (NeMo default): cap on emissions at
  the same encoder frame. Without it, a degenerate model could spin.
- **LSTM state commit on non-blank only**: blank predictions don't
  advance the prediction-net state. This is key — committing on every
  step degrades quality.
- **Vocab**: 8193 tokens including blank at idx 8192. Word-start marker
  is `▁` (U+2581).

## Bundle (~ 2.5 GB on disk)

Downloaded from `istupakov/parakeet-tdt-0.6b-v3-onnx` on HuggingFace
into `<config-dir>/parakeet-fp32/`:

| File | Size | Role |
|---|---|---|
| `nemo128.onnx` | 140 KB | waveform → 128-bin mel features |
| `encoder-model.onnx` | 41 MB | Conformer encoder graph |
| `encoder-model.onnx.data` | 2.4 GB | external weights |
| `decoder_joint-model.onnx` | 73 MB | TDT pred-net + joint |
| `vocab.txt` | 92 KB | 8193 tokens, one per line |

Streaming download with progress callback already lives in
`core/src/parakeet.rs::download_bundle`. The FFI / UI hook for "click
to download" is the natural follow-up.

## Cargo features

```toml
local-stt-parakeet      = ["dep:ort", "dep:ndarray"]
local-stt-parakeet-cuda = ["local-stt-parakeet", "ort/cuda"]
local-stt-parakeet-coreml = ["local-stt-parakeet", "ort/coreml"]
```

Default builds DON'T pull `ort` — keeps the cold compile time the same
for everyone not opting in. Bundle path / presence helpers live in the
always-on part of the module so the UI can render the "Parakeet not
yet downloaded" state without a feature dance.

## What's done in this commit

- `core/src/parakeet.rs` — module with bundle paths, download, feature
  gate, stub `transcribe()`. ~250 LoC.
- `core/Cargo.toml` — `ort = "=2.0.0-rc.10"` (pinned: rc.12 has a
  compile-time bug on the VitisAI EP that breaks even when the feature
  is off), `ndarray = "0.16"`, `reqwest blocking` for the download.
- 2 unit tests (`vocab_size_and_blank_match_bundle`,
  `bundle_dir_returns_path`).
- This document + the inline header in `parakeet.rs`.

## What's next (in priority order)

1. **Native ort inference** — implement `transcribe()` against
   ort 2.0.0-rc.10 API:
   - `Session::builder().commit_from_file(path)` for each of the 3 ONNX files
   - mel forward: `inputs![ "waveforms" => Value::from_array(wave_2d), "waveforms_lens" => Value::from_array(len_1d) ]`
   - encoder forward: same shape, output `outputs` is `[1, 1024, T']`
     transposed to `[1, T', 1024]` in onnx_asr — verify which layout
     `decoder_joint` actually wants
   - LSTM state init zeros `[2, 1, 640]` — read the hidden-size from
     `decoder_joint.inputs["input_states_1"].shape[-1]` so a future
     bundle revision doesn't silently break
   - greedy TDT loop as in the diagram above
   - vocab lookup (`▁` → space)

   The Python reference is small enough (~50 LoC of decoder loop) that
   a 1:1 port is the right strategy. **Don't reinvent the algorithm**;
   just translate.

2. **GPU paths** — add CUDA EP on Windows (`ort::CUDAExecutionProvider`
   when the `local-stt-parakeet-cuda` feature is on), CoreML EP on
   Mac. Both are CPU-fallback-safe.

3. **FFI** — `dimmy_parakeet_bundle_present`, `dimmy_parakeet_download`
   (returns immediately + emits progress events through the existing
   event callback), `dimmy_parakeet_transcribe(samples_ptr, len)`.

4. **UI hook** — Settings → Voice input → "Local backend" dropdown:
   `Whisper.cpp` (current) | `Parakeet TDT v3 FP32`. On switch to
   Parakeet without the bundle present, show a download CTA.

5. **Chunked path for long audio** — reuse the existing VAD-driven
   chunking pipeline (`preprocess.rs`); the Python benchmark showed
   5 s windows with no overlap already produce identical text to the
   full-audio call, so no special dedup is needed at the chunk
   boundary for Parakeet (unlike Whisper).

6. **Tests** — `core/tests/parakeet_e2e.rs` (already drafted in this
   branch's history but removed pending the inference impl). Reads
   3 fixtures (short 1.5 s, medium 13 s, long 50 s) from the
   `audio_debug` dir + asserts non-empty Italian text. Skips if the
   bundle isn't present.

## Reference: Python loop ported from

Both files are vendored under
`/home/konrad/.local/lib/python3.10/site-packages/onnx_asr/`:

- `models/nemo.py` — `NemoConformerTdt._decode` (overrides RNN-T's
  to additionally pull the duration argmax from the tail of the
  joint output)
- `asr.py` lines 192-228 — `_AsrWithTransducerDecoding._decoding`
  (the greedy outer loop)

These are the canonical references. When you write the Rust port,
sanity-check by running the Python on the same fixture and comparing
text. They should be byte-identical because the loop is deterministic.

## Build commands

```bash
# Default build (no Parakeet — current behaviour preserved)
cd core
cargo build --release --lib

# Parakeet CPU
cargo build --release --lib --features local-stt-parakeet

# Parakeet + CUDA on Win
cargo build --release --lib --features local-stt-parakeet-cuda

# Parakeet + CoreML on Mac
cargo build --release --lib --features local-stt-parakeet-coreml

# Tests (default features)
cargo test --lib parakeet
```

## Mac validation (2026-05-05)

Local Mac dev session, M-series Apple Silicon, no microphone (validated
via committed JFK fixture, not live recording).

- `cargo build --release --lib --target aarch64-apple-darwin --features local-stt-metal,local-llm-metal,local-stt-parakeet-coreml` — clean, ~52 s.
- Xcode `Debug` build of `Dimmy.app` succeeds end-to-end. The new shell-script build phase `Bundle onnxruntime.dylib` (`AE000004`) runs `scripts/download-onnxruntime.sh` and copies + ad-hoc-signs the 33 MB dylib into `Dimmy.app/Contents/Frameworks/libonnxruntime.dylib`. `ENABLE_USER_SCRIPT_SANDBOXING = NO` is required project-wide so the script's `codesign` call doesn't 503 inside Xcode's sandbox.
- `parakeet_smoke <jfk.wav>` cold path: 6.1 s for 11 s audio (1.8× realtime). A second invocation in a fresh process is 2.7 s (4.0× realtime) thanks to OS-level mmap caching of the 2.4 GB encoder weights.
- `cargo test --release --test parakeet_e2e --features local-stt-parakeet -- --test-threads=1` → all 4 tests pass (empty PCM, JFK transcribe, italian fixtures skipped without recordings, warm-call deterministic). The binary then SIGABRTs at process exit — known cosmetic noise tracked as STT-002 in `known-bugs.md`.
- Output text: `"And so, my fellow Americans, ask not what your country can do for you, ask what you can do for your country."` — byte-identical to the upstream reference, no leading-phoneme drop.

## Risks / open questions

- **`ort` 2.0.0-rc.10 API stability**: rc.12 broke compilation on a
  field that doesn't exist in onnxruntime headers. Pin is hard-required.
  When 2.0.0 stable lands we should re-pin and drop the workaround.
- **Encoder weights file (`.data`) external load**: ort needs the
  `.data` file in the same directory as `encoder-model.onnx`. The
  download helper already places it correctly; verify on Mac (paths
  with spaces in `Application Support/dimmy/`).
- **Bundle redistribution**: 2.5 GB inside the installer is a no-go.
  Plan stays "first-run download with progress" — same UX as the
  whisper.cpp models today.
- **Mac GPU**: CoreML EP via ort is documented but I haven't yet
  verified Parakeet TDT specifically runs on it. Worst case: CPU
  fall-back, which is still sub-second on M-series.

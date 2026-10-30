# Local model inventory and redistribution licences

Snapshot 2026-10-30. Sources: `core/src/{local_stt,local_llm,parakeet,qwen_asr,whistle,diarize,nemotron,silero,gtcrn,coreml_encoder}.rs`; licences read from the Hugging Face API `cardData` of each source repo (and of its `base_model`). This is an engineering inventory, not legal advice.

"SHA pinned": **yes** = SHA-256 hard-coded in Dimmy. **runtime** = Dimmy reads HF's `x-linked-etag` at download time (`download.rs`), so integrity is checked but nothing is frozen in source. None of the "runtime" rows pin a revision (`resolve/main`), so a mirror must freeze one.

| # | Model | Used by | Source @ revision | Size | SHA pinned | Licence | Mirror? | Conditions |
|---|---|---|---|---|---|---|---|---|
| 1 | Whisper ggml: tiny/base q8, small q5_1, medium q5_0, large-v3-turbo q5/q8, large-v3 q5, large-v3 f16 | local STT (whisper) | `ggerganov/whisper.cpp` @ main | 42 MB - 2.95 GB | runtime | MIT | yes | keep MIT notice |
| 2 | Whisper large-v3 q8 | local STT | `Pomni/whisper-large-v3-ggml-allquants` @ main | 1.58 GB | runtime | Apache-2.0 (base openai/whisper-large-v3: Apache-2.0) | yes | licence text + NOTICE, state changes (quantized) |
| 3 | Distil-Large-v3.5 q8 / q5 (EN) | local STT | `Pomni/distil-large-v3.5-ggml-allquants` @ main | 818 / 538 MB | runtime | MIT (base distil-whisper: MIT) | yes | keep MIT notice |
| 4 | Whisper Core ML encoders (`*-encoder.mlmodelc`) | Mac STT accel | `ggerganov/whisper.cpp` @ main | up to ~1.1 GB | runtime | MIT | yes | keep MIT notice |
| 5 | Silero VAD v6.2.0 ggml (bundled, not downloaded) | VAD | `ggml-org/whisper-vad` @ main | 885 KB | no (in installer) | MIT | yes | MIT notice (already in THIRD_PARTY_NOTICES) |
| 6 | Parakeet TDT 0.6B v3 ONNX (5 files) | local STT (Win/Linux) | `istupakov/parakeet-tdt-0.6b-v3-onnx` @ main | ~2.5 GB | runtime | CC-BY-4.0 (base nvidia/parakeet-tdt-0.6b-v3: CC-BY-4.0) | conditional | attribution to NVIDIA, link licence, indicate changes (ONNX conversion) |
| 7 | Parakeet TDT v3 Core ML (fetched by FluidAudio, not by Dimmy code) | local STT (Mac) | `FluidInference/parakeet-tdt-0.6b-v3-coreml` | ~3 GB | no (FluidAudio) | CC-BY-4.0 | conditional | as #6; Dimmy does not control the URL |
| 8 | Gemma 4 GGUF: E2B/E4B/12B/26B-A4B, QAT UD-Q4_K_XL and plain Q2-Q8 (13 files) | local LLM (default: E2B QAT) | `unsloth/gemma-4-{E2B,E4B,12B,26B-A4B}-it[-qat]-GGUF` @ main | 2.5 - 13.6 GB | runtime | Apache-2.0 (cards, and google/gemma-4-E2B-it) | yes | licence text; see note A |
| 9 | TranslateGemma 4B / 12B Q4_K_M | local LLM (translate) | `mradermacher/translategemma-{4b,12b}-it-GGUF` @ main | 2.4 / 7.0 GB | runtime | `gemma` (Gemma Terms of Use) | conditional | see note B |
| 10 | Qwen3-4B-Instruct-2507 Q4_K_M | local LLM | `unsloth/Qwen3-4B-Instruct-2507-GGUF` @ main | 2.4 GB | runtime | Apache-2.0 | yes | licence text |
| 11 | Qwen3.5 2B / 4B Q4_K_M | local LLM | `unsloth/Qwen3.5-{2B,4B}-GGUF` @ main | 1.2 / 2.6 GB | runtime | Apache-2.0 | yes | licence text |
| 12 | Phi-4-mini-instruct Q4_K_M | local LLM | `matrixportalx/Phi-4-mini-instruct-Q4_K_M-GGUF` @ main | 2.5 GB | runtime | MIT (base microsoft/Phi-4-mini-instruct: MIT) | yes | keep MIT notice; third-party quant, prefer re-quantizing or verifying vs Microsoft base |
| 13 | MiniCPM5-2B Q4_K_M | local LLM | `openbmb/MiniCPM5-2B-GGUF` @ main | 1.5 GB | runtime | Apache-2.0 (card tag only; no LICENSE file checked) | yes | licence text; see note C |
| 14 | Qwen3-ASR 0.6B / 1.7B Q8_0 + mmproj | local STT (qwen) | `ggml-org/Qwen3-ASR-{0.6B,1.7B}-GGUF` @ main | 0.97 / 2.4 GB | runtime | **unknown** at repo level (no licence tag); base Qwen/Qwen3-ASR: Apache-2.0 | unknown | see note D |
| 15 | Qwen3-ASR 0.6B Core ML int8 / f32 (fetched by FluidAudio) | local STT (Mac) | `FluidInference/qwen3-asr-0.6b-coreml` | 2.9 / 4.0 GB | no (FluidAudio) | Apache-2.0 | yes | licence text |
| 16 | Nemotron 3.5 ASR streaming 0.6B q8_0 GGUF | streaming dictation / meetings | `nvidia/nemotron-3.5-asr-streaming-0.6b` @ `ea30d66` | 742 MB | **yes** | OpenMDW-1.1 (card: `other`, name openmdw-1.1) | conditional | see note E |
| 17 | Nemotron-3-Diarization q8_0 GGUF | live speakers | `nvidia/Nemotron-3-Diarization` @ `f667ed7` | 107 MB | **yes** | OpenMDW-1.1 | conditional | see note E |
| 18 | Nemotron-3-Diarization int8 ONNX + preprocessor | post-meeting diarization | `joosthel/Nemotron-3-Diarization-ONNX` @ `4a911fc` | 104 MB | **yes** (2 files) | OpenMDW-1.1 (base nvidia) | conditional | see note E; third-party conversion |
| 19 | Whistle `whistle.cact` | local STT (whistle) | `Cactus-Compute/whistle` @ `b358dda` | 16.9 MB | **yes** | Apache-2.0 (repo has LICENSE) | yes | licence text |
| 20 | Whistle engine (`libneedle3` inside Cactus Python wheels, per OS) | whistle runtime | `Cactus-Compute/needle3` @ `2ae1132` `/python` | 0.5 - 0.7 MB | **yes** (per wheel) | Apache-2.0 (repo has LICENSE) | yes | licence text; ships binaries only, source not in that repo |
| 21 | GTCRN `gtcrn_simple.onnx` (bundled, not downloaded) | denoise (off by default) | github.com/Xiaobin-Rong/gtcrn | 535 KB | no (in installer) | MIT | yes | MIT notice (already in THIRD_PARTY_NOTICES) |

Not a model download: the Nemotron engine archives (cpu/cuda/metal) come from our own GitHub release `engine-nemotron-66d50d9`, SHA-256 pinned in `nemotron.rs`. Nothing to mirror there.

## Tally (21 rows)

- Allowed (MIT / Apache-2.0, notice only): 14 (#1-5, 8, 10-13, 15, 19-21)
- Conditional (attribution or use terms): 6 (#6, 7, 9, 16, 17, 18)
- Not allowed: 0
- Unknown: 1 (#14)

## Notes on flagged items

- **A. Gemma 4 (#8).** The unsloth cards and Google's own card say `apache-2.0`, yet the card metadata still links `ai.google.dev/gemma/docs/gemma_4_license`. Read that page and Google's LICENSE file before mirroring; if it differs from Apache-2.0, treat #8 like #9.
- **B. TranslateGemma (#9).** Licence is the Gemma Terms of Use, not Apache. Redistribution is allowed if the recipient gets the Terms and the use restrictions (Gemma Prohibited Use Policy) are passed on, the modification is noted, and the Gemma notice is kept. The Q4_K_M file is a derivative, so the Terms apply to the mirror. The quantizer (mradermacher) is a third party: mirror from their file at a pinned revision, not Google's.
- **C. MiniCPM5 (#13).** Only the HF card tag was read. Confirm the repo's LICENSE file and any commercial-use registration clause before mirroring.
- **D. Qwen3-ASR GGUF (#14).** The ggml-org repos declare no licence, only `base_model`. The base is Apache-2.0, and a quantized derivative of an Apache-2.0 work can be redistributed, but the repo owner gave no statement, so it is recorded as unknown. Options: ask ggml-org to add the tag, or re-convert from `Qwen/Qwen3-ASR-*` ourselves. #15 is a different (Core ML) artifact and does not settle it.
- **E. Nemotron (#16-18).** OpenMDW-1.1 is a permissive model licence covering weights and code: redistribution is allowed, but the licence text and NVIDIA's copyright/attribution notices must travel with the files. #16's card lists `license: other`, so the notice must be taken from the repo's own LICENSE/NOTICE, not assumed. #18 is a third-party conversion of NVIDIA weights and inherits the same terms.
- **F. CC-BY-4.0 (#6, 7).** Needs credit to NVIDIA as creator, the licence link, and a statement that the file was converted. No use restrictions.
- **G. Unpinned (#1-4, 8-15).** No revision or SHA in source. Mirroring is the chance to freeze both (see below).

## How to mirror (checklist)

1. Download each file at one exact upstream revision; record the commit hash and the file's SHA-256.
2. Upload byte-identical (no re-quantizing, no re-compression) so the hash is unchanged. Where Dimmy pins a SHA (#16-20), the mirror must match that value exactly; run `sha256sum` after upload and compare.
3. Put the upstream LICENSE (and NOTICE if any) next to the weights in the mirror repo, and set `license:` in the model card to the same id.
4. Model card: credit the original authors, link the source repo + revision + SHA-256, and state "unmodified copy" (or what changed, for CC-BY and Gemma).
5. For Gemma Terms (#9): include the Terms text and Prohibited Use Policy link, and a notice that use is subject to them.
6. For OpenMDW (#16-18): include the full licence text and NVIDIA's copyright notice.
7. Point Dimmy at `<mirror>/resolve/<revision>/...`, never `main`, and add the SHA-256 to the catalogue (extend the `Pinned` pattern from `nemotron.rs`) so a mirror drift fails loudly.
8. Do not mirror #14 until the licence question is settled; #7/#15 are licence-clear but FluidAudio owns their URLs, so mirroring them needs a FluidAudio change.
9. Keep `core/assets/THIRD_PARTY_NOTICES.md` in sync for anything bundled in the installer (#5, #21).

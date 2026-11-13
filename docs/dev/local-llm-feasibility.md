# Local LLM Enhancement — Feasibility Study

**Status, checked against the code on 2026-11-11:**

- This is the study from 2026-04-12, written before the feature existed. The
  feature shipped: `core/src/local_llm.rs`, cargo features `local-llm`,
  `local-llm-vulkan`, `local-llm-metal`, `local-llm-cuda` (`core/Cargo.toml`).
  The frozen Windows build includes `local-llm-vulkan`.
- The binding is `llama-cpp-4` (git `eugenehp/llama-cpp-rs`), not
  `llama-cpp-2`. Nothing in the product uses Ollama; the Ollama numbers below
  are the study's test rig.
- Three paths use the local model when `llm_mode == "local"`: dictation
  rewrite (`ffi.rs::dimmy_process_with_llm` → `local_llm::process_text_local`),
  command mode (`dimmy_command_transform` → `process_raw_prompt_local`) and
  meeting recap (`dimmy_llm_call_raw` → `process_raw_prompt_local`, also with
  a `local:<file>.gguf` recap model). The routing is in `ffi.rs`, not `llm.rs`.
- The default model is `local_llm::DEFAULT_LLM_MODEL =
  "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf"` (2500 MB in the catalogue), not the
  Q4_K_M file recommended below. The catalogue `AVAILABLE_LLM_MODELS` has 18
  models (Gemma 4, TranslateGemma, Qwen 3, Qwen 3.5, Phi-4 Mini, MiniCPM5).
- Whisper and the LLM do NOT always share the GPU. Before an LLM load
  `should_evict_stt` drops the cached whisper and Qwen3-ASR models when the
  LLM's estimated footprint (1.5 x file size + 1024 MB) is more than a third
  of the GPU memory that `hardware::detect()` reports
  (`local_llm::stt_should_yield`), and again when context creation fails. An
  idle LLM is unloaded after 5 minutes (`IDLE_UNLOAD`).
- Thinking is handled per model, not by avoiding chat templates: the gguf's
  own template is applied, `skip_thinking_suffix` pre-fills an empty think
  block where the template has `enable_thinking`, and `PieceFilter` hides
  reasoning tokens.

> Date: 2026-04-12
> Hardware tested: NVIDIA T600 Laptop GPU (4 GB VRAM), Intel i7, 16 GB RAM
> Status on that date: **Feasible on 4+ GB VRAM GPUs. Backlogged for v1.1.**
> (Shipped since; see the status block above.)

## Goal

Run LLM text enhancement (grammar correction, professional rewrite, summarization, prompt reformulation) entirely offline, alongside local Whisper STT, on the same GPU.

## Models Tested

All results in this section are from 2026-04-12, run through Ollama or Google
AI Studio, not through Dimmy's own code.

### Gemma 4 26B MoE (via Google AI Studio — cloud baseline)
- **Quality:** Excellent across all styles. Perfect Italian.
- **Speed:** Cloud, ~1s per response.
- **Verdict:** Quality ceiling — this is what we're trying to match locally.

### Gemma 4 E2B Q4_K_M (via Ollama on T600)
- **Actual parameters:** 5.1B (MoE, "E2B" = effective 2B active)
- **File size:** 7.2 GB (Q4_K_M quantization)
- **VRAM usage:** 1624 MB in VRAM + rest offloaded to CPU RAM
- **Speed:** 27 tok/s (partial GPU offload)
- **Quality (thinking OFF):**

| Style | Output | Time | Verdict |
|---|---|---|---|
| Correct | Perfect punctuation, apostrophes, capitalization | 3.3s | Excellent |
| Professional | "Ieri sono stato dal meccanico e mi ha comunicato..." | 3.5s | Excellent (needs "keep same language" reinforcement) |
| Summarize | "Problema motore: cambio filtro olio. Pastiglie freni consumate. 500 euro." | 1.7s | Excellent |
| Prompt | "Implementa un sistema di cache in Rust per memorizzare un modello di ML..." | 2.1s | Excellent |

- **Critical:** Must disable thinking mode (`think: false`). With thinking ON, generates 300-500 tokens of hidden reasoning → 20+ seconds.
- **Critical:** System prompt must explicitly say "Keep the SAME language as the input" or it defaults to English for some styles.

### Gemma 3 1B (via Ollama on T600)
- **Speed:** 56-68 tok/s
- **Quality:** Unusable for Italian — translates to English, ignores "keep same language"
- **Verdict:** Rejected

### SmolLM2 1.7B (via Ollama on T600)
- **Speed:** 44 tok/s
- **Quality:** Unusable — invents words ("esatorio", "francheti delle fiamme"), translates to English
- **Verdict:** Rejected

## VRAM Budget (T600, 4 GB)

```
Whisper large-v3-turbo Q5:  558 MB
Gemma 4 E2B Q4_K_M:       1624 MB  (partial, rest on CPU)
─────────────────────────────────────
Total:                     2182 MB / 4096 MB  ✅ fits
Free:                      1914 MB
```

On 2026-04-12 both models fit in VRAM together, with whisper at Q5 (558 MB).
The product does not rely on that. Measured 2026-09-04 on a 3.9 GB T600
(comment in `local_llm.rs::llm_cache::generate`): whisper
large-v3-turbo q8_0 held about 2.1 GB, Gemma 4 E2B Q4 loaded beside it and
the process aborted in inference. So the LLM load now evicts the cached
whisper model when the two would not fit (`should_evict_stt`), and whisper
reloads the next time STT runs.

## Estimated End-to-End Flow (Offline)

An estimate from 2026-04-12, before any Dimmy code ran a local LLM.

```
Speak 5s → Whisper: 2s → Gemma 4 E2B: 2-3s → Paste
           TOTAL: ~4-5 seconds, fully offline
```

Compare: Groq cloud LLM enhancement takes <500ms.

## Architecture Plan

The plan as written on 2026-04-12. Where the build differs, the text says
"as built".

whisper.cpp and llama.cpp both use ggml as their tensor backend. They create separate Vulkan/CUDA/Metal contexts but can coexist on the same GPU device simultaneously.

```
┌──────────── GPU (shared VRAM) ────────────┐
│                                            │
│  whisper.cpp          llama.cpp            │
│  (via whisper-rs)     (via llama-cpp-4)    │
│  Whisper model        Gemma/LLM model      │
│       │                    │               │
│       └────────┬───────────┘               │
│                ▼                           │
│          ggml backend                      │
│    (Vulkan / CUDA / Metal)                 │
└────────────────────────────────────────────┘
```

### What changes

| Component | Change | Effort |
|---|---|---|
| `Cargo.toml` | Add the llama.cpp dep (planned `llama-cpp-2`; as built `llama-cpp-4`), `local-llm` + `local-llm-vulkan/metal/cuda` features | Small |
| `local_llm.rs` (new) | Model catalogue, download, cache (same pattern as `local_stt.rs`) | planned ~150 lines; about 2400 today, tests included |
| `llm.rs` | Planned: an `if llm_mode == "local"` routing branch. As built the branch is in `ffi.rs` (`dimmy_process_with_llm`, `dimmy_command_transform`, `dimmy_llm_call_raw`); `llm.rs` has no local branch | ~20 lines |
| `ffi.rs` | FFI functions for LLM model management (as built: `dimmy_list_llm_models`, `dimmy_download_llm_model`, `dimmy_llm_model_exists`) | ~50 lines |
| Config | New fields: `llm_mode`, `local_llm_model` | Small |
| Platform UIs | LLM model dropdown in settings (all 3 platforms) | Medium |
| CI | Add `local-llm-vulkan/metal` to build matrix | Small |

### What does NOT change

- whisper-rs / local_stt.rs — planned untouched. As built the two are coupled: `local_llm.rs` reads `local_stt::gpu_backend_status()` and calls `local_stt::clear_model_cache()` to free VRAM
- Audio pipeline — untouched
- Cloud providers — continue working as before
- Existing FFI functions — unchanged
- Feature is opt-in behind feature flag

### Key implementation notes

1. **Thinking mode must be disabled** — Gemma 4 generates hidden reasoning tokens by default. The plan was to avoid any chat template with `<think>`. As built, the gguf's own chat template IS used (`apply_chat_template`; `render_gemma4_prompt` for Gemma 4), and thinking is handled by `skip_thinking_suffix` (pre-fills an empty think block when the template has `enable_thinking`) and `PieceFilter` (hides reasoning tokens from the output).
2. **System prompt needs "keep same language" reinforcement** — Small models tend to default to English. As built the local path does not use the `PREAMBLE` in `llm.rs`: `build_local_system_prompt` takes the style text from `LlmStyle::instruction()` and `anchor_text` NAMES the language ("Write your entire answer in Italian."). See `local-llm-quality-2026-09-05.md` for why.
3. **GPU device selection** — as built: `local_stt::gpu_backend_status()` returns the device (or `Unavailable`, which loads the model on CPU). There is no `preferred_gpu_device()` in the code today.
4. **macOS** — use `local-llm-metal` feature for Apple Silicon acceleration. llama.cpp has excellent Metal support.
5. **Model download** — as built: `local_llm::download_model` on the shared `download::download_resumable` (Range resume, SHA-256, GGUF magic check). GGUF models from HuggingFace.

## Recommended Default Model

The recommendation of 2026-04-12. Today's default is
`gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf` (Gemma 4 E2B QAT, 2500 MB in the
catalogue), set on 2026-10-23; Phi-4 Mini was the default from 2026-05-18
until then.

**Gemma 4 E2B Q4_K_M** (`gemma-4-e2b-it-q4_k_m.gguf`)
- 7.2 GB download (the size this study recorded for the Ollama build; the catalogue file `gemma-4-E2B-it-Q4_K_M.gguf` is listed at 3100 MB), but partial GPU offload makes it usable on 4 GB VRAM
- Best quality among models that fit alongside Whisper
- 140+ languages including Italian
- Apache 2.0 license

## Minimum Hardware Requirements

Estimates from 2026-04-12, not measured on the shipped feature.

| Config | VRAM | Performance |
|---|---|---|
| STT only (local whisper) | 1 GB+ | Works on any GPU |
| STT + LLM (whisper + gemma 4 e2b) | 4 GB+ | 4-5s end-to-end |
| STT + LLM (full GPU) | 8 GB+ | ~2s end-to-end |

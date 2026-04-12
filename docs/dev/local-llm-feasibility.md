# Local LLM Enhancement — Feasibility Study

> Date: 2026-04-12
> Hardware tested: NVIDIA T600 Laptop GPU (4 GB VRAM), Intel i7, 16 GB RAM
> Status: **Feasible on 4+ GB VRAM GPUs. Backlogged for v1.1.**

## Goal

Run LLM text enhancement (grammar correction, professional rewrite, summarization, prompt reformulation) entirely offline, alongside local Whisper STT, on the same GPU.

## Models Tested

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

Both models coexist in VRAM simultaneously. No swapping needed.

## Estimated End-to-End Flow (Offline)

```
Speak 5s → Whisper: 2s → Gemma 4 E2B: 2-3s → Paste
           TOTAL: ~4-5 seconds, fully offline
```

Compare: Groq cloud LLM enhancement takes <500ms.

## Architecture Plan

whisper.cpp and llama.cpp both use ggml as their tensor backend. They create separate Vulkan/CUDA/Metal contexts but can coexist on the same GPU device simultaneously.

```
┌──────────── GPU (shared VRAM) ────────────┐
│                                            │
│  whisper.cpp          llama.cpp            │
│  (via whisper-rs)     (via llama-cpp-2)    │
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
| `Cargo.toml` | Add `llama-cpp-2` dep, `local-llm` + `local-llm-vulkan/metal/cuda` features | Small |
| `local_llm.rs` (new) | Model catalogue, download, cache (same pattern as `local_stt.rs`) | ~150 lines |
| `llm.rs` | Add `if llm_mode == "local"` routing branch | ~20 lines |
| `ffi.rs` | FFI functions for LLM model management | ~50 lines |
| Config | New fields: `llm_mode`, `local_llm_model` | Small |
| Platform UIs | LLM model dropdown in settings (all 3 platforms) | Medium |
| CI | Add `local-llm-vulkan/metal` to build matrix | Small |

### What does NOT change

- whisper-rs / local_stt.rs — untouched
- Audio pipeline — untouched
- Cloud providers — continue working as before
- Existing FFI functions — unchanged
- Feature is opt-in behind feature flag

### Key implementation notes

1. **Thinking mode must be disabled** — Gemma 4 generates hidden reasoning tokens by default. When calling llama.cpp, do NOT use chat template that enables `<think>` mode. Use raw completion or a template without thinking tags.
2. **System prompt needs "keep same language" reinforcement** — Small models tend to default to English. The PREAMBLE in `llm.rs` already says "Keep the same language" but may need to be stronger for local models.
3. **GPU device selection** — reuse `preferred_gpu_device()` from `local_stt.rs` (already auto-detects discrete GPU).
4. **macOS** — use `local-llm-metal` feature for Apple Silicon acceleration. llama.cpp has excellent Metal support.
5. **Model download** — reuse existing `download_model()` infrastructure. GGUF models from HuggingFace.

## Recommended Default Model

**Gemma 4 E2B Q4_K_M** (`gemma-4-e2b-it-q4_k_m.gguf`)
- 7.2 GB download, but partial GPU offload makes it usable on 4 GB VRAM
- Best quality among models that fit alongside Whisper
- 140+ languages including Italian
- Apache 2.0 license

## Minimum Hardware Requirements

| Config | VRAM | Performance |
|---|---|---|
| STT only (local whisper) | 1 GB+ | Works on any GPU |
| STT + LLM (whisper + gemma 4 e2b) | 4 GB+ | 4-5s end-to-end |
| STT + LLM (full GPU) | 8 GB+ | ~2s end-to-end |

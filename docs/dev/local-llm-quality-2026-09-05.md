# Local LLM quality — what was actually broken

> Measured 2026-09-05 on an NVIDIA T600 Laptop GPU (4 GB VRAM).
> Models: Gemma 4 E2B (Q4_K_M and QAT Q4_K_XL), Phi-4 Mini Q4, Qwen 3 4B Q4.
> Material: real dictations from the user's own history, never invented text.

The local LLM was believed to be "mediocre". It was not mediocre; it was
broken in four places, and every one of them was our code. This is the
record, because the same shape of mistake will happen again.

## The four defects

### 1. `n_batch` was never set — the app DIED

llama.cpp asserts `n_tokens_all <= n_batch` (llama-context.cpp:1599). We
sized `n_ctx` to fit the prompt and handed the model the whole thing in one
decode, but never set `n_batch`, so it stayed at llama.cpp's default of 2048.
Above ~1500 words the assert fires — and a GGML_ASSERT is an abort, not an
error we can catch. Dimmy simply vanished.

Local recap therefore worked only on very short meetings, which is why one
machine produced a recap at n_ctx 6144 (a one-minute meeting) while another
died five times out of five at 6656 — same GPU, same model. That looked like
a Vulkan bug, a driver bug, a VRAM shortage and a model incompatibility in
turn. It was none of them.

After the fix: a real 2-hour transcript at n_ctx 36352 in 229 s.

### 2. The repetition penalty was 106, where sane is 1.1

`LlamaSampler::penalties_simple` in the llama-cpp-rs fork called
`llama_sampler_init_penalties` with the argument order that function had
years earlier:

    header:  (penalty_last_n: i32, repeat: f32, freq: f32, present: f32)
    fork:    (n_vocab,             eos_id,      nl_id,     penalty_last_n)

Same types, so it compiled in silence, and `penalty_repeat` became the EOS
token id — about 106 on Gemma 4.

A penalty that high forbids the model from reusing any word it has already
said, so it reaches for synonyms until the sentence stops meaning anything.
Every mangled local output came from this: the broken grammar, the drift into
English on an Italian meeting, the collapsed lists, the hundred-word tails of
filler.

One real 35-minute meeting, same model, same transcript:

| penalty | topics | decisions | actions | prose |
|---|---|---|---|---|
| 106 | 5 | 0 | 3 | mangled |
| 1.1 | 23 | 2 | 15 | correct Italian |

**This landed in the same commit (0707759, 2026-05-18) that demoted Gemma in
favour of Phi-4 Mini**, for "emoji spam, meta-commentary, hallucinations" —
which is what an insane repetition penalty produces. That verdict was formed
on a broken system and should be re-taken.

### 3. The style instructions were too short, and never named the language

    // Ultra-short style instructions — small models need direct, simple commands
    LlmStyle::Professional => "Rewrite in formal, professional business tone."

    assert!(prompt.len() < 200, "local prompt must be short for small models")

A belief written into a comment and pinned by a test. Never measured, and
wrong on both counts. 192 trials per configuration — 4 models x 8 styles x 6
real dictations:

| configuration | wrong language | left unchanged |
|---|---:|---:|
| short instructions (shipped) | 54 / 192 | 21 / 192 |
| full instructions, no anchor | 53 / 192 | 4 / 192 |
| short instructions + "same language" | 32 / 192 | 25 / 192 |
| full instructions + "same language" | 42 / 192 | 1 / 192 |
| **full instructions + language NAMED** | **1 / 192** | **2 / 192** |

Two independent failures, each with its own remedy and neither fixing the
other. Three words are easy to ignore — a fifth of outputs came back
untouched. And no short form said to stay in the user's language, so a model
handed an English order over Italian speech answered in English:
systematically on Summarize and Professional, the two whose instructions
never mentioned language at all.

NAMING the language is what closes it. "Answer in the same language as the
input" asks a 4B model to reason about its own input; "Write your entire
answer in Italian" does not. The translate instruction has carried a note
saying exactly this since June — small models ignore "translate to en" and
follow "translate to English" — and the same indirection was quietly costing
us everywhere else. Phi-4 Mini went from 19 wrong-language answers in 48 to
0; Qwen from 14 to 0.

Validated on 288 trials over NINE DIFFERENT dictations, none used to choose
any of this, 15 to 110 words, technical and profane: 1 wrong language.

### 4. The anchor contradicted the translate instruction

Caught before shipping, by testing translation rather than assuming a change
to the styles was confined to the styles:

    "Write your entire answer in Italian. Then translate the entire output to English."

The anchor now stands down whenever a translation is requested.

## Where each model actually stands

After all four fixes, 72 trials each (nine fresh dictations x eight styles):

| model | wrong language | unchanged | translates | speed |
|---|---:|---:|---|---|
| Gemma 4 E2B Q4 | 0 | 0 | 1 of 4 | fast |
| Gemma 4 E2B QAT | 1 | 4 | 2 of 4 | fastest |
| Phi-4 Mini | 0 | 2 | 4 of 4 | slowest |
| Qwen 3 4B | 0 | 2 | 4 of 4 | slow |

**Gemma does not translate.** The instruction names the target language
plainly and it still returns the Italian, or half-German half-Italian. That
one is the model, not the prompt — and it fails SILENTLY: a user who picks
Gemma and asks for French gets their Italian back with no error and no
warning. Not yet addressed.

The QAT models earn their download: the E2B QAT used 1224 MiB of VRAM against
the plain Q4's 1408 while extracting decisions the plain quantisation missed
entirely (23 against 2 on one meeting, though with repeats a dedup pass
should collapse).

## Recap quality — technique beats model

One pass over a 9099-word meeting produces garbage on a 2-4B model: mangled
markers, bullet lists collapsed onto one line, and on one run a drift into
Spanish. The same model on an 1800-word slice is clean. The failure is
LENGTH, which is what the literature reports (smaller chunks beat full
context; "lost in the middle"; length-induced degradation dominating the
error for long inputs).

| technique | time | result |
|---|---:|---|
| one pass, 11-marker template | 98 s | 1 marker of 11 |
| one pass, 4 sections | 98 s | markers mangled, lists collapsed |
| chunked map + labelled reduce | 56 s | correct structure, right content |

Faster AND better. Two things did most of it: the section markers are written
by OUR code rather than asked of the model, and the classification
(topic/decision/action) happens at the MAP step, where the model still has
the words that were said. Asking the reduce to sort a flat bullet list into
sections produced three identical sections.

`core/src/bin/recap_mapreduce.rs` implements this. It is NOT in the product.

## Two lessons that generalise

**Every time the model was blamed, it was our code.** Vulkan, VRAM, the
driver, "Gemma can't do this" — four times in one night, four times wrong.
When a local model behaves stupidly, suspect the harness first.

**A belief in a comment is not a measurement.** "Small models need direct,
simple commands" was reasonable, load-bearing, guarded by a test, and
backwards. The assertion on `prompt.len() < 200` is what kept it alive.

## Benches left behind

- `core/src/bin/llm_style_matrix.rs` — every style over the same phrases, one
  model per process (`LlamaBackend::init` refuses a second call). Scores
  wrong-language, unchanged and scaffolding leaks rather than taste.
- `core/src/bin/recap_mapreduce.rs` — chunked recap; the shape a product
  implementation should follow.
- `core/src/bin/llm_ctx_sweep.rs` — one context size per process, because an
  abort takes the process with it.

## Still open

- Gemma's silent translation failure needs covering: warn, or pick the model
  from what the user asked for.
- The map-reduce recap is measured but not in the product.
- Duplicate points from overlapping chunks need a dedup pass.
- `core/tests/llm_flows.rs` covers exactly what broke here, is `#[ignore]`,
  and runs in no workflow. A small GGUF plus the four objective measures
  above would have caught all of it.

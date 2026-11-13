# Local LLM quality — what was actually broken

**Status, checked against the code on 2026-11-11:**

- A local recap is ONE generation call over the whole prompt, not a chunked
  one: `ffi.rs::dimmy_llm_call_raw` → `local_llm::process_raw_prompt_local` →
  `llm_cache::generate(model, "", prompt, capped, creative = false, stream = true)`
  (`core/src/local_llm.rs:1662`). Empty system prompt, output capped at 4096
  tokens (`max_tokens.min(4096)`), greedy decoding with a repetition penalty
  of 1.1 over the last 64 tokens, prompt fed in `PROMPT_BATCH = 512`-token
  slices, q8_0 KV cache with flash attention tried first.
- It streams. Each visible piece leaves as an `llm_stream` event
  (`start` / `delta` / `end`, `llm::emit_recap_stream_event`), and both hosts
  render it (`AppViewModel.cs` and `DimmyCore.swift`, `case "llm_stream"`).
  The dictation rewrite (`process_text_local`) passes `stream = false`.
- The map-reduce recap in `core/src/bin/recap_mapreduce.rs` is a bench only.
  Nothing in `core/src/` outside `bin/`, in either host, or in a workflow
  calls it. That does NOT mean a one-pass recap is bad today: the "garbage in
  one pass" result below is one 9099-word meeting on 2026-09-04/05, on a
  2-4B model of that date (the bench header names Gemma 4 E2B), before the
  four changes in the next line. It was not measured again.
- Changed in the recap path since this was written: the spoken language is
  named in the prompt (2026-09-07, see `recap-language-2026-09-07.md`);
  decoding stops when the model starts copying the prompt back (2026-09-15,
  `prompt_echo_start`); reasoning tokens are fed back but hidden (2026-09-30,
  `PieceFilter`, `skip_thinking_suffix`); each model is prompted in its own
  chat format instead of ChatML (2026-10-22, `apply_chat_template` with the
  gguf's template, `render_gemma4_prompt` for Gemma 4).
- The catalogue (`local_llm::AVAILABLE_LLM_MODELS`) lists 18 models: eleven
  Gemma 4 (E2B / E4B / 12B / 26B-A4B QAT, E2B Q4 and Q5, E4B Q3, Q4 and Q8,
  12B Q4 and Q2), TranslateGemma 4B and 12B, Qwen 3 4B, Phi-4 Mini,
  Qwen 3.5 2B and 4B, MiniCPM5 2B. `DEFAULT_LLM_MODEL` is
  `gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf` since 2026-10-23. Qwen 3.5 and MiniCPM5
  entered on 2026-09-30 and are not measured anywhere in this document.

> Measured 2026-09-05 on an NVIDIA T600 Laptop GPU (4 GB VRAM).
> Models: Gemma 4 E2B (Q4_K_M and QAT Q4_K_XL), Phi-4 Mini Q4, Qwen 3 4B Q4.
> Material: real dictations from the user's own history, never invented text.
> Every table below is a measurement from that date, on those models and the
> settings of that day. Read "now" in the text as "after the 2026-09-05 fix".

The local LLM was believed to be "mediocre". It was not mediocre; it was
broken in six places, and every one of them was our code. This is the
record, because the same shape of mistake will happen again.

## The defects

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

After the fix: a real 2-hour transcript at n_ctx 36352 in 229 s (measured
2026-09-05, when the fix set `n_batch` equal to `n_ctx`). Since 2026-09-07
`n_batch` is `PROMPT_BATCH = 512` and the prompt is decoded in 512-token
slices (`local_llm.rs::llm_cache::generate`), which keeps the assert
satisfied with a smaller compute buffer. The 229 s was not measured again.

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

**This landed in the same commit (216210c9, 2026-05-18) that demoted Gemma in
favour of Phi-4 Mini**, for "emoji spam, meta-commentary, hallucinations" —
which is what an insane repetition penalty produces. That verdict was formed
on a broken system and has since been re-taken: Gemma 4 E2B QAT is
`DEFAULT_LLM_MODEL` again since 2026-10-23 (e670adc9).

Today the values are passed by name to upstream llama-cpp-4:
`LlamaSampler::penalties(n_vocab, PENALTY_LAST_N = 64, PENALTY_REPEAT = 1.1,
0.0, 0.0)`. Recap and the fidelity styles decode greedily after it; only
Gen-Z, Emoji, Boomer and Imbruttito sample (top-k 40, top-p 0.9, temp 0.6).

### 3. The style instructions were too short, and never named the language

    // Ultra-short style instructions — small models need direct, simple commands
    LlmStyle::Professional => "Rewrite in formal, professional business tone."

    assert!(prompt.len() < 200, "local prompt must be short for small models")

A belief written into a comment and pinned by a test. Never measured, and
wrong on both counts. 192 trials per configuration — 4 models x 8 styles x 6
real dictations:

| configuration | wrong language | left unchanged |
|---|---:|---:|
| short instructions (shipped until this fix) | 54 / 192 | 21 / 192 |
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

In the code today: `build_local_system_prompt` takes the cloud text from
`LlmStyle::instruction()`, and `anchor_text` appends "Write your entire
answer in <language name>." With no configured language (auto-detect) it
falls back to "Answer in the SAME LANGUAGE as the input text." The quoted
short forms and the `prompt.len() < 200` assertion are gone.

### 4. Translation: three contradictions, found one at a time

The user reported a translation coming back as three lines — the corrected
Italian, a heading the model invented (`*****english translation***`), and then
the English. Three separate causes, each hidden behind the one before it.

**The anchor fought the translate instruction.** "Write your entire answer in
Italian. Then translate the entire output to English." Caught before shipping,
by testing translation rather than assuming a change to the styles stayed in
the styles.

**The style instruction fought it too.** Every cloud style ends by pinning the
language — right until a translation is asked for. `Correct` says "Preserve the
original meaning, intent, and language exactly", and we appended "then translate
to English" to it. The clause is now stripped when translating, by clause and
not by sentence: some of them share a sentence with the task itself ("expand …
while keeping the same meaning and language" would otherwise delete Elaborate
entirely). `no_language_clause_survives_a_translation` walks all eleven styles
and fails if one gets through, which keeps the strip list honest when the
shared cloud text is reworded.

**And the remaining instruction was still two steps.** "Do X. Then translate the
result into English" reads as two actions and got answered with two: the styled
sentence, then a second copy labelled `English:`. The language is now folded
INTO the transformation — "Translate into English while applying this
transformation" — so there is one thing to do.

Both wordings stay in that sentence deliberately, because the models are not
interchangeable about it. Over four target languages:

| phrasing | phi | gemma-QAT | qwen |
|---|---:|---:|---:|
| "translate the entire output to X" | 4/4 | 1/4 | 4/4 |
| "write your entire answer in X" | 1/4 | 4/4 | 4/4 |
| "translate the result into X" | 4/4 | 0/4 | 4/4 |
| both together | 4/4 | 3/4 | 4/4 |

Gemma follows an output-language instruction and shrugs at "translate"; Phi-4
Mini is the exact opposite and echoes the Italian straight back without it.
Neither is worse — they key on different words — so saying both costs a clause
and stops the result depending on which model the user picked.

Also fixed here: translating a language into itself asked for a step that does
not exist, and the model answered by doing the whole job twice. Same source and
target is now a no-op.

### 5. The model said everything twice, and the marker that showed why

Non-translate styles came back doubled or tripled. The generation loop stopped
on `<end_of_turn>` (Gemma), `</s>` (Llama) and `<|endoftext|>` — but not on
`<|im_end|>`, the ChatML pair. Qwen 3 uses ChatML, and at that time we
prompted EVERY model in ChatML (`apply_chat_template` was called with no
template until 2026-10-22), the unsloth QAT ggufs included, so the model
closed its turn, we kept decoding, it opened a new one and said everything
again.

Today `PieceFilter::classify` stops on nine markers (`<|end|>`, `<turn|>`,
`<|turn>`, `<end_of_turn>`, `<start_of_turn>`, `</s>`, `<|endoftext|>`,
`<|im_end|>`, `<|im_start|>`), and each model is prompted in its own format:
the gguf's template through `apply_chat_template`, `render_gemma4_prompt`
for Gemma 4, ChatML only as the fallback.

The giveaway was a stray `</|im_end|>` reaching a user's text: the strip regex
expected the slash AFTER the pipe and never matched this spelling. Stripping it
afterwards had been hiding the cause while keeping the damage.

### 6. A full GPU disabled the feature in silence

Reported from a staging install: whisper transcribed, the recap worked, LLM
enhancement did nothing, a red pill for two seconds, no notification.

    [ggml ERROR] failed to allocate Vulkan1 buffer of size 316407808
    ERROR: Local LLM failed: context creation failed

Whisper is evicted when the LLM model is LOADED, but a cached model skips that
path and whisper reloads whenever STT runs:

    17:41  recap      loads Qwen, evicts whisper, works
    17:43  dictation  "Using cached LLM model" -> whisper is back -> 316 MB
                      will not fit -> fail

Order-dependent, not configuration-dependent: the same machine never hit it
under C:\ because the model was changed between tries, and changing model takes
the load path. Context creation now retries after evicting whisper (and
Qwen3-ASR): the same q8_0-KV context a second time, then a plain f16 context
(`local_llm.rs::llm_cache::generate`).

The silence was the worse half. The failure emitted a message-only `error`
event, which flashes the pill and vanishes — the exact shape that left four Groq
403s unexplained in July and the 413s in August. Both cloud legs were given a
structured `source` after those; the LOCAL leg was not, so it repeated it a
third time. And the pill was erased by its own success: an LLM failure degrades
gracefully and still returns the raw transcript, so the paste succeeded and the
checkmark overwrote the red before the error timer's five seconds could run.

The local leg now sends the structured event too:
`error_event_payload("llm", …, "local", category)` with category
`out_of_memory`, `model_missing` or `local_model`
(`ffi.rs::dimmy_process_with_llm`).

## Where each model actually stands

Measured 2026-09-05, with every model still prompted in ChatML (the wrong
format for Gemma 4 and Phi-4, fixed 2026-10-22) and before Qwen 3.5,
MiniCPM5 and TranslateGemma were in the catalogue. Not a ranking of today's
catalogue. After all four fixes, 72 trials each (nine fresh dictations x
eight styles):

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
warning. On 2026-11-11 the code still has no warning and no automatic model
switch for this. What changed: TranslateGemma 4B and 12B are in the
catalogue as the translation models, and Gemma 4 is now prompted in its own
format. Gemma 4's translation was not measured again after that.

The QAT models earn their download: the E2B QAT used 1224 MiB of VRAM against
the plain Q4's 1408 while extracting decisions the plain quantisation missed
entirely (23 against 2 on one meeting, though with repeats a dedup pass
should collapse).

## Recap quality — technique beats model

**A measurement from 2026-09-04/05, not a description of today's recap.** The
product still recaps in one pass (see Status at the top), but with the
language, echo-stop, reasoning and chat-format changes made after this
date, and with models (Qwen 3.5) that did not exist in the catalogue then.
Nobody repeated this comparison on the current code.

On that date, one pass over a 9099-word meeting produced garbage on a 2-4B
model (the bench header names Gemma 4 E2B): mangled markers, bullet lists
collapsed onto one line, and on one run a drift into Spanish. The same model on an 1800-word slice was
clean. The failure was LENGTH, which is what the literature reports (smaller
chunks beat full context; "lost in the middle"; length-induced degradation
dominating the error for long inputs).

Same meeting, same date:

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

`core/src/bin/recap_mapreduce.rs` implements this. It is NOT in the product
(still true on 2026-11-11: `process_raw_prompt_local` makes one
`llm_cache::generate` call per recap, and no product code calls the bench).

## A clause that was measured and thrown away

Verbosity on short dictations was real — a 17-word input coming back as 150
words. The obvious remedy, telling the model to stay close to the input and
invent nothing, was written, measured and removed the same day:

| style | without | with |
|---|---:|---:|
| Correct | x1.1 | x0.9 |
| Comprehensible | x1.4 | x1.2 |
| Professional | x1.4 | x1.2 |
| Imbruttito | x3.2 | x2.9 |

It shaved 10-15%, left the worst offender essentially untouched, and raised the
count of outputs the model declined to change at all (Comprehensible 2 of 6 to
4 of 6). Trading "says too much" for "does nothing" is not a trade.

The verbosity comes from the style instructions themselves: Acronyms hands the
model twenty acronyms to use, Imbruttito a page of anglicisms. A 4B model reads
a list and tries to use all of it. Trimming those is the fix, they are shared
with the cloud path, and it needs its own measured pass.

The first measurement of that clause was worthless and nearly shipped it: the
sample set was 25-90 word phrases, and the rambling only appears on the 13-23
word ones. Measuring where it does not break proves nothing.

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

Checked again on 2026-11-11; all four are still open in the code.

- Gemma's silent translation failure needs covering: warn, or pick the model
  from what the user asked for.
- The map-reduce recap is measured (once, 2026-09-04/05) but not in the
  product. The one-pass recap the product does run was not measured again
  after the later fixes.
- Duplicate points from overlapping chunks need a dedup pass (in the bench).
- `core/tests/llm_flows.rs` covers exactly what broke here, is `#[ignore]`,
  and runs in no workflow. A small GGUF plus the four objective measures
  above would have caught all of it. The same holds for
  `core/tests/local_llm_thinking.rs` (added 2026-09-30, `#[ignore]`). Only
  `core/tests/llm_request_shape.rs` runs in CI (`e2e-tests.yml`), and it
  checks cloud request bodies, not local output.

# Why the recap now names the language — measured 2026-09-07

Read this before changing anything about the recap's output language, the
whisper language-ID path, or the local LLM's context sizing. Everything below
was measured on real meetings from a real install; the numbers are the reason
the code looks the way it does.

## The defect

The recap prompt asked for the output language in words:

> Auto-detect from the transcript. For mixed languages, pick the dominant one.
> Do NOT translate. If the transcript is in Italian, write the recap in Italian.

A frontier cloud model obeys that. **A 4 B local model does not.** On an
Italian meeting (`Pianificazione strategia prodotto rollout`, 888 s, Italian
transcript) Gemma 4 E2B produced an English recap under an Italian title, and
Qwen 3 4B did the same on a separate Italian transcript.

The Rust core made it worse by omission: `process_raw_prompt_local` calls the
model with an **empty system prompt**, so the only mention of language was the
one the model was ignoring. Dictation has had a language anchor since
`local_llm.rs::anchor_text`; the recap never got one.

## What was tried, and what each attempt showed

Same transcript, same model (Qwen 3 4B Q4), same token budget. Only the
instruction moved.

| Instruction | Where | Output language | Section markers |
|---|---|---|---|
| none (today) | — | **English** | English |
| "Answer in the SAME LANGUAGE as the input text." | system | **English** | English |
| "Write your entire answer in Italian." | system | Italian | English |
| "Write your entire answer in Italian." | user prompt | Italian | **`===DECISIONI===`** |
| ditto + "keep the ===NAME=== markers in English" | user prompt | Italian | English |

Two conclusions, both load-bearing:

1. **The self-referential form does not work on a small model.** Only naming
   the language does. This matches the note already in `anchor_text`: the
   fallback wording took wrong-language answers from 54 to 32, not to zero.
2. **Naming the language makes the model translate the section markers**,
   which are identifiers the parser matches exactly — a translated marker
   silently costs the user a whole section. The sentence protecting them was
   already in `mcp-server/templates/recap.md` and missing from both host
   templates. It is now in all three.

⚠️ The marker clause **helps but does not guarantee**: on a longer transcript
Qwen translated `===DECISIONS===` anyway, with the clause present. Two
observations, one each way — the difference may be length or may be chance.
If a section goes missing from a local recap, this is the first suspect, and
the untested option is moving the anchor into the system role, where markers
survived in the one measurement we have.

## Where the language comes from

Not from the settings combo. That says "I speak Italian"; it does not say
"this meeting was in Italian", and nobody changes it before a call with a
foreign client.

Whisper knows: its decoder emits a language token before transcribing, and
`whisper_lang_auto_detect` reads that out as a probability vector without
decoding a word. `core/src/lang_detect.rs` samples five 30 s windows spread
across the recording and takes a majority.

**10 meetings, 10 correct, every window unanimous.** Four English, six
Italian. Confidence 0.99-1.00 with `large-v3-turbo`; `tiny` and `base` gave
the same verdicts at 0.66-1.00 — lower margins, never a wrong vote.

| model | 5 windows | verdicts |
|---|---|---|
| large-v3-turbo (834 MB) | 60 s | 10/10 |
| base (78 MB) | 4 s | identical |
| tiny (42 MB) | 3 s | identical |

Why several windows: one window on a real English meeting scored 0.61 and
another 0.66 — right, but not by much. More importantly, whisper's own
detection looks only at the FIRST 30 s, which in our recordings is the consent
announcement plus "hi, how are you". The first 20 s are skipped for the same
reason.

Cost is independent of duration (3 s on a 2-minute meeting and on a
34-minute one) because the mel is computed per window, not over the whole
file. Computing it over the whole recording per window cost 155 s — that was
the probe being wasteful, not the method.

## The 2026-07-29 note was half wrong

`project_stt_language_autodetect_2026_07_29` concluded local detection was
unusable: *"clear English audio -> `it` at 99.8%, every window, every offset"*.
That conclusion blocked this whole approach, and it does not reproduce.

Running whisper's own auto mode (`whisper_full` with `detect_language`, read
back via `whisper_full_lang_id`) and the dedicated call **on the same 30 s
slices**, both models, four meetings: **24 windows out of 24 agree with each
other and with reality.**

What auto mode really does is return **zero segments** — the transcription
comes back empty, which `local_stt.rs:979` already documented, noting the
detection itself was confident and correct (p=0.97 Italian). The July
experiment appears to have read the language through that broken path.

Consequence: "detect-then-force", rejected in July as costing 2x, costs 3 s.
It is now the sound way to pick a language, and the same lever is available
for the transcription itself — see below.

## Two memory fixes that fell out of this

Both were found while making Qwen 3 4B usable on a 4 GB card, and both are
about us asking for too much rather than the GPU being too small:

- **The compute buffer.** `n_batch` was set to the whole context to satisfy
  llama.cpp's `n_tokens_all <= n_batch` GGML_ASSERT (which kills the process
  rather than returning an error). The prompt is now fed in 512-token slices,
  which satisfies the assert by construction and sizes the buffer for a slice.
- **The KV cache.** Qwen 3 4B asked for **1008 MiB** of KV on a 15-minute
  meeting — more than Gemma 4 E2B needs for everything — and the context
  failed while the weights had loaded fine. Stored as `q8_0` it is 574 MiB and
  the model runs. Quantised V needs flash attention, so the two are set
  together, with a fallback to the plain f16 context for backends that refuse.

## Deliberately not done

- **Picking the STT language from the same detection.** It would fix the
  transcript at the source instead of the recap after the fact, and two
  meetings in this corpus are English speech transcribed into Italian because
  the pin was wrong. But it belongs in `meeting.rs`, on the transcription
  thread and never the capture loop, with a decision about the first minutes
  already transcribed. Separate work.
- **A language selector in the UI.** The whole point is that nobody wants to
  set it per meeting.
- **Detection for cloud recap models.** They already get the language right;
  1.4 s for nothing.

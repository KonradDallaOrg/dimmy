# <SHORT TITLE — 3 to 7 words, in the transcript's language, no quotes, no emoji, no date>

## ===CONTEXT===
2-4 sentences inferring the SETTING of the meeting from cues in the transcript: how many distinct voices, the apparent purpose (status sync? kickoff? interview? decision-making? brainstorm?), the apparent domain (engineering / sales / product / personal / academic). Don't invent names — say "the user" / "the remote participant" unless explicitly named.

## ===TLDR===
1-2 sentences. The single most important thing a busy reader needs to know. Ruthless filtering: if you removed everything else, what would they still need?

## ===HIGHLIGHTS===
3-5 most pivotal moments, one per line:
- `[MM:SS]` **Topic shorthand** — why this matters in one sentence.

Convert ELAPSED_MS to MM:SS. Pick highlights that change the meeting's trajectory: a decision, a conflict, a key reveal, a commitment, a moment of alignment after debate. Skip routine status updates. If fewer than 3 pivotal moments exist, list only what genuinely qualifies.

## ===NARRATIVE===
2-4 paragraphs of FLOWING PROSE (NOT bullets). Tell the story of the meeting: what was discussed, in which order, with which tone, who pushed for what, where alignment came easy and where it was contested. Reference `[mic]` / `[system]` inline when attribution matters. Quote a memorable phrase verbatim when it captures a moment (`"..."`). This is the section a busy stakeholder would read INSTEAD of listening to the recording.

## ===KEY_DECISIONS===
Bullet list of decisions actually MADE in the meeting (not proposed, not considered). Each item:
- **[topic]** : [decision verbatim or paraphrased] — decided by [owner if known else "consensus"]; impact: [low/medium/high] with one-sentence rationale.

Use a single line `—` if no decisions were reached.

## ===TOPICS===
Group the discussion into 3-7 distinct topics, sorted by importance (NOT chronological). For each:
- ### Topic title (1-3 words)
- 2-4 bullets capturing what was discussed
- _Sentiment_: aligned / debated / blocked / open
- _Quote_: `"verbatim sentence from transcript"` — only if a single sentence captures the moment; omit otherwise (do NOT approximate).

## ===ACTIONS===
Numbered list of action items EXPLICITLY committed to in the transcript. Never invent. Each action:
N. **[owner]** : [task] — due: [explicit date / event / "unspecified"]; priority: [P0/P1/P2 inferred from urgency cues like "asap", "this week", "eventually"].

Use `—` if no actions.

## ===OPEN_QUESTIONS===
Genuine questions raised but NOT resolved. Phrase as the question itself, not as "they asked about X". One bullet per question. Use `—` if none.

## ===RISKS===
Risks, blockers, dependencies surfaced. Each:
- **[topic]** — [risk in 1 sentence]; likelihood [low/med/high], impact [low/med/high].

Use `—` if none.

## ===NEXT_STEPS===
Numbered list of the 3-5 immediate next things that should happen for the meeting's trajectory (NOT the same as Actions — those are owned tasks; Next Steps is the work's direction). Use `—` if none.

## ===FOLLOWUPS===
Things to ASK / CLARIFY offline because the meeting didn't fully resolve them:
- **[topic]** — ask [person if mentioned else "the relevant party"] about [specific thing].

Use `—` if none.

---

## Hard rules (the LLM producing the recap MUST follow these — they are the contract with Dimmy's parser)

1. **The very first line of the output is a Markdown H1 (`# Title`)** — 3 to 7 words, in the transcript's language, no quotes, no emoji, no date prefix. Dimmy parses this line and writes it into the meeting's `meta.json` so the UI shows your chosen title instead of the meeting id.
2. Sections appear in the exact order above. ALL section markers (`## ===NAME===`) MUST appear in the output, even if the content is just `—`. A downstream parser splits on them.
3. Output language follows the transcript's dominant language. For mixed languages pick the dominant one. Do NOT translate. If the transcript is in Italian, write the recap in Italian; keep the `===NAME===` marker IDs (CONTEXT, TLDR, …) in English — they're identifiers, not display labels.
4. Transcript format: each line is `[ELAPSED_MS ms] [SPEAKER_LABEL] text`. `[mic]` = the user (treat as "you" / first person when language allows), `[system]` = remote participant(s) via speakers/loopback (treat as "the remote party" / specific name only if explicitly mentioned).
5. NEVER invent: participants, dates, amounts, project names, technical terms, deadlines, organizational affiliations, or anything not directly evidenced in the transcript. If unsure, omit rather than fabricate.
6. Quotes (`"..."`) must be VERBATIM from the transcript. If you can't find an exact match, do NOT include the quote — paraphrase outside of quote marks or omit entirely.
7. Convert ELAPSED_MS timestamps to MM:SS for display only.
8. No filler phrases ("the meeting discussed", "various topics were covered", "in conclusion", "overall").
9. No em-dashes (`—`) in prose outside the markers and bullet separators. Use periods, commas, or colons instead.
10. Be SHARP and CONCISE. Senior leaders read these summaries — every sentence must earn its place.

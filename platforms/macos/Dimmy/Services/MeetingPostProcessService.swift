import Foundation

// MARK: - MeetingPostProcessService
//
// Shared meeting recap pipeline. Used by both:
//   - MeetingViewModel.stopAndProcess (Stop button inside the meeting
//     window)
//   - PillWindow Stop routing (when the user closes the meeting window
//     and stops via the pill — the meeting window may not be alive to
//     run the recap)
//
// Mirrors the Win-side `Services/MeetingPostProcessService.cs`. Keeps
// the prompt + parser + persistence in one place so every entry point
// produces the same recap shape.
//
// The structured-recap prompt is ported VERBATIM from
// `MeetingWindow.xaml.cs::BuildStructuredRecapPrompt` (Win). Do NOT
// rewrite or "tidy up" the wording — the parser keys off the exact
// `===KEY===` markers and the model is sensitive to the framing.
// The prompt body lives below.

enum MeetingPostProcessService {
    // MARK: - Public entry point

    struct Result {
        let recapMarkdown: String  // full markdown for `recap.md`
        let actionsPlain: String   // plain-text fallback for the `actions` field
        let sections: [String: String]  // raw section dict for UI rendering
    }

    enum Failure: Error, CustomStringConvertible {
        case emptyTranscript
        case llm(DimmyCore.LlmRawError)
        case unknown(String)

        var description: String {
            switch self {
            case .emptyTranscript: return "Empty transcript — nothing to summarise"
            case .llm(let e):
                // Translate FFI rc → user-actionable text. The raw enum
                // values land in Settings hints (subStatusLabel) so the
                // user knows where to fix it.
                switch e {
                case .notConfigured:
                    return "No LLM provider configured. Open Settings → LLM and add a key (Anthropic, OpenAI, or Google) before generating a recap."
                case .httpError:
                    return "LLM request failed — check your network and API key, then click Regenerate. Details in Console.app under \"dimmy\"."
                case .emptyPrompt, .invalidArgs:
                    return "Internal error preparing the LLM call. Click Regenerate; if it persists, please report this."
                case .notInitialized:
                    return "Dimmy core isn't ready yet — wait a moment then click Regenerate."
                case .unknown(let code):
                    return "LLM call failed (rc=\(code)). See Console.app under \"dimmy\"."
                }
            case .unknown(let s): return s
            }
        }
    }

    /// Run the full recap pipeline:
    ///   1. Build the Notion-style structured prompt from `transcript`.
    ///   2. Call `dimmy_llm_call_raw` with the user-picked recap model
    ///      (or auto-detected from the configured LLM URL).
    ///   3. Parse the response into the section dictionary.
    ///   4. Build the markdown body for `recap.md`.
    ///   5. Persist `recap.md` + `actions` (plain fallback) via
    ///      `dimmy_meeting_save_post_process(dir, recap, actions, nil)`.
    ///
    /// Blocking — call from a background thread. The LLM call can take
    /// 10–60 s for reasoning-tier models; the FFI side bumps the
    /// timeout to 600 s for Anthropic adaptive thinking.
    static func runRecap(dir: String,
                         transcript: String,
                         modelOverride: String? = nil) -> Swift.Result<Result, Failure> {
        let trimmed = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .failure(.emptyTranscript) }

        let prompt = buildStructuredRecapPrompt(transcript: trimmed)
        let model = (modelOverride?.isEmpty == false) ? modelOverride! : pickRecapModel()
        // 32K tokens — same ceiling Win uses to give Opus 4.7 / Gemini
        // 3.1 Pro headroom for adaptive-thinking budgets. The provider
        // dispatch in core/src/llm.rs auto-picks the right thinking
        // shape based on the model id.
        let llmResult = DimmyCore.shared.llmCallRaw(
            prompt: prompt,
            modelOverride: model,
            maxTokens: 32_768
        )
        switch llmResult {
        case .failure(let err):
            return .failure(.llm(err))
        case .success(let raw):
            let sections = parseStructuredRecap(raw)
            let markdown = buildMarkdownFromSections(sections)
            let actions = sections["ACTIONS"] ?? ""
            DimmyCore.shared.meetingSavePostProcess(
                dir: dir,
                recap: markdown,
                actions: actions
            )
            return .success(Result(
                recapMarkdown: markdown,
                actionsPlain: actions,
                sections: sections
            ))
        }
    }

    // MARK: - Prompt (Win parity — keep verbatim)

    static let sectionKeys: [String] = [
        "CONTEXT",
        "TLDR",
        "HIGHLIGHTS",
        "NARRATIVE",
        "KEY_DECISIONS",
        "TOPICS",
        "ACTIONS",
        "OPEN_QUESTIONS",
        "RISKS",
        "NEXT_STEPS",
        "FOLLOWUPS",
    ]

    static func buildStructuredRecapPrompt(transcript: String) -> String {
        // Verbatim port of MeetingWindow.xaml.cs::BuildStructuredRecapPrompt.
        // Notion-style recap targeting reasoning-tier models (Opus 4.7
        // adaptive thinking, Gemini 3.1 Pro thinkingLevel=high, GPT-5).
        // Leading spaces matter for the parser ===KEY=== markers — do
        // not reflow.
        return """
        You are a senior meeting analyst writing a polished, Notion-style summary of an audio recording. Output ONLY markdown with the EXACT marker headings shown — a downstream parser splits on them.

        ## Transcript format
        Each line: `[ELAPSED_MS ms] [SPEAKER_LABEL] text`.
        Speaker labels: `[mic]` = the user recording (treat as "you" / first person when the language allows), `[system]` = remote participant(s) coming through speakers/loopback (treat as "the remote party" / "interlocutor" / specific name only if explicitly mentioned in the transcript). When only `[mic]` is present, the recording is monologue / dictation; when only `[system]` is present, the user was a silent listener.

        ## Output language
        Auto-detect from the transcript. For mixed languages, pick the dominant one. Do NOT translate. If the transcript is in Italian, write the recap in Italian.

        ## Sections (emit ALL of them, in this order)

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

        ## Hard rules
        - Output the sections in the exact order above. ALL section markers must appear, even if the section content is just `—`.
        - Output language follows the transcript dominant language.
        - NEVER invent: participants, dates, amounts, project names, technical terms, deadlines, organizational affiliations, or anything not directly evidenced in the transcript. If unsure, omit rather than fabricate.
        - Quotes (`"..."`) must be VERBATIM from the transcript. If you can't find an exact match, do NOT include the quote — paraphrase outside of quote marks or omit entirely.
        - Convert ELAPSED_MS timestamps to MM:SS for display only. Use the original [N ms] only for internal reasoning if needed.
        - No filler phrases ("the meeting discussed", "various topics were covered", "in conclusion", "overall").
        - No em-dashes (`—`) in prose outside the markers and bullet separators. Use periods, commas, or colons instead.
        - Be SHARP and CONCISE. Senior leaders read these summaries — every sentence must earn its place.

        ## Transcript
        \(transcript)
        """
    }

    // MARK: - Parser

    /// Split the LLM output into sections keyed by the marker name.
    /// Falls back to `{TLDR: <whole response>}` if no markers were
    /// emitted (older models, off-prompt outputs).
    static func parseStructuredRecap(_ raw: String) -> [String: String] {
        // ===KEY=== markers are ASCII, so character-distance is safe
        // (no multi-byte ambiguity inside the marker itself).
        var hits: [(Range<String.Index>, String)] = []
        for key in sectionKeys {
            let marker = "===\(key)==="
            if let r = raw.range(of: marker, options: .caseInsensitive) {
                hits.append((r, key))
            }
        }
        if hits.isEmpty {
            return ["TLDR": raw.trimmingCharacters(in: .whitespacesAndNewlines)]
        }
        hits.sort { $0.0.lowerBound < $1.0.lowerBound }
        var result: [String: String] = [:]
        for i in 0..<hits.count {
            let contentStart = hits[i].0.upperBound
            let contentEnd = i + 1 < hits.count ? hits[i + 1].0.lowerBound : raw.endIndex
            var content = String(raw[contentStart..<contentEnd])
                .trimmingCharacters(in: .whitespacesAndNewlines)
            content = content.trimmingCharacters(in: CharacterSet(charactersIn: "# \n\r"))
            result[hits[i].1] = content
        }
        return result
    }

    // MARK: - Markdown builder

    /// Reverse of parseStructuredRecap: stitch the section dict back
    /// into a single markdown body suitable for `recap.md`. Used for
    /// persistence + clipboard copy.
    static func buildMarkdownFromSections(_ s: [String: String]) -> String {
        let titles: [(String, String)] = [
            ("CONTEXT", "Context"),
            ("TLDR", "TL;DR"),
            ("HIGHLIGHTS", "Highlights"),
            ("NARRATIVE", "Narrative"),
            ("KEY_DECISIONS", "Key decisions"),
            ("TOPICS", "Topics"),
            ("ACTIONS", "Actions"),
            ("OPEN_QUESTIONS", "Open questions"),
            ("RISKS", "Risks"),
            ("NEXT_STEPS", "Next steps"),
            ("FOLLOWUPS", "Follow-ups"),
        ]
        var out = ""
        for (key, title) in titles {
            guard let body = s[key], !body.isEmpty else { continue }
            out += "## \(title)\n\n\(body)\n\n"
        }
        return out.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Inverse of buildMarkdownFromSections — split a persisted
    /// recap.md back into the section dict for re-display when the
    /// user re-opens a past meeting from the sidebar.
    static func parseMarkdownIntoSections(_ markdown: String) -> [String: String] {
        let titles: [(String, String)] = [
            ("Context", "CONTEXT"),
            ("TL;DR", "TLDR"),
            ("Highlights", "HIGHLIGHTS"),
            ("Narrative", "NARRATIVE"),
            ("Key decisions", "KEY_DECISIONS"),
            ("Topics", "TOPICS"),
            ("Actions", "ACTIONS"),
            ("Open questions", "OPEN_QUESTIONS"),
            ("Risks", "RISKS"),
            ("Next steps", "NEXT_STEPS"),
            ("Follow-ups", "FOLLOWUPS"),
        ]
        var indices: [(Int, String)] = []
        for (display, key) in titles {
            let header = "## \(display)"
            if let r = markdown.range(of: header) {
                let intIdx = markdown.distance(from: markdown.startIndex, to: r.lowerBound)
                indices.append((intIdx, key))
            }
        }
        if indices.isEmpty {
            return ["TLDR": markdown.trimmingCharacters(in: .whitespacesAndNewlines)]
        }
        indices.sort { $0.0 < $1.0 }
        var result: [String: String] = [:]
        for (i, (start, key)) in indices.enumerated() {
            let lo = markdown.index(markdown.startIndex, offsetBy: start)
            let endOfHeader = markdown[lo...].firstIndex(of: "\n") ?? markdown.endIndex
            let contentStart = markdown.index(after: endOfHeader)
            let contentEnd = i + 1 < indices.count
                ? markdown.index(markdown.startIndex, offsetBy: indices[i + 1].0)
                : markdown.endIndex
            let body = String(markdown[contentStart..<contentEnd])
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !body.isEmpty { result[key] = body }
        }
        return result
    }
}

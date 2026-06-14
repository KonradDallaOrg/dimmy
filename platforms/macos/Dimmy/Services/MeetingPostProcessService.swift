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
                    return "No LLM provider configured. Open Settings → LLM and add a key (Anthropic, OpenAI, or Google), OR switch to Local mode + download a Gemma model."
                case .httpError:
                    return "LLM request failed — check your network and API key, then click Regenerate. Details in Console.app under \"dimmy\"."
                case .emptyPrompt, .invalidArgs:
                    return "Internal error preparing the LLM call. Click Regenerate; if it persists, please report this."
                case .notInitialized:
                    return "Dimmy core isn't ready yet — wait a moment then click Regenerate."
                case .localModelMissing:
                    return "Local LLM model not on disk. Open Settings → LLM → download a Gemma variant (E2B Q4 = fast, E4B Q4 = best balance) before generating a recap."
                case .modelNotSupported:
                    // -5 from dimmy_llm_call_raw: 404 because the
                    // picked model id is not valid for the recap
                    // endpoint's vendor (e.g. gpt-5 vs api.anthropic.com).
                    return "Recap model is not supported by the recap endpoint. Open Settings → Recap and pick a model that matches the endpoint vendor."
                case .unauthorized:
                    // -6: 401/403 — the recap-side key is missing
                    // or unauthorized for the override URL.
                    return "Recap API key is missing or unauthorized. Open Settings → Recap to fix it."
                case .rateLimited:
                    // -7: 429 — too many requests this minute.
                    return "Recap rate limited (429). Try again in a minute, or pick a faster model in Settings → Recap."
                case .networkError:
                    // -8: socket / DNS / TLS error reaching the
                    // recap endpoint. Network problem, not auth.
                    return "Network error reaching the recap endpoint. Check your connection and try again."
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
                         modelOverride: String? = nil,
                         notionAutoSend: Bool = false,
                         meetingType: String = "") -> Swift.Result<Result, Failure> {
        let trimmed = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .failure(.emptyTranscript) }

        // Read the user's notes.md (the live + Done tabs share this
        // single file) and fold them into the prompt as HIGH PRIORITY
        // emphasis. Missing file → empty string → no notes section.
        let notesURL = URL(fileURLWithPath: dir).appendingPathComponent("notes.md")
        let notes = (try? String(contentsOf: notesURL, encoding: .utf8)) ?? ""
        let prompt = buildStructuredRecapPrompt(transcript: trimmed, notes: notes, meetingType: meetingType)
        let model = (modelOverride?.isEmpty == false) ? modelOverride! : pickRecapModel()
        // 32K tokens — same ceiling Win uses to give Opus 4.7 / Gemini
        // 3.1 Pro headroom for adaptive-thinking budgets. The provider
        // dispatch in core/src/llm.rs auto-picks the right thinking
        // shape based on the model id.
        let started = Date()
        let llmResult = DimmyCore.shared.llmCallRaw(
            prompt: prompt,
            modelOverride: model,
            maxTokens: 32_768
        )
        let elapsedMs = Int64(Date().timeIntervalSince(started) * 1000)
        let providerLabel = TelemetryBuckets.provider(currentLlmApiUrl())
        let modelLabel = TelemetryBuckets.recapModel(model)
        let processingLabel = TelemetryBuckets.processingMs(elapsedMs)
        switch llmResult {
        case .failure(let err):
            DimmyCore.shared.trackEvent("meeting.recap_completed", [
                "provider": providerLabel,
                "recap_model_bucket": modelLabel,
                "processing_ms_bucket": processingLabel,
                "success": false,
            ])
            return .failure(.llm(err))
        case .success(let raw):
            DimmyCore.shared.trackEvent("meeting.recap_completed", [
                "provider": providerLabel,
                "recap_model_bucket": modelLabel,
                "processing_ms_bucket": processingLabel,
                "success": true,
            ])
            let sections = parseStructuredRecap(raw)
            let markdown = buildMarkdownFromSections(sections)
            let actions = sections["ACTIONS"] ?? ""
            DimmyCore.shared.meetingSavePostProcess(
                dir: dir,
                recap: markdown,
                actions: actions
            )
            // Best-effort copy into the user's export folder (Obsidian /
            // Drive / Dropbox sync). No-op when unconfigured; never throws.
            tryExportRecap(dir: dir, title: sections["__TITLE__"])
            // Auto-send to Notion if user has token + auto-send on.
            // Best-effort: failure is logged but does not block the
            // recap pipeline. The user can always retry manually from
            // the Done view's "Send to Notion" button.
            if notionAutoSend,
               DimmyCore.shared.notionHasToken {
                var notionOk = false
                if let json = DimmyCore.shared.notionSendRecap(meetingDir: dir),
                   let data = json.data(using: .utf8),
                   let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   (dict["ok"] as? Bool) == true {
                    notionOk = true
                    NSLog("[Dimmy] Notion auto-send ok url=\(dict["page_url"] as? String ?? "")")
                } else {
                    NSLog("[Dimmy] Notion auto-send failed (silent fallback to manual button)")
                }
                DimmyCore.shared.trackEvent("notion.recap_sent", [
                    "auto": true,
                    "ok": notionOk,
                ])
            }
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

    // MARK: - Meeting type (Auto + override + __TYPE__ chip)
    //
    // A meeting type NEVER changes the 11-section contract — it only
    // shifts emphasis via a hint injected into the prompt (like the
    // listener's notes). `key` is persisted in the invisible
    // `<!-- dimmy-type: KEY -->` line of recap.md; `label` is the chip
    // text; `guidance` is the emphasis hint. "auto" = the model
    // classifies + emits the detected key; "general" = neutral fallback.
    // Keep in sync with the Win copy (MeetingRecapHelpers.MeetingTypes).

    struct MeetingTypeInfo { let key: String; let label: String; let guidance: String }

    static let meetingTypes: [MeetingTypeInfo] = [
        MeetingTypeInfo(key: "auto", label: "Auto-detect", guidance: ""),
        MeetingTypeInfo(key: "one_on_one", label: "1:1",
            guidance: "A 1:1 between two people. Emphasize feedback exchanged, career/growth topics, sentiment and rapport, and personal commitments. ACTIONS and FOLLOWUPS matter most; KEY_DECISIONS are often light."),
        MeetingTypeInfo(key: "standup", label: "Standup / status",
            guidance: "A short status sync. In NARRATIVE and TOPICS organize by person or workstream: what is done, in progress, and blocked. Surface blockers prominently in RISKS. Keep it brief."),
        MeetingTypeInfo(key: "planning", label: "Planning",
            guidance: "A planning, sprint or roadmap session. Emphasize goals, scoped work, owners, estimates and deadlines under ACTIONS and NEXT_STEPS; capture scope decisions in KEY_DECISIONS and dependencies in RISKS."),
        MeetingTypeInfo(key: "technical", label: "Technical / design review",
            guidance: "A technical or design review. Emphasize the options and approaches considered with their tradeoffs in TOPICS, the chosen direction in KEY_DECISIONS, unresolved technical questions in OPEN_QUESTIONS, and risks or dependencies in RISKS."),
        MeetingTypeInfo(key: "brainstorm", label: "Brainstorming",
            guidance: "A brainstorming or ideation session. In TOPICS group ideas by theme; in HIGHLIGHTS surface the most promising directions. Expect KEY_DECISIONS to be light or empty (this is divergent thinking, not decisions). Capture parked ideas in FOLLOWUPS."),
        MeetingTypeInfo(key: "interview", label: "Interview (hiring)",
            guidance: "A hiring or candidate interview. Stay factual and evidence-based. In TOPICS capture the areas probed and what the candidate demonstrated; surface strengths in HIGHLIGHTS and concerns in RISKS. Do NOT invent a hire / no-hire verdict unless it was explicitly stated."),
        MeetingTypeInfo(key: "customer", label: "Customer call",
            guidance: "A customer-facing call (sales, discovery, or user research). Emphasize the customer's needs, pain points and goals, objections or concerns, and any commitments. Capture feature requests and insights in TOPICS; relationship or deal next steps in NEXT_STEPS."),
        MeetingTypeInfo(key: "lecture", label: "Lecture / talk",
            guidance: "A talk, lecture, webinar or presentation the user is mostly listening to. Emphasize the key concepts and structured takeaways in TOPICS and HIGHLIGHTS, and write a teaching-quality NARRATIVE. ACTIONS and KEY_DECISIONS are usually empty (this is learning, not a working meeting)."),
        MeetingTypeInfo(key: "general", label: "General", guidance: ""),
    ]

    static let typeTagPrefix = "<!-- dimmy-type:"

    // MARK: - Recap export folder (Obsidian / Drive / Dropbox sync)

    /// UserDefaults key for the recap export folder. Empty = disabled.
    /// Win parity: UiPreferences.RecapExportFolder.
    static let recapExportFolderKey = "recapExportFolder"

    /// Turn a meeting title into a safe markdown filename STEM (no extension).
    /// Replaces filesystem-illegal chars (`\ / : * ? " < > |`) and control
    /// chars with a space, collapses whitespace runs, trims trailing
    /// dots/spaces, caps length at 120. Empty / all-illegal → "meeting".
    /// Mirror of Win MeetingRecapHelpers.SanitizeRecapFileName.
    static func sanitizeRecapFileName(_ title: String?) -> String {
        let trimmed = (title ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return "meeting" }
        let illegal: Set<Character> = ["\\", "/", ":", "*", "?", "\"", "<", ">", "|"]
        var out = ""
        for ch in trimmed {
            if illegal.contains(ch) || ch.isNewline
                || (ch.asciiValue != nil && ch.asciiValue! < 0x20) {
                out.append(" ")
            } else {
                out.append(ch)
            }
        }
        out = out.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        while let last = out.last, last == "." || last == " " { out.removeLast() }
        if out.isEmpty { return "meeting" }
        if out.count > 120 {
            out = String(out.prefix(120))
            while let last = out.last, last == "." || last == " " { out.removeLast() }
        }
        return out.isEmpty ? "meeting" : out
    }

    /// Copy `<dir>/recap.md` into the configured export folder, named from the
    /// meeting title as `<stem> (<meeting-id>).md`. The meeting-id suffix lets
    /// a REGENERATE of the same meeting overwrite its own file while never
    /// clobbering a different meeting with the same title. No-op + never
    /// throws when unset / missing / unwritable.
    static func tryExportRecap(dir: String, title: String?) {
        guard !dir.isEmpty else { return }
        let folder = (UserDefaults.standard.string(forKey: recapExportFolderKey) ?? "")
            .trimmingCharacters(in: .whitespaces)
        guard !folder.isEmpty else { return }
        let recapURL = URL(fileURLWithPath: dir).appendingPathComponent("recap.md")
        guard FileManager.default.fileExists(atPath: recapURL.path) else { return }
        let folderURL = URL(fileURLWithPath: folder)
        do {
            try FileManager.default.createDirectory(
                at: folderURL, withIntermediateDirectories: true)
            let stem = sanitizeRecapFileName(title)
            let meetingId = URL(fileURLWithPath: dir).lastPathComponent
            let fileName = meetingId.isEmpty ? "\(stem).md" : "\(stem) (\(meetingId)).md"
            let dest = folderURL.appendingPathComponent(fileName)
            if FileManager.default.fileExists(atPath: dest.path) {
                try FileManager.default.removeItem(at: dest)
            }
            try FileManager.default.copyItem(at: recapURL, to: dest)
            NSLog("[Dimmy] recap exported to \(dest.path)")
        } catch {
            NSLog("[Dimmy] recap export failed: \(error.localizedDescription)")
        }
    }

    /// Friendly label for a stored key, or nil for "auto"/unknown (no chip).
    static func friendlyTypeLabel(_ key: String?) -> String? {
        guard let key = key, !key.isEmpty, key != "auto" else { return nil }
        return meetingTypes.first { $0.key.caseInsensitiveCompare(key) == .orderedSame && $0.key != "auto" }?.label
    }

    /// Normalise a raw type token (LLM tag or UI pick) to a known key;
    /// unknown / empty → "general".
    static func normalizeTypeKey(_ key: String?) -> String {
        if let key = key?.trimmingCharacters(in: .whitespaces), !key.isEmpty,
           let hit = meetingTypes.first(where: { $0.key.caseInsensitiveCompare(key) == .orderedSame }) {
            return hit.key
        }
        return "general"
    }

    /// Extract the `<!-- dimmy-type: KEY -->` tag from the preamble (before
    /// the first section marker / heading). Used by both parse paths.
    static func extractTypeTag(_ raw: String) -> String? {
        for line in raw.split(separator: "\n", omittingEmptySubsequences: false) {
            let t = line.trimmingCharacters(in: .whitespaces)
            if t.lowercased().hasPrefix(typeTagPrefix) {
                if let end = t.range(of: "-->") {
                    let prefixEnd = t.index(t.startIndex, offsetBy: typeTagPrefix.count)
                    let inner = String(t[prefixEnd..<end.lowerBound])
                    return normalizeTypeKey(inner.trimmingCharacters(in: .whitespaces))
                }
            }
            // The tag lives in the preamble only — stop at the first section.
            if t.hasPrefix("===") || t.hasPrefix("## ") { break }
        }
        return nil
    }

    private static func classifierKeyList() -> String {
        meetingTypes.filter { $0.key != "auto" }.map { $0.key }.joined(separator: ", ")
    }

    /// The meeting-type block injected into the recap prompt for a given
    /// type key. "auto"/empty → ask the model to classify + emit the tag.
    static func typeGuidanceBlock(_ meetingType: String) -> String {
        let typeKey = meetingType.trimmingCharacters(in: .whitespaces).isEmpty
            ? "auto" : meetingType.trimmingCharacters(in: .whitespaces)
        if typeKey == "auto" {
            return "## Meeting type (classify, then tailor)\n"
                + "First infer the meeting type from the transcript. IMMEDIATELY after the title "
                + "line, output exactly one line: `\(typeTagPrefix) KEY -->` where KEY is the single "
                + "best fit from: \(classifierKeyList()). Then write every section with the emphasis "
                + "appropriate to that type — for some types a section will naturally be light or "
                + "empty (`—`), and that is correct. Never mention the type tag in the prose.\n"
        }
        let info = meetingTypes.first { $0.key == typeKey } ?? meetingTypes.last!
        return "## Meeting type: \(info.label)\n"
            + (info.guidance.isEmpty ? "" : info.guidance + " ")
            + "Keep ALL 11 sections; let the ones that do not apply be `—`. IMMEDIATELY after the "
            + "title line, output exactly one line: `\(typeTagPrefix) \(info.key) -->`. "
            + "Never mention the type tag in the prose.\n"
    }

    static func buildStructuredRecapPrompt(transcript: String, notes: String = "", meetingType: String = "") -> String {
        // Verbatim port of MeetingWindow.xaml.cs::BuildStructuredRecapPrompt.
        // Notion-style recap targeting reasoning-tier models (Opus 4.7
        // adaptive thinking, Gemini 3.1 Pro thinkingLevel=high, GPT-5).
        // Leading spaces matter for the parser ===KEY=== markers — do
        // not reflow.
        let trimmedNotes = notes.trimmingCharacters(in: .whitespacesAndNewlines)
        let typeBlock = typeGuidanceBlock(meetingType)
        // Listener's notes (notes.md) are the user's own emphasis — fold
        // them in as a HIGH PRIORITY appendix after the transcript so the
        // model weights timestamps + content. Verbatim port of Win
        // MeetingRecapHelpers.cs::BuildStructuredRecapPrompt (line ~215).
        let notesSection: String
        if trimmedNotes.isEmpty {
            notesSection = ""
        } else {
            notesSection = "\n\n═══════════════════════════════════════════════════════════════════\n"
                + "## Listener's notes (HIGH PRIORITY — the user's own emphasis)\n"
                + "These notes were written by the person recording, during and/or after "
                + "the meeting, to flag what matters to them. A leading `[mm:ss]` marks when "
                + "during the meeting the note was taken — align it with the transcript at "
                + "that time. Treat the notes as the single strongest signal of importance: "
                + "weight their content and the discussion around their timestamp heavily, "
                + "surface them prominently in the relevant sections, and reflect any "
                + "explicit asks or to-dos under ACTIONS. Never ignore or drop a note.\n\n"
                + trimmedNotes
        }
        return """
        You are a senior meeting analyst writing a polished, Notion-style summary of an audio recording. Output ONLY markdown with the EXACT marker headings shown — a downstream parser splits on them.

        ## Title (the very first thing you output)
        The VERY FIRST line of your output MUST be a Markdown H1 (`# Title`) — a 3-to-7-word short title for the meeting, in the transcript's language, no quotes, no emoji, no date. Dimmy parses this line and stores it in the meeting's metadata so the UI shows your title instead of the meeting id.

        ## Transcript format
        Each line: `[ELAPSED_MS ms] [SPEAKER_LABEL] text`.
        Speaker labels: `[mic]` = the user recording (treat as "you" / first person when the language allows), `[system]` = remote participant(s) coming through speakers/loopback (treat as "the remote party" / "interlocutor" / specific name only if explicitly mentioned in the transcript). When only `[mic]` is present, the recording is monologue / dictation; when only `[system]` is present, the user was a silent listener.

        ## Output language
        Auto-detect from the transcript. For mixed languages, pick the dominant one. Do NOT translate. If the transcript is in Italian, write the recap in Italian.

        \(typeBlock)
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
        - The very first line MUST be `# <Short title>` (3-7 words, transcript's language, no quotes, no emoji, no date). Without this Dimmy's UI falls back to showing the raw meeting id.
        - Output the sections in the exact order above. ALL section markers must appear, even if the section content is just `—`.
        - Output language follows the transcript dominant language.
        - NEVER invent: participants, dates, amounts, project names, technical terms, deadlines, organizational affiliations, or anything not directly evidenced in the transcript. If unsure, omit rather than fabricate.
        - Quotes (`"..."`) must be VERBATIM from the transcript. If you can't find an exact match, do NOT include the quote — paraphrase outside of quote marks or omit entirely.
        - Convert ELAPSED_MS timestamps to MM:SS for display only. Use the original [N ms] only for internal reasoning if needed.
        - No filler phrases ("the meeting discussed", "various topics were covered", "in conclusion", "overall").
        - No em-dashes (`—`) in prose outside the markers and bullet separators. Use periods, commas, or colons instead.
        - Be SHARP and CONCISE. Senior leaders read these summaries — every sentence must earn its place.

        ═══════════════════════════════════════════════════════════════════
        FINAL REMINDER before you start: the very first line of your
        response must be `# ` followed by a 3-7 word title. NOT `## ===CONTEXT===`,
        NOT a blank line, NOT an apology. JUST `# <title>` on line one.
        ═══════════════════════════════════════════════════════════════════

        ## Transcript
        \(transcript)\(notesSection)
        """
    }

    // MARK: - Parser

    /// Split the LLM output into sections keyed by the marker name.
    /// Falls back to `{TLDR: <whole response>}` if no markers were
    /// emitted (older models, off-prompt outputs).
    static func parseStructuredRecap(_ raw: String) -> [String: String] {
        // Capture the `# Title` H1 (prompt rule) BEFORE splitting on
        // markers, so it survives the parse→build round-trip — no
        // canonical section key matches it otherwise. Stashed under the
        // sentinel `__TITLE__`, which buildMarkdownFromSections re-emits
        // on line 1 so Rust save_post_process parses it into
        // meta.json::title. Mirror of Win ParseStructuredRecap.
        var capturedTitle: String?
        for line in raw.split(separator: "\n", omittingEmptySubsequences: false) {
            let t = line.trimmingCharacters(in: .whitespaces)
            // single-`#` H1 only; "## " section headings must not match.
            if t.hasPrefix("# ") {
                let title = String(t.dropFirst(2)).trimmingCharacters(in: .whitespaces)
                if !title.isEmpty, title.count <= 200 { capturedTitle = title }
                break
            }
        }
        // Invisible `<!-- dimmy-type: KEY -->` tag → __TYPE__ sentinel.
        let capturedType = extractTypeTag(raw)

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
            // Drop any type-tag line so it never shows inside the TLDR card.
            let cleaned = raw.split(separator: "\n", omittingEmptySubsequences: false)
                .filter { !$0.trimmingCharacters(in: .whitespaces).lowercased().hasPrefix(typeTagPrefix) }
                .joined(separator: "\n")
            var fallback = ["TLDR": cleaned.trimmingCharacters(in: .whitespacesAndNewlines)]
            if let capturedTitle { fallback["__TITLE__"] = capturedTitle }
            if let capturedType { fallback["__TYPE__"] = capturedType }
            return fallback
        }
        hits.sort { $0.0.lowerBound < $1.0.lowerBound }
        var result: [String: String] = [:]
        if let capturedTitle { result["__TITLE__"] = capturedTitle }
        if let capturedType { result["__TYPE__"] = capturedType }
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
        // Re-emit the LLM-chosen title as the first line so Rust
        // save_post_process can parse it back into meta.json::title.
        // Without this the title round-trips to nothing (no ===NAME===
        // marker matches it). See parseStructuredRecap.
        if let title = s["__TITLE__"]?.trimmingCharacters(in: .whitespaces), !title.isEmpty {
            out += "# \(title)\n"
            // Persist the resolved meeting type as an invisible tag right
            // under the title so the chip survives a reopen. Only for a
            // resolved type (skip auto/unknown). Renderers discard the
            // preamble, so it never shows in a card.
            if let tkey = s["__TYPE__"], friendlyTypeLabel(tkey) != nil {
                out += "\(typeTagPrefix) \(tkey.trimmingCharacters(in: .whitespaces)) -->\n"
            }
            out += "\n"
        }
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
        let capturedType = extractTypeTag(markdown)
        if indices.isEmpty {
            var fallback = ["TLDR": markdown.trimmingCharacters(in: .whitespacesAndNewlines)]
            if let capturedType { fallback["__TYPE__"] = capturedType }
            return fallback
        }
        indices.sort { $0.0 < $1.0 }
        var result: [String: String] = [:]
        if let capturedType { result["__TYPE__"] = capturedType }
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

    /// Read `llm_api_url` from the on-disk config so the telemetry
    /// provider bucket reflects whatever's currently selected. Same
    /// "read config.json directly" pattern as `pickRecapModel()` so
    /// we can call this from non-MainActor contexts without a hop.
    /// Empty string on any failure — the bucket helper degrades to
    /// "unset" which the Rust dispatcher allow-lists.
    fileprivate static func currentLlmApiUrl() -> String {
        guard let cfgURL = DimmyCore.shared.configDirURL?
            .appendingPathComponent("config.json")
        else { return "" }
        guard let data = try? Data(contentsOf: cfgURL),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let url = json["llm_api_url"] as? String
        else { return "" }
        return url
    }
}

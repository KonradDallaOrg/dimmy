using System;
using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Pure helpers behind the meeting-recap pipeline. Extracted out of
/// <see cref="Dimmy.Windows.Views.MeetingWindow"/> so they're unit-
/// testable without spinning up a XAML host.
///
/// Three concerns:
/// <list type="bullet">
/// <item><see cref="BuildStructuredRecapPrompt"/> — assembles the
///   Notion-quality prompt sent to the LLM. The 11 section markers
///   (CONTEXT, TLDR, HIGHLIGHTS, NARRATIVE, KEY_DECISIONS, TOPICS,
///   ACTIONS, OPEN_QUESTIONS, RISKS, NEXT_STEPS, FOLLOWUPS) are the
///   contract between the prompt and <see cref="ParseStructuredRecap"/>.</item>
/// <item><see cref="ParseStructuredRecap"/> — splits the LLM response
///   into a section dictionary on the `===NAME===` markers. Tolerant
///   of missing / out-of-order sections; falls through to a single
///   TLDR entry if no markers are present.</item>
/// <item><see cref="BuildMarkdownFromSections"/> — emits canonical
///   recap.md from the section dictionary, in fixed display order.
///   Skips empty / placeholder (`—`) sections.</item>
/// </list>
///
/// Section keys are the wire contract: the prompt asks the LLM to emit
/// `===KEY===` markers, the parser scans for them, the renderer (this)
/// reads them back. Renaming a key in any one of the three breaks the
/// round-trip silently — covered by
/// <c>Dimmy.Windows.Tests.Helpers.MeetingRecapHelpersTests</c>.
/// </summary>
public static class MeetingRecapHelpers
{
    /// <summary>Canonical section keys, in the order they appear in the
    /// prompt and in the rendered markdown. Public so tests can assert
    /// the contract without copy-pasting the list.</summary>
    public static readonly IReadOnlyList<string> CanonicalSectionKeys = new[]
    {
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
    };

    /// <summary>Display headings for each section in the rendered
    /// markdown. Same ordering as <see cref="CanonicalSectionKeys"/>.</summary>
    public static readonly IReadOnlyDictionary<string, string> SectionHeadings = new Dictionary<string, string>
    {
        ["CONTEXT"] = "Context",
        ["TLDR"] = "TL;DR",
        ["HIGHLIGHTS"] = "Highlights",
        ["NARRATIVE"] = "Narrative",
        ["KEY_DECISIONS"] = "Key decisions",
        ["TOPICS"] = "Topics discussed",
        ["ACTIONS"] = "Action items",
        ["OPEN_QUESTIONS"] = "Open questions",
        ["RISKS"] = "Risks & blockers",
        ["NEXT_STEPS"] = "Next steps",
        ["FOLLOWUPS"] = "Follow-ups",
    };

    /// <summary>
    /// Meeting-type taxonomy. A type NEVER changes the 11-section
    /// contract — it only shifts emphasis (which sections fill, which
    /// fall to `—`) via a hint injected into the prompt, exactly like the
    /// listener's notes. <c>Key</c> is the machine value persisted in the
    /// invisible <c>&lt;!-- dimmy-type: KEY --&gt;</c> line of recap.md;
    /// <c>Label</c> is the dropdown + chip text; <c>Guidance</c> is the
    /// emphasis hint. <c>auto</c> = the model classifies and emits the
    /// detected key; <c>general</c> = neutral fallback.
    /// Keep in sync with the Mac copy in
    /// <c>MeetingPostProcessService.swift</c> (meetingTypes).
    /// </summary>
    public sealed record MeetingTypeInfo(string Key, string Label, string Guidance);

    public static readonly IReadOnlyList<MeetingTypeInfo> MeetingTypes = new[]
    {
        new MeetingTypeInfo("auto", "Auto-detect", ""),
        new MeetingTypeInfo("one_on_one", "1:1",
            "A 1:1 between two people. Emphasize feedback exchanged, career/growth topics, " +
            "sentiment and rapport, and personal commitments. ACTIONS and FOLLOWUPS matter most; " +
            "KEY_DECISIONS are often light."),
        new MeetingTypeInfo("standup", "Standup / status",
            "A short status sync. In NARRATIVE and TOPICS organize by person or workstream: " +
            "what is done, in progress, and blocked. Surface blockers prominently in RISKS. Keep it brief."),
        new MeetingTypeInfo("planning", "Planning",
            "A planning, sprint or roadmap session. Emphasize goals, scoped work, owners, estimates " +
            "and deadlines under ACTIONS and NEXT_STEPS; capture scope decisions in KEY_DECISIONS and " +
            "dependencies in RISKS."),
        new MeetingTypeInfo("technical", "Technical / design review",
            "A technical or design review. Emphasize the options and approaches considered with their " +
            "tradeoffs in TOPICS, the chosen direction in KEY_DECISIONS, unresolved technical questions " +
            "in OPEN_QUESTIONS, and risks or dependencies in RISKS."),
        new MeetingTypeInfo("brainstorm", "Brainstorming",
            "A brainstorming or ideation session. In TOPICS group ideas by theme; in HIGHLIGHTS surface " +
            "the most promising directions. Expect KEY_DECISIONS to be light or empty (this is divergent " +
            "thinking, not decisions). Capture parked ideas in FOLLOWUPS."),
        new MeetingTypeInfo("interview", "Interview (hiring)",
            "A hiring or candidate interview. Stay factual and evidence-based. In TOPICS capture the areas " +
            "probed and what the candidate demonstrated; surface strengths in HIGHLIGHTS and concerns in " +
            "RISKS. Do NOT invent a hire / no-hire verdict unless it was explicitly stated."),
        new MeetingTypeInfo("customer", "Customer call",
            "A customer-facing call (sales, discovery, or user research). Emphasize the customer's needs, " +
            "pain points and goals, objections or concerns, and any commitments. Capture feature requests " +
            "and insights in TOPICS; relationship or deal next steps in NEXT_STEPS."),
        new MeetingTypeInfo("lecture", "Lecture / talk",
            "A talk, lecture, webinar or presentation the user is mostly listening to. Emphasize the key " +
            "concepts and structured takeaways in TOPICS and HIGHLIGHTS, and write a teaching-quality " +
            "NARRATIVE. ACTIONS and KEY_DECISIONS are usually empty (this is learning, not a working meeting)."),
        new MeetingTypeInfo("general", "General", ""),
    };

    /// <summary>Turn a meeting title into a safe, cross-platform markdown
    /// filename STEM (no extension, no directory). Replaces characters
    /// illegal on Windows (<c>\ / : * ? " &lt; &gt; |</c>) and control chars
    /// with a space, collapses whitespace runs, trims trailing dots/spaces
    /// (illegal as a Windows filename ending), and caps the length. Empty /
    /// all-illegal input → <c>"meeting"</c>. Pure — unit-tested by
    /// <c>MeetingRecapHelpersTests</c>; the export IO lives in
    /// <c>Services.RecapExportService</c>.</summary>
    public static string SanitizeRecapFileName(string? title)
    {
        if (string.IsNullOrWhiteSpace(title)) return "meeting";
        var sb = new System.Text.StringBuilder(title.Length);
        foreach (var ch in title.Trim())
        {
            if (ch is '\\' or '/' or ':' or '*' or '?' or '"' or '<' or '>' or '|'
                || char.IsControl(ch))
                sb.Append(' ');
            else
                sb.Append(ch);
        }
        var cleaned = System.Text.RegularExpressions.Regex
            .Replace(sb.ToString(), @"\s+", " ").Trim()
            .TrimEnd('.', ' ');
        if (cleaned.Length == 0) return "meeting";
        if (cleaned.Length > 120)
            cleaned = cleaned.Substring(0, 120).TrimEnd('.', ' ');
        return cleaned.Length == 0 ? "meeting" : cleaned;
    }

    /// <summary>Friendly label for a stored type key, or null when the
    /// key is unknown / "auto" (no chip shown for an unresolved type).</summary>
    public static string? FriendlyTypeLabel(string? key)
    {
        if (string.IsNullOrWhiteSpace(key) || key == "auto") return null;
        foreach (var t in MeetingTypes)
            if (string.Equals(t.Key, key, StringComparison.OrdinalIgnoreCase) && t.Key != "auto")
                return t.Label;
        return null;
    }

    /// <summary>Normalise a raw type token (from the LLM tag or a UI pick)
    /// to a known key; unknown / empty → "general".</summary>
    public static string NormalizeTypeKey(string? key)
    {
        if (!string.IsNullOrWhiteSpace(key))
            foreach (var t in MeetingTypes)
                if (string.Equals(t.Key, key.Trim(), StringComparison.OrdinalIgnoreCase))
                    return t.Key;
        return "general";
    }

    private const string TypeTagPrefix = "<!-- dimmy-type:";

    /// <summary>The list of selectable type keys (excluding "auto"), for the
    /// auto-classify instruction.</summary>
    private static string ClassifierKeyList() =>
        string.Join(", ", MeetingTypes.Where(t => t.Key != "auto").Select(t => t.Key));

    /// <summary>
    /// Build the Notion-quality structured-recap prompt for a meeting
    /// transcript. Designed for reasoning-tier models (Opus 4.7 +
    /// extended thinking, Gemini 3.1 Pro thinkingLevel=high, GPT-5).
    /// Asks for richer analysis: context inference, narrative prose,
    /// sentiment-tagged topics, priority inference on actions,
    /// time-stamped highlights, follow-ups list.
    ///
    /// The output language is implicit (auto-detected by the LLM from
    /// the transcript). Section markers `===NAME===` are the contract
    /// with <see cref="ParseStructuredRecap"/>.
    ///
    /// <paramref name="meetingType"/> is a taxonomy key (see
    /// <see cref="MeetingTypes"/>). Empty or "auto" → the model classifies
    /// the meeting and emits the detected key; a specific key injects that
    /// type's emphasis hint. EITHER way the 11 sections are unchanged — the
    /// type only nudges which sections fill vs fall to `—`.
    /// </summary>
    public static string BuildStructuredRecapPrompt(string transcript, string notes = "", string meetingType = "")
    {
        var typeKey = string.IsNullOrWhiteSpace(meetingType) ? "auto" : meetingType.Trim();
        string typeBlock;
        if (typeKey == "auto")
        {
            typeBlock =
                "## Meeting type (classify, then tailor)\n" +
                "First infer the meeting type from the transcript. IMMEDIATELY after the title " +
                "line, output exactly one line: `" + TypeTagPrefix + " KEY -->` where KEY is the " +
                "single best fit from: " + ClassifierKeyList() + ". Then write every section with " +
                "the emphasis appropriate to that type — for some types a section will naturally be " +
                "light or empty (`—`), and that is correct. Never mention the type tag in the prose.\n\n";
        }
        else
        {
            var info = MeetingTypes.FirstOrDefault(t => t.Key == typeKey) ?? MeetingTypes.Last();
            typeBlock =
                "## Meeting type: " + info.Label + "\n" +
                (string.IsNullOrEmpty(info.Guidance) ? "" : info.Guidance + " ") +
                "Keep ALL 11 sections; let the ones that do not apply be `—`. IMMEDIATELY after the " +
                "title line, output exactly one line: `" + TypeTagPrefix + " " + info.Key + " -->`. " +
                "Never mention the type tag in the prose.\n\n";
        }
        return
            "You are a senior meeting analyst writing a polished, Notion-style " +
            "summary of an audio recording. Output ONLY markdown with the EXACT " +
            "marker headings shown — a downstream parser splits on them.\n\n" +

            "## Title (the very first thing you output)\n" +
            "The VERY FIRST line of your output MUST be a Markdown H1 (`# Title`) — " +
            "a 3-to-7-word short title for the meeting, in the transcript's language, " +
            "no quotes, no emoji, no date. Dimmy parses this line and stores it in the " +
            "meeting's metadata so the UI shows your title instead of the meeting id.\n\n" +

            "## Transcript format\n" +
            "Each line: `[ELAPSED_MS ms] [SPEAKER_LABEL] text`.\n" +
            "Speaker labels: `[mic]` = the user recording (treat as \"you\" / first person " +
            "when the language allows), `[system]` = remote participant(s) coming through " +
            "speakers/loopback (treat as \"the remote party\" / \"interlocutor\" / specific " +
            "name only if explicitly mentioned in the transcript). When only `[mic]` is " +
            "present, the recording is monologue / dictation; when only `[system]` is " +
            "present, the user was a silent listener.\n\n" +

            "## Output language\n" +
            "Auto-detect from the transcript. For mixed languages, pick the dominant one. " +
            "Do NOT translate. If the transcript is in Italian, write the recap in Italian.\n\n" +

            typeBlock +

            "## Sections (emit ALL of them, in this order)\n\n" +

            "## ===CONTEXT===\n" +
            "2-4 sentences inferring the SETTING of the meeting from cues in the " +
            "transcript: how many distinct voices, the apparent purpose (status sync? " +
            "kickoff? interview? decision-making? brainstorm?), the apparent domain " +
            "(engineering / sales / product / personal / academic). Don't invent names — " +
            "say \"the user\" / \"the remote participant\" unless explicitly named.\n\n" +

            "## ===TLDR===\n" +
            "1-2 sentences. The single most important thing a busy reader needs to know. " +
            "Ruthless filtering: if you removed everything else, what would they still need?\n\n" +

            "## ===HIGHLIGHTS===\n" +
            "3-5 most pivotal moments, one per line:\n" +
            "- `[MM:SS]` **Topic shorthand** — why this matters in one sentence.\n" +
            "Convert ELAPSED_MS to MM:SS. Pick highlights that change the meeting's " +
            "trajectory: a decision, a conflict, a key reveal, a commitment, a moment of " +
            "alignment after debate. Skip routine status updates. If fewer than 3 pivotal " +
            "moments exist, list only what genuinely qualifies.\n\n" +

            "## ===NARRATIVE===\n" +
            "2-4 paragraphs of FLOWING PROSE (NOT bullets). Tell the story of the meeting: " +
            "what was discussed, in which order, with which tone, who pushed for what, " +
            "where alignment came easy and where it was contested. Reference `[mic]` / " +
            "`[system]` inline when attribution matters. Quote a memorable phrase verbatim " +
            "when it captures a moment (`\"...\"`). This is the section a busy stakeholder " +
            "would read INSTEAD of listening to the recording.\n\n" +

            "## ===KEY_DECISIONS===\n" +
            "Bullet list of decisions actually MADE in the meeting (not proposed, not " +
            "considered). Each item:\n" +
            "- **[topic]** : [decision verbatim or paraphrased] — decided by " +
            "[owner if known else \"consensus\"]; impact: [low/medium/high] with one-sentence " +
            "rationale.\n" +
            "Use a single line `—` if no decisions were reached.\n\n" +

            "## ===TOPICS===\n" +
            "Group the discussion into 3-7 distinct topics, sorted by importance (NOT " +
            "chronological). For each:\n" +
            "- ### Topic title (1-3 words)\n" +
            "- 2-4 bullets capturing what was discussed\n" +
            "- _Sentiment_: aligned / debated / blocked / open\n" +
            "- _Quote_: `\"verbatim sentence from transcript\"` — only if a single " +
            "sentence captures the moment; omit otherwise (do NOT approximate).\n\n" +

            "## ===ACTIONS===\n" +
            "Numbered list of action items EXPLICITLY committed to in the transcript. " +
            "Never invent. Each action:\n" +
            "N. **[owner]** : [task] — due: [explicit date / event / \"unspecified\"]; " +
            "priority: [P0/P1/P2 inferred from urgency cues like \"asap\", \"this week\", " +
            "\"eventually\"].\n" +
            "Use `—` if no actions.\n\n" +

            "## ===OPEN_QUESTIONS===\n" +
            "Genuine questions raised but NOT resolved. Phrase as the question itself, " +
            "not as \"they asked about X\". One bullet per question. Use `—` if none.\n\n" +

            "## ===RISKS===\n" +
            "Risks, blockers, dependencies surfaced. Each:\n" +
            "- **[topic]** — [risk in 1 sentence]; likelihood [low/med/high], impact [low/med/high].\n" +
            "Use `—` if none.\n\n" +

            "## ===NEXT_STEPS===\n" +
            "Numbered list of the 3-5 immediate next things that should happen for the " +
            "meeting's trajectory (NOT the same as Actions — those are owned tasks; " +
            "Next Steps is the work's direction). Use `—` if none.\n\n" +

            "## ===FOLLOWUPS===\n" +
            "Things to ASK / CLARIFY offline because the meeting didn't fully resolve them:\n" +
            "- **[topic]** — ask [person if mentioned else \"the relevant party\"] about " +
            "[specific thing].\n" +
            "Use `—` if none.\n\n" +

            "## Hard rules\n" +
            "- The very first line MUST be `# <Short title>` (3-7 words, transcript's language, " +
            "no quotes, no emoji, no date). Without this Dimmy's UI falls back to showing the " +
            "raw meeting id.\n" +
            "- Output the sections in the exact order above. ALL section markers must " +
            "appear, even if the section content is just `—`.\n" +
            "- Output language follows the transcript dominant language.\n" +
            "- NEVER invent: participants, dates, amounts, project names, technical terms, " +
            "deadlines, organizational affiliations, or anything not directly evidenced in " +
            "the transcript. If unsure, omit rather than fabricate.\n" +
            "- Quotes (`\"...\"`) must be VERBATIM from the transcript. If you can't find " +
            "an exact match, do NOT include the quote — paraphrase outside of quote marks " +
            "or omit entirely.\n" +
            "- Convert ELAPSED_MS timestamps to MM:SS for display only. Use the original " +
            "[N ms] only for internal reasoning if needed.\n" +
            "- No filler phrases (\"the meeting discussed\", \"various topics were covered\", " +
            "\"in conclusion\", \"overall\").\n" +
            "- No em-dashes (`—`) in prose outside the markers and bullet separators. " +
            "Use periods, commas, or colons instead.\n" +
            "- Be SHARP and CONCISE. Senior leaders read these summaries — every sentence " +
            "must earn its place.\n\n" +

            "═══════════════════════════════════════════════════════════════════\n" +
            "FINAL REMINDER before you start: the very first line of your\n" +
            "response must be `# ` followed by a 3-7 word title. NOT `## ===CONTEXT===`,\n" +
            "NOT a blank line, NOT an apology. JUST `# <title>` on line one.\n" +
            "═══════════════════════════════════════════════════════════════════\n\n" +

            "## Transcript\n" + transcript +
            (string.IsNullOrWhiteSpace(notes)
                ? ""
                : "\n\n═══════════════════════════════════════════════════════════════════\n" +
                  "## Listener's notes (HIGH PRIORITY — the user's own emphasis)\n" +
                  "These notes were written by the person recording, during and/or after " +
                  "the meeting, to flag what matters to them. A leading `[mm:ss]` marks when " +
                  "during the meeting the note was taken — align it with the transcript at " +
                  "that time. Treat the notes as the single strongest signal of importance: " +
                  "weight their content and the discussion around their timestamp heavily, " +
                  "surface them prominently in the relevant sections, and reflect any " +
                  "explicit asks or to-dos under ACTIONS. Never ignore or drop a note.\n\n" +
                  notes.Trim());
    }

    /// <summary>
    /// Split an LLM response into sections, scanning for the canonical
    /// `===NAME===` markers. Tolerant of:
    /// <list type="bullet">
    /// <item>Missing sections (their key just won't appear in the result).</item>
    /// <item>Sections in a different order than the prompt requested
    ///   (parsed by their position in the response, not the canonical order).</item>
    /// <item>Markers that the LLM wrote with surrounding `## ` headings —
    ///   the leading `#` characters are trimmed from the captured content.</item>
    /// </list>
    ///
    /// If the response contains no canonical markers at all, falls
    /// through to a single <c>TLDR</c> entry containing the whole raw
    /// text — defensive fallback for older / off-template responses.
    /// </summary>
    /// <summary>
    /// Friendly heading → canonical key map for the persisted markdown
    /// format (`## Heading`). Drives the storage-format read path and
    /// tolerates LLM-language drift ("Decisions" vs "Key decisions",
    /// "Topics" vs "Topics discussed", "Tl;dr" vs "TL;DR", etc.).
    /// Case- and whitespace-insensitive lookup via the dictionary
    /// comparer + normalisation in <see cref="MapHeadingToCanonical"/>.
    /// </summary>
    private static readonly Dictionary<string, string> HeadingToKey = new(StringComparer.OrdinalIgnoreCase)
    {
        { "context", "CONTEXT" }, { "background", "CONTEXT" }, { "setting", "CONTEXT" },
        { "tldr", "TLDR" }, { "tl;dr", "TLDR" }, { "tl,dr", "TLDR" }, { "summary", "TLDR" }, { "executive summary", "TLDR" },
        { "highlights", "HIGHLIGHTS" }, { "key points", "HIGHLIGHTS" }, { "key moments", "HIGHLIGHTS" },
        { "narrative", "NARRATIVE" }, { "discussion", "NARRATIVE" }, { "details", "NARRATIVE" }, { "story", "NARRATIVE" },
        { "key decisions", "KEY_DECISIONS" }, { "decisions", "KEY_DECISIONS" }, { "decided", "KEY_DECISIONS" },
        { "topics", "TOPICS" }, { "topics discussed", "TOPICS" }, { "topics covered", "TOPICS" }, { "agenda", "TOPICS" },
        { "actions", "ACTIONS" }, { "action items", "ACTIONS" }, { "todos", "ACTIONS" }, { "to-dos", "ACTIONS" }, { "to do", "ACTIONS" }, { "tasks", "ACTIONS" },
        { "open questions", "OPEN_QUESTIONS" }, { "questions", "OPEN_QUESTIONS" }, { "unresolved", "OPEN_QUESTIONS" }, { "unresolved questions", "OPEN_QUESTIONS" },
        { "risks", "RISKS" }, { "risks & blockers", "RISKS" }, { "risks and blockers", "RISKS" }, { "blockers", "RISKS" }, { "concerns", "RISKS" },
        { "next steps", "NEXT_STEPS" }, { "next step", "NEXT_STEPS" }, { "what's next", "NEXT_STEPS" }, { "going forward", "NEXT_STEPS" },
        { "follow-ups", "FOLLOWUPS" }, { "followups", "FOLLOWUPS" }, { "follow ups", "FOLLOWUPS" }, { "follow-up", "FOLLOWUPS" }, { "follow up", "FOLLOWUPS" },
    };

    private static string? MapHeadingToCanonical(string heading)
    {
        // Normalise whitespace runs to a single space so "Topics  discussed"
        // / "topics\tdiscussed" still match.
        var key = System.Text.RegularExpressions.Regex.Replace(heading.Trim(), @"\s+", " ");
        return HeadingToKey.TryGetValue(key, out var v) ? v : null;
    }

    /// <summary>
    /// Try to interpret a single line as a section boundary. Accepts:
    ///   <list type="bullet">
    ///   <item>`===NAME===` — the wire-format marker (canonical key).</item>
    ///   <item>`## ===NAME===` — same with the markdown heading prefix
    ///        the LLM emits.</item>
    ///   <item>`## Heading` — the persisted storage format (mapped via
    ///        <see cref="HeadingToKey"/> synonyms).</item>
    ///   </list>
    /// Returns the canonical key or <c>null</c> if the line is body
    /// content (or an unknown heading).
    /// </summary>
    private static string? TryParseSectionLine(string trimmed)
    {
        var s = trimmed;
        // Strip an optional markdown heading prefix so we treat
        // `## ===CONTEXT===` and bare `===CONTEXT===` identically.
        while (s.StartsWith("#"))
        {
            s = s.Substring(1);
            if (s.StartsWith(" ") || s.StartsWith("\t")) s = s.TrimStart();
        }

        // Wire format: ===NAME===
        if (s.StartsWith("===") && s.Length > 6)
        {
            int close = s.IndexOf("===", 3, StringComparison.Ordinal);
            if (close > 3)
            {
                var name = s.Substring(3, close - 3).Trim();
                foreach (var k in CanonicalSectionKeys)
                {
                    if (string.Equals(k, name, StringComparison.OrdinalIgnoreCase)) return k;
                }
                // Unknown ===NAME=== marker — not a known section, treat as body.
                return null;
            }
        }

        // Storage format: friendly heading via synonym map.
        // Strip a trailing `:` ("## TL;DR:") and trailing `#` runs
        // (ATX-style "## Heading ##") before lookup.
        var stripped = s.TrimEnd('#', ' ', '\t', '\r', '\n', ':').Trim();
        if (stripped.Length == 0) return null;
        return MapHeadingToCanonical(stripped);
    }

    /// <summary>
    /// Unified, permissive recap parser. Accepts the LLM wire format
    /// (`===NAME===` markers), the persisted storage format (`## Heading`
    /// markdown — possibly with synonym variants), and any mix of the two.
    /// Captures an optional leading `# Title` under the sentinel key
    /// <c>__TITLE__</c> for the build / Rust meta.json round-trip.
    ///
    /// Permissive contract:
    ///   <list type="bullet">
    ///   <item>Missing sections → omitted from the result (caller hides cards).</item>
    ///   <item>Out-of-order sections → fine; emit order is enforced at write time
    ///        by <see cref="BuildMarkdownFromSections"/>.</item>
    ///   <item>Unknown `## Heading` mid-section → treated as body content so
    ///        nested headings (e.g. `### Subtopic` under TOPICS) don't break the
    ///        section split.</item>
    ///   <item>Empty input or input with NO recognised section → falls back to
    ///        a single `TLDR` containing the whole body (minus the captured
    ///        title) so the user always sees their content, even on weird LLM
    ///        output.</item>
    ///   </list>
    /// Burned 2026-05-30: the old two-parser design (`===` first, fall back to
    /// `##` only when count==1 AND length-heuristic matched) silently dumped
    /// recent recaps (with `# Title` + `## Heading` markdown) into a single
    /// TLDR card because the `__TITLE__` sentinel inflated the count past the
    /// fallback gate.
    /// </summary>
    public static Dictionary<string, string> ParseStructuredRecap(string raw)
    {
        var result = new Dictionary<string, string>();
        if (string.IsNullOrWhiteSpace(raw)) return result;

        var lines = raw.Replace("\r\n", "\n").Split('\n');
        string? currentKey = null;
        var sb = new System.Text.StringBuilder();

        void Flush()
        {
            if (currentKey != null)
            {
                var body = sb.ToString().Trim();
                if (body.Length > 0) result[currentKey] = body;
            }
            sb.Clear();
        }

        foreach (var line in lines)
        {
            var trimmed = line.TrimStart();

            // Section boundary (marker or friendly heading)?
            var sectionKey = TryParseSectionLine(trimmed);
            if (sectionKey != null)
            {
                Flush();
                currentKey = sectionKey;
                continue;
            }

            // Optional leading `# Title` — captured ONCE, only before any
            // section opens. Won't intercept `## ...` (handled above) or
            // body lines starting with `#`.
            if (currentKey == null
                && !result.ContainsKey("__TITLE__")
                && trimmed.StartsWith("# ")
                && !trimmed.StartsWith("## "))
            {
                var title = trimmed.Substring(2).TrimEnd('#', ' ', '\t', '\r', '\n').Trim();
                if (title.Length > 0 && title.Length <= 200)
                {
                    result["__TITLE__"] = title;
                    continue;
                }
            }

            // Optional `<!-- dimmy-type: KEY -->` tag — captured ONCE before
            // any section opens. Invisible in rendered markdown (it sits in
            // the preamble the renderer discards); drives the type chip and
            // round-trips via BuildMarkdownFromSections.
            if (currentKey == null
                && !result.ContainsKey("__TYPE__")
                && trimmed.StartsWith(TypeTagPrefix, StringComparison.OrdinalIgnoreCase))
            {
                var inner = trimmed.Substring(TypeTagPrefix.Length);
                int end = inner.IndexOf("-->", StringComparison.Ordinal);
                if (end >= 0)
                {
                    result["__TYPE__"] = NormalizeTypeKey(inner.Substring(0, end).Trim());
                    continue;
                }
            }

            if (currentKey != null) sb.AppendLine(line);
        }
        Flush();

        // Fallback: if no canonical section landed, dump the whole body into
        // TLDR (minus the title if we captured one) so the user always sees
        // something. The OLDER recap format with no markers at all hits this
        // path; covered by `Parse_no_markers_falls_back_to_TLDR_with_whole_text`.
        // Sentinels (__TITLE__, __TYPE__) are NOT real sections — exclude
        // both so a recap that only produced a title/type tag still hits
        // the TLDR fallback instead of showing an empty card.
        int realCount = result.Count(kv => kv.Key != "__TITLE__" && kv.Key != "__TYPE__");
        if (realCount == 0)
        {
            var body = raw.Trim();
            if (result.TryGetValue("__TITLE__", out var title) && title is not null)
            {
                // Strip the first `# Title` line from the body so we don't
                // duplicate it inside the TLDR card.
                var pattern = $@"^\s*#\s+{System.Text.RegularExpressions.Regex.Escape(title)}\s*(\r?\n)?";
                body = System.Text.RegularExpressions.Regex.Replace(
                    body, pattern, "",
                    System.Text.RegularExpressions.RegexOptions.Multiline).Trim();
            }
            // Strip the invisible type tag so it never lands in a visible card.
            body = System.Text.RegularExpressions.Regex.Replace(
                body, @"^\s*<!--\s*dimmy-type:.*?-->\s*(\r?\n)?", "",
                System.Text.RegularExpressions.RegexOptions.Multiline
                | System.Text.RegularExpressions.RegexOptions.IgnoreCase).Trim();
            if (body.Length > 0) result["TLDR"] = body;
        }
        return result;
    }

    /// <summary>
    /// Render the section dictionary into the canonical recap.md format.
    /// Sections are emitted in <see cref="CanonicalSectionKeys"/> order
    /// regardless of input dict order, with their <see cref="SectionHeadings"/>
    /// as level-2 markdown headings. Empty sections, missing sections,
    /// and placeholder `—` sections are skipped — only sections with
    /// real content land in the file.
    /// </summary>
    public static string BuildMarkdownFromSections(Dictionary<string, string> s)
    {
        var sb = new System.Text.StringBuilder();
        // Re-emit the LLM-chosen title as the first line so the Rust
        // `save_post_process` path can parse it back into meta.json.
        // Without this, the title round-trips to /dev/null because no
        // ===NAME=== marker matches it. See ParseStructuredRecap.
        if (s.TryGetValue("__TITLE__", out var title)
            && !string.IsNullOrWhiteSpace(title))
        {
            sb.AppendLine($"# {title.Trim()}");
            // Persist the resolved meeting type as an invisible tag right
            // under the title so the chip survives a reopen. Only for a
            // resolved type (skip "auto"/unknown). Renderers discard the
            // preamble, so it never shows in a card.
            if (s.TryGetValue("__TYPE__", out var tkey) && FriendlyTypeLabel(tkey) != null)
                sb.AppendLine($"{TypeTagPrefix} {tkey.Trim()} -->");
            sb.AppendLine();
        }
        void AppendSection(string key, string heading)
        {
            if (s.TryGetValue(key, out var v) && !string.IsNullOrWhiteSpace(v) && v.Trim() != "—")
            {
                sb.AppendLine($"## {heading}\n").AppendLine(v.Trim()).AppendLine();
            }
        }
        foreach (var key in CanonicalSectionKeys)
        {
            if (SectionHeadings.TryGetValue(key, out var heading))
                AppendSection(key, heading);
        }
        return sb.ToString();
    }

    /// Translate a `dimmy_llm_call_raw` rc into a short user-facing
    /// message. Pure logic — no XAML / FFI / App dependencies — so it
    /// lives here alongside the prompt + parser helpers and the Tests
    /// project can link this file without dragging the rest of the
    /// app in. Keep in sync with the rc table on `dimmy_llm_call_raw`
    /// in core/src/ffi.rs.
    ///
    /// SECURITY: the helper takes ONLY the rc + the user's curated
    /// model id. It NEVER reads an HTTP response body — that would
    /// risk leaking transcript fragments via 4xx error payloads. The
    /// xUnit test `RecapRcToUserMessage_never_echoes_caller_supplied_body`
    /// pins this invariant.
    public static string RecapRcToUserMessage(int rc, string modelOverride)
    {
        var modelHint = string.IsNullOrWhiteSpace(modelOverride) ? "auto" : modelOverride;
        return rc switch
        {
            -2 => "Configure an LLM API key + URL first.",
            -3 => "LLM HTTP call failed — provider returned an unexpected error. See dimmy.log.",
            -4 => "Local LLM model is not available. Pick a model in Settings → LLM.",
            -5 => $"Recap model '{modelHint}' is not supported by the recap endpoint. Pick a different model in Settings → Recap.",
            -6 => "Recap API key is missing or unauthorized. Open Settings → Recap to fix it.",
            -7 => "Recap rate limited (429). Try again in a minute, or pick a faster model.",
            -8 => "Network error reaching the recap endpoint. Check your connection.",
            -9 => $"The recap is too large for '{modelHint}' (413). Pick a model with a bigger context, or a higher-tier provider.",
            _ => $"LLM call returned {rc} — see dimmy.log.",
        };
    }
}

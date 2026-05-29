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
    /// </summary>
    public static string BuildStructuredRecapPrompt(string transcript, string notes = "")
    {
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
    public static Dictionary<string, string> ParseStructuredRecap(string raw)
    {
        var result = new Dictionary<string, string>();
        // Capture the `# Title` H1 (if the LLM honoured the prompt rule)
        // BEFORE we split on ===NAME=== markers. Without this the
        // title gets lost in the parse→build round-trip because no
        // canonical section key matches it. We stash it under a
        // sentinel key `__TITLE__` which `BuildMarkdownFromSections`
        // re-emits on line 1. The Rust `save_post_process` then
        // parses the resulting recap.md's first H1 and persists it
        // into `meta.json::title`.
        var titleMatch = System.Text.RegularExpressions.Regex.Match(
            raw, @"^\s*#\s+(?<t>[^\r\n]+?)\s*$",
            System.Text.RegularExpressions.RegexOptions.Multiline);
        if (titleMatch.Success)
        {
            var t = titleMatch.Groups["t"].Value.Trim();
            if (t.Length > 0 && t.Length <= 200)
            {
                result["__TITLE__"] = t;
            }
        }
        var indices = new SortedDictionary<int, string>();
        foreach (var k in CanonicalSectionKeys)
        {
            var marker = $"===" + k + "===";
            int idx = raw.IndexOf(marker, StringComparison.OrdinalIgnoreCase);
            if (idx >= 0) indices[idx] = k;
        }
        if (indices.Count == 0)
        {
            result["TLDR"] = raw.Trim();
            return result;
        }
        var ordered = indices.ToList();
        for (int i = 0; i < ordered.Count; i++)
        {
            var (start, key) = (ordered[i].Key, ordered[i].Value);
            var marker = $"===" + key + "===";
            int contentStart = start + marker.Length;
            int contentEnd = i + 1 < ordered.Count ? ordered[i + 1].Key : raw.Length;
            var content = raw.Substring(contentStart, contentEnd - contentStart).Trim();
            // Trim BOTH ends of stray markdown noise. Leading: `## `
            // bleeding from the LLM prefacing the marker with a heading.
            // Trailing: `## ` that belongs to the next section's heading
            // (the marker scan stops at `===`, not `## ===`, so the
            // captured range overruns by 3 chars).
            content = content.Trim('#', ' ', '\n', '\r');
            result[key] = content;
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
            sb.AppendLine($"# {title.Trim()}").AppendLine();
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
            _ => $"LLM call returned {rc} — see dimmy.log.",
        };
    }
}

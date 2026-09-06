using System.Collections.Generic;
using System.Linq;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// Coverage for the meeting-recap pipeline's pure helpers — extracted
/// out of MeetingWindow so they can be tested without a XAML host.
///
/// The wire contract is the set of `===NAME===` markers tying together:
///   prompt asks the LLM to emit them →
///   parser scans for them →
///   markdown renderer reads them.
/// Renaming a section key in any one of the three breaks the round-trip
/// silently in production. These tests assert the contract end-to-end.
/// </summary>
public class MeetingRecapHelpersTests
{
    // ── BuildStructuredRecapPrompt ─────────────────────────────────

    [Fact]
    public void Prompt_includes_every_canonical_section_marker()
    {
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("transcript body");
        foreach (var key in MeetingRecapHelpers.CanonicalSectionKeys)
        {
            var marker = "===" + key + "===";
            Assert.Contains(marker, prompt);
        }
    }

    [Fact]
    public void Prompt_section_markers_appear_in_canonical_order()
    {
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("");
        int previousIdx = -1;
        foreach (var key in MeetingRecapHelpers.CanonicalSectionKeys)
        {
            var marker = "===" + key + "===";
            int idx = prompt.IndexOf(marker, System.StringComparison.Ordinal);
            Assert.True(idx > previousIdx,
                $"section {key} must appear after the previous canonical section");
            previousIdx = idx;
        }
    }

    [Fact]
    public void Prompt_appends_transcript_verbatim_at_end()
    {
        const string transcript = "[0 ms] [mic] hello world\n[2000 ms] [system] hi back";
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt(transcript);
        Assert.EndsWith("## Transcript\n" + transcript, prompt);
    }

    [Fact]
    public void Prompt_handles_empty_transcript_without_crashing()
    {
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("");
        Assert.Contains("===CONTEXT===", prompt);
        Assert.EndsWith("## Transcript\n", prompt);
    }

    [Fact]
    public void Prompt_does_not_translate_directive_present()
    {
        // The "Do NOT translate" directive is load-bearing — without it
        // models default to English regardless of source language. Test
        // it explicitly so a future "tighten the prompt" pass doesn't
        // accidentally drop it.
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("");
        Assert.Contains("Do NOT translate", prompt);
    }

    // ── ParseStructuredRecap ───────────────────────────────────────

    [Fact]
    public void Parse_full_response_extracts_all_sections()
    {
        var raw = string.Join("\n", new[]
        {
            "## ===CONTEXT===",
            "Two engineers in a status sync.",
            "## ===TLDR===",
            "Decided to ship.",
            "## ===HIGHLIGHTS===",
            "- [00:30] **Decision** — ship now.",
            "## ===NARRATIVE===",
            "First paragraph.\n\nSecond paragraph.",
            "## ===KEY_DECISIONS===",
            "- **Ship**: green-light",
            "## ===TOPICS===",
            "### Release\n- detail",
            "## ===ACTIONS===",
            "1. **alice** : do thing",
            "## ===OPEN_QUESTIONS===",
            "- when?",
            "## ===RISKS===",
            "—",
            "## ===NEXT_STEPS===",
            "1. release",
            "## ===FOLLOWUPS===",
            "- ask qa",
        });

        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);

        foreach (var key in MeetingRecapHelpers.CanonicalSectionKeys)
            Assert.True(sections.ContainsKey(key), $"missing key {key}");
        Assert.Contains("Two engineers", sections["CONTEXT"]);
        Assert.Equal("Decided to ship.", sections["TLDR"]);
        Assert.Equal("—", sections["RISKS"]);
        Assert.Contains("- ask qa", sections["FOLLOWUPS"]);
    }

    [Fact]
    public void Parse_missing_sections_return_only_what_was_present()
    {
        var raw = "## ===TLDR===\nShort answer.\n\n## ===ACTIONS===\n1. do it";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal(2, sections.Count);
        Assert.Equal("Short answer.", sections["TLDR"]);
        Assert.Equal("1. do it", sections["ACTIONS"]);
        Assert.False(sections.ContainsKey("CONTEXT"));
        Assert.False(sections.ContainsKey("FOLLOWUPS"));
    }

    [Fact]
    public void Parse_no_markers_falls_back_to_TLDR_with_whole_text()
    {
        const string raw = "  Just a flat string with no markers. Old-shape response.  ";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Single(sections);
        Assert.Equal(raw.Trim(), sections["TLDR"]);
    }

    [Fact]
    public void Parse_handles_out_of_canonical_order_response()
    {
        // Some models don't honour requested section order; parser must
        // recover by scanning marker positions, not assuming canonical
        // order in the response.
        var raw = "## ===ACTIONS===\nP1 task\n\n## ===TLDR===\nThe bottom line";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("The bottom line", sections["TLDR"]);
        Assert.Equal("P1 task", sections["ACTIONS"]);
    }

    [Fact]
    public void Parse_marker_with_lowercase_still_matches()
    {
        // case-insensitive marker lookup — defensive against models
        // that emit `===tldr===` instead of `===TLDR===`.
        var raw = "## ===tldr===\nlower-cased body";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.True(sections.ContainsKey("TLDR"));
        Assert.Equal("lower-cased body", sections["TLDR"]);
    }

    // ── Storage format (## Heading) — the persisted recap.md path ──

    [Fact]
    public void Parse_persisted_markdown_with_friendly_headings_extracts_sections()
    {
        // Persisted recap.md uses `## Heading` (not `===NAME===`).
        // Unified parser must accept this format directly — without it the
        // sidebar-load path silently dumped the whole body into the single
        // TLDR card (burned 2026-05-30).
        var raw = string.Join("\n", new[]
        {
            "# Meeting title here",
            "",
            "## Context",
            "Two engineers in a status sync.",
            "",
            "## TL;DR",
            "Ship now.",
            "",
            "## Highlights",
            "- key moment",
            "",
            "## Key decisions",
            "- decided x",
            "",
            "## Topics discussed",
            "### Subtopic stays as body",
            "- detail",
        });
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("Meeting title here", sections["__TITLE__"]);
        Assert.Contains("Two engineers", sections["CONTEXT"]);
        Assert.Equal("Ship now.", sections["TLDR"]);
        Assert.Equal("- key moment", sections["HIGHLIGHTS"]);
        Assert.Equal("- decided x", sections["KEY_DECISIONS"]);
        // `### Subtopic` must stay inside TOPICS body — nested headings
        // are NOT section boundaries.
        Assert.Contains("### Subtopic", sections["TOPICS"]);
        Assert.Contains("- detail", sections["TOPICS"]);
    }

    [Fact]
    public void Parse_persisted_with_title_no_markers_does_not_collapse_into_single_TLDR()
    {
        // The exact bug class from 2026-05-30: `# Title` + `## Headings` →
        // old parser produced {__TITLE__, TLDR=full_body} (count==2), the
        // loader's heuristic flipped markerParsed=true on count>1 and
        // skipped the heading-name fallback → ApplyDoneSections rendered
        // the whole body into the single TLDR card.
        var raw = "# Sample title\n\n## Context\nctx body\n\n## TL;DR\ntldr body";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("Sample title", sections["__TITLE__"]);
        Assert.Equal("ctx body", sections["CONTEXT"]);
        Assert.Equal("tldr body", sections["TLDR"]);
        // CRITICAL: TLDR must NOT contain the whole body — just its own section.
        Assert.DoesNotContain("ctx body", sections["TLDR"]);
        Assert.DoesNotContain("Sample title", sections["TLDR"]);
    }

    [Theory]
    [InlineData("Decisions", "KEY_DECISIONS")]
    [InlineData("Topics", "TOPICS")]
    [InlineData("Topics discussed", "TOPICS")]
    [InlineData("Action items", "ACTIONS")]
    [InlineData("TODOs", "ACTIONS")]
    [InlineData("Tasks", "ACTIONS")]
    [InlineData("Blockers", "RISKS")]
    [InlineData("Risks and blockers", "RISKS")]
    [InlineData("Follow ups", "FOLLOWUPS")]
    [InlineData("Followups", "FOLLOWUPS")]
    [InlineData("Follow-up", "FOLLOWUPS")]
    [InlineData("Open questions", "OPEN_QUESTIONS")]
    [InlineData("Questions", "OPEN_QUESTIONS")]
    [InlineData("Summary", "TLDR")]
    [InlineData("Background", "CONTEXT")]
    [InlineData("Discussion", "NARRATIVE")]
    [InlineData("Highlights", "HIGHLIGHTS")]
    [InlineData("Key points", "HIGHLIGHTS")]
    public void Parse_heading_synonym_maps_to_canonical_key(string heading, string expectedKey)
    {
        // Each `## Heading` synonym must route to the right canonical
        // key — covers LLM-language drift across runs / providers.
        var raw = $"## {heading}\nbody for {heading}";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.True(sections.ContainsKey(expectedKey),
            $"heading '{heading}' should map to {expectedKey}, got keys: {string.Join(",", sections.Keys)}");
        Assert.Contains($"body for {heading}", sections[expectedKey]);
    }

    [Fact]
    public void Parse_hybrid_markers_and_friendly_headings_works()
    {
        // Mixed: LLM emitted some `===NAME===` markers and the prompt
        // structure used `## Heading` for others. Parser handles both
        // in a single pass.
        var raw = "# A title\n\n## ===CONTEXT===\nctx\n\n## TL;DR\nbottom line\n\n## ===ACTIONS===\n1. do it";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("A title", sections["__TITLE__"]);
        Assert.Equal("ctx", sections["CONTEXT"]);
        Assert.Equal("bottom line", sections["TLDR"]);
        Assert.Equal("1. do it", sections["ACTIONS"]);
    }

    [Fact]
    public void Parse_unknown_heading_treated_as_body_not_section_boundary()
    {
        // An LLM-invented `## Stakeholders` (not in synonym map) must NOT
        // close the previous section — it stays as body, preserving the
        // currently open section's content.
        var raw = "## TL;DR\nlead\n\n## Stakeholders\nalice, bob\n\nmore TLDR";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Single(sections);
        Assert.Contains("lead", sections["TLDR"]);
        Assert.Contains("## Stakeholders", sections["TLDR"]);
        Assert.Contains("more TLDR", sections["TLDR"]);
    }

    [Fact]
    public void Parse_only_title_no_sections_falls_back_to_TLDR_without_title_dup()
    {
        // Edge: a recap with ONLY a title line + body, no markers, no
        // friendly headings. Should land everything (except the title
        // line) in TLDR — and the title line itself must not be
        // duplicated inside the TLDR card body.
        var raw = "# Just a title\n\nBody paragraph one.\nBody paragraph two.";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("Just a title", sections["__TITLE__"]);
        Assert.Contains("Body paragraph one.", sections["TLDR"]);
        Assert.Contains("Body paragraph two.", sections["TLDR"]);
        Assert.DoesNotContain("# Just a title", sections["TLDR"]);
    }

    [Fact]
    public void Parse_section_with_trailing_colon_or_ATX_close_still_matches()
    {
        // Tolerate `## TL;DR:` and ATX-style `## Decisions ##` — common
        // markdown variants the LLM may emit.
        var raw = "## TL;DR:\nfirst body\n\n## Decisions ##\nsecond body";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("first body", sections["TLDR"]);
        Assert.Equal("second body", sections["KEY_DECISIONS"]);
    }

    [Fact]
    public void Parse_empty_or_whitespace_returns_empty_dict()
    {
        Assert.Empty(MeetingRecapHelpers.ParseStructuredRecap(""));
        Assert.Empty(MeetingRecapHelpers.ParseStructuredRecap("   \n\t  "));
    }

    [Fact]
    public void Parse_title_too_long_is_not_captured()
    {
        // Defensive: prompt asks for 3-7 words but a misbehaving model
        // could emit a paragraph as the H1. Guard at 200 chars.
        var longTitle = new string('x', 201);
        var raw = $"# {longTitle}\n\n## TL;DR\nbody";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.False(sections.ContainsKey("__TITLE__"));
        Assert.Equal("body", sections["TLDR"]);
    }

    // ── BuildMarkdownFromSections ──────────────────────────────────

    [Fact]
    public void Build_emits_canonical_order_regardless_of_input_dict_order()
    {
        var input = new Dictionary<string, string>
        {
            // Insert in REVERSE canonical order — output must still be
            // in the canonical sequence.
            ["FOLLOWUPS"] = "follow1",
            ["TLDR"] = "tl;dr body",
            ["CONTEXT"] = "context body",
        };
        var md = MeetingRecapHelpers.BuildMarkdownFromSections(input);
        int idxContext = md.IndexOf("## Context", System.StringComparison.Ordinal);
        int idxTldr = md.IndexOf("## TL;DR", System.StringComparison.Ordinal);
        int idxFollow = md.IndexOf("## Follow-ups", System.StringComparison.Ordinal);
        Assert.True(idxContext < idxTldr, "Context must come before TL;DR");
        Assert.True(idxTldr < idxFollow, "TL;DR must come before Follow-ups");
    }

    [Fact]
    public void Build_skips_empty_and_dash_placeholder_sections()
    {
        var input = new Dictionary<string, string>
        {
            ["TLDR"] = "real tl;dr",
            ["ACTIONS"] = "—",          // placeholder ⇒ skipped
            ["RISKS"] = "  ",           // whitespace only ⇒ skipped
            ["NEXT_STEPS"] = "",        // empty ⇒ skipped
        };
        var md = MeetingRecapHelpers.BuildMarkdownFromSections(input);
        Assert.Contains("## TL;DR", md);
        Assert.DoesNotContain("## Action items", md);
        Assert.DoesNotContain("## Risks", md);
        Assert.DoesNotContain("## Next steps", md);
    }

    [Fact]
    public void Build_includes_section_heading_with_double_newline_before_body()
    {
        // Markdown spec for level-2 heading needs blank line between
        // heading and body content. StringBuilder.AppendLine uses
        // Environment.NewLine, so CRLF on Windows / LF on Unix — assert
        // structurally instead of pinning the literal byte sequence.
        var md = MeetingRecapHelpers.BuildMarkdownFromSections(
            new Dictionary<string, string> { ["TLDR"] = "the body" });
        Assert.Contains("## TL;DR", md);
        Assert.Contains("the body", md);
        // Blank line between heading and body — independent of \n vs \r\n.
        int hdrEnd = md.IndexOf("## TL;DR", System.StringComparison.Ordinal) + "## TL;DR".Length;
        int bodyStart = md.IndexOf("the body", hdrEnd, System.StringComparison.Ordinal);
        Assert.True(bodyStart > hdrEnd, "body must come after heading");
        var between = md.Substring(hdrEnd, bodyStart - hdrEnd);
        // Two newline runs in the gap (heading line break + blank-line
        // separator). Counts both \n and \r\n.
        int newlineCount = between.Replace("\r\n", "\n").Count(c => c == '\n');
        Assert.True(newlineCount >= 2, $"expected ≥2 newlines between heading and body, got {newlineCount}");
    }

    [Fact]
    public void Build_empty_dict_returns_empty_string()
    {
        Assert.Equal("", MeetingRecapHelpers.BuildMarkdownFromSections(
            new Dictionary<string, string>()));
    }

    // ── End-to-end: parse → build round-trip ──────────────────────

    [Fact]
    public void RoundTrip_parse_then_build_preserves_content_for_filled_sections()
    {
        var raw = "## ===CONTEXT===\nA two-person sync.\n\n## ===TLDR===\nShip now."
            + "\n\n## ===ACTIONS===\n1. **alice**: ship";
        var parsed = MeetingRecapHelpers.ParseStructuredRecap(raw);
        var rebuilt = MeetingRecapHelpers.BuildMarkdownFromSections(parsed);

        // Headings must be the canonical user-facing labels (not the
        // ===KEY=== markers — those are wire format only).
        Assert.Contains("## Context", rebuilt);
        Assert.Contains("A two-person sync.", rebuilt);
        Assert.Contains("## TL;DR", rebuilt);
        Assert.Contains("Ship now.", rebuilt);
        Assert.Contains("## Action items", rebuilt);
        Assert.Contains("1. **alice**: ship", rebuilt);
        // Markers should NOT bleed through to the rendered markdown.
        Assert.DoesNotContain("===CONTEXT===", rebuilt);
    }

    // ── Contract guard ─────────────────────────────────────────────

    [Fact]
    public void CanonicalSectionKeys_have_matching_SectionHeadings()
    {
        // Every canonical key must have a display heading. Catches the
        // class of bug where a new section is added to the prompt but
        // forgotten in the renderer.
        foreach (var key in MeetingRecapHelpers.CanonicalSectionKeys)
        {
            Assert.True(MeetingRecapHelpers.SectionHeadings.ContainsKey(key),
                $"{key} is missing from SectionHeadings");
        }
        Assert.Equal(MeetingRecapHelpers.CanonicalSectionKeys.Count,
            MeetingRecapHelpers.SectionHeadings.Count);
    }

    // ── Meeting-type (Auto + override + __TYPE__ chip) ─────────────

    [Fact]
    public void Prompt_default_asks_model_to_classify_meeting_type()
    {
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("body");
        Assert.Contains("classify", prompt, System.StringComparison.OrdinalIgnoreCase);
        Assert.Contains("<!-- dimmy-type:", prompt);
        // The classifier key list is offered to the model.
        Assert.Contains("brainstorm", prompt);
        Assert.Contains("customer", prompt);
    }

    [Fact]
    public void Prompt_specific_type_injects_label_and_guidance_and_tag()
    {
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("body", "", "brainstorm");
        Assert.Contains("Brainstorming", prompt);            // the label
        Assert.Contains("group ideas by theme", prompt);     // the guidance
        Assert.Contains("<!-- dimmy-type: brainstorm -->", prompt); // the forced tag
    }

    [Fact]
    public void Prompt_with_type_still_emits_all_11_sections_in_order()
    {
        // INVARIANCE: a meeting type only nudges emphasis — it must NEVER
        // change the 11-section contract or its order.
        var prompt = MeetingRecapHelpers.BuildStructuredRecapPrompt("body", "", "customer");
        int last = -1;
        foreach (var key in MeetingRecapHelpers.CanonicalSectionKeys)
        {
            int idx = prompt.IndexOf($"==={key}===", System.StringComparison.Ordinal); // marker is ===KEY===
            Assert.True(idx > last, $"section {key} out of order or missing with a forced type");
            last = idx;
        }
    }

    [Fact]
    public void Parse_captures_type_tag_into_sentinel()
    {
        var raw = "# Sync\n<!-- dimmy-type: customer -->\n\n## ===TLDR===\nbody";
        var s = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("Sync", s["__TITLE__"]);
        Assert.Equal("customer", s["__TYPE__"]);
        Assert.Equal("body", s["TLDR"]);
        // The tag must NOT leak into a visible section.
        Assert.DoesNotContain("dimmy-type", s["TLDR"]);
    }

    [Fact]
    public void Parse_unknown_type_tag_normalizes_to_general()
    {
        var raw = "# T\n<!-- dimmy-type: nonsense -->\n\n## ===TLDR===\nb";
        var s = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("general", s["__TYPE__"]);
    }

    [Fact]
    public void Parse_type_tag_only_still_falls_back_to_TLDR()
    {
        // A recap with a title + type tag but NO section markers must still
        // land the body in TLDR (the __TYPE__ sentinel is not a real section).
        var raw = "# Title\n<!-- dimmy-type: lecture -->\n\nThe whole body.";
        var s = MeetingRecapHelpers.ParseStructuredRecap(raw);
        Assert.Equal("lecture", s["__TYPE__"]);
        Assert.Contains("The whole body.", s["TLDR"]);
        Assert.DoesNotContain("dimmy-type", s["TLDR"]);
    }

    [Fact]
    public void RoundTrip_type_tag_survives_build_and_reparse()
    {
        var sections = new Dictionary<string, string>
        {
            ["__TITLE__"] = "Quarter plan",
            ["__TYPE__"] = "planning",
            ["TLDR"] = "Ship in Q3.",
        };
        var md = MeetingRecapHelpers.BuildMarkdownFromSections(sections);
        Assert.Contains("<!-- dimmy-type: planning -->", md);
        var reparsed = MeetingRecapHelpers.ParseStructuredRecap(md);
        Assert.Equal("planning", reparsed["__TYPE__"]);
        Assert.Equal("Quarter plan", reparsed["__TITLE__"]);
        Assert.Equal("Ship in Q3.", reparsed["TLDR"]);
    }

    [Fact]
    public void Build_does_not_emit_tag_for_auto_or_unknown_type()
    {
        var auto = MeetingRecapHelpers.BuildMarkdownFromSections(new Dictionary<string, string>
        {
            ["__TITLE__"] = "T", ["__TYPE__"] = "auto", ["TLDR"] = "x",
        });
        Assert.DoesNotContain("dimmy-type", auto);
    }

    [Theory]
    [InlineData("auto", null)]
    [InlineData("", null)]
    [InlineData("nonsense", null)]
    [InlineData("brainstorm", "Brainstorming")]
    [InlineData("one_on_one", "1:1")]
    public void FriendlyTypeLabel_only_resolves_known_non_auto_keys(string? key, string? expected)
    {
        Assert.Equal(expected, MeetingRecapHelpers.FriendlyTypeLabel(key));
    }

    [Theory]
    [InlineData("customer", "customer")]
    [InlineData(" brainstorm ", "brainstorm")]
    [InlineData("CUSTOMER", "customer")]
    [InlineData("zzz", "general")]
    [InlineData("", "general")]
    public void NormalizeTypeKey_maps_known_and_falls_back_to_general(string key, string expected)
    {
        Assert.Equal(expected, MeetingRecapHelpers.NormalizeTypeKey(key));
    }

    [Fact]
    public void MeetingTypes_contains_auto_and_general_and_unique_keys()
    {
        var keys = MeetingRecapHelpers.MeetingTypes.Select(t => t.Key).ToList();
        Assert.Contains("auto", keys);
        Assert.Contains("general", keys);
        Assert.Equal(keys.Count, keys.Distinct().Count());
        // Every non-auto type has a non-empty label.
        foreach (var t in MeetingRecapHelpers.MeetingTypes)
            Assert.False(string.IsNullOrWhiteSpace(t.Label), $"type {t.Key} has no label");
    }

    // ── SanitizeRecapFileName (recap export) ─────────────────────────

    [Theory]
    [InlineData("Sprint planning Q3", "Sprint planning Q3")]
    [InlineData("Roadmap: Q3/Q4 review", "Roadmap Q3 Q4 review")]
    [InlineData("Bug? \"weird\" <crash> | fix*", "Bug weird crash fix")]
    [InlineData("  spaced   out  title  ", "spaced out title")]
    [InlineData("trailing dots...", "trailing dots")]
    public void SanitizeRecapFileName_strips_illegal_and_collapses(string input, string expected)
    {
        Assert.Equal(expected, MeetingRecapHelpers.SanitizeRecapFileName(input));
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData(null)]
    [InlineData("///:::")]
    [InlineData("...")]
    public void SanitizeRecapFileName_empty_or_all_illegal_falls_back_to_meeting(string? input)
    {
        Assert.Equal("meeting", MeetingRecapHelpers.SanitizeRecapFileName(input));
    }

    [Fact]
    public void SanitizeRecapFileName_caps_length_and_has_no_illegal_chars()
    {
        var huge = new string('a', 500) + "/\\:*?\"<>|";
        var result = MeetingRecapHelpers.SanitizeRecapFileName(huge);
        Assert.True(result.Length <= 120, $"length {result.Length} exceeds cap");
        Assert.DoesNotContain('/', result);
        Assert.DoesNotContain('\\', result);
        Assert.DoesNotContain(':', result);
        Assert.False(result.EndsWith(".") || result.EndsWith(" "),
            "filename stem must not end with a dot or space");
    }

    // ── Internal dimmy-* markers must never reach a visible card ──────

    [Fact]
    public void Parse_swallows_the_ai_generated_marker_in_the_preamble()
    {
        // core::meeting::mark_ai_generated stamps this on every recap for EU
        // AI Act art. 50(2). It is machine-readable metadata, not content: if
        // it reached the TLDR card the user would read a raw HTML comment.
        const string raw =
            "# Autenticazione tag NFC\n"
            + "<!-- dimmy-ai-generated: true; by: Dimmy -->\n"
            + "<!-- dimmy-type: technical -->\n"
            + "\n"
            + "Il corpo del riassunto.";
        var sections = MeetingRecapHelpers.ParseStructuredRecap(raw);

        Assert.Equal("Autenticazione tag NFC", sections["__TITLE__"]);
        Assert.Equal("technical", sections["__TYPE__"]);
        foreach (var value in sections.Values)
        {
            Assert.DoesNotContain("dimmy-ai-generated", value);
            Assert.DoesNotContain("<!--", value);
        }
        Assert.Contains("Il corpo del riassunto.", sections["TLDR"]);
    }

    [Fact]
    public void DimmyMarkerLine_recognises_internal_markers_only()
    {
        Assert.True(MeetingRecapHelpers.DimmyMarkerLine("<!-- dimmy-type: technical -->"));
        Assert.True(MeetingRecapHelpers.DimmyMarkerLine("<!-- dimmy-ai-generated: true; by: Dimmy -->"));
        // A future marker is caught without anyone editing this rule, which is
        // the point of keeping it generic.
        Assert.True(MeetingRecapHelpers.DimmyMarkerLine("<!-- dimmy-anything-else: 1 -->"));

        // Not ours, or not a comment: must be left alone as content.
        Assert.False(MeetingRecapHelpers.DimmyMarkerLine("<!-- TODO: something -->"));
        Assert.False(MeetingRecapHelpers.DimmyMarkerLine("dimmy-type: technical"));
        Assert.False(MeetingRecapHelpers.DimmyMarkerLine("Testo che cita dimmy-type a meta' riga"));
        Assert.False(MeetingRecapHelpers.DimmyMarkerLine(""));
    }

    // -- Return-code messages -------------------------------------

    /// <summary>A local model that ran out of GPU memory must not be
    /// described as a provider failure.
    ///
    /// The core returned -3 for every local error and -3 reads as "LLM
    /// HTTP call failed - provider returned an unexpected error", so a
    /// user 200 MB short of VRAM went looking at their network and their
    /// API key. Reproduced 2026-09-06 with Qwen 3 4B on a 4 GB card.</summary>
    [Fact]
    public void Out_of_vram_message_talks_about_the_gpu_not_the_provider()
    {
        var msg = MeetingRecapHelpers.RecapRcToUserMessage(-12, "Qwen3-4B-Instruct-2507-Q4_K_M.gguf");

        Assert.Contains("Qwen3-4B", msg);
        Assert.DoesNotContain("HTTP", msg);
        Assert.DoesNotContain("provider", msg);
        Assert.Contains("GPU", msg);
    }

    [Fact]
    public void Local_failure_codes_carry_their_own_telemetry_bucket()
    {
        Assert.Equal("out_of_memory", MeetingRecapHelpers.RecapRcToCategory(-12));
        Assert.Equal("local_model", MeetingRecapHelpers.RecapRcToCategory(-13));
        // -3 stays what it was: the cloud HTTP bucket.
        Assert.Equal("unknown", MeetingRecapHelpers.RecapRcToCategory(-3));
    }

}

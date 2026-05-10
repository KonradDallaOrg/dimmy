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
}

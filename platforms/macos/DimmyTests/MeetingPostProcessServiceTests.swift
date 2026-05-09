import XCTest
@testable import Dimmy

// MARK: - MeetingPostProcessServiceTests
//
// Unit tests for the structured-recap prompt + parser + markdown
// builder. Mirror of Win xUnit `MeetingRecapPromptTests` shape, with
// extra coverage for round-trip and edge cases (missing sections,
// case-insensitive markers, content trimming).
//
// These tests cover pure-logic surfaces — no FFI calls, no file IO —
// so they're hermetic and run in milliseconds.

final class MeetingPostProcessServiceTests: XCTestCase {
    // MARK: - Prompt builder

    func testPromptIncludesAllSectionMarkers() {
        let prompt = MeetingPostProcessService.buildStructuredRecapPrompt(
            transcript: "[0 ms] [mic] hello world"
        )
        for key in MeetingPostProcessService.sectionKeys {
            XCTAssertTrue(
                prompt.contains("===\(key)==="),
                "Prompt must contain marker for section \(key)"
            )
        }
    }

    func testPromptIncludesTranscriptVerbatim() {
        let transcript = "[123 ms] [mic] this is a unique sentinel 0xDEADBEEF\n[456 ms] [system] response"
        let prompt = MeetingPostProcessService.buildStructuredRecapPrompt(transcript: transcript)
        XCTAssertTrue(prompt.contains(transcript), "Transcript must appear in the prompt verbatim")
    }

    func testPromptIncludesLanguageRule() {
        let prompt = MeetingPostProcessService.buildStructuredRecapPrompt(transcript: "x")
        XCTAssertTrue(prompt.contains("Auto-detect"))
        XCTAssertTrue(prompt.contains("Italian"))  // language example
    }

    func testPromptIncludesHardRules() {
        let prompt = MeetingPostProcessService.buildStructuredRecapPrompt(transcript: "x")
        XCTAssertTrue(prompt.contains("NEVER invent"))
        XCTAssertTrue(prompt.contains("Hard rules"))
        XCTAssertTrue(prompt.contains("VERBATIM"))
    }

    // MARK: - Parser

    func testParseAllSectionsPresent() {
        let raw = """
        ===CONTEXT===
        Two-person status sync.

        ===TLDR===
        Decisions made on pricing.

        ===HIGHLIGHTS===
        - [00:30] **Pricing** — agreed on tiers.

        ===NARRATIVE===
        The conversation began with pricing.

        ===KEY_DECISIONS===
        - **Pricing** : 3-tier — decided by consensus

        ===TOPICS===
        ### Pricing
        - tiered model

        ===ACTIONS===
        1. **Marketing** : draft launch copy

        ===OPEN_QUESTIONS===
        - what about international?

        ===RISKS===
        —

        ===NEXT_STEPS===
        1. ship next week

        ===FOLLOWUPS===
        —
        """
        let sections = MeetingPostProcessService.parseStructuredRecap(raw)
        XCTAssertEqual(sections["CONTEXT"], "Two-person status sync.")
        XCTAssertEqual(sections["TLDR"], "Decisions made on pricing.")
        XCTAssertTrue(sections["HIGHLIGHTS"]?.contains("Pricing") == true)
        XCTAssertTrue(sections["NARRATIVE"]?.contains("conversation") == true)
        XCTAssertTrue(sections["KEY_DECISIONS"]?.contains("3-tier") == true)
        XCTAssertTrue(sections["TOPICS"]?.contains("Pricing") == true)
        XCTAssertTrue(sections["ACTIONS"]?.contains("Marketing") == true)
        XCTAssertTrue(sections["OPEN_QUESTIONS"]?.contains("international") == true)
        XCTAssertEqual(sections["RISKS"], "—")
        XCTAssertTrue(sections["NEXT_STEPS"]?.contains("ship") == true)
        XCTAssertEqual(sections["FOLLOWUPS"], "—")
    }

    func testParseMissingSectionsReturnsAvailableOnes() {
        let raw = """
        ===TLDR===
        Just the summary.

        ===ACTIONS===
        1. Do something
        """
        let sections = MeetingPostProcessService.parseStructuredRecap(raw)
        XCTAssertEqual(sections["TLDR"], "Just the summary.")
        XCTAssertTrue(sections["ACTIONS"]?.contains("Do something") == true)
        XCTAssertNil(sections["RISKS"])
        XCTAssertNil(sections["TOPICS"])
    }

    func testParseFallbackWhenNoMarkersPresent() {
        let raw = "The model didn't produce any markers, just raw prose."
        let sections = MeetingPostProcessService.parseStructuredRecap(raw)
        XCTAssertEqual(sections.count, 1)
        XCTAssertEqual(sections["TLDR"], raw)
    }

    func testParseIsCaseInsensitiveOnMarkers() {
        let raw = """
        ===tldr===
        lowercase marker
        """
        let sections = MeetingPostProcessService.parseStructuredRecap(raw)
        XCTAssertEqual(sections["TLDR"], "lowercase marker")
    }

    func testParseTrimsLeadingHashesAndWhitespace() {
        let raw = """
        ===TLDR===
        ## stray markdown header
        body
        """
        let sections = MeetingPostProcessService.parseStructuredRecap(raw)
        // Leading "## " is stripped by the parser's trim characters.
        let body = sections["TLDR"] ?? ""
        XCTAssertFalse(body.hasPrefix("#"), "Leading #s should be stripped: \(body)")
        XCTAssertTrue(body.contains("body"))
    }

    func testParseHandlesSectionsInUnusualOrder() {
        // Even if the model emits sections out of order, the parser
        // must associate content with the correct marker.
        let raw = """
        ===ACTIONS===
        do thing

        ===TLDR===
        summary
        """
        let sections = MeetingPostProcessService.parseStructuredRecap(raw)
        XCTAssertEqual(sections["TLDR"], "summary")
        XCTAssertEqual(sections["ACTIONS"], "do thing")
    }

    // MARK: - Markdown builder

    func testBuildMarkdownEmitsSectionsInCanonicalOrder() {
        let sections = [
            "CONTEXT": "ctx body",
            "TLDR": "tldr body",
            "ACTIONS": "actions body",
            "RISKS": "risks body",
        ]
        let md = MeetingPostProcessService.buildMarkdownFromSections(sections)
        // CONTEXT must come before TLDR, TLDR before ACTIONS, etc.
        let ctxIdx = md.range(of: "## Context")!.lowerBound
        let tldrIdx = md.range(of: "## TL;DR")!.lowerBound
        let actionsIdx = md.range(of: "## Actions")!.lowerBound
        let risksIdx = md.range(of: "## Risks")!.lowerBound
        XCTAssertLessThan(ctxIdx, tldrIdx)
        XCTAssertLessThan(tldrIdx, actionsIdx)
        XCTAssertLessThan(actionsIdx, risksIdx)
    }

    func testBuildMarkdownSkipsEmptySections() {
        let sections = ["TLDR": "the summary", "RISKS": ""]
        let md = MeetingPostProcessService.buildMarkdownFromSections(sections)
        XCTAssertTrue(md.contains("## TL;DR"))
        XCTAssertFalse(md.contains("## Risks"), "Empty bodies must be skipped")
    }

    func testBuildMarkdownEmitsEmptyForEmptyDictionary() {
        XCTAssertEqual(MeetingPostProcessService.buildMarkdownFromSections([:]), "")
    }

    // MARK: - Round trip

    func testParseBuildParseRoundtripPreservesSections() {
        let original: [String: String] = [
            "TLDR": "summary line one. summary line two.",
            "KEY_DECISIONS": "- **Pricing** : tiered structure",
            "ACTIONS": "1. **Marketing** : draft copy",
            "RISKS": "—",
        ]
        let md = MeetingPostProcessService.buildMarkdownFromSections(original)
        let roundTrip = MeetingPostProcessService.parseMarkdownIntoSections(md)
        for (k, v) in original {
            XCTAssertEqual(
                roundTrip[k]?.trimmingCharacters(in: .whitespacesAndNewlines),
                v.trimmingCharacters(in: .whitespacesAndNewlines),
                "Round-trip mismatch for section \(k)"
            )
        }
    }

    func testParseMarkdownIntoSectionsFallsBackToTldrWhenNoHeaders() {
        let md = "Plain old text without any ## headings."
        let sections = MeetingPostProcessService.parseMarkdownIntoSections(md)
        XCTAssertEqual(sections.count, 1)
        XCTAssertEqual(sections["TLDR"], md)
    }

    // MARK: - Section key set integrity

    func testSectionKeysAreUnique() {
        let keys = MeetingPostProcessService.sectionKeys
        XCTAssertEqual(keys.count, Set(keys).count, "Section keys must be unique")
    }

    func testSectionKeysIncludeNotionStyleNewSections() {
        // Win added these in the v2 prompt — verify Mac mirror.
        let keys = MeetingPostProcessService.sectionKeys
        for required in ["CONTEXT", "TLDR", "HIGHLIGHTS", "NARRATIVE", "KEY_DECISIONS",
                          "TOPICS", "ACTIONS", "OPEN_QUESTIONS", "RISKS", "NEXT_STEPS",
                          "FOLLOWUPS"] {
            XCTAssertTrue(keys.contains(required), "Section key set missing \(required)")
        }
    }
}

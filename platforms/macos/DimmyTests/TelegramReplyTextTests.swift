import XCTest
@testable import Dimmy

// MARK: - TelegramReplyTextTests
//
// The reply Dimmy sends back into Telegram is the only thing the sender
// sees: they shared a voice note from a phone and walked away from the Mac.
// An empty or malformed message there is worse than no feature at all, so
// the shapes a real recap.md can take are pinned here.
// Mirror of Win `Helpers/TelegramReplyTextTests.cs`.

// `TelegramService` is main-actor isolated, and so are its statics.
@MainActor
final class TelegramReplyTextTests: XCTestCase {
    private let recap = """
    # Integration flows for Lobuten
    <!-- dimmy-ai-generated: true; by: Dimmy; model: claude-opus-5 -->
    <!-- dimmy-type: technical -->

    ## Context
    Three voices, one of them remote.

    ## TL;DR
    The flows are settled.
    One architectural question is still open.

    ## Highlights
    - something else entirely
    """

    func testTitleAndSummaryAreQuotedAndNothingAfterThem() {
        XCTAssertEqual(
            TelegramService.replyText(fromRecapMarkdown: recap),
            "Integration flows for Lobuten\n\nThe flows are settled. One architectural question is still open.")
    }

    func testTheAiMarkingCommentIsNeverSentBack() {
        let reply = TelegramService.replyText(fromRecapMarkdown: recap) ?? ""
        XCTAssertFalse(reply.contains("dimmy-ai-generated"))
        XCTAssertFalse(reply.contains("<!--"))
    }

    func testARecapInAnotherLanguageStillYieldsItsSummary() {
        // Only the heading is translated; the marker the renderer writes is
        // what this matches on.
        XCTAssertEqual(
            TelegramService.replyText(
                fromRecapMarkdown: "# Riunione tecnica\n\n## TL;DR\nTutto deciso.\n\n## Punti chiave\n- altro\n"),
            "Riunione tecnica\n\nTutto deciso.")
    }

    func testARecapWithoutASummaryFallsBackToTheTitle() {
        XCTAssertEqual(
            TelegramService.replyText(fromRecapMarkdown: "# Just a title\n\n## Context\nnope\n"),
            "Just a title")
    }

    func testNothingWorthQuotingReturnsNil() {
        for markdown in ["", "   \n  \n", "## Context\nno title, no summary\n"] {
            XCTAssertNil(TelegramService.replyText(fromRecapMarkdown: markdown), "for: \(markdown)")
        }
    }
}

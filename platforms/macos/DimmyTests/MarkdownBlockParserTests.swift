import XCTest
@testable import Dimmy

// MARK: - MarkdownBlockParserTests
//
// Audit follow-up for the meeting Done view: pre-fix the renderer
// only handled `# `..`### `, `> `, dash/star bullets, and `N. ` numbered
// lists. `#### ` collapsed into paragraph text and code fences rendered
// as literal triple-backticks. Tests pin the new shape so a regression
// can't quietly downgrade the recap UI.

final class MarkdownBlockParserTests: XCTestCase {

    // MARK: Headings

    func testHeadingLevels1Through4() {
        let blocks = MarkdownBlockParser.parse("""
        # H1
        ## H2
        ### H3
        #### H4
        """)
        XCTAssertEqual(blocks.count, 4)
        XCTAssertEqual(blocks[0], .heading(level: 1, text: "H1"))
        XCTAssertEqual(blocks[1], .heading(level: 2, text: "H2"))
        XCTAssertEqual(blocks[2], .heading(level: 3, text: "H3"))
        XCTAssertEqual(blocks[3], .heading(level: 4, text: "H4"))
    }

    func testFiveHashesIsParagraphNotHeading() {
        // Past level 4 we treat it as inline text — recaps don't go that
        // deep, and the renderer doesn't have a typography slot for it.
        let blocks = MarkdownBlockParser.parse("##### too deep")
        XCTAssertEqual(blocks, [.paragraph(text: "##### too deep")])
    }

    func testHeadingMustHaveSpaceAfterHashes() {
        let blocks = MarkdownBlockParser.parse("##nospace")
        XCTAssertEqual(blocks, [.paragraph(text: "##nospace")])
    }

    // MARK: Quotes

    func testBlockQuote() {
        let blocks = MarkdownBlockParser.parse("> a quoted line")
        XCTAssertEqual(blocks, [.quote(text: "a quoted line")])
    }

    // MARK: Bullets + numbered

    func testDashBulletAndStarBullet() {
        let blocks = MarkdownBlockParser.parse("- one\n* two")
        XCTAssertEqual(blocks, [.bullet(text: "one"), .bullet(text: "two")])
    }

    func testNumberedList() {
        let blocks = MarkdownBlockParser.parse("1. first\n2. second\n12. twelfth")
        XCTAssertEqual(blocks, [
            .numbered(n: 1, text: "first"),
            .numbered(n: 2, text: "second"),
            .numbered(n: 12, text: "twelfth"),
        ])
    }

    func testFourDigitNumberFallsThroughToParagraph() {
        // "2026. is a year" must not render as item #2026.
        let blocks = MarkdownBlockParser.parse("2026. is a year")
        XCTAssertEqual(blocks, [.paragraph(text: "2026. is a year")])
    }

    // MARK: Code fences

    func testFencedCodeBlockWithBackticks() {
        let blocks = MarkdownBlockParser.parse("""
        ```
        let x = 1
        let y = 2
        ```
        """)
        XCTAssertEqual(blocks, [.codeBlock(lines: ["let x = 1", "let y = 2"])])
    }

    func testFencedCodeBlockWithLanguageTag() {
        // ```swift is the common pattern when the LLM picks a language.
        // The tag must NOT leak into the rendered content.
        let blocks = MarkdownBlockParser.parse("""
        ```swift
        print("hi")
        ```
        """)
        XCTAssertEqual(blocks, [.codeBlock(lines: ["print(\"hi\")"])])
    }

    func testFencedCodeBlockWithTildes() {
        // Both ``` and ~~~ fences are accepted — recap LLMs sometimes
        // prefer one over the other.
        let blocks = MarkdownBlockParser.parse("""
        ~~~
        a
        ~~~
        """)
        XCTAssertEqual(blocks, [.codeBlock(lines: ["a"])])
    }

    func testCodeBlockPreservesInternalBlankLines() {
        // A blank line inside a fenced block is part of the code, not a
        // separator — must NOT collapse into `.blank`.
        let blocks = MarkdownBlockParser.parse("""
        ```
        a

        b
        ```
        """)
        XCTAssertEqual(blocks, [.codeBlock(lines: ["a", "", "b"])])
    }

    func testUnclosedCodeBlockTakesEverythingToEnd() {
        // Defensive: if the LLM forgets the closing fence, we still
        // render the trailing content as code rather than dropping it.
        let blocks = MarkdownBlockParser.parse("""
        ```
        a
        b
        """)
        XCTAssertEqual(blocks, [.codeBlock(lines: ["a", "b"])])
    }

    // MARK: Mixed + blank handling

    func testBlankLinesEmitBlankBlocks() {
        let blocks = MarkdownBlockParser.parse("alpha\n\nbeta")
        XCTAssertEqual(blocks, [
            .paragraph(text: "alpha"),
            .blank,
            .paragraph(text: "beta"),
        ])
    }

    func testRealisticRecapShape() {
        // Stress test mirroring a recap section the LLM might emit.
        let blocks = MarkdownBlockParser.parse("""
        ## Highlights
        - First takeaway
        - Second takeaway

        > Memorable quote

        1. Step one
        2. Step two
        """)
        XCTAssertEqual(blocks, [
            .heading(level: 2, text: "Highlights"),
            .bullet(text: "First takeaway"),
            .bullet(text: "Second takeaway"),
            .blank,
            .quote(text: "Memorable quote"),
            .blank,
            .numbered(n: 1, text: "Step one"),
            .numbered(n: 2, text: "Step two"),
        ])
    }
}

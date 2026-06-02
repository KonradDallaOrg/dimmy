import XCTest
@testable import Dimmy

// MARK: - SelectionCaptureFlowTests
//
// `SelectionCaptureFlow` has two capture paths — AX (`kAXSelectedTextAttribute`)
// first, synthetic Cmd+C via NSPasteboard as fallback. Neither is unit-testable
// in isolation:
//
//   - AX: `AXUIElementCreateSystemWide()` returns a live WindowServer
//     CFType. `kAXFocusedUIElement` queries whoever holds focus at call
//     time. In headless XCTest there's nothing focused and no way to
//     mock `AXUIElement` (CFType, not a Swift protocol).
//   - Synthetic Cmd+C: requires real CGEvent posting against the
//     CGEventTap permission of the test process; the pasteboard
//     round-trip races real-world apps.
//
// What IS testable is the normalization contract that both paths funnel
// through: nil / empty / whitespace-only ⇒ nil; anything else ⇒ verbatim.
// That's the contract command-mode + dictionary callers rely on to
// decide between "transform/replace selection" and "generate-and-insert"
// (or, for the dictionary path, "add word" vs "show workflow hint").
//
// The real AX↔fallback wiring is verified by manual smoke (TextEdit /
// Notes AX hit, VSCode / Notion Cmd+C fallback, password field both-fail).

final class SelectionCaptureFlowTests: XCTestCase {

    func testNormalizeNilIsNil() {
        XCTAssertNil(SelectionCaptureFlow.normalizeCapturedText(nil))
    }

    func testNormalizeEmptyStringIsNil() {
        XCTAssertNil(SelectionCaptureFlow.normalizeCapturedText(""))
    }

    func testNormalizeWhitespaceOnlyIsNil() {
        XCTAssertNil(SelectionCaptureFlow.normalizeCapturedText("   "))
        XCTAssertNil(SelectionCaptureFlow.normalizeCapturedText("\t\t"))
        XCTAssertNil(SelectionCaptureFlow.normalizeCapturedText("\n\n"))
        XCTAssertNil(SelectionCaptureFlow.normalizeCapturedText(" \t\n "))
    }

    func testNormalizePreservesNonEmptyTextVerbatim() {
        XCTAssertEqual(SelectionCaptureFlow.normalizeCapturedText("hello"), "hello")
    }

    func testNormalizePreservesLeadingAndTrailingWhitespace() {
        // Contract: don't strip — the user might have selected the
        // leading space deliberately. The command-mode prompt builder
        // receives exactly what was selected.
        XCTAssertEqual(SelectionCaptureFlow.normalizeCapturedText("  hello  "), "  hello  ")
    }

    func testNormalizePreservesMultilineText() {
        let multi = "line one\nline two\nline three"
        XCTAssertEqual(SelectionCaptureFlow.normalizeCapturedText(multi), multi)
    }

    func testNormalizePreservesUnicode() {
        let s = "Ciao 👋 Mondo — naïve façade"
        XCTAssertEqual(SelectionCaptureFlow.normalizeCapturedText(s), s)
    }

    func testNormalizeSingleNonWhitespaceCharSurvives() {
        // The smallest valid selection is one non-whitespace char.
        XCTAssertEqual(SelectionCaptureFlow.normalizeCapturedText("a"), "a")
    }

    func testNormalizeWhitespaceWrappingSingleCharSurvives() {
        // The non-whitespace inside makes the trimmed-empty check fail,
        // so the original (with wrapping spaces) is returned.
        XCTAssertEqual(SelectionCaptureFlow.normalizeCapturedText(" a "), " a ")
    }
}

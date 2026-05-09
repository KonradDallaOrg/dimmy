import XCTest
@testable import Dimmy

// MARK: - MeetingHistoryRowTests
//
// Tests for the dir-name → friendly-title pretty printer + the
// recap.md → first-meaningful-heading title extractor. Pure logic;
// no FFI, no network. The recap-md path uses a temp file under
// FileManager.default.temporaryDirectory which is auto-cleaned up.

final class MeetingHistoryRowTests: XCTestCase {
    // MARK: - prettifyDirName (via titleFor with no recap)

    func testTitleForFallsBackToPrettifiedDirName() {
        let title = MeetingHistoryRow.titleFor(dirName: "2026-05-09T14-32-08", recapPath: nil)
        XCTAssertEqual(title, "2026-05-09 14:32")
    }

    func testTitleForStripsUuidSuffix() {
        let title = MeetingHistoryRow.titleFor(
            dirName: "2026-05-09T14-32-08_abc123-def456",
            recapPath: nil
        )
        XCTAssertEqual(title, "2026-05-09 14:32")
    }

    func testTitleForReturnsRawNameWhenNotTimestampShape() {
        let title = MeetingHistoryRow.titleFor(dirName: "freeform-name", recapPath: nil)
        XCTAssertEqual(title, "freeform-name")
    }

    // MARK: - titleFor — recap.md preferred when present

    func testTitleForReadsFirstHeadingFromRecapMd() throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("dimmy-test-\(UUID().uuidString).md")
        try "# Q3 Roadmap Sync\n\n## TL;DR\nThings.".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let title = MeetingHistoryRow.titleFor(
            dirName: "2026-05-09T14-32-08",
            recapPath: tmp.path
        )
        XCTAssertEqual(title, "Q3 Roadmap Sync")
    }

    func testTitleForSkipsTldrAndContextMarkers() throws {
        // The recap.md emitted by buildMarkdownFromSections starts with
        // the section title (e.g. "## Context"). The titleFor helper
        // should skip these structural headings and fall back to the
        // dir-name pretty print, so the sidebar shows the timestamp
        // not "Context".
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("dimmy-test-\(UUID().uuidString).md")
        try "## TL;DR\n\nbody\n\n## Context\nbody2".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let title = MeetingHistoryRow.titleFor(
            dirName: "2026-05-09T14-32-08",
            recapPath: tmp.path
        )
        // Falls back to the dir name because TL;DR / Context are
        // structural and skipped.
        XCTAssertEqual(title, "2026-05-09 14:32")
    }

    func testTitleForSkipsMarkerLines() throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("dimmy-test-\(UUID().uuidString).md")
        try "## ===CONTEXT===\nbody\n\n# Quarterly review".write(
            to: tmp, atomically: true, encoding: .utf8
        )
        defer { try? FileManager.default.removeItem(at: tmp) }

        let title = MeetingHistoryRow.titleFor(
            dirName: "2026-05-09T14-32-08",
            recapPath: tmp.path
        )
        XCTAssertEqual(title, "Quarterly review")
    }

    // MARK: - subtitleFor

    func testSubtitleForRendersDateAndTime() {
        // Use a fixed reference date to make the formatter output
        // predictable without depending on the test runner's locale.
        // We can't dictate the locale here, so we instead assert on
        // contains() of the year + a digit (defensive shape check).
        var components = DateComponents()
        components.year = 2026
        components.month = 5
        components.day = 9
        components.hour = 14
        components.minute = 32
        let cal = Calendar(identifier: .gregorian)
        let date = cal.date(from: components)!

        let subtitle = MeetingHistoryRow.subtitleFor(date: date)
        XCTAssertTrue(subtitle.contains("2026"), "Expected year in subtitle: \(subtitle)")
        XCTAssertFalse(subtitle.isEmpty)
    }
}

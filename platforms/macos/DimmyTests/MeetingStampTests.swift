import XCTest

@testable import Dimmy

// MARK: - MeetingStampTests
//
// Pins the block the Recording-view "Add note" button appends to notes.md,
// through the pure `MeetingViewModel.appendingNote(notes:text:timerLabel:)`.
// Hermetic — no ViewModel, no FFI, no clock.
//
// Why pin this: Windows writes the same file (`MeetingWindow.SubmitNote`:
// `**[mm:ss]** text` + a blank line, `h:mm:ss` past the hour) and the recap
// reads notes.md as the user's highest-priority emphasis. The two platforms
// must produce one format.

final class MeetingStampTests: XCTestCase {
    func testFirstNoteIsABoldStampAndTheText() {
        XCTAssertEqual(
            MeetingViewModel.appendingNote(notes: "", text: "Check the budget", timerLabel: "00:05:42"),
            "**[05:42]** Check the budget\n\n")
    }

    func testNotesAccumulateAsSeparateBlocks() {
        let one = MeetingViewModel.appendingNote(notes: "", text: "first", timerLabel: "00:00:10")!
        let two = MeetingViewModel.appendingNote(notes: one, text: "second", timerLabel: "00:12:03")
        XCTAssertEqual(two, "**[00:10]** first\n\n**[12:03]** second\n\n")
    }

    func testTextTypedByHandKeepsItsOwnBlock() {
        // Notes edited freely in the Done view need not end with a blank line.
        XCTAssertEqual(
            MeetingViewModel.appendingNote(notes: "Context: kickoff", text: "go", timerLabel: "00:00:30"),
            "Context: kickoff\n\n**[00:30]** go\n\n")
    }

    func testPastTheHourTheStampKeepsTheHours() {
        // The old "Stamp time" dropped them: 01:00:00 became [00:00].
        XCTAssertEqual(
            MeetingViewModel.appendingNote(notes: "", text: "late", timerLabel: "01:02:03"),
            "**[1:02:03]** late\n\n")
    }

    func testABlankNoteAddsNothing() {
        XCTAssertNil(MeetingViewModel.appendingNote(notes: "x", text: "  \n ", timerLabel: "00:00:01"))
    }

    func testTheNoteIsTrimmedButKeepsItsLines() {
        XCTAssertEqual(
            MeetingViewModel.appendingNote(notes: "", text: "\n- one\n- two  \n", timerLabel: "00:01:00"),
            "**[01:00]** - one\n- two\n\n")
    }
}

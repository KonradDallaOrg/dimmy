import XCTest

@testable import Dimmy

// MARK: - MeetingStampTests
//
// Pins the format produced by `MeetingViewModel.stamping(notes:timerLabel:)`,
// the pure helper that powers the Recording-view Notes tab "Stamp time"
// button. Hermetic — no ViewModel, no FFI, no clock.
//
// Why pin this: the Win Recording-view Notes tab uses `[mm:ss]` stamps
// (NOT `[hh:mm:ss]`). A regression here would diverge the Mac and Win
// notes.md files and confuse the recap prompt that reads notes.md as
// high-priority emphasis.

final class MeetingStampTests: XCTestCase {
    func testEmptyBufferGetsBareStamp() {
        // No leading separator when the buffer starts empty — the very
        // first stamp must not produce a spurious blank line.
        let out = MeetingViewModel.stamping(notes: "", timerLabel: "00:05:42")
        XCTAssertEqual(out, "[05:42] ")
    }

    func testNonEmptyBufferGainsLeadingNewline() {
        // Mid-meeting stamp: keep prior text intact, push the stamp on
        // its own line. Trailing space lets the user type immediately.
        let out = MeetingViewModel.stamping(notes: "ACTION: send recap", timerLabel: "00:12:03")
        XCTAssertEqual(out, "ACTION: send recap\n[12:03] ")
    }

    func testBufferAlreadyEndingInNewlineNoExtraNewline() {
        // The previous line is already terminated — don't stack
        // newlines (would render an empty line in the markdown).
        let out = MeetingViewModel.stamping(notes: "Topic A\n", timerLabel: "01:00:00")
        XCTAssertEqual(out, "Topic A\n[00:00] ")
    }

    func testHHMMSSStripsTheHourPrefix() {
        // "HH:MM:SS" → "[MM:SS] " (Win shape). 8 chars is the trigger.
        let out = MeetingViewModel.stamping(notes: "", timerLabel: "02:34:56")
        XCTAssertEqual(out, "[34:56] ")
    }

    func testShorterLabelKeptVerbatim() {
        // < 8 chars → no strip (graceful fallback if timerLabel ever
        // changes shape; the user still gets a stamp, not a crash).
        let out = MeetingViewModel.stamping(notes: "", timerLabel: "12:34")
        XCTAssertEqual(out, "[12:34] ")
    }

    func testSequentialStampsCompose() {
        // Two stamps in a row produce two lines — proves the helper is
        // idempotent and you can call it repeatedly via the button.
        let one = MeetingViewModel.stamping(notes: "", timerLabel: "00:00:10")
        let two = MeetingViewModel.stamping(notes: one + "first note", timerLabel: "00:00:25")
        XCTAssertEqual(two, "[00:10] first note\n[00:25] ")
    }
}

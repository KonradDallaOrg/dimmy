import XCTest

@testable import Dimmy

// MARK: - UpdateGateTests
//
// Pins the pure `UpdateService.mayEndProcess(_:)` decision that vetoes a
// Sparkle check (and a Sparkle install) while a meeting records.
// Installing relaunches the app, which kills the capture threads
// mid-write: the audio survives, but the recording is cut short, never
// finalized, and orphaned with no recap. Burned on Windows 2026-09-04 —
// Velopack applied a staged update 6 minutes into a real meeting. Mac
// parity with `Dimmy.Windows/Services/UpdateGate.cs`.

final class UpdateGateTests: XCTestCase {

    func testAnIdleAppMayBeReplaced() {
        XCTAssertTrue(UpdateService.mayEndProcess(0))
    }

    func testARecordingMeetingBlocksTheUpdate() {
        XCTAssertFalse(UpdateService.mayEndProcess(1))
    }

    func testALockFailureCountsAsRecording() {
        // We could not read the meeting state. The safe answer is the one
        // that cannot destroy a recording: an update waits, a lost meeting
        // does not come back.
        XCTAssertFalse(UpdateService.mayEndProcess(-1))
    }

    func testAnyUnexpectedReturnCountsAsRecording() {
        XCTAssertFalse(UpdateService.mayEndProcess(2))
        XCTAssertFalse(UpdateService.mayEndProcess(Int32.min))
    }
}

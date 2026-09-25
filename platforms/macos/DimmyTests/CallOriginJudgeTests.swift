import XCTest

@testable import Dimmy

// MARK: - CallOriginJudgeTests
//
// Pins when the Mac host decides the call a meeting is recording has
// ended. Hermetic: no CoreAudio, no FFI — `CallOriginJudge` fed with what
// the host would observe, second by second.
//
// The failure these guard against is the expensive one: a headset or
// Jabra switch read as the call ending. The recording stops mid-call and,
// because the core then holds the call back from restarting, the rest of
// it is never recorded. The core side of the same story (no ghost
// recordings, no restarts) is `core/tests/call_detect_scenarios.rs`.

final class CallOriginJudgeTests: XCTestCase {
    private var judge = CallOriginJudge()

    /// Feed `secs` seconds of one observation, one per second, from `from`.
    /// Returns the first verdict that was not `.undecided` / `.inCall`
    /// together with when it came, or nil if none did.
    @discardableResult
    private func run(from: TimeInterval, secs: Int, alive: Bool = true, audio: Bool,
                     deviceChange: TimeInterval? = nil) -> (String, TimeInterval)? {
        for i in 0..<secs {
            let now = from + TimeInterval(i)
            if case .ended(let why) = judge.judge(now: now, processAlive: alive,
                                                  usingAudio: audio,
                                                  lastDeviceChange: deviceChange) {
                return (why, now)
            }
        }
        return nil
    }

    func testACallInProgressIsInCall() {
        XCTAssertEqual(judge.judge(now: 0, processAlive: true, usingAudio: true,
                                   lastDeviceChange: nil), .inCall)
    }

    /// Teams closed: an answer, not an absence. No waiting.
    func testClosingTheAppEndsTheCallAtOnce() {
        XCTAssertEqual(judge.judge(now: 10, processAlive: false, usingAudio: false,
                                   lastDeviceChange: nil),
                       .ended("origin process gone"))
    }

    /// Hung up, app left open: ends after the confirmation window, not
    /// before, and not much after.
    func testHangingUpEndsWithinTheConfirmationWindow() {
        let ended = run(from: 100, secs: 60, audio: false)
        XCTAssertEqual(ended?.1, 100 + CallOriginJudge.releaseConfirm)
    }

    /// The old 2 s rule: an in-app device switch pausing audio for a few
    /// seconds must not end the recording.
    func testAShortGapInsideTheCallDoesNotEndIt() {
        XCTAssertNil(run(from: 0, secs: 8, audio: false))
        XCTAssertNil(run(from: 8, secs: 600, audio: true))
    }

    /// Quiet, back, quiet again: the clock restarts each time.
    func testReturningAudioResetsTheClock() {
        XCTAssertNil(run(from: 0, secs: 10, audio: false))
        XCTAssertNil(run(from: 10, secs: 1, audio: true))
        XCTAssertNil(run(from: 11, secs: 10, audio: false))
        XCTAssertEqual(run(from: 21, secs: 30, audio: false)?.1,
                       11 + CallOriginJudge.releaseConfirm)
    }

    /// A Bluetooth headset renegotiating, or a Jabra plugged in: audio can
    /// be gone for tens of seconds (31 s measured on Windows). With the
    /// devices changing as it went quiet, the call is not over.
    func testAHeadsetSwitchDoesNotEndTheCall() {
        for gap in [5, 13, 20, 31, 45] {
            judge = CallOriginJudge()
            XCTAssertNil(run(from: 0, secs: 60, audio: true))
            let changed: TimeInterval = 60
            XCTAssertNil(run(from: 60, secs: gap, audio: false, deviceChange: changed),
                         "a \(gap)s headset switch ended the call")
            XCTAssertNil(run(from: 60 + TimeInterval(gap), secs: 600, audio: true,
                             deviceChange: changed))
        }
    }

    /// The device change can arrive a moment after the app went quiet
    /// (the app lets go first, the HAL reports second). Still a switch.
    func testADeviceChangeReportedJustAfterTheGapStartsStillCounts() {
        XCTAssertNil(run(from: 0, secs: 3, audio: false))
        XCTAssertNil(run(from: 3, secs: 40, audio: false, deviceChange: 3))
    }

    /// Hung up and took the headset off: the call still ends — once the
    /// devices have settled — rather than never.
    func testAHangupWithADeviceChangeStillEnds() {
        let ended = run(from: 100, secs: 300, audio: false, deviceChange: 101)
        XCTAssertEqual(ended?.1, 101 + CallOriginJudge.deviceSettle)
    }

    /// A device change long before the call went quiet is irrelevant.
    func testAnOldDeviceChangeDoesNotDelayTheEnd() {
        let ended = run(from: 500, secs: 60, audio: false, deviceChange: 100)
        XCTAssertEqual(ended?.1, 500 + CallOriginJudge.releaseConfirm)
    }

    /// Devices that never stop changing cannot keep a recording alive for
    /// ever.
    func testTheBackstopBoundsEverything() {
        var ended: (String, TimeInterval)?
        for i in 0..<400 where ended == nil {
            let now = TimeInterval(i)
            if case .ended(let why) = judge.judge(now: now, processAlive: true,
                                                  usingAudio: false, lastDeviceChange: now) {
                ended = (why, now)
            }
        }
        XCTAssertEqual(ended?.1, CallOriginJudge.backstop)
    }

    /// `.undecided` always says when to look again, and looking then decides.
    func testUndecidedNamesTheMomentItWillKnow() {
        guard case .undecided(let after) = judge.judge(
            now: 0, processAlive: true, usingAudio: false, lastDeviceChange: nil)
        else { return XCTFail("expected undecided") }
        XCTAssertEqual(after, CallOriginJudge.releaseConfirm)
        XCTAssertEqual(judge.judge(now: after, processAlive: true, usingAudio: false,
                                   lastDeviceChange: nil),
                       .ended("origin released the audio"))
    }
}

import XCTest
@testable import Dimmy

/// Which transcript line a seek lands on. The waveform is the only scrubber
/// in the Done view, so this mapping is the whole of "the transcript follows
/// the audio" — when it returns nothing, the transcript simply does not move
/// and the two look unsynced.
final class TranscriptSeekTests: XCTestCase {

    private typealias Turn = MeetingDoneView.TranscriptTurn

    /// A meeting transcribed in 30 s windows: each line is stamped at the END
    /// of its window, so nothing at all is stamped before 00:00:30.
    private let turns = [
        Turn(id: 0, text: "[00:00:30] [mic] Allora, buongiorno a tutti.", seconds: 30),
        Turn(id: 1, text: "[00:01:00] [mic] Facciamo il punto sullo sprint.", seconds: 60),
        Turn(id: 2, text: "[00:01:30] [mic] Sara, i test sul grounding.", seconds: 90),
    ]

    func testSeekLandsOnTheLineBeingSpoken() {
        XCTAssertEqual(MeetingDoneView.turn(at: 75, in: turns)?.id, 1)
        XCTAssertEqual(MeetingDoneView.turn(at: 60, in: turns)?.id, 1)
        XCTAssertEqual(MeetingDoneView.turn(at: 10_000, in: turns)?.id, 2)
    }

    func testSeekBeforeTheFirstStampFallsBackToTheFirstLine() {
        // Used to return nil, which left the transcript wherever it was for
        // the whole opening window. Windows falls back the same way
        // (MeetingWindow.xaml.cs: hit ??= _doneTurnAnchors[0]).
        XCTAssertEqual(MeetingDoneView.turn(at: 0, in: turns)?.id, 0)
        XCTAssertEqual(MeetingDoneView.turn(at: 29.9, in: turns)?.id, 0)
    }

    func testUnstampedLinesAreSkippedNotSelected() {
        let mixed = [
            Turn(id: 0, text: "imported line with no stamp", seconds: nil),
            Turn(id: 1, text: "[00:00:30] [mic] stamped", seconds: 30),
        ]
        XCTAssertEqual(MeetingDoneView.turn(at: 0, in: mixed)?.id, 1)
        XCTAssertEqual(MeetingDoneView.turn(at: 45, in: mixed)?.id, 1)
    }

    func testATranscriptWithNoStampsAtAllHasNothingToJumpTo() {
        let none = [Turn(id: 0, text: "plain text", seconds: nil)]
        XCTAssertNil(MeetingDoneView.turn(at: 12, in: none))
        XCTAssertNil(MeetingDoneView.turn(at: 0, in: []))
    }

    // The stamp parser itself, since the mapping above is only as good as it.
    func testElapsedSecondsReadsBothStampShapes() {
        XCTAssertEqual(MeetingDoneView.elapsedSeconds(from: "[00:12:00] [mic] x"), 720)
        XCTAssertEqual(MeetingDoneView.elapsedSeconds(from: "[12:00] x"), 720)
        XCTAssertEqual(MeetingDoneView.elapsedSeconds(from: "[  1234 ms] x"), 1.234)
        XCTAssertNil(MeetingDoneView.elapsedSeconds(from: "no stamp here"))
    }
}

/// Mic and system have to read as two voices, the way Windows'
/// TranscriptRenderer paints them: without the split, both sides of a call
/// are one undifferentiated wall of monospaced text.
final class TranscriptSpeakerTests: XCTestCase {

    func testCurrentFormatSplitsIntoStampTrackAndBody() {
        let p = MeetingDoneView.parseTurn("[00:12:00] [mic] ciao a tutti")
        XCTAssertEqual(p.speaker, "mic")
        XCTAssertEqual(p.body, "ciao a tutti")
        XCTAssertEqual(MeetingDoneView.stamp(from: "[00:12:00] [mic] ciao"), "00:12:00")
    }

    func testTheLegacyMillisecondStampStillSplits() {
        let p = MeetingDoneView.parseTurn("[  1234 ms] [system] hello")
        XCTAssertEqual(p.speaker, "system")
        XCTAssertEqual(p.body, "hello")
    }

    func testAnUnknownTrackIsNotTreatedAsASpeaker() {
        // Only mic and system exist. Anything else is body text, rendered
        // whole rather than half-eaten by a bad prefix strip.
        let p = MeetingDoneView.parseTurn("[00:01:00] [paolo] ciao")
        XCTAssertNil(p.speaker)
        XCTAssertEqual(p.body, "[00:01:00] [paolo] ciao")
    }

    func testAPlainLineIsPassedThroughUntouched() {
        let p = MeetingDoneView.parseTurn("imported transcript, no prefix")
        XCTAssertNil(p.speaker)
        XCTAssertEqual(p.body, "imported transcript, no prefix")
        XCTAssertNil(MeetingDoneView.stamp(from: "imported transcript, no prefix"))
    }

    @MainActor
    func testTheTwoTracksNeverShareATint() {
        for dark in [true, false] {
            XCTAssertNotEqual(
                MeetingDoneView.speakerTint("mic", dark: dark),
                MeetingDoneView.speakerTint("system", dark: dark)
            )
        }
        // An unstamped line reads as the mic track rather than as nothing.
        XCTAssertEqual(
            MeetingDoneView.speakerTint(nil, dark: true),
            MeetingDoneView.speakerTint("mic", dark: true)
        )
    }
}

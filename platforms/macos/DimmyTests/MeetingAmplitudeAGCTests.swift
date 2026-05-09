import XCTest
@testable import Dimmy

// MARK: - MeetingAmplitudeAGCTests
//
// Reproduces the regression that shipped on `staging-mac-v2-parity`:
// the meeting waveform was a `CGFloat.random(in:)` walk because the
// Mac viewmodel didn't poll `dimmy_get_amplitude` /
// `dimmy_get_loopback_amplitude`. Tests pin:
//
//   1. The display-AGC formula (sqrt + 1.4 boost, clamped to [0, 1])
//   2. The scrolling-FIFO push behaviour (drop oldest, append newest)
//   3. NaN/Inf guards (FFI clamps but the UI must not crash)
//   4. Pad-on-warmup so bars don't pop in from nothing
//
// All pure-data — no @MainActor, no DimmyCore, no FFI.

final class MeetingAmplitudeAGCTests: XCTestCase {

    // MARK: displayLevel

    func testDisplayLevelMapsZeroToZero() {
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(0), 0)
    }

    func testDisplayLevelClampsToOne() {
        // Anything ≥ ~0.51 is already saturating thanks to the 1.4
        // multiplier on the sqrt; full-scale must stay clamped.
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(1.0), 1.0, accuracy: 1e-6)
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(2.0), 1.0, accuracy: 1e-6)
    }

    func testDisplayLevelMatchesWinFormula() {
        // sqrt(0.25) * 1.4 = 0.5 * 1.4 = 0.7 → must NOT saturate.
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(0.25), 0.7, accuracy: 1e-6)
        // sqrt(0.04) * 1.4 = 0.2 * 1.4 = 0.28 — typical quiet speech.
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(0.04), 0.28, accuracy: 1e-6)
    }

    func testDisplayLevelHandlesNaN() {
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(.nan), 0)
    }

    func testDisplayLevelHandlesInfinity() {
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(.infinity), 0)
    }

    func testDisplayLevelHandlesNegative() {
        // Defensive: FFI guarantees [0, 1], but a corrupted read must
        // not produce a NaN sqrt.
        XCTAssertEqual(MeetingAmplitudeAGC.displayLevel(-0.5), 0)
    }

    // MARK: push (scrolling FIFO)

    func testPushOnEmptyBufferPadsLeftWithZeros() {
        let result = MeetingAmplitudeAGC.push(
            [],
            mic: 0.6,
            system: 0.3,
            capacity: 4
        )
        XCTAssertEqual(result.count, 4)
        XCTAssertEqual(result[0], .zero)
        XCTAssertEqual(result[1], .zero)
        XCTAssertEqual(result[2], .zero)
        XCTAssertEqual(result[3], MeetingAmplitudeSample(mic: 0.6, system: 0.3))
    }

    func testPushDropsOldestWhenFull() {
        var buf: [MeetingAmplitudeSample] = [
            MeetingAmplitudeSample(mic: 0.1, system: 0),
            MeetingAmplitudeSample(mic: 0.2, system: 0),
            MeetingAmplitudeSample(mic: 0.3, system: 0),
        ]
        buf = MeetingAmplitudeAGC.push(buf, mic: 0.4, system: 0, capacity: 3)
        XCTAssertEqual(buf.count, 3)
        XCTAssertEqual(buf[0].mic, 0.2, accuracy: 1e-6)
        XCTAssertEqual(buf[1].mic, 0.3, accuracy: 1e-6)
        XCTAssertEqual(buf[2].mic, 0.4, accuracy: 1e-6)
    }

    func testPushClampsValuesIntoUnitRange() {
        // Defence-in-depth: a corrupted upstream value must not poison
        // the buffer. The view divides by halfHeight using the value
        // and would render off-screen if we let > 1 through.
        let buf = MeetingAmplitudeAGC.push(
            [],
            mic: 1.7,
            system: -0.3,
            capacity: 1
        )
        XCTAssertEqual(buf.count, 1)
        XCTAssertEqual(buf[0].mic, 1.0)
        XCTAssertEqual(buf[0].system, 0.0)
    }

    func testPushMaintainsCapacityAcrossManyTicks() {
        var buf: [MeetingAmplitudeSample] = []
        for i in 0..<200 {
            let level = CGFloat(i % 10) / 10.0
            buf = MeetingAmplitudeAGC.push(
                buf,
                mic: level,
                system: 0,
                capacity: 32
            )
            XCTAssertEqual(buf.count, 32, "tick \(i): buffer must always be exactly capacity")
        }
        // Last tick had i=199 → level = 9/10 = 0.9
        XCTAssertEqual(Double(buf.last?.mic ?? -1), 0.9, accuracy: 1e-6)
    }

    func testPushIsDeterministic() {
        // Same inputs → same output. Catches regressions where
        // someone reintroduces a random walk thinking it "looks
        // better".
        let a = MeetingAmplitudeAGC.push([], mic: 0.5, system: 0.2, capacity: 4)
        let b = MeetingAmplitudeAGC.push([], mic: 0.5, system: 0.2, capacity: 4)
        XCTAssertEqual(a, b)
    }
}

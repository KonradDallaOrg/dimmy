import XCTest

@testable import Dimmy

// MARK: - SystemAudioProcessTapDecisionTests
//
// Pins the pure `shouldRebuildForOutputChange(builtUID:currentUID:)`
// decision used by `rescanAndRebuildIfNeeded` to follow the system
// default output when it flips mid-meeting (BT (dis)connect, wired
// unplug/replug, Sound prefs change). Mac parity with the Windows
// `loopback_should_follow_default` decision fixed in 80540e3 — same
// class of bug (loopback bound to stale endpoint → silent capture)
// + same fix shape (compare bound vs current, rebuild on diff).

@available(macOS 14.4, *)
final class SystemAudioProcessTapDecisionTests: XCTestCase {

    func testNoRebuildWhenBothNil() {
        // No default output known at build time (anchorless aggregate) and
        // none known now — nothing to follow.
        XCTAssertFalse(
            SystemAudioProcessTap.shouldRebuildForOutputChange(
                builtUID: nil, currentUID: nil))
    }

    func testNoRebuildWhenUIDsMatch() {
        // Steady state: tap was built against this output and the system
        // default still points there.
        XCTAssertFalse(
            SystemAudioProcessTap.shouldRebuildForOutputChange(
                builtUID: "BuiltInSpeakerDevice_UID",
                currentUID: "BuiltInSpeakerDevice_UID"))
    }

    func testRebuildWhenDefaultFlipsToDifferentDevice() {
        // The bug case: BT headset connects mid-meeting, macOS flips the
        // default output, the aggregate's anchor goes stale → must rebuild.
        XCTAssertTrue(
            SystemAudioProcessTap.shouldRebuildForOutputChange(
                builtUID: "BuiltInSpeakerDevice_UID",
                currentUID: "AirPodsPro_BT_UID"))
    }

    func testRebuildWhenDeferredGainsADefault() {
        // Edge: deferred state (no anchor) but a default output now exists.
        // Promoting deferred → live by triggering a rebuild lets `start()`
        // anchor against the live default on its first pass.
        XCTAssertTrue(
            SystemAudioProcessTap.shouldRebuildForOutputChange(
                builtUID: nil, currentUID: "BuiltInSpeakerDevice_UID"))
    }

    func testNoRebuildWhenCurrentDefaultMomentarilyMissing() {
        // Transient: between unplug events macOS may report no default
        // for a tick. Don't tear down a working tap on that — the next
        // listener fire will catch the new default and trigger the
        // rebuild then. Suppresses thrashing on multi-event bursts.
        XCTAssertFalse(
            SystemAudioProcessTap.shouldRebuildForOutputChange(
                builtUID: "BuiltInSpeakerDevice_UID", currentUID: nil))
    }
}

// MARK: - LoopbackRateEstimatorTests
//
// Pins the decision that stops the core from believing a lying tap. The
// aggregate's nominal rate is a REQUEST; with a Bluetooth-HFP output the
// sub-device clocks at 16 kHz while the format keeps reporting 48 kHz. The
// core then saw src == canonical, took the identity path (no resampler at
// all) and wrote `audio_system.wav` 3x fast (colleague's Mac, 2026-07-21).
// Frames over host time is the delivery rate by definition — these tests
// drive that arithmetic without CoreAudio, so they run on any machine.

final class LoopbackRateEstimatorTests: XCTestCase {

    /// Apple-silicon mach timebase (24 MHz), so the tick arithmetic below is
    /// exact and the test cannot pass by rounding.
    private static let ticksPerSecond = 24_000_000.0

    /// Host clock, shared across every `drive` in a test — time must move
    /// forward across phases, or the second phase would rewind it and the
    /// elapsed arithmetic would wrap.
    private var hostTime: UInt64 = 1_000_000

    override func setUp() {
        super.setUp()
        hostTime = 1_000_000
    }

    /// Drive `callbacks` IO-proc fires of `frames` samples each, spaced
    /// `ticksPerCallback` apart, and return the FIRST verdict reached.
    @discardableResult
    private func drive(
        _ estimator: inout LoopbackRateEstimator,
        frames: Int,
        ticksPerCallback: UInt64,
        callbacks: Int
    ) -> (rate: Int32, isNew: Bool)? {
        var firstVerdict: (rate: Int32, isNew: Bool)?
        for _ in 0..<callbacks {
            let verdict = estimator.observe(
                deliveredFrames: frames,
                hostTime: hostTime,
                ticksPerSecond: Self.ticksPerSecond)
            if firstVerdict == nil { firstVerdict = verdict }
            hostTime &+= ticksPerCallback
        }
        return firstVerdict
    }

    func testCatchesTheLyingHfpTap() {
        // THE bug: 320 frames every 20 ms is 16 kHz of real content, while
        // the aggregate declares 48 kHz. 480000 ticks == 20 ms at 24 MHz.
        var estimator = LoopbackRateEstimator()
        let verdict = drive(
            &estimator, frames: 320, ticksPerCallback: 480_000, callbacks: 20)
        XCTAssertEqual(verdict?.rate, 16_000)
        XCTAssertEqual(verdict?.isNew, true)
    }

    func testTrustsAnHonestTap() {
        // 512 frames every 256000 ticks (10.67 ms) IS 48 kHz. The estimator
        // must confirm the declaration, not invent a disagreement.
        var estimator = LoopbackRateEstimator()
        let verdict = drive(
            &estimator, frames: 512, ticksPerCallback: 256_000, callbacks: 40)
        XCTAssertEqual(verdict?.rate, 48_000)
    }

    func testNoVerdictBeforeTheObservationWindow() {
        // Enough callbacks but only ~100 ms of them: below
        // minObservationSeconds, so no verdict yet. Deciding on a short
        // window is how a startup burst would get mistaken for a rate.
        var estimator = LoopbackRateEstimator()
        let verdict = drive(
            &estimator, frames: 320, ticksPerCallback: 120_000, callbacks: 10)
        XCTAssertNil(verdict)
        XCTAssertNil(estimator.settled)
    }

    func testDoesNotReviseOnBriefDisagreement() {
        // A standing verdict is heavily damped: ~0.4 s of contrary evidence
        // is not enough to move it. This is the "bursty delivery" case that
        // got the July reactive override reverted.
        var estimator = LoopbackRateEstimator()
        drive(&estimator, frames: 320, ticksPerCallback: 480_000, callbacks: 20)
        XCTAssertEqual(estimator.settled, 16_000)

        let later = drive(
            &estimator, frames: 512, ticksPerCallback: 256_000, callbacks: 40)
        XCTAssertEqual(estimator.settled, 16_000)
        XCTAssertEqual(later?.rate, 16_000)
        XCTAssertEqual(later?.isNew, false, "a standing verdict is not re-announced")
    }

    func testRevisesOnASustainedBluetoothProfileFlip() {
        // The case a frozen latch would have missed entirely: the headset
        // keeps the SAME device UID while its profile flips A2DP 48 kHz →
        // HFP 16 kHz as the mic opens. No default-output change fires, so
        // `shouldRebuildForOutputChange` sees nothing and the tap is never
        // rebuilt — the estimator has to notice by itself.
        var estimator = LoopbackRateEstimator()
        drive(&estimator, frames: 512, ticksPerCallback: 256_000, callbacks: 40)
        XCTAssertEqual(estimator.settled, 48_000, "starts on A2DP")

        // 6 s of consistent 16 kHz. The first window straddles the flip and
        // is discarded as inconsistent; the two after it agree and revise.
        drive(&estimator, frames: 320, ticksPerCallback: 480_000, callbacks: 300)
        XCTAssertEqual(estimator.settled, 16_000, "follows the profile flip")
    }

    func testFlappingFasterThanTheWindowNeverWins() {
        // Alternating profiles produce windows that STRADDLE the changes, and
        // 16 kHz averaged with 48 kHz reads as ~32 kHz — itself a standard
        // rate, so proximity alone cannot reject it. Without the half-window
        // consistency check this test installed 32 kHz, a rate the hardware
        // never ran at. Verified against a simulation of the algorithm before
        // it was written here.
        var estimator = LoopbackRateEstimator()
        drive(&estimator, frames: 512, ticksPerCallback: 256_000, callbacks: 40)
        XCTAssertEqual(estimator.settled, 48_000)

        for _ in 0..<6 {
            drive(&estimator, frames: 320, ticksPerCallback: 480_000, callbacks: 55)
            drive(&estimator, frames: 512, ticksPerCallback: 256_000, callbacks: 100)
        }
        XCTAssertEqual(estimator.settled, 48_000, "never adopts an averaged rate")
    }

    func testRefusesAReadingThatIsNoStandardRate() {
        // 370 frames per 10 ms is 37 kHz — more than 12 % from both 32 kHz
        // and 44.1 kHz. That is not a rate, it is noise (or a HAL hiccup);
        // keep watching rather than latch a number nothing can clock at.
        var estimator = LoopbackRateEstimator()
        let verdict = drive(
            &estimator, frames: 370, ticksPerCallback: 240_000, callbacks: 60)
        XCTAssertNil(verdict)
        XCTAssertNil(estimator.settled)
    }

    func testIgnoresEmptyAndDegenerateCallbacks() {
        // Silent buffers and a failed timebase must not be counted as
        // evidence of anything.
        var estimator = LoopbackRateEstimator()
        for _ in 0..<50 {
            XCTAssertNil(
                estimator.observe(
                    deliveredFrames: 0, hostTime: 1, ticksPerSecond: Self.ticksPerSecond))
            XCTAssertNil(
                estimator.observe(
                    deliveredFrames: 320, hostTime: 1, ticksPerSecond: 0))
        }
        XCTAssertNil(estimator.settled)
    }

    func testSnapPicksTheNearestStandardRate() {
        XCTAssertEqual(LoopbackRateEstimator.snap(15_900), 16_000)
        XCTAssertEqual(LoopbackRateEstimator.snap(47_500), 48_000)
        XCTAssertEqual(LoopbackRateEstimator.snap(43_900), 44_100)
        XCTAssertEqual(LoopbackRateEstimator.snap(8_100), 8_000)
    }

    func testDisplayVersionPrefersTheTaggedBuild() {
        // release.yml sets MARKETING_VERSION from Cargo.toml and
        // CURRENT_PROJECT_VERSION from the tag. rc.1 and rc.2 share the
        // marketing number, so only the tagged one identifies the build.
        XCTAssertEqual(
            UpdateService.displayVersion(short: "0.6.74", build: "0.6.74-rc.2"),
            "0.6.74-rc.2")
        // A stable cut: the two are equal, and the answer is unchanged.
        XCTAssertEqual(
            UpdateService.displayVersion(short: "0.6.74", build: "0.6.74"), "0.6.74")
    }

    func testDisplayVersionFallsBackWhenTheBuildDoesNotExtendIt() {
        // A local Xcode build inherits the .pbxproj defaults, which do not
        // extend the marketing number. Showing them would present a stale
        // or meaningless value as fact.
        XCTAssertEqual(UpdateService.displayVersion(short: "1.0", build: "1"), "1.0")
        XCTAssertEqual(
            UpdateService.displayVersion(short: "0.6.74", build: "0.6.65"), "0.6.74")
        XCTAssertEqual(UpdateService.displayVersion(short: "0.6.74", build: nil), "0.6.74")
        XCTAssertEqual(UpdateService.displayVersion(short: "0.6.74", build: ""), "0.6.74")
    }

    func testDisplayVersionNeverReturnsEmpty() {
        // The hero renders this unconditionally; an empty string would
        // print "Dimmy " with a dangling space.
        XCTAssertEqual(UpdateService.displayVersion(short: nil, build: nil), "0.0.0")
        XCTAssertEqual(UpdateService.displayVersion(short: "", build: ""), "0.0.0")
        XCTAssertEqual(
            UpdateService.displayVersion(short: nil, build: "0.6.74-rc.2"), "0.6.74-rc.2")
    }

    func testStandardRateOnlyAcceptsACloseReading() {
        // Within 12 % → accepted; adrift → rejected. Same tolerance as the
        // Rust `reconcile_loopback_rate` canary, so the two ends agree.
        XCTAssertEqual(LoopbackRateEstimator.standardRate(forMeasured: 16_100), 16_000)
        XCTAssertEqual(LoopbackRateEstimator.standardRate(forMeasured: 47_000), 48_000)
        XCTAssertNil(LoopbackRateEstimator.standardRate(forMeasured: 37_000))
        XCTAssertNil(LoopbackRateEstimator.standardRate(forMeasured: 0))
    }
}

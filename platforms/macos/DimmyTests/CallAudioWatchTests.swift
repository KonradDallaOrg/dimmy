import CoreAudio
import XCTest

@testable import Dimmy

// MARK: - CallAudioWatchTests
//
// Pins how the Mac host tells a call that ENDED from a call that was
// PUSHED OFF ITS DEVICE (Bluetooth headset dropping, Jabra plugged in and
// taking over). Hermetic: `CallAudioWatch` is fed the evidence CoreAudio
// would report — process alive, audio IO running, which devices it uses,
// which devices exist, the defaults. There is no clock anywhere: every
// verdict here would be the same whether the gap lasted one second or ten
// minutes, which is the point.
//
// The core half — one call is one recording, the next call records, a
// stopped call stays stopped — is `core/tests/call_detect_scenarios.rs`.

final class CallAudioWatchTests: XCTestCase {
    private let mac: AudioObjectID = 10       // built-in mic + speakers
    private let airpods: AudioObjectID = 20
    private let jabra: AudioObjectID = 30

    private var watch = CallAudioWatch()

    private func picture(using: Set<AudioObjectID>, input: AudioObjectID,
                         output: AudioObjectID) -> CallAudioWatch.Picture {
        CallAudioWatch.Picture(devicesInUse: using, defaultInput: input, defaultOutput: output)
    }

    /// A call running on the AirPods, which are the default device.
    private func inCallOnAirPods() {
        let state = watch.observe(alive: true, running: true,
                                  now: picture(using: [airpods], input: airpods, output: airpods),
                                  deviceAlive: { _ in true })
        XCTAssertEqual(state, .inCall)
    }

    private func stopped(devices alive: Set<AudioObjectID>, input: AudioObjectID,
                         output: AudioObjectID, processAlive: Bool = true) -> CallAudioWatch.State {
        watch.observe(alive: processAlive, running: false,
                      now: picture(using: [], input: input, output: output),
                      deviceAlive: alive.contains)
    }

    // MARK: ended

    func testHangingUpWithEverythingInPlaceIsReleased() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac, airpods], input: airpods, output: airpods), .released)
    }

    func testClosingTheAppIsGone() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac, airpods], input: airpods, output: airpods,
                               processAlive: false), .gone)
    }

    /// Exiting is an answer even in the middle of a move.
    func testExitingWhileMovingIsGone() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac), .moving)
        XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac, processAlive: false), .gone)
    }

    // MARK: moved, not ended

    /// The AirPods dropped out (walked away, switched to the phone): the
    /// device the call was on no longer exists.
    func testTheHeadsetDisappearingIsAMove() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac), .moving)
    }

    /// A Jabra plugged in becomes the default; the call was on the default.
    func testTheDefaultMovingToANewHeadsetIsAMove() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac, airpods, jabra], input: jabra, output: jabra), .moving)
    }

    /// Only the input default moved (a USB mic plugged in) — still a move.
    func testOnlyTheInputDefaultMovingIsAMove() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac, airpods, jabra], input: jabra, output: airpods), .moving)
    }

    /// Moving holds however long it lasts and however many times the
    /// evidence is re-read — even once the headset is back with every
    /// default where it was. Only the process can end it.
    func testAMoveHoldsUntilTheProcessAnswers() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac), .moving)
        for _ in 0..<1_000 {
            XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac), .moving)
        }
        XCTAssertEqual(stopped(devices: [mac, airpods], input: airpods, output: airpods), .moving)
    }

    /// The call resumes on the new device, then hangs up there: that
    /// hang-up is judged against the new device and ends it.
    func testAfterAMoveTheNextHangUpEndsTheCall() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac, jabra], input: jabra, output: jabra), .moving)
        XCTAssertEqual(watch.observe(alive: true, running: true,
                                     now: picture(using: [jabra], input: jabra, output: jabra),
                                     deviceAlive: { _ in true }), .inCall)
        XCTAssertEqual(stopped(devices: [mac, jabra], input: jabra, output: jabra), .released)
    }

    // MARK: things that must NOT count as a move

    /// A default change that does not concern the device the call was on:
    /// the call was on the AirPods explicitly, the Mac speaker default
    /// changed. Hanging up is still hanging up.
    func testADefaultChangeOnAnotherDeviceIsNotAMove() {
        _ = watch.observe(alive: true, running: true,
                          now: picture(using: [airpods], input: mac, output: mac),
                          deviceAlive: { _ in true })
        XCTAssertEqual(stopped(devices: [mac, airpods, jabra], input: jabra, output: jabra), .released)
    }

    /// Unplugging a device the call was not using does not keep it alive.
    func testLosingAnUnusedDeviceIsNotAMove() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [airpods], input: airpods, output: airpods), .released)
    }

    /// Hanging up flips a Bluetooth headset from HFP back to A2DP. Same
    /// device, same default — the watch never looks at the sample rate, so
    /// this is released, not a move that would record for ever.
    func testTheProfileFlipCausedByHangingUpIsReleased() {
        inCallOnAirPods()
        XCTAssertEqual(stopped(devices: [mac, airpods], input: airpods, output: airpods), .released)
    }

    /// A process that runs without reporting devices keeps the devices it
    /// was last seen on.
    func testRunningWithoutDeviceListKeepsTheLastOne() {
        inCallOnAirPods()
        _ = watch.observe(alive: true, running: true,
                          now: picture(using: [], input: airpods, output: airpods),
                          deviceAlive: { _ in true })
        XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac), .moving)
    }

    /// Never seen in a call: nothing to be moved from.
    func testNeverSeenRunningIsReleased() {
        XCTAssertEqual(stopped(devices: [mac], input: mac, output: mac), .released)
    }
}

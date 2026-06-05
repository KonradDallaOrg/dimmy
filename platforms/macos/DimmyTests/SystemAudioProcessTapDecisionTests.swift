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

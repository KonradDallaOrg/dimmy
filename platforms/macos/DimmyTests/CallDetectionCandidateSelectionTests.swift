import XCTest

@testable import Dimmy

// MARK: - CallDetectionCandidateSelectionTests
//
// Pins the pure pid → call-app selection that the mid-meeting adoption
// (`adoptCallOriginDuringMeetingTick`) and the pre-meeting scan
// (`scanRunningInputProcesses`) both rely on. Hermetic: no CoreAudio,
// no FFI, no AppKit — just `CallDetectionManager.firstCallCandidate(...)`
// with a fake resolver. A regression here would silently break either
// the "stop suggestion when Teams closes" flow (manual meeting) or the
// "Record now" nudge attribution (pre-meeting).
//
// Sibling of the Rust unit tests in `core/src/call_detector.rs` and the
// FFI round-trip in `core/tests/v2_ffi.rs`. Those cover the state
// machine; this one covers the Swift-side candidate picker.

final class CallDetectionCandidateSelectionTests: XCTestCase {
    /// Resolver mock: simulates `NSRunningApplication`/bundle lookup
    /// without touching AppKit. Maps pid → app id; nil for pids that
    /// would resolve to a system bundle or no bundle at all (daemons).
    private func makeResolver(_ map: [pid_t: String]) -> (pid_t) -> String? {
        return { pid in map[pid] }
    }

    func testExcludesSelfPid() {
        // Dimmy itself holds the mic during a meeting (cpal capture).
        // Adopting Dimmy as the call origin would fire a spurious
        // "session_ended" the moment we stopped the meeting.
        let resolve = makeResolver([42: "dimmy", 100: "teams"])
        let pick = CallDetectionManager.firstCallCandidate(
            pids: [42, 100], selfPid: 42, resolve: resolve)
        XCTAssertEqual(pick?.0, 100)
        XCTAssertEqual(pick?.1, "teams")
    }

    func testReturnsNilWhenNoPidResolves() {
        // Manual meeting with NO call app running — only Dimmy (excluded)
        // and resolve-to-nil daemons present. Adoption must skip, not
        // bind a phantom origin (which would later trigger a false stop).
        let resolve = makeResolver([:])
        let pick = CallDetectionManager.firstCallCandidate(
            pids: [42, 200, 300], selfPid: 42, resolve: resolve)
        XCTAssertNil(pick)
    }

    func testPicksLowestPidOfMultipleCallApps() {
        // When several call apps hold the mic simultaneously (Zoom + Slack
        // huddle, say), pick deterministically — lowest pid wins. Set
        // iteration order is non-deterministic, so without explicit
        // sorting the test (and production) would flake between origins.
        let resolve = makeResolver([100: "zoom", 50: "slack", 200: "teams"])
        let pick = CallDetectionManager.firstCallCandidate(
            pids: [200, 100, 50], selfPid: 1, resolve: resolve)
        XCTAssertEqual(pick?.0, 50)
        XCTAssertEqual(pick?.1, "slack")
    }

    func testSkipsSystemAndDaemonPids() {
        // Real-world mix: ControlCenter (resolves nil — in systemBundleIgnore),
        // a no-bundle daemon (resolves nil), Dimmy itself (selfPid), and
        // Teams. The picker must skip nil-resolving pids AND selfPid, then
        // surface Teams.
        let resolve = makeResolver([400: "teams"])  // others → nil
        let pick = CallDetectionManager.firstCallCandidate(
            pids: [42, 50, 100, 400], selfPid: 42, resolve: resolve)
        XCTAssertEqual(pick?.0, 400)
        XCTAssertEqual(pick?.1, "teams")
    }

    func testEmptyPidSetReturnsNil() {
        // Pre-meeting scan with nothing holding the mic — adoption skips,
        // pre-meeting nudge stays quiet. Pure no-op shape.
        let pick = CallDetectionManager.firstCallCandidate(
            pids: [], selfPid: 42, resolve: { _ in "teams" })
        XCTAssertNil(pick)
    }

    func testSelfAloneReturnsNil() {
        // Only Dimmy in the input set (manual meeting recording the user's
        // own voice, no call running). Must NOT adopt Dimmy as origin
        // even if a resolver would map our pid to "dimmy".
        let resolve = makeResolver([42: "dimmy"])
        let pick = CallDetectionManager.firstCallCandidate(
            pids: [42], selfPid: 42, resolve: resolve)
        XCTAssertNil(pick)
    }
}

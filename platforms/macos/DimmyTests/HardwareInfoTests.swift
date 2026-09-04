import XCTest

@testable import Dimmy

// MARK: - HardwareInfoTests
//
// Pins `HardwareInfo.prefersCloud`, the Mac half of the onboarding
// local-vs-cloud preselection. Exact mirror of the Windows
// `OnboardingPreselect.For` so the two platforms cannot drift into
// recommending opposite things on the same machine.

final class HardwareInfoTests: XCTestCase {

    private func info(_ fitness: String) -> HardwareInfo {
        HardwareInfo(
            name: "Test GPU", vramMB: 4096, dedicated: true,
            appleSilicon: false, fitness: fitness, line: nil)
    }

    func testOnlyPoorHardwareSendsTheUserToTheCloud() {
        XCTAssertTrue(info("poor").prefersCloud)
        XCTAssertFalse(info("good").prefersCloud)
        XCTAssertFalse(info("tight").prefersCloud)
    }

    func testAnUnreadableMachineIsNotPushedToTheCloud() {
        // "unknown" means we could not read the GPU, which is not the same
        // as knowing it is weak. Local needs no account and works offline.
        XCTAssertFalse(info("unknown").prefersCloud)
    }

    func testAnUnrecognisedVerdictKeepsTheLocalDefault() {
        XCTAssertFalse(info("").prefersCloud)
        XCTAssertFalse(info("something we never ship").prefersCloud)
    }
}

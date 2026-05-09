import XCTest
@testable import Dimmy

// MARK: - AppStateRecapModelTests
//
// Tests that `recap_model_override` round-trips through AppState's
// applyConfig / toRustConfig. Mirror of Win xUnit
// `SettingsViewModelTests.RecapModelOverride_*` shape.
//
// AppState is @MainActor isolated, so tests are too.

@MainActor
final class AppStateRecapModelTests: XCTestCase {
    // AppState is a singleton with a private init; we test against
    // `.shared` and stash/restore the field around each test so we
    // don't poison other tests in the suite.
    private var savedOverride: String = ""

    override func setUp() {
        super.setUp()
        savedOverride = AppState.shared.recapModelOverride
    }

    override func tearDown() {
        AppState.shared.recapModelOverride = savedOverride
        super.tearDown()
    }

    func testApplyConfigPicksUpRecapModelOverride() {
        let state = AppState.shared
        state.recapModelOverride = ""
        state.loadFromRustConfig(["recap_model_override": "claude-opus-4-7"])
        XCTAssertEqual(state.recapModelOverride, "claude-opus-4-7")
    }

    func testApplyConfigEmptyClearsToEmpty() {
        let state = AppState.shared
        state.recapModelOverride = "gpt-5"
        state.loadFromRustConfig(["recap_model_override": ""])
        XCTAssertEqual(state.recapModelOverride, "")
    }

    func testApplyConfigMissingKeyLeavesValueUnchanged() {
        let state = AppState.shared
        state.recapModelOverride = "gemini-2.5-pro"
        // Apply a config that doesn't mention recap_model_override —
        // the existing value must survive.
        state.loadFromRustConfig(["llm_api_url": "https://api.openai.com/v1"])
        XCTAssertEqual(state.recapModelOverride, "gemini-2.5-pro")
    }

    func testToRustConfigEmitsRecapModelOverride() {
        let state = AppState.shared
        state.recapModelOverride = "claude-sonnet-4-6"
        let config = state.toRustConfig()
        XCTAssertEqual(config["recap_model_override"] as? String, "claude-sonnet-4-6")
    }

    func testToRustConfigEmitsEmptyForAuto() {
        let state = AppState.shared
        state.recapModelOverride = ""
        let config = state.toRustConfig()
        XCTAssertEqual(config["recap_model_override"] as? String, "")
    }

    func testFullRoundtripPreservesRecapModelOverride() {
        let state = AppState.shared
        state.recapModelOverride = "gpt-4o"
        let snapshot = state.toRustConfig()

        state.recapModelOverride = ""
        state.loadFromRustConfig(snapshot)
        XCTAssertEqual(state.recapModelOverride, "gpt-4o")
    }
}

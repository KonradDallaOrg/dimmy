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
        // recap_model_override is gated behind includeRecap:true since
        // 2026-05-15 (wipe protection). The Recap-page Picker passes
        // the flag explicitly.
        let state = AppState.shared
        state.recapModelOverride = "claude-sonnet-4-6"
        let config = state.toRustConfig(includeRecap: true)
        XCTAssertEqual(config["recap_model_override"] as? String, "claude-sonnet-4-6")
    }

    func testToRustConfigDefaultOmitsRecapModelOverride() {
        // Default flags → recap_model_override is OMITTED so a
        // non-Recap-page save can't accidentally wipe a user's
        // explicit pick. Same pattern as notion_target_id.
        let state = AppState.shared
        state.recapModelOverride = "claude-opus-4-7"
        let config = state.toRustConfig()  // defaults
        XCTAssertNil(config["recap_model_override"],
                     "recap_model_override must be omitted from default toRustConfig")
    }

    func testToRustConfigEmitsEmptyForAuto() {
        let state = AppState.shared
        state.recapModelOverride = ""
        let config = state.toRustConfig(includeRecap: true)
        XCTAssertEqual(config["recap_model_override"] as? String, "")
    }

    func testFullRoundtripPreservesRecapModelOverride() {
        // Round-trip through the explicit Recap-save path. Without
        // includeRecap:true the snapshot would lose the field.
        let state = AppState.shared
        state.recapModelOverride = "gpt-4o"
        let snapshot = state.toRustConfig(includeRecap: true)

        state.recapModelOverride = ""
        state.loadFromRustConfig(snapshot)
        XCTAssertEqual(state.recapModelOverride, "gpt-4o")
    }

    // MARK: - recap_api_url (override for different-provider recap)
    //
    // Mirror of the Win xUnit RecapApiUrl_* tests. Empty default =
    // "inherit llm_api_url". Non-empty = the recap call uses the
    // override URL + vendor-scoped key resolved by Rust.

    private var savedRecapApiUrl: String = ""

    func testApplyConfigPicksUpRecapApiUrl() {
        let state = AppState.shared
        savedRecapApiUrl = state.recapApiUrl
        defer { state.recapApiUrl = savedRecapApiUrl }

        state.recapApiUrl = ""
        state.loadFromRustConfig([
            "recap_api_url": "https://api.anthropic.com/v1/messages"
        ])
        XCTAssertEqual(state.recapApiUrl, "https://api.anthropic.com/v1/messages")
    }

    func testApplyConfigMissingRecapApiUrlClearsToEmpty() {
        // The override is a "set-or-empty" field — a config without
        // the key means "no override" (= inherit). Pin that the
        // loader normalises a missing key to empty rather than
        // preserving a stale prior value (different from
        // recap_model_override which preserves on missing).
        let state = AppState.shared
        savedRecapApiUrl = state.recapApiUrl
        defer { state.recapApiUrl = savedRecapApiUrl }

        state.recapApiUrl = "https://stale.example.com/v1/chat"
        state.loadFromRustConfig(["llm_api_url": "https://api.openai.com/v1"])
        XCTAssertEqual(state.recapApiUrl, "")
    }

    func testToRustConfigEmitsRecapApiUrl() {
        let state = AppState.shared
        savedRecapApiUrl = state.recapApiUrl
        defer { state.recapApiUrl = savedRecapApiUrl }

        state.recapApiUrl =
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        let config = state.toRustConfig(includeRecap: true)
        XCTAssertEqual(
            config["recap_api_url"] as? String,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        )
    }

    func testToRustConfigDefaultOmitsRecapApiUrl() {
        // Wipe protection — default toRustConfig must NOT emit
        // recap_api_url. Pin the contract so a future refactor can't
        // accidentally re-introduce the 2026-05-15 wipe bug.
        let state = AppState.shared
        savedRecapApiUrl = state.recapApiUrl
        defer { state.recapApiUrl = savedRecapApiUrl }

        state.recapApiUrl = "https://api.anthropic.com/v1/messages"
        let config = state.toRustConfig()
        XCTAssertNil(config["recap_api_url"],
                     "recap_api_url must be omitted from default toRustConfig")
    }

    func testToRustConfigDefaultOmitsLlmIdentityFields() {
        // Sibling protection for LLM identity. A non-LLM-page save
        // (theme toggle, hotkey rebind, etc.) must NOT carry these
        // fields — otherwise a transient empty AppState would wipe
        // the user's LLM provider config on disk. Exact bug observed
        // on the user's Win machine, mirrored here for Mac.
        let state = AppState.shared
        // Snapshot+restore in case the singleton has values.
        let savedUrl = state.llmApiUrl
        let savedModel = state.llmApiModel
        let savedAuth = state.llmAuthMethod
        let savedMode = state.llmMode
        defer {
            state.llmApiUrl = savedUrl
            state.llmApiModel = savedModel
            state.llmAuthMethod = savedAuth
            state.llmMode = savedMode
        }
        state.llmApiUrl = "https://api.anthropic.com/v1/messages"
        state.llmApiModel = "claude-opus-4-7"
        state.llmAuthMethod = "subscription"
        state.llmMode = "cloud"
        let config = state.toRustConfig()
        XCTAssertNil(config["llm_api_url"], "llm_api_url must be omitted")
        XCTAssertNil(config["llm_api_model"], "llm_api_model must be omitted")
        XCTAssertNil(config["llm_auth_method"], "llm_auth_method must be omitted")
        XCTAssertNil(config["llm_mode"], "llm_mode must be omitted")
        XCTAssertNil(config["llm_use_same_key"], "llm_use_same_key must be omitted")
        XCTAssertNil(config["llm_enabled"], "llm_enabled must be omitted")
        XCTAssertNil(config["local_llm_model"], "local_llm_model must be omitted")
        // llm_style is a user preference, stays default-emitted.
        XCTAssertNotNil(config["llm_style"])
    }

    func testToRustConfigIncludeLlmEmitsAllLlmIdentityFields() {
        let state = AppState.shared
        let savedUrl = state.llmApiUrl
        let savedModel = state.llmApiModel
        let savedAuth = state.llmAuthMethod
        defer {
            state.llmApiUrl = savedUrl
            state.llmApiModel = savedModel
            state.llmAuthMethod = savedAuth
        }
        state.llmApiUrl = "https://api.anthropic.com/v1/messages"
        state.llmApiModel = "claude-opus-4-7"
        state.llmAuthMethod = "subscription"
        let config = state.toRustConfig(includeLlm: true)
        XCTAssertEqual(config["llm_api_url"] as? String,
                       "https://api.anthropic.com/v1/messages")
        XCTAssertEqual(config["llm_api_model"] as? String,
                       "claude-opus-4-7")
        XCTAssertEqual(config["llm_auth_method"] as? String,
                       "subscription")
    }

    func testToRustConfigEmitsEmptyForInheritDefault() {
        let state = AppState.shared
        savedRecapApiUrl = state.recapApiUrl
        defer { state.recapApiUrl = savedRecapApiUrl }

        state.recapApiUrl = ""
        let config = state.toRustConfig(includeRecap: true)
        XCTAssertEqual(config["recap_api_url"] as? String, "")
    }

    func testFullRoundtripPreservesRecapApiUrl() {
        let state = AppState.shared
        savedRecapApiUrl = state.recapApiUrl
        defer { state.recapApiUrl = savedRecapApiUrl }

        state.recapApiUrl = "https://my-private-proxy.internal/v1/chat"
        let snapshot = state.toRustConfig(includeRecap: true)
        state.recapApiUrl = "something else"
        state.loadFromRustConfig(snapshot)
        XCTAssertEqual(state.recapApiUrl, "https://my-private-proxy.internal/v1/chat")
    }
}

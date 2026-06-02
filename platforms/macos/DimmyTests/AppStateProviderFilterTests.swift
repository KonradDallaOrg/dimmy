import XCTest
@testable import Dimmy

// MARK: - AppStateProviderFilterTests
//
// Tests that the `available*Presets()` helpers filter the picker lists
// to only show providers whose keys are actually saved. These power the
// MacVoicePage / MacOutputPage filtered dropdowns, the Mac equivalent
// of the "keys live only on Providers and keys" model: STT / LLM /
// Recap pickers should never offer a provider the user hasn't connected.
//
// AppState is @MainActor isolated.

@MainActor
final class AppStateProviderFilterTests: XCTestCase {

    private var saved: SavedKeyState = SavedKeyState()

    struct SavedKeyState {
        var hasGroqKey = false
        var hasOpenaiKey = false
        var hasOpenrouterKey = false
        var hasGeminiKey = false
        var hasDeepgramKey = false
        var hasFireworksKey = false
        var hasTogetherKey = false
        var llmKeyByVendor: [String: Bool] = [:]
        var recapKeyByVendor: [String: Bool] = [:]
        var claudeCodeReady = false
    }

    override func setUp() {
        super.setUp()
        let s = AppState.shared
        saved = SavedKeyState(
            hasGroqKey: s.hasGroqKey,
            hasOpenaiKey: s.hasOpenaiKey,
            hasOpenrouterKey: s.hasOpenrouterKey,
            hasGeminiKey: s.hasGeminiKey,
            hasDeepgramKey: s.hasDeepgramKey,
            hasFireworksKey: s.hasFireworksKey,
            hasTogetherKey: s.hasTogetherKey,
            llmKeyByVendor: s.llmKeyByVendor,
            recapKeyByVendor: s.recapKeyByVendor,
            claudeCodeReady: s.claudeCodeReady
        )
        clearAllKeys()
    }

    override func tearDown() {
        let s = AppState.shared
        s.hasGroqKey = saved.hasGroqKey
        s.hasOpenaiKey = saved.hasOpenaiKey
        s.hasOpenrouterKey = saved.hasOpenrouterKey
        s.hasGeminiKey = saved.hasGeminiKey
        s.hasDeepgramKey = saved.hasDeepgramKey
        s.hasFireworksKey = saved.hasFireworksKey
        s.hasTogetherKey = saved.hasTogetherKey
        s.llmKeyByVendor = saved.llmKeyByVendor
        s.recapKeyByVendor = saved.recapKeyByVendor
        s.claudeCodeReady = saved.claudeCodeReady
        super.tearDown()
    }

    private func clearAllKeys() {
        let s = AppState.shared
        s.hasGroqKey = false
        s.hasOpenaiKey = false
        s.hasOpenrouterKey = false
        s.hasGeminiKey = false
        s.hasDeepgramKey = false
        s.hasFireworksKey = false
        s.hasTogetherKey = false
        s.llmKeyByVendor = [:]
        s.recapKeyByVendor = [:]
        s.claudeCodeReady = false
    }

    // MARK: - availableSttPresets

    func testSttFilterCustomAlwaysPresent() {
        let presets = AppState.shared.availableSttPresets()
        XCTAssertTrue(presets.contains { $0.provider == .custom },
                      "Custom STT preset must always be available even with no keys")
    }

    func testSttFilterEmptyKeystoreShowsOnlyCustom() {
        let presets = AppState.shared.availableSttPresets()
        XCTAssertEqual(presets.count, 1,
                       "With no keys saved, only Custom should be in the picker")
        XCTAssertEqual(presets.first?.provider, .custom)
    }

    func testSttFilterGroqOnlyWhenGroqKeySaved() {
        let s = AppState.shared
        s.hasGroqKey = true
        let presets = s.availableSttPresets()
        XCTAssertTrue(presets.contains { $0.provider == .groq })
        XCTAssertFalse(presets.contains { $0.provider == .openai })
        XCTAssertFalse(presets.contains { $0.provider == .gemini })
    }

    func testSttFilterAcceptsLlmScopeKey() {
        // Reality: users often set keys only via the LLM Output card
        // (legacy path) or via the Providers page (which writes all
        // applicable scopes, but only for vendors capable of both). A
        // Groq LLM-scope key must still surface Groq STT — same key
        // works on the audio endpoint, the Rust dispatcher reuses it.
        let s = AppState.shared
        s.llmKeyByVendor["groq"] = true
        let presets = s.availableSttPresets()
        XCTAssertTrue(presets.contains { $0.provider == .groq },
                      "Groq must appear in STT picker when only the LLM-scope Groq key is saved (key reuse)")
    }

    func testSttFilterAcceptsRecapScopeKey() {
        let s = AppState.shared
        s.recapKeyByVendor["openai"] = true
        let presets = s.availableSttPresets()
        XCTAssertTrue(presets.contains { $0.provider == .openai },
                      "OpenAI must appear in STT picker when only the recap-scope OpenAI key is saved (key reuse)")
    }

    func testSttFilterAllVendorsWhenAllKeysSaved() {
        let s = AppState.shared
        s.hasGroqKey = true
        s.hasOpenaiKey = true
        s.hasDeepgramKey = true
        s.hasGeminiKey = true
        s.hasFireworksKey = true
        s.hasTogetherKey = true
        let presets = s.availableSttPresets()
        XCTAssertEqual(presets.count, SttPreset.presets.count,
                       "With every STT key saved, every preset should be visible")
    }

    // MARK: - availableLlmPresets

    func testLlmFilterCustomAndClaudeCodeAlwaysPresent() {
        let presets = AppState.shared.availableLlmPresets()
        XCTAssertTrue(presets.contains { $0.id == "custom" },
                      "Custom LLM preset must always be available")
        XCTAssertTrue(presets.contains { $0.id == "claude-code" },
                      "Claude Code subscription must always be available — auth handled by local CLI, no API key needed")
    }

    func testLlmFilterEmptyKeystoreShowsOnlyCustomPlusSubscription() {
        let presets = AppState.shared.availableLlmPresets()
        XCTAssertEqual(presets.count, 2)
        XCTAssertTrue(presets.allSatisfy { $0.id == "custom" || $0.id == "claude-code" })
    }

    func testLlmFilterGroqAddsGroqVariants() {
        let s = AppState.shared
        s.llmKeyByVendor["groq"] = true
        let presets = s.availableLlmPresets()
        XCTAssertTrue(presets.contains { $0.apiUrl.contains("groq.com") })
        XCTAssertFalse(presets.contains { $0.apiUrl.contains("openai.com") })
    }

    func testLlmFilterAnthropicAddsCloudWhenKeySaved() {
        let s = AppState.shared
        s.llmKeyByVendor["anthropic"] = true
        let presets = s.availableLlmPresets()
        XCTAssertTrue(presets.contains { $0.apiUrl.contains("anthropic.com") })
    }

    // MARK: - availableRecapModels

    func testRecapFilterAutoAndLocalAlwaysPresent() {
        let opts = AppState.shared.availableRecapModels()
        XCTAssertTrue(opts.contains { $0.provider == .auto },
                      "Auto recap option must always be available — it resolves at call time")
        XCTAssertTrue(opts.contains { $0.provider == .local },
                      "Local Gemma recap options must always be available — no key needed")
    }

    func testRecapFilterEmptyKeystoreHidesAllCloud() {
        let opts = AppState.shared.availableRecapModels()
        XCTAssertFalse(opts.contains { $0.provider == .anthropic })
        XCTAssertFalse(opts.contains { $0.provider == .gemini })
        XCTAssertFalse(opts.contains { $0.provider == .openai })
    }

    func testRecapFilterClaudeCodeReadyShowsAnthropic() {
        let s = AppState.shared
        s.claudeCodeReady = true
        let opts = s.availableRecapModels()
        XCTAssertTrue(opts.contains { $0.provider == .anthropic },
                      "Anthropic recap should surface when Claude Code subscription is connected even without an Anthropic API key")
    }

    func testRecapFilterAnthropicRecapKeyShowsAnthropic() {
        let s = AppState.shared
        s.recapKeyByVendor["anthropic"] = true
        let opts = s.availableRecapModels()
        XCTAssertTrue(opts.contains { $0.provider == .anthropic })
    }

    func testRecapFilterFallsBackToLlmScope() {
        let s = AppState.shared
        s.llmKeyByVendor["gemini"] = true
        let opts = s.availableRecapModels()
        XCTAssertTrue(opts.contains { $0.provider == .gemini },
                      "Gemini recap should be available when the LLM-scope Gemini key is saved (recapUseSameKey fallback)")
    }
}

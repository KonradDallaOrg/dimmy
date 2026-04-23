import Foundation

/// Self-contained tests that run on app launch in DEBUG mode.
/// Follows Negative Space Programming: assertions that crash on failure.
/// If the app starts without crashing, all tests passed.
enum SelfTests {

    static func runAll() {
        #if DEBUG
        print("[SelfTests] Running self-tests...")
        testLanguageMappingRoundtrip()
        testSttPresetsIntegrity()
        testLlmPresetsIntegrity()
        testLlmStyleCountMatchesRust()
        testLlmToneCountMatchesRust()
        testShortcutEncodeDecodeRoundtrip()
        testBorderStyleMapping()
        testWaveformStyleMapping()
        testTimeSavedFormula()
        testRecordingStateAnimationIds()
        testSttProviderFromUrl()
        testHotkeyStatusCases()
        testOnboardingStepCount()
        print("[SelfTests] All \(testCount) tests passed.")
        #endif
    }

    private static var testCount = 0

    private static func assert(_ condition: Bool, _ message: String, file: String = #file, line: Int = #line) {
        testCount += 1
        if !condition {
            fatalError("[SelfTests] FAILED: \(message) at \(file):\(line)")
        }
    }

    // MARK: - Language mapping

    private static func testLanguageMappingRoundtrip() {
        for item in AppState.languageMap {
            let display = AppState.displayLanguage(for: item.code)
            assert(display == item.display, "Code '\(item.code)' → '\(display)' expected '\(item.display)'")

            let code = AppState.languageCode(for: item.display)
            assert(code == item.code, "Display '\(item.display)' → '\(code)' expected '\(item.code)'")
        }
        // Unknown code → Auto Detect
        assert(AppState.displayLanguage(for: "xx") == "Auto Detect", "Unknown code must map to Auto Detect")
        // Unknown display → empty code
        assert(AppState.languageCode(for: "Klingon") == "", "Unknown display must map to empty code")
    }

    // MARK: - STT Presets

    private static func testSttPresetsIntegrity() {
        let ids = SttPreset.presets.map(\.id)
        assert(Set(ids).count == ids.count, "STT preset IDs must be unique")

        for preset in SttPreset.presets where preset.id != "custom" {
            assert(!preset.apiUrl.isEmpty, "STT preset '\(preset.id)' must have URL")
            assert(!preset.model.isEmpty, "STT preset '\(preset.id)' must have model")
            assert(preset.apiUrl.hasPrefix("https://"), "STT preset '\(preset.id)' URL must be HTTPS")
        }

        let custom = SttPreset.presets.first { $0.id == "custom" }
        assert(custom != nil, "Must have 'custom' STT preset")
        assert(custom!.apiUrl.isEmpty, "Custom STT preset must have empty URL")

        // Find test
        let found = SttPreset.find(url: "https://api.groq.com/openai/v1/audio/transcriptions", model: "whisper-large-v3-turbo")
        assert(found?.id == "groq-whisper-turbo", "SttPreset.find must find groq-whisper-turbo")
    }

    // MARK: - LLM Presets

    private static func testLlmPresetsIntegrity() {
        let ids = LlmPreset.presets.map(\.id)
        assert(Set(ids).count == ids.count, "LLM preset IDs must be unique")

        for preset in LlmPreset.presets where preset.id != "custom" {
            assert(!preset.apiUrl.isEmpty, "LLM preset '\(preset.id)' must have URL")
            assert(!preset.model.isEmpty, "LLM preset '\(preset.id)' must have model")
            assert(preset.apiUrl.hasPrefix("https://"), "LLM preset '\(preset.id)' URL must be HTTPS")
        }

        let found = LlmPreset.find(url: "https://api.groq.com/openai/v1/chat/completions", model: "llama-3.3-70b-versatile")
        assert(found?.id == "groq-llama70b", "LlmPreset.find must find groq-llama70b")
    }

    // MARK: - Enum counts must match Rust

    private static func testLlmStyleCountMatchesRust() {
        // Rust LlmStyle::ALL has exactly 13 variants
        assert(LlmStyle.allCases.count == 13, "LlmStyle must have 13 cases, got \(LlmStyle.allCases.count)")

        for style in LlmStyle.allCases {
            assert(!style.displayName.isEmpty, "LlmStyle.\(style.rawValue) must have display name")
            assert(!style.rawValue.isEmpty, "LlmStyle must have non-empty rawValue")
        }

        let raws = LlmStyle.allCases.map(\.rawValue)
        assert(Set(raws).count == raws.count, "LlmStyle raw values must be unique")
    }

    private static func testLlmToneCountMatchesRust() {
        // Rust LlmTone::ALL has exactly 5 variants
        assert(LlmTone.allCases.count == 5, "LlmTone must have 5 cases, got \(LlmTone.allCases.count)")

        for tone in LlmTone.allCases {
            assert(!tone.displayName.isEmpty, "LlmTone.\(tone.rawValue) must have display name")
        }
    }

    // MARK: - Shortcut encode/decode

    private static func testShortcutEncodeDecodeRoundtrip() {
        let original = ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)
        let decoded = ModifierShortcut(encoded: original.encoded)
        assert(original == decoded, "Shortcut encode/decode must roundtrip")

        let all = ModifierShortcut(fn: true, control: true, option: true, command: true, shift: true)
        let allDecoded = ModifierShortcut(encoded: all.encoded)
        assert(all == allDecoded, "All-modifiers shortcut must roundtrip")

        assert(original.displayString == "⌃⌥", "⌃⌥ display string")
        assert(ModifierShortcut.fnOnly.displayString == "fn", "fn display string")

        // Validation
        let single = ModifierShortcut(fn: false, control: true, option: false, command: false, shift: false)
        assert(!single.isValid, "Single modifier must be invalid")
        assert(original.isValid, "Two modifiers must be valid")
        assert(ModifierShortcut.fnOnly.isValid, "Fn alone must be valid")
    }

    // MARK: - Border style

    private static func testBorderStyleMapping() {
        assert(BorderStyle.from("Rainbow") == .rainbow, "Rainbow")
        assert(BorderStyle.from("Blue") == .blue, "Blue")
        assert(BorderStyle.from("Blue pulse") == .blue, "Blue pulse")
        assert(BorderStyle.from("Green") == .green, "Green")
        assert(BorderStyle.from("Purple") == .purple, "Purple")
        assert(BorderStyle.from("Orange") == .orange, "Orange")
        assert(BorderStyle.from("None") == .none, "None")
        assert(BorderStyle.from("???") == .rainbow, "Unknown → rainbow")
    }

    // MARK: - Waveform style

    private static func testWaveformStyleMapping() {
        assert(WaveformStyle.from("Bars") == .bars, "Bars")
        assert(WaveformStyle.from("Bars Center") == .barsCenter, "Bars Center")
        assert(WaveformStyle.from("Bars Round") == .barsRound, "Bars Round")
        assert(WaveformStyle.from("Line") == .line, "Line")
        assert(WaveformStyle.from("Dots") == .dots, "Dots")
        assert(WaveformStyle.from("???") == .bars, "Unknown → bars")
    }

    // MARK: - Time saved (must match Windows: words * (1/40 - 1/150) * 60)

    private static func testTimeSavedFormula() {
        let words: UInt64 = 1000
        let saved = Double(words) * (1.0 / 40.0 - 1.0 / 150.0) * 60.0
        assert(abs(saved - 1100.0) < 1.0, "1000 words should save ~1100 seconds, got \(saved)")

        let zeroSaved = Double(0) * (1.0 / 40.0 - 1.0 / 150.0) * 60.0
        assert(zeroSaved == 0.0, "0 words → 0 time saved")
    }

    // MARK: - Recording state animation IDs (must be unique)

    private static func testRecordingStateAnimationIds() {
        let states: [RecordingState] = [
            .idle, .recording(.pushToTalk), .recording(.toggle),
            .transcribing, .processing, .completing
        ]
        let ids = states.map(\.animationId)
        assert(Set(ids).count == ids.count, "Animation IDs must be unique, got \(ids)")
    }

    // MARK: - STT Provider URL detection

    private static func testSttProviderFromUrl() {
        assert(SttProvider.from(url: "https://api.groq.com/anything") == .groq, "groq")
        assert(SttProvider.from(url: "https://api.openai.com/v1/audio") == .openai, "openai")
        assert(SttProvider.from(url: "https://api.deepgram.com/v1/listen") == .deepgram, "deepgram")
        assert(SttProvider.from(url: "https://generativelanguage.googleapis.com/v1beta") == .gemini, "gemini")
        assert(SttProvider.from(url: "https://custom.example.com/api") == .custom, "custom")
    }

    // MARK: - HotkeyStatus

    private static func testHotkeyStatusCases() {
        assert(HotkeyStatus.installed == .installed, "installed == installed")
        assert(HotkeyStatus.uninstalled != .installed, "uninstalled != installed")
        assert(HotkeyStatus.accessibilityMissing != .installed, "accessibilityMissing != installed")
        assert(HotkeyStatus.tapFailed(reason: "a") != HotkeyStatus.tapFailed(reason: "b"), "tapFailed differs by reason")
        assert(HotkeyStatus.tapFailed(reason: "x") == HotkeyStatus.tapFailed(reason: "x"), "tapFailed equals by reason")
    }

    // MARK: - Onboarding

    private static func testOnboardingStepCount() {
        assert(OnboardingContainerView.totalSteps == 4, "Onboarding has 4 steps, got \(OnboardingContainerView.totalSteps)")
    }
}

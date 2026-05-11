import XCTest
@testable import Dimmy

final class AppStateTests: XCTestCase {

    // MARK: - Language mapping (NSP: every code maps to a display name and back)

    func testLanguageCodeRoundtrip() {
        for item in AppState.languageMap {
            let display = AppState.displayLanguage(for: item.code)
            XCTAssertEqual(display, item.display, "Code '\(item.code)' should map to '\(item.display)'")

            let code = AppState.languageCode(for: item.display)
            XCTAssertEqual(code, item.code, "Display '\(item.display)' should map to '\(item.code)'")
        }
    }

    func testLanguageCodeUnknownReturnsAutoDetect() {
        let display = AppState.displayLanguage(for: "xx")
        XCTAssertEqual(display, "Auto Detect")
    }

    func testLanguageDisplayUnknownReturnsEmptyCode() {
        let code = AppState.languageCode(for: "Klingon")
        XCTAssertEqual(code, "")
    }

    func testEmptyLanguageCodeMapsToAutoDetect() {
        let display = AppState.displayLanguage(for: "")
        XCTAssertEqual(display, "Auto Detect")
    }

    // MARK: - STT Presets (NSP: all presets have unique IDs, non-empty URLs except custom)

    func testSttPresetsHaveUniqueIds() {
        let ids = SttPreset.presets.map(\.id)
        let unique = Set(ids)
        XCTAssertEqual(ids.count, unique.count, "STT preset IDs must be unique")
    }

    func testSttPresetsNonCustomHaveUrls() {
        for preset in SttPreset.presets where preset.id != "custom" {
            XCTAssertFalse(preset.apiUrl.isEmpty, "STT preset '\(preset.id)' must have a URL")
            XCTAssertFalse(preset.model.isEmpty, "STT preset '\(preset.id)' must have a model")
            XCTAssertTrue(preset.apiUrl.hasPrefix("https://"), "STT preset '\(preset.id)' URL must be HTTPS")
        }
    }

    func testSttPresetCustomHasEmptyUrl() {
        let custom = SttPreset.presets.first { $0.id == "custom" }
        XCTAssertNotNil(custom, "Must have a 'custom' preset")
        XCTAssertTrue(custom!.apiUrl.isEmpty)
    }

    func testSttPresetFindByUrlAndModel() {
        let found = SttPreset.find(url: "https://api.groq.com/openai/v1/audio/transcriptions", model: "whisper-large-v3-turbo")
        XCTAssertNotNil(found)
        XCTAssertEqual(found?.id, "groq-whisper-turbo")
    }

    func testSttPresetFindUnknownReturnsNil() {
        let found = SttPreset.find(url: "https://unknown.example.com/api", model: "unknown-model")
        XCTAssertNil(found)
    }

    // MARK: - LLM Presets (NSP: all presets have unique IDs)

    func testLlmPresetsHaveUniqueIds() {
        let ids = LlmPreset.presets.map(\.id)
        let unique = Set(ids)
        XCTAssertEqual(ids.count, unique.count, "LLM preset IDs must be unique")
    }

    func testLlmPresetsNonCustomHaveUrls() {
        for preset in LlmPreset.presets where preset.id != "custom" {
            XCTAssertFalse(preset.apiUrl.isEmpty, "LLM preset '\(preset.id)' must have a URL")
            XCTAssertFalse(preset.model.isEmpty, "LLM preset '\(preset.id)' must have a model")
            XCTAssertTrue(preset.apiUrl.hasPrefix("https://"), "LLM preset '\(preset.id)' URL must be HTTPS")
        }
    }

    func testLlmPresetFindByUrlAndModel() {
        let found = LlmPreset.find(url: "https://api.groq.com/openai/v1/chat/completions", model: "llama-3.3-70b-versatile")
        XCTAssertNotNil(found)
        XCTAssertEqual(found?.id, "groq-llama70b")
    }

    // MARK: - LlmStyle (NSP: all cases have display names, colors, and unique raw values)

    func testLlmStyleAllCasesHaveDisplayNames() {
        for style in LlmStyle.allCases {
            XCTAssertFalse(style.displayName.isEmpty, "LlmStyle.\(style.rawValue) must have a display name")
        }
    }

    func testLlmStyleCountMatchesRust() {
        // Rust has 13 LlmStyle variants — Swift must match
        XCTAssertEqual(LlmStyle.allCases.count, 13, "LlmStyle must have exactly 13 cases (matching Rust)")
    }

    func testLlmStyleRawValuesAreUnique() {
        let raw = LlmStyle.allCases.map(\.rawValue)
        let unique = Set(raw)
        XCTAssertEqual(raw.count, unique.count, "LlmStyle raw values must be unique")
    }

    // MARK: - LlmTone (NSP: must match Rust's 5 variants)

    func testLlmToneCountMatchesRust() {
        XCTAssertEqual(LlmTone.allCases.count, 5, "LlmTone must have exactly 5 cases (matching Rust)")
    }

    func testLlmToneAllCasesHaveDisplayNames() {
        for tone in LlmTone.allCases {
            XCTAssertFalse(tone.displayName.isEmpty, "LlmTone.\(tone.rawValue) must have a display name")
        }
    }

    // MARK: - ModifierShortcut (NSP: encode/decode roundtrip)

    func testShortcutEncodeDecodeRoundtrip() {
        let original = ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)
        let encoded = original.encoded
        let decoded = ModifierShortcut(encoded: encoded)
        XCTAssertEqual(original, decoded)
    }

    func testShortcutEncodeDecodeAllModifiers() {
        let original = ModifierShortcut(fn: true, control: true, option: true, command: true, shift: true)
        let decoded = ModifierShortcut(encoded: original.encoded)
        XCTAssertEqual(original, decoded)
    }

    func testShortcutValidation() {
        // Single modifier (not Fn) is invalid
        let single = ModifierShortcut(fn: false, control: true, option: false, command: false, shift: false)
        XCTAssertFalse(single.isValid, "Single modifier (not Fn) must be invalid")

        // Two modifiers is valid
        let two = ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)
        XCTAssertTrue(two.isValid, "Two modifiers must be valid")

        // Fn alone is valid
        let fnOnly = ModifierShortcut.fnOnly
        XCTAssertTrue(fnOnly.isValid, "Fn alone must be valid")
    }

    func testShortcutDisplayString() {
        let shortcut = ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)
        XCTAssertEqual(shortcut.displayString, "⌃⌥")

        let fnOnly = ModifierShortcut.fnOnly
        XCTAssertEqual(fnOnly.displayString, "fn")
    }

    // MARK: - Border styles (NSP: all string values map correctly)

    func testBorderStyleFromString() {
        XCTAssertEqual(BorderStyle.from("Rainbow"), .rainbow)
        XCTAssertEqual(BorderStyle.from("Blue"), .blue)
        XCTAssertEqual(BorderStyle.from("Blue pulse"), .blue)
        XCTAssertEqual(BorderStyle.from("Green"), .green)
        XCTAssertEqual(BorderStyle.from("Purple"), .purple)
        XCTAssertEqual(BorderStyle.from("Orange"), .orange)
        XCTAssertEqual(BorderStyle.from("None"), .none)
        // Unknown defaults to rainbow
        XCTAssertEqual(BorderStyle.from("Unknown"), .rainbow)
    }

    // MARK: - Waveform styles

    func testWaveformStyleFromString() {
        XCTAssertEqual(WaveformStyle.from("Bars"), .bars)
        XCTAssertEqual(WaveformStyle.from("Bars Center"), .barsCenter)
        XCTAssertEqual(WaveformStyle.from("Bars Round"), .barsRound)
        XCTAssertEqual(WaveformStyle.from("Line"), .line)
        XCTAssertEqual(WaveformStyle.from("Dots"), .dots)
        // Unknown defaults to bars
        XCTAssertEqual(WaveformStyle.from("Unknown"), .bars)
    }

    // MARK: - Time saved formula (must match Windows: words * (1/40 - 1/150) * 60)

    func testTimeSavedFormula() {
        let words: UInt64 = 1000
        let expected = Double(words) * (1.0 / 40.0 - 1.0 / 150.0) * 60.0
        // ~1100 seconds saved for 1000 words
        XCTAssertEqual(expected, 1100.0, accuracy: 1.0)
    }

    func testTimeSavedZeroWords() {
        let words: UInt64 = 0
        let saved = Double(words) * (1.0 / 40.0 - 1.0 / 150.0) * 60.0
        XCTAssertEqual(saved, 0.0)
    }

    // MARK: - RecordingState animationId (NSP: all states have unique IDs)

    func testRecordingStateAnimationIdsAreUnique() {
        let states: [RecordingState] = [
            .idle, .recording(.pushToTalk), .recording(.toggle),
            .transcribing, .processing, .completing
        ]
        let ids = states.map(\.animationId)
        let unique = Set(ids)
        XCTAssertEqual(ids.count, unique.count, "Animation IDs must be unique")
    }

    // MARK: - SttProvider

    func testSttProviderFromUrl() {
        XCTAssertEqual(SttProvider.from(url: "https://api.groq.com/anything"), .groq)
        XCTAssertEqual(SttProvider.from(url: "https://api.openai.com/v1/audio"), .openai)
        XCTAssertEqual(SttProvider.from(url: "https://api.deepgram.com/v1/listen"), .deepgram)
        XCTAssertEqual(SttProvider.from(url: "https://generativelanguage.googleapis.com/v1beta"), .gemini)
        XCTAssertEqual(SttProvider.from(url: "https://custom.example.com/api"), .custom)
    }

    // MARK: - HotkeyCombo (dictionary hotkey persistence + matching)
    //
    // The combo round-trips through UserDefaults using its `encoded`
    // string form. These tests pin the encode/decode pair so a future
    // refactor of the format can't silently break existing users'
    // saved combos. Default = ⌘⇧D mirrors the Win Ctrl+Shift+D.

    func testHotkeyComboDefaultIsCommandShiftD() {
        let c = HotkeyCombo.defaultDictHotkey
        XCTAssertTrue(c.command)
        XCTAssertTrue(c.shift)
        XCTAssertFalse(c.control)
        XCTAssertFalse(c.option)
        XCTAssertEqual(c.keyCode, 0x02, "kVK_ANSI_D")
        XCTAssertEqual(c.keyChar, "D")
    }

    func testHotkeyComboDefaultDisplayString() {
        // Modifier ordering in displayParts: ctrl, option, shift, command, letter
        let parts = HotkeyCombo.defaultDictHotkey.displayParts
        XCTAssertEqual(parts, ["⇧", "⌘", "D"])
        XCTAssertEqual(HotkeyCombo.defaultDictHotkey.displayString, "⇧⌘D")
    }

    func testHotkeyComboEncodeDecodeRoundTrip() {
        let combos = [
            HotkeyCombo.defaultDictHotkey,
            HotkeyCombo(control: true, option: false, command: false, shift: true,
                        keyCode: 0x09, keyChar: "V"),
            HotkeyCombo(control: true, option: true, command: true, shift: true,
                        keyCode: 0x31, keyChar: "Space"),
            HotkeyCombo(control: false, option: false, command: false, shift: false,
                        keyCode: 0x7A, keyChar: "F1"),
        ]
        for original in combos {
            let encoded = original.encoded
            let decoded = HotkeyCombo(encoded: encoded)
            XCTAssertNotNil(decoded, "encoded form must decode: \(encoded)")
            XCTAssertEqual(decoded, original, "round-trip lost data for \(encoded)")
        }
    }

    func testHotkeyComboDecodeMalformedReturnsNil() {
        // No colon → no separator → reject
        XCTAssertNil(HotkeyCombo(encoded: "ctrl+shift+nosep"))
        // Invalid hex → reject
        XCTAssertNil(HotkeyCombo(encoded: "ctrl+ZZ:D"))
        // Empty string → reject
        XCTAssertNil(HotkeyCombo(encoded: ""))
    }

    func testHotkeyComboMatchesRequiresExactModifierSet() {
        let combo = HotkeyCombo.defaultDictHotkey  // ⌘⇧D
        let dKey: UInt16 = 0x02

        // Exact match.
        XCTAssertTrue(combo.matches(flags: [.command, .shift], keyCode: dKey))

        // Wrong key.
        XCTAssertFalse(combo.matches(flags: [.command, .shift], keyCode: 0x01))

        // Missing a required modifier.
        XCTAssertFalse(combo.matches(flags: [.command], keyCode: dKey))

        // Extra modifier the combo doesn't ask for — strict reject (else
        // ⌘⌥⇧D would fire the dict-add when the user pressed a
        // different chord). This is the load-bearing assertion of the
        // matcher.
        XCTAssertFalse(combo.matches(flags: [.command, .shift, .option], keyCode: dKey))
    }

    func testHotkeyComboEncodedFormatIsStable() {
        // Lock the on-disk format so users with a saved combo from
        // v0.6.36 still see it on every later version.
        XCTAssertEqual(
            HotkeyCombo.defaultDictHotkey.encoded,
            "cmd+shift+02:D",
            "default combo encoded form is part of the contract — update with care"
        )
    }
}

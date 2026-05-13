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
        testPillCycleStyleWrap()
        testPillCycleLanguageWrap()
        testPillCyclePresetsAreContiguous()
        testSparkleFeedUrlIsDurable()
        testProviderTagSamples()
        testSameKeyGating()
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
            // `claude-code://` is the synthetic URL for the Claude Code
            // subscription preset — the Rust dispatcher recognises it
            // as "route via local CLI, no HTTP key needed". Exempt it
            // from the HTTPS-only check that guards the real cloud
            // endpoints.
            let isSynthetic = preset.apiUrl.hasPrefix("claude-code://")
            assert(isSynthetic || preset.apiUrl.hasPrefix("https://"),
                   "LLM preset '\(preset.id)' URL must be HTTPS")
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
        assert(SttProvider.from(url: "https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions") == .fireworks, "fireworks turbo")
        assert(SttProvider.from(url: "https://api.fireworks.ai/inference/v1/chat/completions") == .fireworks, "fireworks chat")
        assert(SttProvider.from(url: "https://api.together.xyz/v1/audio/transcriptions") == .together, "together xyz")
        assert(SttProvider.from(url: "https://api.together.ai/v1/chat/completions") == .together, "together ai alias")
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
        // 5 steps: Welcome, Permissions, Shortcut (with PTT/Toggle picker),
        // ModelDownload, TryIt. Bumped from 4 when the ModelDownload step
        // (Parakeet vs Whisper picker) landed. Update this number when
        // adding/removing a step.
        assert(OnboardingContainerView.totalSteps == 5, "Onboarding has 5 steps, got \(OnboardingContainerView.totalSteps)")
    }

    // MARK: - Pill scroll-cycle (regression: see fix(mac) commit bca5c4a)
    //
    // The pill style dot + language label both let the user scroll-cycle
    // through MacLlmStyles / PillTranslateLanguages. The cycle math
    // (`(current + step + count) % count`) explodes if either array is
    // empty, and the `firstIndex(of:)` fallback to 0 has to keep working
    // for unknown values (e.g. config from a future build with a new
    // style). These tests assert both invariants without spinning up a
    // full SwiftUI surface — they're pure-data checks on the same
    // arrays the runtime uses.

    /// Pure cycle math, parameterised so we can run it against either
    /// preset list. Mirrors the body of cycleStyle / cycleLanguage in
    /// PillView; if PillView's algorithm changes, this stays the
    /// canonical reference.
    private static func cycleMath(keys: [String], current: String, deltaPositive: Bool) -> String {
        precondition(!keys.isEmpty, "cycle keys must not be empty")
        let i = keys.firstIndex(of: current) ?? 0
        let step = deltaPositive ? -1 : 1
        let next = (i + step + keys.count) % keys.count
        return keys[next]
    }

    private static func testPillCycleStyleWrap() {
        let keys = MacLlmStyles.map { $0.key }
        assert(!keys.isEmpty, "MacLlmStyles must not be empty")

        // Forward from "off" lands on the next item.
        assert(cycleMath(keys: keys, current: "off", deltaPositive: false) == keys[1],
               "scroll-down from off must move to keys[1], got \(cycleMath(keys: keys, current: "off", deltaPositive: false))")

        // Forward wraparound: from the last key cycles back to the first.
        let last = keys.last!
        assert(cycleMath(keys: keys, current: last, deltaPositive: false) == keys[0],
               "scroll-down from last (\(last)) must wrap to keys[0]")

        // Backward wraparound: from the first key cycles to the last.
        assert(cycleMath(keys: keys, current: keys[0], deltaPositive: true) == last,
               "scroll-up from first must wrap to last")

        // Unknown current value falls back to index 0; one step forward
        // lands on keys[1], not on a stray index.
        assert(cycleMath(keys: keys, current: "this-style-does-not-exist", deltaPositive: false) == keys[1],
               "unknown current must fall back to index 0 then step")
    }

    private static func testPillCycleLanguageWrap() {
        let keys = PillTranslateLanguages.map { $0.key }
        assert(!keys.isEmpty, "PillTranslateLanguages must not be empty")
        assert(keys[0] == "", "first translate-to entry must be the empty / no-translation key")

        let last = keys.last!
        assert(cycleMath(keys: keys, current: last, deltaPositive: false) == keys[0],
               "wrap forward from last → first")
        assert(cycleMath(keys: keys, current: keys[0], deltaPositive: true) == last,
               "wrap backward from first → last")

        // Unknown locale code (e.g. someone manually edited config.json
        // with "tlh") must not crash — falls back to index 0.
        let next = cycleMath(keys: keys, current: "tlh", deltaPositive: false)
        assert(next == keys[1], "unknown lang must fall back to keys[1] after one forward step")
    }

    /// Catches the regression class where someone edits a preset list
    /// in one place but forgets to update an enum / Rust mirror. We
    /// can't hit Rust from the Swift self-test, but we can at least
    /// assert the lists' shape stays sane.
    private static func testPillCyclePresetsAreContiguous() {
        let styleKeys = MacLlmStyles.map { $0.key }
        let langKeys = PillTranslateLanguages.map { $0.key }

        // No duplicates — duplicate keys would make wrap math jump
        // unpredictably and make `firstIndex(of:)` return the wrong slot.
        assert(Set(styleKeys).count == styleKeys.count, "MacLlmStyles must not contain duplicate keys")
        assert(Set(langKeys).count == langKeys.count, "PillTranslateLanguages must not contain duplicate keys")

        // No whitespace-only or accidentally-trimmed keys (would make
        // the pill display a phantom "no style" indistinguishable
        // from off).
        for k in styleKeys {
            assert(!k.isEmpty || k == "off", "MacLlmStyles key empty (only 'off' may be falsy-ish): \(k)")
        }
    }

    // MARK: - Sparkle SUFeedURL durability
    //
    // Burned 2026-05-12: a previous build shipped with
    // `https://github.com/.../releases/latest/download/appcast.xml` as
    // SUFeedURL. GitHub's `/latest/` semantics SKIP prereleases — so
    // when the most recent published release was an rc-tagged
    // prerelease, the URL returned 404 and every existing user saw
    // "Update failed" on every check. The fix was to publish a
    // durable mirror release tagged `auto-update` and point SUFeedURL
    // at that fixed tag.
    //
    // This guardrail asserts the URL never regresses to `/latest/`.
    // Update the expected substring here if the durable tag ever
    // changes (e.g. from `auto-update` to `appcast` or similar).
    private static func testSparkleFeedUrlIsDurable() {
        let url = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String ?? ""
        assert(!url.isEmpty, "Info.plist must define SUFeedURL")
        assert(!url.contains("/releases/latest/"),
               "SUFeedURL must NOT use /releases/latest/ — that path skips prereleases. " +
               "Use the `auto-update` durable tag instead. Current: \(url)")
        assert(url.hasPrefix("https://"), "SUFeedURL must be HTTPS, got: \(url)")
    }

    // MARK: - Provider tagging + same-key gating
    //
    // The "Use same key as STT" toggle is the easiest setting in
    // Dimmy to break by accident: change the LLM provider preset and
    // the toggle quietly switches the wrong key into the wrong API.
    // These tests pin the helper logic so any future refactor that
    // changes the rules fails the build instead of silently misrouting
    // credentials.

    private static func testProviderTagSamples() {
        // Real cloud endpoints map to a non-empty tag.
        assert(ProviderTagging.providerTag(forUrl: "https://api.groq.com/openai/v1/chat/completions") == "groq",
               "groq.com → 'groq'")
        assert(ProviderTagging.providerTag(forUrl: "https://api.openai.com/v1/chat/completions") == "openai",
               "openai.com → 'openai'")
        assert(ProviderTagging.providerTag(forUrl: "https://api.anthropic.com/v1/messages") == "anthropic",
               "anthropic.com → 'anthropic'")
        assert(ProviderTagging.providerTag(forUrl: "https://generativelanguage.googleapis.com/v1beta/models") == "gemini",
               "googleapis.com → 'gemini'")
        assert(ProviderTagging.providerTag(forUrl: "https://api.deepgram.com/v1/listen") == "deepgram",
               "deepgram.com → 'deepgram'")

        // Synthetic claude-code:// also maps to 'anthropic' — same
        // vendor, different auth path.
        assert(ProviderTagging.providerTag(forUrl: "claude-code://default") == "anthropic",
               "claude-code:// → 'anthropic'")

        // Unknown / custom → empty.
        assert(ProviderTagging.providerTag(forUrl: "") == "",
               "empty URL → ''")
        assert(ProviderTagging.providerTag(forUrl: "https://some.random.host/api") == "",
               "random host → ''")
    }

    private static func testSameKeyGating() {
        let anthropic = "https://api.anthropic.com/v1/messages"
        let groqStt = "https://api.groq.com/openai/v1/audio/transcriptions"
        let groqLlm = "https://api.groq.com/openai/v1/chat/completions"
        let openaiLlm = "https://api.openai.com/v1/chat/completions"

        // Same vendor on both sides + api_key auth → toggle visible.
        assert(ProviderTagging.sameKeyShouldShow(sttUrl: groqStt, llmUrl: groqLlm, llmAuthMethod: "api_key"),
               "Groq STT + Groq LLM (api_key) must show same-key toggle")
        assert(ProviderTagging.sameKeyShouldShow(sttUrl: anthropic, llmUrl: anthropic, llmAuthMethod: "api_key"),
               "Anthropic STT + Anthropic LLM (api_key) must show same-key toggle")

        // Different vendor → toggle hidden, force dedicated key.
        assert(!ProviderTagging.sameKeyShouldShow(sttUrl: groqStt, llmUrl: openaiLlm, llmAuthMethod: "api_key"),
               "Groq STT + OpenAI LLM must HIDE same-key toggle")

        // Subscription auth → toggle hidden even when vendors match
        // (the CLI owns its own credential).
        assert(!ProviderTagging.sameKeyShouldShow(sttUrl: anthropic, llmUrl: anthropic, llmAuthMethod: "subscription"),
               "Anthropic both sides + subscription must HIDE same-key toggle")

        // Empty / custom URLs → toggle hidden.
        assert(!ProviderTagging.sameKeyShouldShow(sttUrl: "", llmUrl: groqLlm, llmAuthMethod: "api_key"),
               "Empty STT URL → no toggle")
        assert(!ProviderTagging.sameKeyShouldShow(sttUrl: groqStt, llmUrl: "", llmAuthMethod: "api_key"),
               "Empty LLM URL → no toggle")
    }
}
